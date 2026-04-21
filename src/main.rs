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
    let cli = parse_cli()?;

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

    if cli.print_pair_code {
        let pair_code = state.generate_pair_code();
        tracing::info!(
            code = %pair_code.code,
            expires_in_seconds = pair_code.expires_in.as_secs(),
            remaining_attempts = pair_code.remaining_attempts,
            "Host-approved one-time pairing code generated"
        );
    }

    capture::spawn_capture_worker(state.clone());
    server::spawn_server(state.clone());

    match cli.run_mode {
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

struct CliOptions {
    run_mode: RunMode,
    print_pair_code: bool,
}

fn parse_cli() -> Result<CliOptions> {
    let mut run_mode = RunMode::Tui;
    let mut print_pair_code = false;

    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--headless" => run_mode = RunMode::Headless,
            "--print-pair-code" => print_pair_code = true,
            "-h" | "--help" => {
                print_help();
                process::exit(0);
            }
            _ => anyhow::bail!("unknown argument: {arg}"),
        }
    }

    Ok(CliOptions {
        run_mode,
        print_pair_code,
    })
}

fn print_help() {
    println!(
        "\
RustOp Viewer

Usage:
  rustopviewer [--headless] [--print-pair-code]

Options:
  --headless         Run the host runtime without the local terminal UI.
  --print-pair-code  Generate and log one host-approved one-time pairing code at startup.
  -h, --help         Show this help text.
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
        "Initial pairing still requires a host-approved one-time pairing code or an already trusted browser"
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
