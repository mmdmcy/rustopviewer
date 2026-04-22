use anyhow::{Context, Result};
use axum::{
    Form, Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{
            CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, ETAG, IF_NONE_MATCH,
            InvalidHeaderValue, LOCATION, PRAGMA, REFERRER_POLICY, SET_COOKIE, USER_AGENT,
        },
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env,
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    thread,
    time::Duration,
};
use tokio::net::TcpListener;

use crate::{
    input::{self, InputRequest},
    network,
    security::{
        IssuePairingError, SESSION_COOKIE_NAME, SESSION_MAX_LIFETIME, SessionAuthError,
        SessionGrant, TRUSTED_BROWSER_COOKIE_NAME, TRUSTED_BROWSER_MAX_LIFETIME,
        TrustedBrowserAuthError,
    },
    state::AppState,
};

type ApiResult<T> = Result<T, (StatusCode, String)>;

const INDEX_HTML: &str = include_str!("../assets/remote.html");
const SESSION_HEADER_NAME: &str = "x-rov-session";
const TRUSTED_BROWSER_HEADER_NAME: &str = "x-rov-trusted";

pub fn spawn_server(state: Arc<AppState>) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::error!(error = %err, "Failed to build the web server runtime");
                return;
            }
        };

        if let Err(err) = runtime.block_on(run_server(state)) {
            tracing::error!(error = %err, "Remote control web server stopped");
        }
    });
}

async fn run_server(state: Arc<AppState>) -> Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/admin/pair-code", post(admin_pair_code))
        .route("/api/pair", post(pair))
        .route("/pair/browser", post(pair_browser))
        .route("/api/session/restore", post(restore_session))
        .route("/api/status", get(status))
        .route("/api/frame.jpg", get(frame))
        .route("/api/input", post(input))
        .layer(DefaultBodyLimit::max(8 * 1024))
        .with_state(state.clone());

    let loopback_address = SocketAddr::from(([127, 0, 0, 1], state.port()));
    let loopback_listener = TcpListener::bind(loopback_address).await.with_context(|| {
        format!("failed to bind the remote control server on {loopback_address}")
    })?;

    let mut active_tailnet_ips = HashSet::new();
    let mut servers = tokio::task::JoinSet::new();
    spawn_listener(
        &mut servers,
        loopback_listener,
        ListenerKind::Loopback,
        app.clone(),
    );
    bind_extra_ipv4_listeners(&mut servers, state.port(), app.clone()).await;
    refresh_tailscale_listeners(
        &mut servers,
        &mut active_tailnet_ips,
        state.port(),
        app.clone(),
    )
    .await;

    loop {
        tokio::select! {
            joined = servers.join_next() => {
                let Some(joined) = joined else {
                    return Err(anyhow::anyhow!("remote control server stopped unexpectedly"));
                };

                match joined {
                    Ok((ListenerKind::Loopback, Err(err))) => {
                        return Err(err).context("loopback listener stopped");
                    }
                    Ok((ListenerKind::Loopback, Ok(()))) => {
                        return Err(anyhow::anyhow!("loopback listener exited unexpectedly"));
                    }
                    Ok((ListenerKind::Extra(ip), Err(err))) => {
                        tracing::warn!(error = %err, ip = %ip, "Extra listener stopped");
                    }
                    Ok((ListenerKind::Extra(ip), Ok(()))) => {
                        tracing::warn!(ip = %ip, "Extra listener exited");
                    }
                    Ok((ListenerKind::Tailscale(ip), Err(err))) => {
                        tracing::warn!(error = %err, ip = %ip, "Tailscale listener stopped");
                        active_tailnet_ips.remove(&ip);
                    }
                    Ok((ListenerKind::Tailscale(ip), Ok(()))) => {
                        tracing::warn!(ip = %ip, "Tailscale listener exited");
                        active_tailnet_ips.remove(&ip);
                    }
                    Err(err) => {
                        return Err(anyhow::anyhow!(err).context("remote control listener task crashed"));
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                refresh_tailscale_listeners(
                    &mut servers,
                    &mut active_tailnet_ips,
                    state.port(),
                    app.clone(),
                ).await;
            }
        }
    }
}

async fn index() -> Response {
    let mut response = Response::new(Body::from(INDEX_HTML));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    apply_security_headers(headers, true);
    response
}

#[derive(Serialize)]
struct PairCodeResponse {
    code: String,
    expires_in_seconds: u64,
    remaining_attempts: u8,
}

async fn admin_pair_code(
    State(state): State<Arc<AppState>>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_loopback_admin(remote_addr)?;
    let snapshot = state.generate_pair_code();
    tracing::info!(
        remote_addr = %remote_addr,
        code = %snapshot.code,
        expires_in_seconds = snapshot.expires_in.as_secs(),
        remaining_attempts = snapshot.remaining_attempts,
        "Host-approved one-time pairing code generated"
    );

    let mut response = Json(PairCodeResponse {
        code: snapshot.code,
        expires_in_seconds: snapshot.expires_in.as_secs(),
        remaining_attempts: snapshot.remaining_attempts,
    })
    .into_response();
    apply_security_headers(response.headers_mut(), false);
    Ok(response)
}

#[derive(Deserialize)]
struct PairRequest {
    code: String,
    #[serde(default = "default_true")]
    remember_browser: bool,
}

#[derive(Deserialize)]
struct PairBrowserFormRequest {
    code: String,
    remember_browser: Option<String>,
}

fn default_true() -> bool {
    true
}

async fn pair(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<PairRequest>,
) -> ApiResult<Response> {
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    let grant = match state.issue_pairing_session(
        &request.code,
        user_agent.clone(),
        request.remember_browser,
    ) {
        Ok(grant) => grant,
        Err(error) => {
            tracing::warn!(
                error = ?error,
                remember_browser = request.remember_browser,
                user_agent = user_agent.as_deref().unwrap_or("unknown"),
                "Browser approval request failed"
            );
            return Err(pairing_error_response(error));
        }
    };
    tracing::info!(
        remember_browser = request.remember_browser,
        user_agent = user_agent.as_deref().unwrap_or("unknown"),
        "Browser approved successfully"
    );
    state
        .enable_remote_control_for_paired_client()
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to arm remote control after pairing".to_string(),
            )
        })?;

    let mut response = StatusCode::NO_CONTENT.into_response();
    let secure_cookie = request_is_https(&headers);
    apply_session_cookies(response.headers_mut(), &grant, secure_cookie)?;
    apply_token_headers(response.headers_mut(), &grant)?;
    apply_security_headers(response.headers_mut(), false);
    Ok(response)
}

async fn pair_browser(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(request): Form<PairBrowserFormRequest>,
) -> Response {
    let remember_browser = request.remember_browser.is_some();
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    let grant =
        match state.issue_pairing_session(&request.code, user_agent.clone(), remember_browser) {
            Ok(grant) => grant,
            Err(error) => {
                tracing::warn!(
                    error = ?error,
                    remember_browser,
                    user_agent = user_agent.as_deref().unwrap_or("unknown"),
                    "Browser approval request failed"
                );
                return redirect_with_pair_error(pairing_error_code(&error));
            }
        };
    tracing::info!(
        remember_browser,
        user_agent = user_agent.as_deref().unwrap_or("unknown"),
        "Browser approved successfully"
    );

    if state.enable_remote_control_for_paired_client().is_err() {
        return redirect_with_pair_error("server_error");
    }

    let mut response = Response::new(Body::from(pair_complete_html(&grant)));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    let secure_cookie = request_is_https(&headers);
    if apply_session_cookies(response.headers_mut(), &grant, secure_cookie).is_err() {
        return redirect_with_pair_error("server_error");
    }
    if apply_token_headers(response.headers_mut(), &grant).is_err() {
        return redirect_with_pair_error("server_error");
    }
    apply_security_headers(response.headers_mut(), true);
    response
}

async fn restore_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let trusted_browser_token = trusted_browser_cookie(&headers)?;
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    let grant = state
        .restore_trusted_browser_session(&trusted_browser_token, user_agent)
        .map_err(trusted_browser_restore_error_response)?;

    let mut response = StatusCode::NO_CONTENT.into_response();
    let secure_cookie = request_is_https(&headers);
    apply_session_cookies(response.headers_mut(), &grant, secure_cookie)?;
    apply_token_headers(response.headers_mut(), &grant)?;
    apply_security_headers(response.headers_mut(), false);
    Ok(response)
}

async fn status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Response> {
    let session_id = authorize_session(&headers, &state)?;
    let payload = serde_json::to_vec(&state.status_response()).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to serialize the session status".to_string(),
        )
    })?;
    state.record_status_response(&session_id, payload.len());

    let mut response = Response::new(Body::from(payload));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    apply_security_headers(response.headers_mut(), false);
    Ok(response)
}

async fn frame(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Response> {
    let session_id = authorize_session(&headers, &state)?;

    let frame = state.latest_frame().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "The first monitor frame is not ready yet".to_string(),
        )
    })?;

    if request_etag_matches(&headers, &frame.etag) {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        let etag = HeaderValue::from_str(&frame.etag).map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to attach the frame cache tag".to_string(),
            )
        })?;
        response.headers_mut().insert(ETAG, etag);
        apply_security_headers(response.headers_mut(), false);
        state.record_frame_response(&session_id, 0, true);
        return Ok(response);
    }

    let mut response = Response::new(Body::from(frame.jpeg.as_ref().clone()));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    let etag = HeaderValue::from_str(&frame.etag).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to attach the frame cache tag".to_string(),
        )
    })?;
    headers.insert(ETAG, etag);
    apply_security_headers(headers, false);
    state.record_frame_response(&session_id, frame.byte_len, false);

    Ok(response)
}

async fn input(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<InputRequest>,
) -> ApiResult<StatusCode> {
    authorize_input_session(&headers, &state)?;

    let monitor = match &request {
        InputRequest::Move { .. } | InputRequest::Click { .. } | InputRequest::Button { .. } => {
            Some(state.selected_monitor().ok_or_else(|| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "No monitor is currently selected".to_string(),
                )
            })?)
        }
        InputRequest::Scroll { .. }
        | InputRequest::Text { .. }
        | InputRequest::Key { .. }
        | InputRequest::Shortcut { .. } => None,
    };

    let command = input::command_from_request(request, monitor.as_ref())
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;

    state
        .send_input(command)
        .map_err(|err| (StatusCode::FORBIDDEN, err.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

fn authorize_session(headers: &HeaderMap, state: &AppState) -> ApiResult<String> {
    let session_id = session_cookie(headers)?;
    state
        .authorize_session(&session_id)
        .map(|_| session_id)
        .map_err(session_error_response)
}

fn authorize_input_session(headers: &HeaderMap, state: &AppState) -> ApiResult<()> {
    let session_id = session_cookie(headers)?;
    state
        .authorize_input_session(&session_id)
        .map(|_| ())
        .map_err(session_error_response)
}

fn ensure_loopback_admin(remote_addr: SocketAddr) -> ApiResult<()> {
    if remote_addr.ip().is_loopback() {
        Ok(())
    } else {
        Err((StatusCode::NOT_FOUND, "not found".to_string()))
    }
}

fn session_cookie(headers: &HeaderMap) -> ApiResult<String> {
    token_value(headers, SESSION_HEADER_NAME)
        .or_else(|| cookie_value(headers, SESSION_COOKIE_NAME))
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "The remote session is missing or expired. Pair this browser again.".to_string(),
            )
        })
}

fn trusted_browser_cookie(headers: &HeaderMap) -> ApiResult<String> {
    token_value(headers, TRUSTED_BROWSER_HEADER_NAME)
        .or_else(|| cookie_value(headers, TRUSTED_BROWSER_COOKIE_NAME))
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "This browser is not remembered on the host. Pair it again.".to_string(),
            )
        })
}

fn token_value(headers: &HeaderMap, header_name: &str) -> Option<String> {
    headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn cookie_value(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies
                .split(';')
                .map(str::trim)
                .find_map(|cookie| cookie.split_once('='))
                .filter(|(name, _)| *name == cookie_name)
                .map(|(_, value)| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn session_error_response(error: SessionAuthError) -> (StatusCode, String) {
    match error {
        SessionAuthError::Missing | SessionAuthError::Invalid | SessionAuthError::Expired => (
            StatusCode::UNAUTHORIZED,
            "The remote session is missing or expired. Pair this browser again.".to_string(),
        ),
        SessionAuthError::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many remote input events were sent at once.".to_string(),
        ),
    }
}

fn pairing_error_response(error: IssuePairingError) -> (StatusCode, String) {
    match error {
        IssuePairingError::Pairing(error) => match error {
            crate::security::PairingError::TooManyAttempts => {
                (StatusCode::TOO_MANY_REQUESTS, error.to_string())
            }
            crate::security::PairingError::MissingCode
            | crate::security::PairingError::InvalidCode => {
                (StatusCode::BAD_REQUEST, error.to_string())
            }
            crate::security::PairingError::NoActiveCode
            | crate::security::PairingError::CodeExpired => {
                (StatusCode::UNAUTHORIZED, error.to_string())
            }
        },
        IssuePairingError::Storage => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to persist the trusted browser record".to_string(),
        ),
    }
}

fn pairing_error_code(error: &IssuePairingError) -> &'static str {
    match error {
        IssuePairingError::Pairing(error) => match error {
            crate::security::PairingError::MissingCode => "missing_code",
            crate::security::PairingError::NoActiveCode => "no_active_code",
            crate::security::PairingError::InvalidCode => "invalid_code",
            crate::security::PairingError::TooManyAttempts => "too_many_attempts",
            crate::security::PairingError::CodeExpired => "code_expired",
        },
        IssuePairingError::Storage => "server_error",
    }
}

fn trusted_browser_restore_error_response(error: TrustedBrowserAuthError) -> (StatusCode, String) {
    match error {
        TrustedBrowserAuthError::Missing | TrustedBrowserAuthError::Invalid => (
            StatusCode::UNAUTHORIZED,
            "This browser is not currently remembered on the host. Pair it again.".to_string(),
        ),
        TrustedBrowserAuthError::Storage => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to refresh the remembered browser session".to_string(),
        ),
    }
}

fn redirect_with_pair_error(code: &str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    let location = format!("/?pair_error={code}");
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(LOCATION, value);
    }
    apply_security_headers(response.headers_mut(), false);
    response
}

fn apply_session_cookies(
    headers: &mut HeaderMap,
    grant: &SessionGrant,
    secure_cookie: bool,
) -> ApiResult<()> {
    let cookie_value = session_cookie_header(&grant.session_id, secure_cookie).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to create the session cookie".to_string(),
        )
    })?;
    headers.insert(SET_COOKIE, cookie_value);
    if let Some(trusted_browser_token) = grant.trusted_browser_token.as_deref() {
        let cookie_value = trusted_browser_cookie_header(trusted_browser_token, secure_cookie)
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to create the trusted browser cookie".to_string(),
                )
            })?;
        headers.append(SET_COOKIE, cookie_value);
    }
    Ok(())
}

fn apply_token_headers(headers: &mut HeaderMap, grant: &SessionGrant) -> ApiResult<()> {
    let session = HeaderValue::from_str(&grant.session_id).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to attach the issued session token".to_string(),
        )
    })?;
    headers.insert(SESSION_HEADER_NAME, session);

    if let Some(trusted_browser_token) = grant.trusted_browser_token.as_deref() {
        let trusted = HeaderValue::from_str(trusted_browser_token).map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to attach the trusted browser token".to_string(),
            )
        })?;
        headers.insert(TRUSTED_BROWSER_HEADER_NAME, trusted);
    }

    Ok(())
}

fn pair_complete_html(grant: &SessionGrant) -> String {
    let session_json =
        serde_json::to_string(&grant.session_id).unwrap_or_else(|_| "\"\"".to_string());
    let trusted_json =
        serde_json::to_string(&grant.trusted_browser_token).unwrap_or_else(|_| "null".to_string());
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Workspace Console</title>
</head>
<body>
  <p>Finalizing workspace session...</p>
  <script>
    const sessionToken = {session_json};
    const trustedToken = {trusted_json};
    try {{
      if (sessionToken) {{
        window.localStorage.setItem("rov_session", sessionToken);
      }}
      if (trustedToken) {{
        window.localStorage.setItem("rov_trusted", trustedToken);
      }}
    }} catch (error) {{
      console.warn("Failed to persist workspace tokens", error);
    }}
    window.location.replace("/");
  </script>
</body>
</html>
"#
    )
}

fn request_is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("https"))
        || headers
            .get("forwarded")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("proto=https"))
}

fn trusted_browser_cookie_header(
    trusted_browser_token: &str,
    secure: bool,
) -> Result<HeaderValue, InvalidHeaderValue> {
    let mut value = format!(
        "{TRUSTED_BROWSER_COOKIE_NAME}={trusted_browser_token}; HttpOnly; Path=/; SameSite=Strict; Max-Age={}",
        TRUSTED_BROWSER_MAX_LIFETIME.as_secs()
    );
    if secure {
        value.push_str("; Secure");
    }
    HeaderValue::from_str(&value)
}

fn session_cookie_header(
    session_id: &str,
    secure: bool,
) -> Result<HeaderValue, InvalidHeaderValue> {
    let mut value = format!(
        "{SESSION_COOKIE_NAME}={session_id}; HttpOnly; Path=/; SameSite=Strict; Max-Age={}",
        SESSION_MAX_LIFETIME.as_secs()
    );
    if secure {
        value.push_str("; Secure");
    }
    HeaderValue::from_str(&value)
}

fn apply_security_headers(headers: &mut HeaderMap, is_html: bool) {
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    headers.insert(PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );

    if is_html {
        headers.insert(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
            ),
        );
    } else {
        headers.insert(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
            ),
        );
    }
}

fn request_etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate == "*" || candidate == etag)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ListenerKind {
    Loopback,
    Extra(Ipv4Addr),
    Tailscale(Ipv4Addr),
}

fn spawn_listener(
    servers: &mut tokio::task::JoinSet<(ListenerKind, Result<()>)>,
    listener: TcpListener,
    kind: ListenerKind,
    app: Router,
) {
    servers.spawn(async move {
        let address = listener.local_addr().ok();
        match kind {
            ListenerKind::Loopback => {
                if let Some(address) = address {
                    tracing::info!("Remote control server listening on {address} (loopback)");
                }
            }
            ListenerKind::Extra(ip) => {
                if let Some(address) = address {
                    tracing::info!("Remote control server listening on {address} (extra {ip})");
                }
            }
            ListenerKind::Tailscale(ip) => {
                if let Some(address) = address {
                    tracing::info!("Remote control server listening on {address} (tailscale {ip})");
                }
            }
        }

        let result = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .context("failed while serving remote control requests");
        (kind, result)
    });
}

async fn refresh_tailscale_listeners(
    servers: &mut tokio::task::JoinSet<(ListenerKind, Result<()>)>,
    active_tailnet_ips: &mut HashSet<Ipv4Addr>,
    port: u16,
    app: Router,
) {
    let tailscale_status = network::discover_tailscale_status();
    if tailscale_status.serve_enabled {
        return;
    }

    let tailscale_ips = tailscale_status.tailscale_ips;
    for ip in tailscale_ips {
        if active_tailnet_ips.contains(&ip) {
            continue;
        }

        let address = SocketAddr::new(IpAddr::V4(ip), port);
        match TcpListener::bind(address).await {
            Ok(listener) => {
                active_tailnet_ips.insert(ip);
                spawn_listener(servers, listener, ListenerKind::Tailscale(ip), app.clone());
            }
            Err(err) => {
                if tailscale_port_is_in_use(&err) {
                    tracing::debug!(
                        error = %err,
                        ip = %ip,
                        "Skipping the direct Tailscale listener because this port is already in use"
                    );
                    continue;
                }

                tracing::warn!(error = %err, ip = %ip, "Failed to bind the Tailscale listener");
            }
        }
    }
}

async fn bind_extra_ipv4_listeners(
    servers: &mut tokio::task::JoinSet<(ListenerKind, Result<()>)>,
    port: u16,
    app: Router,
) {
    for ip in configured_extra_listener_ips() {
        let address = SocketAddr::new(IpAddr::V4(ip), port);
        match TcpListener::bind(address).await {
            Ok(listener) => {
                spawn_listener(servers, listener, ListenerKind::Extra(ip), app.clone());
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    ip = %ip,
                    "Failed to bind the configured extra listener"
                );
            }
        }
    }
}

fn configured_extra_listener_ips() -> Vec<Ipv4Addr> {
    env::var("ROV_EXTRA_LISTEN_ADDRS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .filter_map(|entry| {
                    let trimmed = entry.trim();
                    if trimmed.is_empty() {
                        return None;
                    }

                    match trimmed.parse::<Ipv4Addr>() {
                        Ok(ip) => Some(ip),
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                value = trimmed,
                                "Ignoring invalid IPv4 address in ROV_EXTRA_LISTEN_ADDRS"
                            );
                            None
                        }
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn tailscale_port_is_in_use(err: &std::io::Error) -> bool {
    err.kind() == ErrorKind::AddrInUse || err.raw_os_error() == Some(10048)
}

#[cfg(test)]
mod tests {
    use super::{ensure_loopback_admin, tailscale_port_is_in_use};
    use std::io::{Error, ErrorKind};
    use std::net::{Ipv4Addr, SocketAddr};

    #[test]
    fn tailscale_port_conflict_is_treated_as_non_fatal() {
        let err = Error::from(ErrorKind::AddrInUse);
        assert!(tailscale_port_is_in_use(&err));
    }

    #[test]
    fn unrelated_listener_errors_are_not_suppressed() {
        let err = Error::from(ErrorKind::PermissionDenied);
        assert!(!tailscale_port_is_in_use(&err));
    }

    #[test]
    fn local_admin_routes_only_allow_loopback_clients() {
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 45080));
        let remote = SocketAddr::from((Ipv4Addr::new(172, 18, 0, 2), 45080));

        assert!(ensure_loopback_admin(loopback).is_ok());
        assert!(ensure_loopback_admin(remote).is_err());
    }
}
