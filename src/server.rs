use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, PRAGMA},
    },
    response::Response,
    routing::{get, post},
};
use std::{net::SocketAddr, sync::Arc, thread};
use tokio::net::TcpListener;

use crate::{
    input::{self, InputRequest},
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
        .route("/api/status", get(status))
        .route("/api/frame.jpg", get(frame))
        .route("/api/input", post(input))
        .with_state(state.clone());

    let address = SocketAddr::from(([0, 0, 0, 0], state.port()));
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind the remote control server on {address}"))?;

    tracing::info!(
        "Remote control server listening on {}",
        listener.local_addr()?
    );
    axum::serve(listener, app)
        .await
        .context("failed while serving remote control requests")?;

    Ok(())
}

async fn index() -> Response {
    let mut response = Response::new(Body::from(INDEX_HTML));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    headers.insert(PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<crate::model::StatusResponse>> {
    authorize(&headers, &state)?;
    Ok(Json(state.status_response()))
}

async fn frame(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Response> {
    authorize(&headers, &state)?;

    let frame = state.latest_frame().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "The first monitor frame is not ready yet".to_string(),
        )
    })?;

    let mut response = Response::new(Body::from(frame.jpeg.as_ref().clone()));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    headers.insert(PRAGMA, HeaderValue::from_static("no-cache"));

    Ok(response)
}

async fn input(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<InputRequest>,
) -> ApiResult<StatusCode> {
    authorize(&headers, &state)?;

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
        .map_err(|err| (StatusCode::SERVICE_UNAVAILABLE, err.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

fn authorize(headers: &HeaderMap, state: &AppState) -> ApiResult<()> {
    let provided = headers
        .get("x-auth-token")
        .and_then(|value| value.to_str().ok())
        .map(str::trim);
    let expected = state.auth_token();

    if provided == Some(expected.as_str()) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            "Missing or invalid X-Auth-Token header".to_string(),
        ))
    }
}
