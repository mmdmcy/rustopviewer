use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value;
use std::{net::Ipv4Addr, process::Command};

#[derive(Debug, Clone)]
pub struct ConnectionUrl {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct UrlSet {
    pub preferred: ConnectionUrl,
    pub mobile_data_preferred: Option<ConnectionUrl>,
    pub tailscale_https: Option<ConnectionUrl>,
    pub loopback: ConnectionUrl,
    pub tailscale_status: TailscaleStatusSnapshot,
}

#[derive(Debug, Clone)]
pub struct TailscaleStatusSnapshot {
    pub is_installed: bool,
    pub is_running: bool,
    pub needs_login: bool,
    pub magic_dns_enabled: bool,
    pub https_certificates_available: bool,
    pub serve_enabled: bool,
    pub host_name: Option<String>,
    pub dns_name: Option<String>,
    pub tailnet_name: Option<String>,
    pub tailscale_ips: Vec<Ipv4Addr>,
}

impl TailscaleStatusSnapshot {
    fn unavailable() -> Self {
        Self {
            is_installed: false,
            is_running: false,
            needs_login: false,
            magic_dns_enabled: false,
            https_certificates_available: false,
            serve_enabled: false,
            host_name: None,
            dns_name: None,
            tailnet_name: None,
            tailscale_ips: Vec::new(),
        }
    }

    pub fn remote_access_mode(&self) -> RemoteAccessMode {
        if !self.is_installed {
            RemoteAccessMode::NeedsTailscaleInstall
        } else if self.needs_login || !self.is_running {
            RemoteAccessMode::NeedsTailscaleLogin
        } else if self.serve_enabled && self.dns_name.is_some() {
            RemoteAccessMode::ReadyHttps
        } else if self.magic_dns_enabled && self.dns_name.is_some() {
            RemoteAccessMode::NeedsServe
        } else {
            RemoteAccessMode::LocalOnly
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAccessMode {
    ReadyHttps,
    NeedsServe,
    NeedsTailscaleLogin,
    NeedsTailscaleInstall,
    LocalOnly,
}

pub fn discover_urls(port: u16) -> UrlSet {
    let loopback = ConnectionUrl {
        label: "Local loopback".to_string(),
        url: format!("http://127.0.0.1:{port}/"),
    };
    let tailscale_status = discover_tailscale_status();

    let tailscale_https = tailscale_status
        .dns_name
        .as_deref()
        .filter(|_| tailscale_status.serve_enabled)
        .map(|dns_name| ConnectionUrl {
            label: "Tailscale HTTPS".to_string(),
            url: format!("https://{dns_name}/"),
        });

    let preferred = tailscale_https.clone().unwrap_or_else(|| loopback.clone());

    UrlSet {
        preferred,
        mobile_data_preferred: tailscale_https.clone(),
        tailscale_https,
        loopback,
        tailscale_status,
    }
}

pub fn enable_tailscale_https(port: u16) -> Result<()> {
    let port_text = port.to_string();
    run_tailscale_command(&["serve", "--bg", "--yes", &port_text]).map(|_| ())
}

fn discover_tailscale_status() -> TailscaleStatusSnapshot {
    let Some(output) = tailscale_status_output() else {
        return TailscaleStatusSnapshot::unavailable();
    };

    let mut snapshot = parse_tailscale_status(&output).unwrap_or_else(|| TailscaleStatusSnapshot {
        is_installed: true,
        ..TailscaleStatusSnapshot::unavailable()
    });
    snapshot.serve_enabled = discover_tailscale_serve_enabled();
    snapshot
}

fn parse_tailscale_status(output: &[u8]) -> Option<TailscaleStatusSnapshot> {
    let status: TailscaleStatus = serde_json::from_slice(output).ok()?;
    let backend_state = status.backend_state.as_deref().unwrap_or_default();
    let auth_url_present = status
        .auth_url
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let self_node = status.self_node.as_ref();
    let dns_name = self_node
        .and_then(|node| node.dns_name.as_deref())
        .map(str::trim)
        .map(|value| value.trim_end_matches('.').to_string())
        .filter(|value| !value.is_empty());
    let tailscale_ips = self_node
        .map(|node| {
            node.tailscale_ips
                .iter()
                .filter_map(|value| value.parse::<Ipv4Addr>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(TailscaleStatusSnapshot {
        is_installed: true,
        is_running: backend_state.eq_ignore_ascii_case("running"),
        needs_login: auth_url_present || backend_state.eq_ignore_ascii_case("needslogin"),
        magic_dns_enabled: status
            .current_tailnet
            .as_ref()
            .is_some_and(|tailnet| tailnet.magic_dns_enabled),
        https_certificates_available: !status.cert_domains.is_empty(),
        serve_enabled: false,
        host_name: self_node.and_then(|node| node.host_name.clone()),
        dns_name,
        tailnet_name: status.current_tailnet.and_then(|tailnet| tailnet.name),
        tailscale_ips,
    })
}

fn discover_tailscale_serve_enabled() -> bool {
    let Some(output) = tailscale_serve_status_output() else {
        return false;
    };

    parse_tailscale_serve_enabled(&output)
}

fn parse_tailscale_serve_enabled(output: &[u8]) -> bool {
    serde_json::from_slice::<Value>(output)
        .ok()
        .and_then(|value| value.as_object().map(|object| !object.is_empty()))
        .unwrap_or(false)
}

fn tailscale_status_output() -> Option<Vec<u8>> {
    run_tailscale_command(&["status", "--json"]).ok()
}

fn tailscale_serve_status_output() -> Option<Vec<u8>> {
    run_tailscale_command(&["serve", "status", "--json"]).ok()
}

fn run_tailscale_command(args: &[&str]) -> Result<Vec<u8>> {
    let mut last_error: Option<anyhow::Error> = None;

    for candidate in tailscale_command_candidates() {
        match Command::new(candidate).args(args).output() {
            Ok(output) if output.status.success() => return Ok(output.stdout),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let detail = if !stderr.is_empty() {
                    stderr
                } else if !stdout.is_empty() {
                    stdout
                } else {
                    format!("process exited with {}", output.status)
                };
                last_error = Some(anyhow!("{candidate} {} failed: {detail}", args.join(" ")));
            }
            Err(err) => {
                last_error = Some(
                    anyhow!(err).context(format!("{candidate} {} failed to start", args.join(" "))),
                );
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("tailscale CLI is not available")))
}

fn tailscale_command_candidates() -> impl Iterator<Item = &'static str> {
    [
        "tailscale",
        "tailscale.exe",
        r"C:\Program Files\Tailscale\tailscale.exe",
    ]
    .into_iter()
}

#[derive(Debug, Deserialize)]
struct TailscaleStatus {
    #[serde(rename = "BackendState")]
    backend_state: Option<String>,
    #[serde(rename = "AuthURL")]
    auth_url: Option<String>,
    #[serde(rename = "CertDomains", default)]
    cert_domains: Vec<String>,
    #[serde(rename = "Self")]
    self_node: Option<TailscaleNode>,
    #[serde(rename = "CurrentTailnet")]
    current_tailnet: Option<TailscaleTailnet>,
}

#[derive(Debug, Deserialize)]
struct TailscaleNode {
    #[serde(rename = "HostName")]
    host_name: Option<String>,
    #[serde(rename = "DNSName")]
    dns_name: Option<String>,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TailscaleTailnet {
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "MagicDNSEnabled")]
    magic_dns_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::{
        RemoteAccessMode, TailscaleStatusSnapshot, parse_tailscale_serve_enabled,
        parse_tailscale_status,
    };
    use std::net::Ipv4Addr;

    #[test]
    fn tailscale_status_deserializes_mobile_ready_snapshot() {
        let status = parse_tailscale_status(
            br#"{
                "BackendState": "Running",
                "AuthURL": "",
                "CertDomains": [
                    "sparta.tail359cf9.ts.net"
                ],
                "Self": {
                    "HostName": "Sparta",
                    "DNSName": "sparta.tail359cf9.ts.net.",
                    "TailscaleIPs": [
                        "100.124.204.65",
                        "fd7a:115c:a1e0::3401:cca0"
                    ]
                },
                "CurrentTailnet": {
                    "Name": "katteke727@gmail.com",
                    "MagicDNSEnabled": true
                }
            }"#,
        )
        .expect("status json should deserialize");

        assert!(status.is_installed);
        assert!(status.is_running);
        assert!(!status.needs_login);
        assert!(status.magic_dns_enabled);
        assert!(status.https_certificates_available);
        assert_eq!(status.host_name.as_deref(), Some("Sparta"));
        assert_eq!(status.dns_name.as_deref(), Some("sparta.tail359cf9.ts.net"));
        assert_eq!(status.tailnet_name.as_deref(), Some("katteke727@gmail.com"));
        assert_eq!(status.tailscale_ips, vec![Ipv4Addr::new(100, 124, 204, 65)]);
    }

    #[test]
    fn tailscale_serve_status_detects_empty_and_non_empty_json() {
        assert!(!parse_tailscale_serve_enabled(br#"{}"#));
        assert!(parse_tailscale_serve_enabled(
            br#"{"TCP":{"443":{"HTTPS":true}}}"#
        ));
    }

    #[test]
    fn remote_access_mode_reports_expected_states() {
        let unavailable = TailscaleStatusSnapshot::unavailable();
        assert_eq!(
            unavailable.remote_access_mode(),
            RemoteAccessMode::NeedsTailscaleInstall
        );

        let login_required = TailscaleStatusSnapshot {
            is_installed: true,
            needs_login: true,
            ..TailscaleStatusSnapshot::unavailable()
        };
        assert_eq!(
            login_required.remote_access_mode(),
            RemoteAccessMode::NeedsTailscaleLogin
        );

        let serve_needed = TailscaleStatusSnapshot {
            is_installed: true,
            is_running: true,
            magic_dns_enabled: true,
            dns_name: Some("sparta.tail359cf9.ts.net".to_string()),
            ..TailscaleStatusSnapshot::unavailable()
        };
        assert_eq!(
            serve_needed.remote_access_mode(),
            RemoteAccessMode::NeedsServe
        );

        let https_ready = TailscaleStatusSnapshot {
            serve_enabled: true,
            ..serve_needed
        };
        assert_eq!(
            https_ready.remote_access_mode(),
            RemoteAccessMode::ReadyHttps
        );
    }
}
