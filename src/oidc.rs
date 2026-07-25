use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use openidconnect::{
    AuthType, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreClientAuthMethod, CoreProviderMetadata},
    reqwest,
};
use parking_lot::Mutex;
use rand::{Rng, distr::Alphanumeric};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{server::apply_session_cookies, state::AppState};

const FLOW_COOKIE: &str = "rov_oidc_flow";
const FLOW_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING_FLOWS: usize = 64;

type OidcClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Clone, Debug)]
pub struct OidcConfig {
    issuer: String,
    client_id: String,
    client_secret: String,
    redirect_url: String,
    allowed_subjects: HashSet<String>,
    pending: Arc<Mutex<HashMap<String, PendingFlow>>>,
}

#[derive(Clone, Debug)]
struct PendingFlow {
    browser_token_hash: [u8; 32],
    nonce: String,
    pkce_verifier: String,
    expires_at: Instant,
}

impl OidcConfig {
    pub fn from_values(
        issuer: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
        redirect_url: Option<String>,
        allowed_subjects: Option<String>,
    ) -> anyhow::Result<Option<Self>> {
        let configured = [
            issuer.is_some(),
            client_id.is_some(),
            client_secret.is_some(),
            redirect_url.is_some(),
            allowed_subjects.is_some(),
        ];
        if configured.iter().all(|value| !value) {
            return Ok(None);
        }
        if !configured.iter().all(|value| *value) {
            anyhow::bail!(
                "ROV_OIDC_ISSUER, ROV_OIDC_CLIENT_ID, ROV_OIDC_CLIENT_SECRET, ROV_OIDC_REDIRECT_URL, and ROV_OIDC_ALLOWED_SUBJECTS must be set together"
            );
        }

        let issuer = issuer.unwrap();
        let redirect_url = redirect_url.unwrap();
        validate_url("ROV_OIDC_ISSUER", &issuer, false)?;
        validate_url("ROV_OIDC_REDIRECT_URL", &redirect_url, true)?;
        let allowed_subjects = allowed_subjects
            .unwrap()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        if allowed_subjects.is_empty() {
            anyhow::bail!("ROV_OIDC_ALLOWED_SUBJECTS must contain at least one subject");
        }

        Ok(Some(Self {
            issuer,
            client_id: client_id.unwrap(),
            client_secret: client_secret.unwrap(),
            redirect_url,
            allowed_subjects,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    fn secure_cookie(&self) -> bool {
        self.redirect_url.starts_with("https://")
    }
}

fn validate_url(name: &str, raw: &str, callback: bool) -> anyhow::Result<()> {
    let url =
        reqwest::Url::parse(raw).map_err(|_| anyhow::anyhow!("{name} must be an absolute URL"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        anyhow::bail!("{name} contains forbidden URL parts");
    }
    let secure = url.scheme() == "https"
        || (url.scheme() == "http"
            && url.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            }));
    if !secure {
        anyhow::bail!("{name} must use HTTPS; HTTP is allowed only for loopback testing");
    }
    if callback && url.path() != "/auth/oidc/callback" {
        anyhow::bail!("{name} must use the exact /auth/oidc/callback path");
    }
    Ok(())
}

pub async fn start(State(state): State<Arc<AppState>>) -> Response {
    let Some(config) = state.oidc_config() else {
        return (StatusCode::NOT_FOUND, "LinuxMice sign-in is not configured").into_response();
    };
    let Ok((client, _)) = discover_client(config).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "LinuxMice sign-in is temporarily unavailable",
        )
            .into_response();
    };
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf, nonce) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("profile".into()))
        .set_pkce_challenge(challenge)
        .url();
    let browser_token = random_token();
    {
        let mut pending = config.pending.lock();
        let now = Instant::now();
        pending.retain(|_, flow| flow.expires_at > now);
        if pending.len() >= MAX_PENDING_FLOWS {
            return (StatusCode::TOO_MANY_REQUESTS, "Too many pending sign-ins").into_response();
        }
        pending.insert(
            csrf.secret().clone(),
            PendingFlow {
                browser_token_hash: token_hash(&browser_token),
                nonce: nonce.secret().clone(),
                pkce_verifier: verifier.secret().clone(),
                expires_at: now + FLOW_TTL,
            },
        );
    }

    let mut response = Redirect::to(url.as_str()).into_response();
    if let Ok(cookie) = HeaderValue::from_str(&format!(
        "{FLOW_COOKIE}={browser_token}; Max-Age={}; Path=/auth/oidc/callback; HttpOnly; SameSite=Lax{}",
        FLOW_TTL.as_secs(),
        if config.secure_cookie() {
            "; Secure"
        } else {
            ""
        }
    )) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn callback(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(config) = state.oidc_config() else {
        return callback_error(StatusCode::NOT_FOUND, "LinuxMice sign-in is not configured");
    };
    if query.error.is_some() {
        return callback_error(
            StatusCode::UNAUTHORIZED,
            "LinuxMice sign-in was not completed",
        );
    }
    let (Some(code), Some(returned_state), Some(browser_token)) =
        (query.code, query.state, cookie_value(&headers, FLOW_COOKIE))
    else {
        return callback_error(
            StatusCode::BAD_REQUEST,
            "Invalid LinuxMice sign-in response",
        );
    };
    let pending = config.pending.lock().remove(&returned_state);
    let Some(pending) = pending.filter(|flow| {
        flow.expires_at > Instant::now() && token_hash(&browser_token) == flow.browser_token_hash
    }) else {
        return callback_error(
            StatusCode::BAD_REQUEST,
            "LinuxMice sign-in expired or failed browser verification",
        );
    };

    let Ok((client, http_client)) = discover_client(config).await else {
        return callback_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "LinuxMice sign-in is temporarily unavailable",
        );
    };
    let Ok(request) = client.exchange_code(AuthorizationCode::new(code)) else {
        return callback_error(StatusCode::BAD_GATEWAY, "LinuxMice sign-in failed");
    };
    let Ok(tokens) = request
        .set_pkce_verifier(PkceCodeVerifier::new(pending.pkce_verifier))
        .request_async(&http_client)
        .await
    else {
        return callback_error(StatusCode::BAD_GATEWAY, "LinuxMice sign-in failed");
    };
    let Some(id_token) = tokens.id_token() else {
        return callback_error(StatusCode::BAD_GATEWAY, "LinuxMice returned no identity");
    };
    let Ok(claims) = id_token.claims(&client.id_token_verifier(), &Nonce::new(pending.nonce))
    else {
        return callback_error(
            StatusCode::UNAUTHORIZED,
            "LinuxMice identity verification failed",
        );
    };
    if !config.allowed_subjects.contains(claims.subject().as_str()) {
        return callback_error(
            StatusCode::FORBIDDEN,
            "This LinuxMice identity is not allowed",
        );
    }

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let grant = state.issue_identity_session(user_agent);
    let mut response = Redirect::to("/").into_response();
    if apply_session_cookies(response.headers_mut(), &grant, config.secure_cookie()).is_err() {
        return callback_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not create a session",
        );
    }
    clear_flow_cookie(response.headers_mut(), config.secure_cookie());
    response
}

async fn discover_client(config: &OidcConfig) -> Result<(OidcClient, reqwest::Client), String> {
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    let metadata = CoreProviderMetadata::discover_async(
        IssuerUrl::new(config.issuer.clone()).map_err(|error| error.to_string())?,
        &http_client,
    )
    .await
    .map_err(|error| error.to_string())?;
    let advertised = metadata
        .token_endpoint_auth_methods_supported()
        .map(Vec::as_slice);
    let auth_type = if advertised
        .is_none_or(|methods| methods.contains(&CoreClientAuthMethod::ClientSecretBasic))
    {
        AuthType::BasicAuth
    } else if advertised
        .is_some_and(|methods| methods.contains(&CoreClientAuthMethod::ClientSecretPost))
    {
        AuthType::RequestBody
    } else {
        return Err("OIDC provider has no supported confidential-client method".into());
    };
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.client_id.clone()),
        Some(ClientSecret::new(config.client_secret.clone())),
    )
    .set_auth_type(auth_type)
    .set_redirect_uri(
        RedirectUrl::new(config.redirect_url.clone()).map_err(|error| error.to_string())?,
    );
    Ok((client, http_client))
}

fn random_token() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(43)
        .map(char::from)
        .collect()
}

fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}

fn clear_flow_cookie(headers: &mut HeaderMap, secure: bool) {
    if let Ok(cookie) = HeaderValue::from_str(&format!(
        "{FLOW_COOKIE}=; Max-Age=0; Path=/auth/oidc/callback; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )) {
        headers.append(header::SET_COOKIE, cookie);
    }
}

fn callback_error(status: StatusCode, message: &'static str) -> Response {
    (status, message).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oidc_is_optional_and_partial_configuration_is_rejected() {
        assert!(
            OidcConfig::from_values(None, None, None, None, None)
                .unwrap()
                .is_none()
        );
        assert!(
            OidcConfig::from_values(
                Some("https://identity.example.test".into()),
                None,
                None,
                None,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn oidc_requires_https_and_an_owner_allowlist() {
        assert!(
            OidcConfig::from_values(
                Some("http://identity.example.test".into()),
                Some("rov".into()),
                Some("secret".into()),
                Some("https://rov.example.test/auth/oidc/callback".into()),
                Some("owner".into()),
            )
            .is_err()
        );
        assert!(
            OidcConfig::from_values(
                Some("https://identity.example.test".into()),
                Some("rov".into()),
                Some("secret".into()),
                Some("https://rov.example.test/auth/oidc/callback".into()),
                Some(" , ".into()),
            )
            .is_err()
        );
    }
}
