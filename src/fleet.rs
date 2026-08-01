use anyhow::{Context, Result, anyhow};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpStream,
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{network, platform, state::AppState};

pub const FLEET_OFFLINE_AFTER: Duration = Duration::from_secs(45);
pub const FLEET_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDevice {
    pub device_id: String,
    pub device_code: String,
    pub display_name: String,
    pub os: String,
    pub os_family: String,
    pub viewer_url: String,
    pub capabilities: Vec<String>,
    pub last_seen_unix_ms: u128,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetRegisterRequest {
    pub device_id: String,
    pub device_code: String,
    pub display_name: String,
    pub os: String,
    pub os_family: String,
    pub viewer_url: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetHeartbeatRequest {
    pub device_id: String,
    #[serde(default)]
    pub viewer_url: Option<String>,
}

#[derive(Debug, Default)]
pub struct FleetRegistry {
    devices: RwLock<HashMap<String, RegistryEntry>>,
}

#[derive(Debug, Clone)]
struct RegistryEntry {
    device: FleetDevice,
    last_seen: Instant,
}

impl FleetRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, request: FleetRegisterRequest) -> FleetDevice {
        let now = Instant::now();
        let device = FleetDevice {
            device_id: request.device_id.clone(),
            device_code: request.device_code,
            display_name: request.display_name,
            os: request.os,
            os_family: request.os_family,
            viewer_url: normalize_viewer_url(&request.viewer_url),
            capabilities: if request.capabilities.is_empty() {
                vec!["desktop".to_string()]
            } else {
                request.capabilities
            },
            last_seen_unix_ms: unix_now_ms(),
            online: true,
        };
        self.devices.write().insert(
            request.device_id,
            RegistryEntry {
                device: device.clone(),
                last_seen: now,
            },
        );
        device
    }

    pub fn heartbeat(&self, request: FleetHeartbeatRequest) -> Result<FleetDevice> {
        let mut devices = self.devices.write();
        let entry = devices
            .get_mut(&request.device_id)
            .ok_or_else(|| anyhow!("unknown fleet device; register first"))?;
        entry.last_seen = Instant::now();
        entry.device.last_seen_unix_ms = unix_now_ms();
        entry.device.online = true;
        if let Some(url) = request.viewer_url {
            entry.device.viewer_url = normalize_viewer_url(&url);
        }
        Ok(entry.device.clone())
    }

    pub fn list(&self) -> Vec<FleetDevice> {
        let now = Instant::now();
        let mut devices = self.devices.write();
        let mut listed = Vec::with_capacity(devices.len());
        for entry in devices.values_mut() {
            entry.device.online = now.duration_since(entry.last_seen) < FLEET_OFFLINE_AFTER;
            listed.push(entry.device.clone());
        }
        listed.sort_by(|left, right| {
            right
                .online
                .cmp(&left.online)
                .then_with(|| left.display_name.cmp(&right.display_name))
        });
        listed
    }

    #[allow(dead_code)]
    pub fn find(&self, query: &str) -> Option<FleetDevice> {
        let query = query.trim();
        self.list().into_iter().find(|device| {
            device.device_id.eq_ignore_ascii_case(query)
                || device.device_code.eq_ignore_ascii_case(query)
                || device.display_name.eq_ignore_ascii_case(query)
                || device
                    .display_name
                    .split('@')
                    .next()
                    .is_some_and(|part| part.eq_ignore_ascii_case(query))
        })
    }
}

pub fn local_device_identity(state: &AppState) -> FleetRegisterRequest {
    let username = platform::username();
    let hostname = platform::hostname();
    let display_name = match (username.as_deref(), hostname.as_deref()) {
        (Some(username), Some(hostname)) => format!("{username}@{hostname}"),
        (Some(username), None) => username.to_string(),
        (None, Some(hostname)) => hostname.to_string(),
        (None, None) => "RustOp host".to_string(),
    };
    let device_code = state.device_code();
    let device_id = hostname
        .clone()
        .unwrap_or_else(|| device_code.clone())
        .to_ascii_lowercase();
    let urls = network::discover_urls(state.port());
    let viewer_url = urls
        .tailscale_http
        .map(|url| url.url)
        .unwrap_or(urls.preferred.url);
    FleetRegisterRequest {
        device_id,
        device_code,
        display_name,
        os: platform::os_label(),
        os_family: platform::os_family(),
        viewer_url,
        capabilities: vec!["desktop".to_string()],
    }
}

pub fn spawn_local_self_register(state: Arc<AppState>) {
    thread::spawn(move || {
        loop {
            if let Some(registry) = state.fleet_registry() {
                let request = local_device_identity(&state);
                registry.register(request);
            }
            thread::sleep(FLEET_HEARTBEAT_INTERVAL);
        }
    });
}

pub fn spawn_agent_registration(state: Arc<AppState>, registry_url: String, token: String) {
    thread::spawn(move || {
        let mut consecutive_failures = 0u32;
        loop {
            let identity = local_device_identity(&state);
            match register_with_host(&registry_url, &token, &identity) {
                Ok(()) => {
                    if consecutive_failures > 0 {
                        tracing::info!(host = %registry_url, "Reconnected to fleet registry");
                    }
                    consecutive_failures = 0;
                    if let Err(err) = heartbeat_with_host(
                        &registry_url,
                        &token,
                        &FleetHeartbeatRequest {
                            device_id: identity.device_id.clone(),
                            viewer_url: Some(identity.viewer_url.clone()),
                        },
                    ) {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        tracing::warn!(error = %err, "Fleet heartbeat failed");
                    }
                }
                Err(err) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    tracing::warn!(
                        error = %err,
                        host = %registry_url,
                        failures = consecutive_failures,
                        "Fleet registration failed"
                    );
                }
            }
            thread::sleep(FLEET_HEARTBEAT_INTERVAL);
        }
    });
}

pub fn fetch_devices(registry_url: &str, token: &str) -> Result<Vec<FleetDevice>> {
    let body = http_json(
        "GET",
        &join_url(registry_url, "/v1/fleet/devices"),
        token,
        None,
    )?;
    #[derive(Deserialize)]
    struct DevicesResponse {
        devices: Vec<FleetDevice>,
    }
    let payload: DevicesResponse =
        serde_json::from_str(&body).context("invalid fleet devices response")?;
    Ok(payload.devices)
}

fn register_with_host(
    registry_url: &str,
    token: &str,
    request: &FleetRegisterRequest,
) -> Result<()> {
    let body = serde_json::to_string(request)?;
    let _ = http_json(
        "POST",
        &join_url(registry_url, "/v1/fleet/register"),
        token,
        Some(&body),
    )?;
    Ok(())
}

fn heartbeat_with_host(
    registry_url: &str,
    token: &str,
    request: &FleetHeartbeatRequest,
) -> Result<()> {
    let body = serde_json::to_string(request)?;
    let _ = http_json(
        "POST",
        &join_url(registry_url, "/v1/fleet/heartbeat"),
        token,
        Some(&body),
    )?;
    Ok(())
}

fn join_url(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

fn normalize_viewer_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.ends_with('/') {
        trimmed.to_string()
    } else {
        format!("{trimmed}/")
    }
}

fn unix_now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn http_json(method: &str, url: &str, token: &str, body: Option<&str>) -> Result<String> {
    let parsed = parse_http_url(url)?;
    let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port))
        .with_context(|| format!("failed to connect to {}", parsed.host))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .ok();

    let body_bytes = body.unwrap_or("").as_bytes();
    let content_type = if body.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nAccept: application/json\r\n{content_type}Content-Length: {len}\r\nConnection: close\r\n\r\n",
        path = parsed.path,
        host = if parsed.port == 80 {
            parsed.host.clone()
        } else {
            format!("{}:{}", parsed.host, parsed.port)
        },
        len = body_bytes.len(),
    );
    stream.write_all(request.as_bytes())?;
    if !body_bytes.is_empty() {
        stream.write_all(body_bytes)?;
    }
    stream.flush()?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let boundary = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("invalid HTTP response from fleet host")?;
    let (head, body) = response.split_at(boundary);
    let head = std::str::from_utf8(head).context("non-UTF-8 fleet response headers")?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("missing HTTP status")?
        .parse::<u16>()
        .context("invalid HTTP status")?;
    let body_text = String::from_utf8_lossy(body).trim().to_string();
    if !(200..300).contains(&status) {
        anyhow::bail!("fleet host returned status {status}: {body_text}");
    }
    Ok(body_text)
}

struct ParsedHttpUrl {
    host: String,
    port: u16,
    path: String,
}

fn parse_http_url(url: &str) -> Result<ParsedHttpUrl> {
    let url = url.trim();
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("fleet URLs must be http:// (use Tailscale HTTP)"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
        (
            host.to_string(),
            port.parse::<u16>()
                .with_context(|| format!("invalid port in fleet URL: {url}"))?,
        )
    } else {
        (authority.to_string(), 80)
    };
    Ok(ParsedHttpUrl {
        host,
        port,
        path: if path.is_empty() {
            "/".to_string()
        } else {
            path
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{FleetRegisterRequest, FleetRegistry, normalize_viewer_url, parse_http_url};

    #[test]
    fn registry_tracks_online_devices() {
        let registry = FleetRegistry::new();
        registry.register(FleetRegisterRequest {
            device_id: "workstation".into(),
            device_code: "WS-01".into(),
            display_name: "user@workstation".into(),
            os: "Linux".into(),
            os_family: "linux".into(),
            viewer_url: "http://100.64.1.10:45080".into(),
            capabilities: vec!["desktop".into()],
        });
        let devices = registry.list();
        assert_eq!(devices.len(), 1);
        assert!(devices[0].online);
        assert_eq!(devices[0].viewer_url, "http://100.64.1.10:45080/");
    }

    #[test]
    fn parse_http_url_supports_paths() {
        let parsed = parse_http_url("http://100.64.1.10:45080/v1/fleet/devices").unwrap();
        assert_eq!(parsed.host, "100.64.1.10");
        assert_eq!(parsed.port, 45080);
        assert_eq!(parsed.path, "/v1/fleet/devices");
    }

    #[test]
    fn viewer_url_gets_trailing_slash() {
        assert_eq!(
            normalize_viewer_url("http://example.test:45080"),
            "http://example.test:45080/"
        );
    }
}
