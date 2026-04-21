mod capture;
mod config;
mod input;
mod model;
mod network;
mod platform;
mod security;
mod server;
mod state;
mod tui;

use anyhow::{Context, Result};
use state::AppState;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_logging();

    let config_store = config::ConfigStore::new()?;
    let config = config_store.load_or_create()?;
    let monitors = capture::discover_monitors().context("failed to enumerate monitors")?;
    let input_tx = input::spawn_input_worker().context("failed to start input worker")?;
    let is_elevated = platform::is_process_elevated();

    let state = Arc::new(AppState::new(
        config_store,
        config,
        monitors,
        input_tx,
        is_elevated,
    )?);
    state
        .ensure_valid_selected_monitor()
        .context("failed to select an active monitor")?;

    capture::spawn_capture_worker(state.clone());
    server::spawn_server(state.clone());

    tui::run(state).context("failed to run the RustOp Viewer terminal UI")?;

    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,hyper=warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
