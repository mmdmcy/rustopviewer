use anyhow::{Context, Result};
use axum::{
    Form, Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{
            AUTHORIZATION, CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, ETAG,
            IF_NONE_MATCH, InvalidHeaderValue, LOCATION, PRAGMA, REFERRER_POLICY, SET_COOKIE,
            USER_AGENT,
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
    config::StreamProfile,
    fleet::{FleetHeartbeatRequest, FleetRegisterRequest},
    input::{self, InputRequest},
    model::{DeviceInfoResponse, MonitorInfo},
    network, oidc,
    security::{
        IssueAccessPasswordError, IssuePairingError, SESSION_COOKIE_NAME, SESSION_MAX_LIFETIME,
        SessionAuthError, SessionGrant, TRUSTED_BROWSER_COOKIE_NAME, TRUSTED_BROWSER_MAX_LIFETIME,
        TrustedBrowserAuthError,
    },
    state::AppState,
};

type ApiResult<T> = Result<T, (StatusCode, String)>;

const INDEX_HTML: &str = include_str!("../assets/remote.html");
const ADMIN_HTML: &str = include_str!("../assets/admin.html");
const SESSION_HEADER_NAME: &str = "x-rov-session";
const TRUSTED_BROWSER_HEADER_NAME: &str = "x-rov-trusted";
const ADMIN_TOKEN_HEADER_NAME: &str = "x-rov-admin-token";

enum ViewerAuthorization {
    Masterdale,
    Session(String),
}

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
        .route("/admin", get(admin_index))
        .route("/admin/", get(admin_index))
        .route("/api/admin/status", get(admin_status))
        .route("/api/admin/pair-code", post(admin_pair_code))
        .route("/api/admin/device-code", post(admin_device_code))
        .route("/api/admin/access-password", post(admin_access_password))
        .route(
            "/api/admin/access-password/generate",
            post(admin_generate_access_password),
        )
        .route(
            "/api/admin/access-password/clear",
            post(admin_clear_access_password),
        )
        .route("/api/admin/remote-input", post(admin_remote_input))
        .route("/api/admin/stream-profile", post(admin_stream_profile))
        .route("/api/admin/monitor", post(admin_monitor))
        .route(
            "/api/admin/session/disconnect",
            post(admin_disconnect_session),
        )
        .route(
            "/api/admin/trusted-browsers/clear",
            post(admin_clear_trusted_browsers),
        )
        .route("/api/admin/tailscale-url", post(admin_tailscale_url))
        .route("/api/admin/panic-stop", post(admin_panic_stop))
        .route("/api/device", get(device_info))
        .route("/api/login", post(login))
        .route("/auth/oidc/start", get(oidc::start))
        .route("/auth/oidc/callback", get(oidc::callback))
        .route("/api/pair", post(pair))
        .route("/pair/browser", post(pair_browser))
        .route("/api/session/restore", post(restore_session))
        .route("/api/status", get(status))
        .route("/api/frame.jpg", get(frame))
        .route("/api/input", post(input))
        .route("/v1/fleet/register", post(fleet_register))
        .route("/v1/fleet/heartbeat", post(fleet_heartbeat))
        .route("/v1/fleet/devices", get(fleet_devices))
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

async fn admin_index(
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_loopback_admin(remote_addr, &headers)?;
    let mut response = Response::new(Body::from(ADMIN_HTML));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    apply_security_headers(headers, true);
    Ok(response)
}

#[derive(Serialize)]
struct AdminStatusResponse {
    device: DeviceInfoResponse,
    port: u16,
    loopback_url: String,
    config_path: String,
    host_elevated: bool,
    pair_code: Option<AdminPairCodeState>,
    session: Option<AdminSessionState>,
    remote_user_agent: Option<String>,
    trusted_browsers: Vec<AdminTrustedBrowserState>,
    remote_pointer_requested: bool,
    remote_keyboard_requested: bool,
    remote_pointer_enabled: bool,
    remote_keyboard_enabled: bool,
    stream_profile: StreamProfile,
    monitors: Vec<MonitorInfo>,
    selected_monitor_id: Option<u32>,
    admin_token_required: bool,
}

#[derive(Serialize)]
struct AdminPairCodeState {
    code: String,
    expires_in_seconds: u64,
    remaining_attempts: u8,
}

#[derive(Serialize)]
struct AdminSessionState {
    expires_in_seconds: u64,
    bytes_sent: u64,
    frame_responses: u64,
    cached_frame_hits: u64,
    status_responses: u64,
}

#[derive(Serialize)]
struct AdminTrustedBrowserState {
    id: String,
    label: String,
    created_ago_seconds: u64,
    last_seen_ago_seconds: u64,
}

async fn admin_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    json_response(AdminStatusResponse {
        device: state.device_info_response(),
        port: state.port(),
        loopback_url: format!("http://127.0.0.1:{}/", state.port()),
        config_path: state.config_path().display().to_string(),
        host_elevated: state.is_elevated(),
        pair_code: state
            .current_pair_code()
            .map(|pair_code| AdminPairCodeState {
                code: pair_code.code,
                expires_in_seconds: pair_code.expires_in.as_secs(),
                remaining_attempts: pair_code.remaining_attempts,
            }),
        session: state
            .current_remote_session()
            .map(|session| AdminSessionState {
                expires_in_seconds: session.expires_in.as_secs(),
                bytes_sent: session.bytes_sent,
                frame_responses: session.frame_responses,
                cached_frame_hits: session.cached_frame_hits,
                status_responses: session.status_responses,
            }),
        remote_user_agent: state.current_remote_user_agent(),
        trusted_browsers: state
            .trusted_browser_snapshots()
            .into_iter()
            .map(|browser| AdminTrustedBrowserState {
                id: browser.id,
                label: browser.label,
                created_ago_seconds: browser.created_ago.as_secs(),
                last_seen_ago_seconds: browser.last_seen_ago.as_secs(),
            })
            .collect(),
        remote_pointer_requested: state.remote_pointer_requested(),
        remote_keyboard_requested: state.remote_keyboard_requested(),
        remote_pointer_enabled: state.remote_pointer_enabled(),
        remote_keyboard_enabled: state.remote_keyboard_enabled(),
        stream_profile: state.stream_profile(),
        monitors: state.monitors(),
        selected_monitor_id: state.selected_monitor_id(),
        admin_token_required: state.admin_token_required(),
    })
}

#[derive(Serialize)]
struct PairCodeResponse {
    code: String,
    expires_in_seconds: u64,
    remaining_attempts: u8,
}

#[derive(Serialize)]
struct AdminMessageResponse {
    message: String,
}

#[derive(Deserialize)]
struct AdminDeviceCodeRequest {
    device_code: String,
}

#[derive(Deserialize)]
struct AdminAccessPasswordRequest {
    password: String,
}

#[derive(Serialize)]
struct GeneratedAccessPasswordResponse {
    password: String,
    message: String,
}

#[derive(Deserialize)]
struct AdminRemoteInputRequest {
    pointer_enabled: Option<bool>,
    keyboard_enabled: Option<bool>,
}

#[derive(Deserialize)]
struct AdminStreamProfileRequest {
    profile: StreamProfile,
}

#[derive(Deserialize)]
struct AdminMonitorRequest {
    monitor_id: u32,
}

async fn admin_pair_code(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
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

async fn admin_device_code(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(request): Json<AdminDeviceCodeRequest>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    let code = state
        .set_device_code(&request.device_code)
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;
    json_response(AdminMessageResponse {
        message: format!("Device code set to {code}"),
    })
}

async fn admin_access_password(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(request): Json<AdminAccessPasswordRequest>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    state
        .set_access_password(&request.password)
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;
    json_response(AdminMessageResponse {
        message: "Access password configured".to_string(),
    })
}

async fn admin_generate_access_password(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    let password = crate::security::generate_access_password();
    state.set_access_password(&password).map_err(|err| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to store the generated access password: {err}"),
        )
    })?;
    json_response(GeneratedAccessPasswordResponse {
        password,
        message: "Access password generated and stored".to_string(),
    })
}

async fn admin_clear_access_password(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    state.clear_access_password().map_err(|err| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to clear the access password: {err}"),
        )
    })?;
    json_response(AdminMessageResponse {
        message: "Access password disabled".to_string(),
    })
}

async fn admin_remote_input(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(request): Json<AdminRemoteInputRequest>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    if let Some(enabled) = request.pointer_enabled {
        state
            .set_remote_pointer_enabled(enabled)
            .map_err(|err| (StatusCode::FORBIDDEN, err.to_string()))?;
    }
    if let Some(enabled) = request.keyboard_enabled {
        state
            .set_remote_keyboard_enabled(enabled)
            .map_err(|err| (StatusCode::FORBIDDEN, err.to_string()))?;
    }
    json_response(AdminMessageResponse {
        message: "Remote input settings updated".to_string(),
    })
}

async fn admin_stream_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(request): Json<AdminStreamProfileRequest>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    state.set_stream_profile(request.profile).map_err(|err| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to set the stream profile: {err}"),
        )
    })?;
    json_response(AdminMessageResponse {
        message: format!("Stream profile set to {}", request.profile.label()),
    })
}

async fn admin_monitor(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(request): Json<AdminMonitorRequest>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    if !state
        .monitors()
        .iter()
        .any(|monitor| monitor.id == request.monitor_id)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "that monitor is not currently available".to_string(),
        ));
    }
    state
        .set_selected_monitor(request.monitor_id)
        .map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to select the monitor: {err}"),
            )
        })?;
    json_response(AdminMessageResponse {
        message: "Selected monitor updated".to_string(),
    })
}

async fn admin_disconnect_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    state.revoke_remote_session();
    json_response(AdminMessageResponse {
        message: "Remote session disconnected".to_string(),
    })
}

async fn admin_clear_trusted_browsers(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    let count = state.revoke_trusted_browsers().map_err(|err| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to clear trusted browsers: {err}"),
        )
    })?;
    json_response(AdminMessageResponse {
        message: format!("Forgot {count} trusted browser(s)"),
    })
}

async fn admin_tailscale_url(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    network::enable_tailscale_client_url(state.port()).map_err(|err| {
        (
            StatusCode::BAD_GATEWAY,
            format!("failed to enable the Tailscale URL: {err}"),
        )
    })?;
    json_response(AdminMessageResponse {
        message: "Tailscale URL enabled".to_string(),
    })
}

async fn admin_panic_stop(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Response> {
    ensure_admin_api(remote_addr, &headers, &state)?;
    state.panic_stop().map_err(|err| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("panic stop failed: {err}"),
        )
    })?;
    json_response(AdminMessageResponse {
        message: "Remote input, sessions, trusted browsers, pair code, and access password cleared"
            .to_string(),
    })
}

#[derive(Deserialize)]
struct PairRequest {
    code: String,
    #[serde(default = "default_true")]
    remember_browser: bool,
}

#[derive(Deserialize)]
struct LoginRequest {
    device_code: String,
    password: String,
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

async fn device_info(State(state): State<Arc<AppState>>) -> ApiResult<Response> {
    json_response(state.device_info_response())
}

fn json_response<T: Serialize>(value: T) -> ApiResult<Response> {
    let payload = serde_json::to_vec(&value).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to serialize the response".to_string(),
        )
    })?;

    let mut response = Response::new(Body::from(payload));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    apply_security_headers(response.headers_mut(), false);
    Ok(response)
}

async fn login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> ApiResult<Response> {
    ensure_same_origin_request(&headers)?;
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    let grant = match state.issue_access_password_session(
        &request.device_code,
        &request.password,
        user_agent.clone(),
        request.remember_browser,
    ) {
        Ok(grant) => grant,
        Err(error) => {
            tracing::warn!(
                error = ?error,
                remember_browser = request.remember_browser,
                user_agent = user_agent.as_deref().unwrap_or("unknown"),
                "Unattended password login failed"
            );
            return Err(access_password_error_response(error));
        }
    };

    tracing::info!(
        remember_browser = request.remember_browser,
        user_agent = user_agent.as_deref().unwrap_or("unknown"),
        "Unattended password login approved successfully"
    );
    state
        .enable_remote_control_for_paired_client()
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to arm remote control after password login".to_string(),
            )
        })?;

    let mut response = StatusCode::NO_CONTENT.into_response();
    let secure_cookie = request_is_https(&headers);
    apply_session_cookies(response.headers_mut(), &grant, secure_cookie)?;
    apply_token_headers(response.headers_mut(), &grant)?;
    apply_security_headers(response.headers_mut(), false);
    Ok(response)
}

async fn pair(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<PairRequest>,
) -> ApiResult<Response> {
    ensure_same_origin_request(&headers)?;
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
    if let Err((status, message)) = ensure_same_origin_request(&headers) {
        let mut response = (status, message).into_response();
        apply_security_headers(response.headers_mut(), false);
        return response;
    }

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
    ensure_same_origin_request(&headers)?;
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
    let authorization = authorize_viewer(&headers, &state)?;
    let payload = serde_json::to_vec(&state.status_response()).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to serialize the session status".to_string(),
        )
    })?;
    if let ViewerAuthorization::Session(session_id) = &authorization {
        state.record_status_response(session_id, payload.len());
    }

    let mut response = Response::new(Body::from(payload));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    apply_security_headers(response.headers_mut(), false);
    Ok(response)
}

async fn frame(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Response> {
    let authorization = authorize_viewer(&headers, &state)?;

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
        if let ViewerAuthorization::Session(session_id) = &authorization {
            state.record_frame_response(session_id, 0, true);
        }
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
    if let ViewerAuthorization::Session(session_id) = &authorization {
        state.record_frame_response(session_id, frame.byte_len, false);
    }

    Ok(response)
}

async fn input(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<InputRequest>,
) -> ApiResult<StatusCode> {
    ensure_same_origin_request(&headers)?;
    authorize_viewer_input(&headers, &state)?;

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

async fn fleet_register(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<FleetRegisterRequest>,
) -> ApiResult<Response> {
    authorize_masterdale_only(&headers, &state)?;
    let registry = state.fleet_registry().ok_or((
        StatusCode::NOT_FOUND,
        "fleet registry is only available on rustopviewer host mode".to_string(),
    ))?;
    if request.device_id.trim().is_empty() || request.viewer_url.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "device_id and viewer_url are required".to_string(),
        ));
    }
    let device = registry.register(request);
    json_response(device)
}

async fn fleet_heartbeat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<FleetHeartbeatRequest>,
) -> ApiResult<Response> {
    authorize_masterdale_only(&headers, &state)?;
    let registry = state.fleet_registry().ok_or((
        StatusCode::NOT_FOUND,
        "fleet registry is only available on rustopviewer host mode".to_string(),
    ))?;
    let device = registry.heartbeat(request).map_err(|err| {
        (
            StatusCode::NOT_FOUND,
            err.to_string(),
        )
    })?;
    json_response(device)
}

async fn fleet_devices(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    authorize_masterdale_only(&headers, &state)?;
    let registry = state.fleet_registry().ok_or((
        StatusCode::NOT_FOUND,
        "fleet registry is only available on rustopviewer host mode".to_string(),
    ))?;
    json_response(serde_json::json!({ "devices": registry.list() }))
}

fn authorize_masterdale_only(headers: &HeaderMap, state: &AppState) -> ApiResult<()> {
    let Some(token) = bearer_token(headers) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Masterdale bearer token required".to_string(),
        ));
    };
    if !state.authorize_masterdale_token(&token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Masterdale token required or invalid".to_string(),
        ));
    }
    Ok(())
}

fn authorize_viewer(headers: &HeaderMap, state: &AppState) -> ApiResult<ViewerAuthorization> {
    if let Some(token) = bearer_token(headers) {
        return if state.authorize_masterdale_token(&token) {
            state.note_viewer_activity();
            Ok(ViewerAuthorization::Masterdale)
        } else {
            Err((
                StatusCode::UNAUTHORIZED,
                "Masterdale token required or invalid".to_string(),
            ))
        };
    }

    let session_id = session_cookie(headers)?;
    let authorization = state
        .authorize_session(&session_id)
        .map(|_| ViewerAuthorization::Session(session_id))
        .map_err(session_error_response)?;
    state.note_viewer_activity();
    Ok(authorization)
}

fn authorize_viewer_input(headers: &HeaderMap, state: &AppState) -> ApiResult<()> {
    if let Some(token) = bearer_token(headers) {
        if !state.authorize_masterdale_token(&token) {
            return Err((
                StatusCode::UNAUTHORIZED,
                "Masterdale token required or invalid".to_string(),
            ));
        }
        state.note_viewer_activity();
        return if state.authorize_masterdale_input(&token) {
            Ok(())
        } else {
            Err((
                StatusCode::TOO_MANY_REQUESTS,
                "too many remote input requests".to_string(),
            ))
        };
    }

    let session_id = session_cookie(headers)?;
    let authorization = state
        .authorize_input_session(&session_id)
        .map(|_| ())
        .map_err(session_error_response);
    if authorization.is_ok() {
        state.note_viewer_activity();
    }
    authorization
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn ensure_admin_api(
    remote_addr: SocketAddr,
    headers: &HeaderMap,
    state: &AppState,
) -> ApiResult<()> {
    ensure_same_origin_request(headers)?;
    ensure_loopback_admin(remote_addr, headers)?;
    let admin_token = token_value(headers, ADMIN_TOKEN_HEADER_NAME);
    if state.authorize_admin_token(admin_token.as_deref()) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            "admin token required or invalid".to_string(),
        ))
    }
}

fn ensure_same_origin_request(headers: &HeaderMap) -> ApiResult<()> {
    let Some(origin) = headers.get("origin") else {
        return Ok(());
    };
    let origin = origin
        .to_str()
        .ok()
        .and_then(normalize_origin)
        .ok_or_else(|| {
            (
                StatusCode::FORBIDDEN,
                "valid same-origin request headers are required".to_string(),
            )
        })?;
    let Some(expected_origin) = request_expected_origin(headers) else {
        return Err((
            StatusCode::FORBIDDEN,
            "same-origin request headers are required".to_string(),
        ));
    };

    if origin == expected_origin {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "cross-origin state-changing requests are not allowed".to_string(),
        ))
    }
}

fn ensure_loopback_admin(remote_addr: SocketAddr, headers: &HeaderMap) -> ApiResult<()> {
    if remote_addr.ip().is_loopback() && !has_reverse_proxy_admin_headers(headers) {
        return Ok(());
    }

    Err((StatusCode::NOT_FOUND, "not found".to_string()))
}

fn has_reverse_proxy_admin_headers(headers: &HeaderMap) -> bool {
    [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-real-ip",
        "cf-connecting-ip",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
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

fn request_expected_origin(headers: &HeaderMap) -> Option<String> {
    request_authority(headers).map(|authority| format!("{}://{authority}", request_scheme(headers)))
}

fn request_scheme(headers: &HeaderMap) -> String {
    if let Some(proto) = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(first_forwarded_value)
        .map(str::to_ascii_lowercase)
        .filter(|value| value == "http" || value == "https")
    {
        return proto;
    }

    headers
        .get("forwarded")
        .and_then(|value| value.to_str().ok())
        .and_then(forwarded_proto)
        .unwrap_or_else(|| "http".to_string())
}

fn request_authority(headers: &HeaderMap) -> Option<String> {
    if let Some(authority) = headers
        .get("x-forwarded-host")
        .and_then(|value| value.to_str().ok())
        .and_then(first_forwarded_value)
        .filter(|value| !value.is_empty())
    {
        return Some(authority.to_ascii_lowercase());
    }

    if let Some(authority) = headers
        .get("forwarded")
        .and_then(|value| value.to_str().ok())
        .and_then(forwarded_host)
    {
        return Some(authority.to_ascii_lowercase());
    }

    headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn first_forwarded_value(value: &str) -> Option<&str> {
    value
        .split(',')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn forwarded_host(value: &str) -> Option<String> {
    first_forwarded_value(value)?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| {
            if name.trim().eq_ignore_ascii_case("host") {
                Some(value.trim().trim_matches('"').to_string())
            } else {
                None
            }
        })
        .filter(|value| !value.is_empty())
}

fn forwarded_proto(value: &str) -> Option<String> {
    first_forwarded_value(value)?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| {
            if !name.trim().eq_ignore_ascii_case("proto") {
                return None;
            }

            let proto = value.trim().trim_matches('"').to_ascii_lowercase();
            if proto == "http" || proto == "https" {
                Some(proto)
            } else {
                None
            }
        })
}

fn normalize_origin(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }

    let authority = rest
        .split('/')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    ))
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

fn access_password_error_response(error: IssueAccessPasswordError) -> (StatusCode, String) {
    match error {
        IssueAccessPasswordError::Access(error) => match error {
            crate::security::AccessPasswordError::MissingDeviceCode
            | crate::security::AccessPasswordError::MissingPassword => {
                (StatusCode::BAD_REQUEST, error.to_string())
            }
            crate::security::AccessPasswordError::PasswordNotConfigured => {
                (StatusCode::SERVICE_UNAVAILABLE, error.to_string())
            }
            crate::security::AccessPasswordError::InvalidCredentials => {
                (StatusCode::UNAUTHORIZED, error.to_string())
            }
            crate::security::AccessPasswordError::TooManyAttempts => {
                (StatusCode::TOO_MANY_REQUESTS, error.to_string())
            }
        },
        IssueAccessPasswordError::Storage => (
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

pub(crate) fn apply_session_cookies(
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

pub(crate) fn request_is_https(headers: &HeaderMap) -> bool {
    request_scheme(headers) == "https"
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
    // Always bind the app port on Tailscale IPs. An unrelated `tailscale serve`
    // config (for another local service/port) must not hide this listener.

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
    use super::{
        bearer_token, ensure_loopback_admin, ensure_same_origin_request, tailscale_port_is_in_use,
    };
    use axum::http::{HeaderMap, HeaderValue};
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
    fn masterdale_bearer_token_is_read_without_whitespace() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer persistent-masterdale-token"),
        );

        assert_eq!(
            bearer_token(&headers).as_deref(),
            Some("persistent-masterdale-token")
        );
    }

    #[test]
    fn non_bearer_authorization_is_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );

        assert!(bearer_token(&headers).is_none());
    }

    #[test]
    fn local_admin_routes_only_allow_loopback_clients() {
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 45080));
        let remote = SocketAddr::from((Ipv4Addr::new(172, 18, 0, 2), 45080));
        let headers = HeaderMap::new();

        assert!(ensure_loopback_admin(loopback, &headers).is_ok());
        assert!(ensure_loopback_admin(remote, &headers).is_err());
    }

    #[test]
    fn local_admin_routes_reject_reverse_proxied_loopback_requests() {
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 45080));
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

        assert!(ensure_loopback_admin(loopback, &headers).is_err());
    }

    #[test]
    fn same_origin_requests_accept_matching_host() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:45080"));
        headers.insert("origin", HeaderValue::from_static("http://127.0.0.1:45080"));

        assert!(ensure_same_origin_request(&headers).is_ok());
    }

    #[test]
    fn same_origin_requests_accept_forwarded_https_host() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:45080"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("rov.example.test"),
        );
        headers.insert(
            "origin",
            HeaderValue::from_static("https://rov.example.test"),
        );

        assert!(ensure_same_origin_request(&headers).is_ok());
    }

    #[test]
    fn same_origin_requests_reject_cross_origin_posts() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:45080"));
        headers.insert(
            "origin",
            HeaderValue::from_static("https://attacker.example"),
        );

        assert!(ensure_same_origin_request(&headers).is_err());
    }

    #[test]
    fn same_origin_requests_reject_invalid_origin_values() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:45080"));
        headers.insert("origin", HeaderValue::from_static("null"));

        assert!(ensure_same_origin_request(&headers).is_err());
    }
}
