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
use serde::Deserialize;
use state::AppState;
use std::{
    env,
    io::{Read, Write},
    net::TcpStream,
    process,
    sync::Arc,
    thread,
    time::Duration,
};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_logging();
    let cli = parse_cli()?;

    if cli.generate_pair_code {
        return request_pair_code_from_running_host();
    }

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
    generate_pair_code: bool,
}

fn parse_cli() -> Result<CliOptions> {
    let mut run_mode = RunMode::Tui;
    let mut print_pair_code = false;
    let mut generate_pair_code = false;

    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--headless" => run_mode = RunMode::Headless,
            "--print-pair-code" => print_pair_code = true,
            "--generate-pair-code" => generate_pair_code = true,
            "-h" | "--help" => {
                print_help();
                process::exit(0);
            }
            _ => anyhow::bail!("unknown argument: {arg}"),
        }
    }

    if generate_pair_code && (run_mode != RunMode::Tui || print_pair_code) {
        anyhow::bail!(
            "--generate-pair-code cannot be combined with --headless or --print-pair-code"
        );
    }

    Ok(CliOptions {
        run_mode,
        print_pair_code,
        generate_pair_code,
    })
}

fn print_help() {
    println!(
        "\
RustOp Viewer

Usage:
  rustopviewer [--headless] [--print-pair-code]
  rustopviewer --generate-pair-code

Options:
  --headless         Run the host runtime without the local terminal UI.
  --print-pair-code  Generate and log one host-approved one-time pairing code at startup.
  --generate-pair-code
                     Ask the running local host to mint a fresh one-time pairing code.
  -h, --help         Show this help text.
"
    );
}

#[derive(Debug, Deserialize)]
struct PairCodeResponse {
    code: String,
    expires_in_seconds: u64,
    remaining_attempts: u8,
}

fn request_pair_code_from_running_host() -> Result<()> {
    let config_store = config::ConfigStore::new()?;
    let config = config_store.load_or_create()?;
    let response = request_pair_code_over_loopback(config.port)?;
    tracing::info!(
        port = config.port,
        code = %response.code,
        expires_in_seconds = response.expires_in_seconds,
        remaining_attempts = response.remaining_attempts,
        "Host-approved one-time pairing code generated"
    );
    println!("{}", response.code);
    Ok(())
}

fn request_pair_code_over_loopback(port: u16) -> Result<PairCodeResponse> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("failed to reach the running host on 127.0.0.1:{port}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("failed to set the pair-code response timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .context("failed to set the pair-code request timeout")?;

    let request = format!(
        "POST /api/admin/pair-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .context("failed to send the pair-code request to the running host")?;
    stream
        .flush()
        .context("failed to flush the pair-code request to the running host")?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .context("failed to read the pair-code response from the running host")?;
    parse_pair_code_response(&response)
}

fn parse_pair_code_response(response: &[u8]) -> Result<PairCodeResponse> {
    let boundary = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("the running host returned an invalid HTTP response")?;
    let (head, body) = response.split_at(boundary);
    let head = std::str::from_utf8(head).context("the running host returned non-UTF-8 headers")?;
    let status_code = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("the running host did not return an HTTP status code")?
        .parse::<u16>()
        .context("the running host returned an invalid HTTP status code")?;

    if status_code != 200 {
        let message = String::from_utf8_lossy(body).trim().to_string();
        anyhow::bail!(
            "the running host rejected the pair-code request (status {status_code}): {}",
            if message.is_empty() {
                "no additional error details were provided"
            } else {
                &message
            }
        );
    }

    serde_json::from_slice(body)
        .context("the running host returned an invalid pair-code response payload")
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

#[cfg(test)]
mod tests {
    use super::parse_pair_code_response;

    #[test]
    fn pair_code_response_parser_reads_success_payloads() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 67\r\n\r\n{\"code\":\"12345678\",\"expires_in_seconds\":120,\"remaining_attempts\":5}";
        let payload = parse_pair_code_response(response).expect("response should parse");
        assert_eq!(payload.code, "12345678");
        assert_eq!(payload.expires_in_seconds, 120);
        assert_eq!(payload.remaining_attempts, 5);
    }

    #[test]
    fn pair_code_response_parser_surfaces_http_errors() {
        let response = b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found";
        let error = parse_pair_code_response(response).expect_err("error response should fail");
        assert!(error.to_string().contains("status 404"));
    }
}
