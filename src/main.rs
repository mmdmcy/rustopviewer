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
use security::TrustedBrowserStore;
use state::AppState;
use std::{env, process, sync::Arc, thread, time::Duration};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_logging();
    let run_mode = parse_run_mode()?;

    let config_store = config::ConfigStore::new()?;
    let config = config_store.load_or_create()?;
    let trusted_browser_store = TrustedBrowserStore::new(config_store.trusted_browsers_path())?;
    let monitors = capture::discover_monitors().context("failed to enumerate monitors")?;
    let input_tx = input::spawn_input_worker().context("failed to start input worker")?;
    let is_elevated = platform::is_process_elevated();

    let state = Arc::new(AppState::new(
        config_store,
        config,
        monitors,
        input_tx,
        trusted_browser_store,
        is_elevated,
    )?);
    state
        .ensure_valid_selected_monitor()
        .context("failed to select an active monitor")?;

    capture::spawn_capture_worker(state.clone());
    server::spawn_server(state.clone());

    match run_mode {
        RunMode::Tui => {
            tui::run(state).context("failed to run the RustOp Viewer terminal UI")?;
        }
        RunMode::Headless => {
            run_headless(state);
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    Tui,
    Headless,
}

fn parse_run_mode() -> Result<RunMode> {
    let mut run_mode = RunMode::Tui;

    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--headless" => run_mode = RunMode::Headless,
            "-h" | "--help" => {
                print_help();
                process::exit(0);
            }
            _ => anyhow::bail!("unknown argument: {arg}"),
        }
    }

    Ok(run_mode)
}

fn print_help() {
    println!(
        "\
RustOp Viewer

Usage:
  rustopviewer [--headless]

Options:
  --headless   Run the host runtime without the local terminal UI.
  -h, --help   Show this help text.
"
    );
}

fn run_headless(state: Arc<AppState>) {
    let urls = network::discover_urls(state.port());
    tracing::info!(
        port = state.port(),
        preferred_url = %urls.preferred.url,
        loopback_url = %urls.loopback.url,
        tailscale_mode = ?urls.tailscale_status.remote_access_mode(),
        "RustOp Viewer headless runtime is active"
    );
    tracing::info!(
        "Initial pairing still requires a one-time host-approved pairing code from a TUI session or an already trusted browser"
    );

    loop {
        thread::park_timeout(Duration::from_secs(3600));
    }
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
