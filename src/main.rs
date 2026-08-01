mod capture;
mod config;
mod fleet;
mod input;
mod model;
mod network;
mod oidc;
mod platform;
mod security;
mod server;
mod state;
mod tui;

use anyhow::{Context, Result};
use security::TrustedBrowserStore;
use serde::Deserialize;
use state::{AppState, RuntimeAuth, RuntimeRole};
use std::{
    collections::HashMap,
    env, fs,
    io::{ErrorKind, Read, Write},
    net::TcpStream,
    sync::Arc,
    thread,
    time::Duration,
};
use tracing_subscriber::EnvFilter;

const DOTENV_CANDIDATES: [&str; 2] = [".private/rustopviewer.env", ".env"];

fn main() -> Result<()> {
    let env_bootstrap = load_project_env()?;
    init_logging();
    let cli = parse_cli()?;

    if matches!(cli.command, Command::Help) {
        print_help();
        return Ok(());
    }

    if cli.generate_pair_code {
        return request_pair_code_from_running_host(env_bootstrap.admin_token.as_deref());
    }

    if matches!(cli.command, Command::Devices | Command::Open { .. } | Command::Status)
        && !cli.command.command_starts_runtime()
    {
        return run_fleet_query_command(&cli, &env_bootstrap);
    }

    let config_store = config::ConfigStore::new()?;
    let mut config = config_store.load_or_create()?;
    apply_env_bootstrap(&config_store, &mut config, &env_bootstrap)?;
    if cli.has_config_action() {
        apply_config_actions(&config_store, &mut config, &cli)?;
        return Ok(());
    }

    let role = match &cli.command {
        Command::Host => RuntimeRole::Host,
        Command::Agent { .. } => RuntimeRole::Agent,
        _ => RuntimeRole::Standalone,
    };

    let trusted_browser_store = TrustedBrowserStore::new(config_store.trusted_browsers_path())?;
    let monitors = match capture::discover_monitors() {
        Ok(monitors) => monitors,
        Err(err) => {
            tracing::warn!(error = %err, "Display capture is unavailable; continuing without monitors");
            Vec::new()
        }
    };
    let input_tx = input::spawn_input_worker().context("failed to start input worker")?;
    let is_elevated = platform::is_process_elevated();

    let state = Arc::new(AppState::new(
        config_store,
        config,
        monitors,
        input_tx,
        trusted_browser_store,
        is_elevated,
        RuntimeAuth {
            admin_token_hash: env_bootstrap
                .admin_token
                .as_deref()
                .map(security::hash_admin_token),
            masterdale_token_hash: env_bootstrap
                .masterdale_token
                .as_deref()
                .map(security::hash_admin_token),
            oidc: env_bootstrap.oidc.clone(),
        },
        role,
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

    match role {
        RuntimeRole::Host => {
            if env_bootstrap.masterdale_token.is_none() {
                tracing::warn!(
                    "ROV_MASTERDALE_TOKEN/DALE_TOKEN is unset; fleet register/list APIs will reject clients"
                );
            }
            fleet::spawn_local_self_register(state.clone());
        }
        RuntimeRole::Agent => {
            let Command::Agent { host } = &cli.command else {
                unreachable!("agent role requires agent command");
            };
            let token = env_bootstrap.masterdale_token.clone().context(
                "agent mode requires ROV_MASTERDALE_TOKEN or DALE_TOKEN so this device can register",
            )?;
            fleet::spawn_agent_registration(state.clone(), host.clone(), token);
        }
        RuntimeRole::Standalone => {}
    }

    let run_mode = if matches!(cli.command, Command::Agent { .. }) && cli.run_mode == RunMode::Tui {
        RunMode::Headless
    } else {
        cli.run_mode
    };

    match run_mode {
        RunMode::Tui => {
            tui::run(state).context("failed to run the RustOp Viewer terminal UI")?;
        }
        RunMode::Headless => {
            run_headless(state, role);
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    Tui,
    Headless,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Runtime,
    Host,
    Agent { host: String },
    Status,
    Devices,
    Open { device: String },
    Help,
}

impl Command {
    fn command_starts_runtime(&self) -> bool {
        matches!(self, Self::Runtime | Self::Host | Self::Agent { .. })
    }
}

struct CliOptions {
    command: Command,
    run_mode: RunMode,
    print_pair_code: bool,
    generate_pair_code: bool,
    set_device_code: Option<String>,
    set_access_password: Option<String>,
    clear_access_password: bool,
    print_device: bool,
    fleet_host: Option<String>,
}

#[derive(Debug, Default)]
struct EnvBootstrap {
    device_code: Option<String>,
    access_password: Option<String>,
    admin_token: Option<String>,
    masterdale_token: Option<String>,
    fleet_host: Option<String>,
    oidc: Option<oidc::OidcConfig>,
}

impl CliOptions {
    fn has_config_action(&self) -> bool {
        self.set_device_code.is_some()
            || self.set_access_password.is_some()
            || self.clear_access_password
            || self.print_device
    }
}

fn run_fleet_query_command(cli: &CliOptions, env_bootstrap: &EnvBootstrap) -> Result<()> {
    match &cli.command {
        Command::Status => {
            let config_store = config::ConfigStore::new()?;
            let config = config_store.load_or_create()?;
            let urls = network::discover_urls(config.port);
            println!("device_code={}", config.device_code);
            println!("port={}", config.port);
            println!("preferred_url={}", urls.preferred.url);
            println!("loopback_url={}", urls.loopback.url);
            if let Some(host) = resolve_fleet_host(cli, env_bootstrap) {
                println!("fleet_host={host}");
            }
            if env_bootstrap.masterdale_token.is_some() {
                println!("masterdale_token=configured");
            } else {
                println!("masterdale_token=missing");
            }
            Ok(())
        }
        Command::Devices => {
            let host = resolve_fleet_host(cli, env_bootstrap)
                .context("pass --host URL or set ROV_FLEET_HOST")?;
            let token = env_bootstrap
                .masterdale_token
                .as_deref()
                .context("ROV_MASTERDALE_TOKEN or DALE_TOKEN is required")?;
            let devices = fleet::fetch_devices(&host, token)?;
            println!(
                "{:<16} {:<8} {:<22} {}",
                "DEVICE", "ONLINE", "OS", "URL"
            );
            for device in devices {
                println!(
                    "{:<16} {:<8} {:<22} {}",
                    device.device_id,
                    if device.online { "yes" } else { "no" },
                    truncate(&device.os, 22),
                    device.viewer_url
                );
            }
            Ok(())
        }
        Command::Open { device } => {
            let host = resolve_fleet_host(cli, env_bootstrap)
                .context("pass --host URL or set ROV_FLEET_HOST")?;
            let token = env_bootstrap
                .masterdale_token
                .as_deref()
                .context("ROV_MASTERDALE_TOKEN or DALE_TOKEN is required")?;
            let devices = fleet::fetch_devices(&host, token)?;
            let found = devices
                .into_iter()
                .find(|entry| {
                    entry.device_id.eq_ignore_ascii_case(device)
                        || entry.device_code.eq_ignore_ascii_case(device)
                        || entry.display_name.eq_ignore_ascii_case(device)
                })
                .with_context(|| format!("device {device:?} was not found in the fleet registry"))?;
            println!("{}", found.viewer_url);
            Ok(())
        }
        _ => Ok(()),
    }
}

fn resolve_fleet_host(cli: &CliOptions, env_bootstrap: &EnvBootstrap) -> Option<String> {
    cli.fleet_host
        .clone()
        .or_else(|| env_bootstrap.fleet_host.clone())
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        format!(
            "{}…",
            value.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

fn load_project_env() -> Result<EnvBootstrap> {
    let values = load_dotenv_values()?;
    let device_code = env_value(&values, "ROV_DEVICE_CODE");
    let access_password = env_value(&values, "ROV_ACCESS_PASSWORD");
    let admin_token = env_value(&values, "ROV_ADMIN_TOKEN");
    let masterdale_token =
        env_value(&values, "ROV_MASTERDALE_TOKEN").or_else(|| env_value(&values, "DALE_TOKEN"));
    let fleet_host = env_value(&values, "ROV_FLEET_HOST");
    let oidc = oidc::OidcConfig::from_values(
        env_value(&values, "ROV_OIDC_ISSUER"),
        env_value(&values, "ROV_OIDC_CLIENT_ID"),
        env_value(&values, "ROV_OIDC_CLIENT_SECRET"),
        env_value(&values, "ROV_OIDC_REDIRECT_URL"),
        env_value(&values, "ROV_OIDC_ALLOWED_SUBJECTS"),
    )?;

    if let Some(token) = admin_token.as_deref()
        && token.chars().count() < 16
    {
        anyhow::bail!("ROV_ADMIN_TOKEN must be at least 16 characters");
    }
    if let Some(token) = masterdale_token.as_deref()
        && token.chars().count() < 16
    {
        anyhow::bail!("ROV_MASTERDALE_TOKEN/DALE_TOKEN must be at least 16 characters");
    }

    Ok(EnvBootstrap {
        device_code,
        access_password,
        admin_token,
        masterdale_token,
        fleet_host,
        oidc,
    })
}

fn load_dotenv_values() -> Result<HashMap<String, String>> {
    for path in DOTENV_CANDIDATES {
        match fs::read_to_string(path) {
            Ok(content) => {
                return parse_dotenv(&content).with_context(|| format!("failed to parse {path}"));
            }
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("failed to read {path}")),
        }
    }

    Ok(HashMap::new())
}

fn parse_dotenv(content: &str) -> Result<HashMap<String, String>> {
    let mut values = HashMap::new();
    for (line_index, line) in content.lines().enumerate() {
        let Some((key, value)) = parse_dotenv_line(line)
            .with_context(|| format!("failed to parse .env line {}", line_index + 1))?
        else {
            continue;
        };
        values.insert(key, value);
    }
    Ok(values)
}

fn parse_dotenv_line(line: &str) -> Result<Option<(String, String)>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }

    let line = line.strip_prefix("export ").unwrap_or(line).trim();
    let (key, value) = line.split_once('=').context("expected KEY=VALUE syntax")?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        anyhow::bail!("invalid environment variable name");
    }

    Ok(Some((key.to_string(), parse_dotenv_value(value.trim()))))
}

fn parse_dotenv_value(value: &str) -> String {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        return value[1..value.len() - 1]
            .replace("\\n", "\n")
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }

    value
        .split_once(" #")
        .map(|(head, _)| head)
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn env_value(values: &HashMap<String, String>, key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .or_else(|| values.get(key).cloned())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn apply_env_bootstrap(
    config_store: &config::ConfigStore,
    config: &mut config::AppConfig,
    bootstrap: &EnvBootstrap,
) -> Result<()> {
    let mut changed = false;
    if let Some(device_code) = bootstrap.device_code.as_deref() {
        let device_code = config::normalize_device_code(device_code).ok_or_else(|| {
            anyhow::anyhow!("ROV_DEVICE_CODE must be 4-32 ASCII letters, numbers, '-' or '_'")
        })?;
        if config.device_code != device_code {
            config.device_code = device_code;
            changed = true;
        }
    }

    if let Some(password) = bootstrap.access_password.as_deref() {
        let password_changed = config
            .access_password
            .as_ref()
            .map(|existing| !security::access_password_matches_config(existing, password))
            .unwrap_or(true);
        if password_changed {
            config.access_password = Some(
                security::access_password_config_from_plaintext(password)
                    .map_err(|err| anyhow::anyhow!(err.to_string()))?,
            );
            changed = true;
        }
    }

    if changed {
        config_store.save(config)?;
    }

    Ok(())
}

fn parse_cli() -> Result<CliOptions> {
    let mut run_mode = RunMode::Tui;
    let mut print_pair_code = false;
    let mut generate_pair_code = false;
    let mut set_device_code = None;
    let mut set_access_password = None;
    let mut clear_access_password = false;
    let mut print_device = false;
    let mut fleet_host = None;
    let mut command = Command::Runtime;
    let mut positional = Vec::new();

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "host" if positional.is_empty() && command == Command::Runtime => {
                command = Command::Host;
            }
            "agent" if positional.is_empty() && command == Command::Runtime => {
                command = Command::Agent {
                    host: String::new(),
                };
            }
            "status" if positional.is_empty() && command == Command::Runtime => {
                command = Command::Status;
            }
            "devices" if positional.is_empty() && command == Command::Runtime => {
                command = Command::Devices;
            }
            "open" if positional.is_empty() && command == Command::Runtime => {
                command = Command::Open {
                    device: String::new(),
                };
            }
            "--host" => {
                let value = args.next().context("--host requires a URL")?;
                match &mut command {
                    Command::Agent { host } => *host = value.clone(),
                    _ => fleet_host = Some(value),
                }
            }
            "--headless" => run_mode = RunMode::Headless,
            "--print-pair-code" => print_pair_code = true,
            "--generate-pair-code" => generate_pair_code = true,
            "--set-device-code" => {
                set_device_code = Some(
                    args.next()
                        .context("--set-device-code requires a device code value")?,
                );
            }
            "--set-access-password" => {
                set_access_password = Some(
                    args.next()
                        .context("--set-access-password requires a password value")?,
                );
            }
            "--clear-access-password" => clear_access_password = true,
            "--print-device" => print_device = true,
            "-h" | "--help" => {
                command = Command::Help;
            }
            other if other.starts_with('-') => anyhow::bail!("unknown argument: {other}"),
            other => positional.push(other.to_string()),
        }
    }

    if let Command::Open { device } = &mut command {
        *device = positional
            .first()
            .cloned()
            .context("open requires a device id, code, or display name")?;
        positional.clear();
    }

    if let Command::Agent { host } = &mut command {
        if host.is_empty() {
            *host = fleet_host
                .clone()
                .or_else(|| env::var("ROV_FLEET_HOST").ok())
                .context("agent requires --host URL or ROV_FLEET_HOST")?;
        }
        if !host.starts_with("http://") && !host.starts_with("https://") {
            *host = format!("http://{host}");
        }
    }

    if !positional.is_empty() && !matches!(command, Command::Help) {
        anyhow::bail!("unexpected arguments: {}", positional.join(" "));
    }

    if generate_pair_code && (run_mode != RunMode::Tui || print_pair_code) {
        anyhow::bail!(
            "--generate-pair-code cannot be combined with --headless or --print-pair-code"
        );
    }

    if generate_pair_code
        && (set_device_code.is_some()
            || set_access_password.is_some()
            || clear_access_password
            || print_device)
    {
        anyhow::bail!("--generate-pair-code cannot be combined with device config options");
    }

    if set_access_password.is_some() && clear_access_password {
        anyhow::bail!("--set-access-password cannot be combined with --clear-access-password");
    }

    if (run_mode != RunMode::Tui || print_pair_code)
        && (set_device_code.is_some()
            || set_access_password.is_some()
            || clear_access_password
            || print_device)
    {
        anyhow::bail!("device config options cannot be combined with runtime options");
    }

    Ok(CliOptions {
        command,
        run_mode,
        print_pair_code,
        generate_pair_code,
        set_device_code,
        set_access_password,
        clear_access_password,
        print_device,
        fleet_host,
    })
}

fn print_help() {
    println!(
        "\
RustOp Viewer

Usage:
  rustopviewer host [--headless] [--print-pair-code]
  rustopviewer agent --host URL [--headless]
  rustopviewer status
  rustopviewer devices [--host URL]
  rustopviewer open <device> [--host URL]
  rustopviewer [--headless] [--print-pair-code]
  rustopviewer --generate-pair-code
  rustopviewer --set-device-code CODE
  rustopviewer --set-access-password PASSWORD
  rustopviewer --clear-access-password
  rustopviewer --print-device

Fleet commands:
  host               Run as the fleet registry + local desktop host.
  agent --host URL   Run as a capture agent and register with the host.
  status             Print local device status and preferred URLs.
  devices            List devices registered on the fleet host.
  open <device>      Print the viewer URL for a registered device.

Options:
  --host URL         Fleet registry URL (also ROV_FLEET_HOST).
  --headless         Run without the local terminal UI.
  --print-pair-code  Generate and log one host-approved one-time pairing code at startup.
  --generate-pair-code
                     Ask the running local host to mint a fresh one-time pairing code.
  --set-device-code CODE
                     Set this host's custom dashboard/login device code.
  --set-access-password PASSWORD
                     Set the unattended access password for this host.
  --clear-access-password
                     Disable unattended password login for this host.
  --print-device     Print this host's device code and dashboard label.
  -h, --help         Show this help text.

Auth:
  Set the same DALE_TOKEN / ROV_MASTERDALE_TOKEN on every device.
"
    );
}

fn apply_config_actions(
    config_store: &config::ConfigStore,
    config: &mut config::AppConfig,
    cli: &CliOptions,
) -> Result<()> {
    if let Some(device_code) = cli.set_device_code.as_deref() {
        config.device_code = config::normalize_device_code(device_code).ok_or_else(|| {
            anyhow::anyhow!("device codes must be 4-32 ASCII letters, numbers, '-' or '_'")
        })?;
        config_store.save(config)?;
        println!("Device code: {}", config.device_code);
    }

    if let Some(password) = cli.set_access_password.as_deref() {
        config.access_password = Some(
            security::access_password_config_from_plaintext(password)
                .map_err(|err| anyhow::anyhow!(err.to_string()))?,
        );
        config_store.save(config)?;
        println!("Access password: configured");
    }

    if cli.clear_access_password {
        config.access_password = None;
        config_store.save(config)?;
        println!("Access password: disabled");
    }

    if cli.print_device {
        let username = platform::username();
        let hostname = platform::hostname();
        let label = match (username.as_deref(), hostname.as_deref()) {
            (Some(username), Some(hostname)) => format!("{username}@{hostname}"),
            (Some(username), None) => username.to_string(),
            (None, Some(hostname)) => hostname.to_string(),
            (None, None) => "RustOp host".to_string(),
        };
        println!("Device code: {}", config.device_code);
        println!("Host: {label}");
        println!("OS: {}", platform::os_label());
        println!(
            "Access password: {}",
            if config.access_password.is_some() {
                "configured"
            } else {
                "disabled"
            }
        );
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct PairCodeResponse {
    code: String,
    expires_in_seconds: u64,
    remaining_attempts: u8,
}

fn request_pair_code_from_running_host(admin_token: Option<&str>) -> Result<()> {
    let config_store = config::ConfigStore::new()?;
    let config = config_store.load_or_create()?;
    let response = request_pair_code_over_loopback(config.port, admin_token)?;
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

fn request_pair_code_over_loopback(
    port: u16,
    admin_token: Option<&str>,
) -> Result<PairCodeResponse> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("failed to reach the running host on 127.0.0.1:{port}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("failed to set the pair-code response timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .context("failed to set the pair-code request timeout")?;

    let admin_header = admin_token
        .map(|token| format!("X-ROV-Admin-Token: {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /api/admin/pair-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{admin_header}Content-Length: 0\r\nConnection: close\r\n\r\n"
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

fn run_headless(state: Arc<AppState>, role: RuntimeRole) {
    let urls = network::discover_urls(state.port());
    tracing::info!(
        port = state.port(),
        role = ?role,
        preferred_url = %urls.preferred.url,
        loopback_url = %urls.loopback.url,
        admin_url = %format!("http://127.0.0.1:{}/admin", state.port()),
        fleet_enabled = state.fleet_enabled(),
        tailscale_mode = ?urls.tailscale_status.remote_access_mode(),
        "RustOp Viewer headless runtime is active"
    );
    if matches!(role, RuntimeRole::Standalone | RuntimeRole::Host) {
        tracing::info!(
            "Initial pairing still requires a host-approved one-time pairing code, Masterdale token, or an already trusted browser"
        );
    }

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
    use super::{parse_dotenv, parse_pair_code_response};

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

    #[test]
    fn dotenv_parser_reads_bootstrap_values() {
        let values = parse_dotenv(
            r#"
# comment
ROV_DEVICE_CODE=WORKSTATION-01
ROV_ACCESS_PASSWORD="sample unattended password"
export ROV_ADMIN_TOKEN='sample-admin-token'
"#,
        )
        .expect("dotenv should parse");

        assert_eq!(
            values.get("ROV_DEVICE_CODE").map(String::as_str),
            Some("WORKSTATION-01")
        );
        assert_eq!(
            values.get("ROV_ACCESS_PASSWORD").map(String::as_str),
            Some("sample unattended password")
        );
        assert_eq!(
            values.get("ROV_ADMIN_TOKEN").map(String::as_str),
            Some("sample-admin-token")
        );
    }
}
