use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{
            CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, ETAG, IF_NONE_MATCH,
            InvalidHeaderValue, PRAGMA, REFERRER_POLICY, SET_COOKIE, USER_AGENT,
        },
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::{
    collections::HashSet,
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
    security::{PairingError, SESSION_COOKIE_NAME, SESSION_MAX_LIFETIME, SessionAuthError},
    state::AppState,
};

type ApiResult<T> = Result<T, (StatusCode, String)>;

const INDEX_HTML: &str = include_str!("../assets/remote.html");

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
        .route("/api/pair", post(pair))
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

#[derive(Deserialize)]
struct PairRequest {
    code: String,
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

    let grant = state
        .issue_pairing_session(&request.code, user_agent)
        .map_err(pairing_error_response)?;
    state
        .enable_remote_control_for_paired_phone()
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to arm remote control after pairing".to_string(),
            )
        })?;

    let mut response = StatusCode::NO_CONTENT.into_response();
    let secure_cookie = request_is_https(&headers);
    let cookie_value = session_cookie_header(&grant.session_id, secure_cookie).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to create the session cookie".to_string(),
        )
    })?;
    response.headers_mut().insert(SET_COOKIE, cookie_value);
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

    let monitor = state.selected_monitor().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "No monitor is currently selected".to_string(),
        )
    })?;

    let command = input::command_from_request(request, &monitor)
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

fn session_cookie(headers: &HeaderMap) -> ApiResult<String> {
    let cookies = headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "The remote session is missing or expired. Pair this phone again.".to_string(),
            )
        })?;

    cookies
        .split(';')
        .map(str::trim)
        .find_map(|cookie| cookie.split_once('='))
        .filter(|(name, _)| *name == SESSION_COOKIE_NAME)
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "The remote session is missing or expired. Pair this phone again.".to_string(),
            )
        })
}

fn session_error_response(error: SessionAuthError) -> (StatusCode, String) {
    match error {
        SessionAuthError::Missing | SessionAuthError::Invalid | SessionAuthError::Expired => (
            StatusCode::UNAUTHORIZED,
            "The remote session is missing or expired. Pair this phone again.".to_string(),
        ),
        SessionAuthError::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many remote input events were sent at once.".to_string(),
        ),
    }
}

fn pairing_error_response(error: PairingError) -> (StatusCode, String) {
    match error {
        PairingError::TooManyAttempts => (StatusCode::TOO_MANY_REQUESTS, error.to_string()),
        PairingError::MissingCode | PairingError::InvalidCode => {
            (StatusCode::BAD_REQUEST, error.to_string())
        }
        PairingError::NoActiveCode | PairingError::CodeExpired => {
            (StatusCode::UNAUTHORIZED, error.to_string())
        }
    }
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
            ListenerKind::Tailscale(ip) => {
                if let Some(address) = address {
                    tracing::info!("Remote control server listening on {address} (tailscale {ip})");
                }
            }
        }

        let result = axum::serve(listener, app)
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

fn tailscale_port_is_in_use(err: &std::io::Error) -> bool {
    err.kind() == ErrorKind::AddrInUse || err.raw_os_error() == Some(10048)
}

#[cfg(test)]
mod tests {
    use super::tailscale_port_is_in_use;
    use std::io::{Error, ErrorKind};

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
}
