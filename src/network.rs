use if_addrs::{IfAddr, get_if_addrs};
use serde::Deserialize;
use std::{collections::BTreeSet, net::Ipv4Addr, process::Command};

#[derive(Debug, Clone)]
pub struct ConnectionUrl {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct UrlSet {
    pub preferred: ConnectionUrl,
    pub tailscale: Vec<ConnectionUrl>,
    pub tailscale_https: Option<ConnectionUrl>,
    pub lan: Vec<ConnectionUrl>,
    pub loopback: ConnectionUrl,
}

pub fn discover_urls(port: u16, token: &str) -> UrlSet {
    let loopback = build_url("Local loopback", Ipv4Addr::LOCALHOST, port, token);
    let mut seen = BTreeSet::new();
    let mut tailscale = Vec::new();
    let mut lan = Vec::new();
    let tailscale_https = discover_tailscale_dns_name().map(|dns_name| ConnectionUrl {
        label: "Expected HTTPS URL after Tailscale Serve".to_string(),
        url: format!("https://{dns_name}/?token={token}"),
    });

    if let Ok(addresses) = get_if_addrs() {
        for address in addresses {
            let IfAddr::V4(v4) = address.addr else {
                continue;
            };

            let ip = v4.ip;
            if ip.is_loopback() || !seen.insert(ip) {
                continue;
            }

            let entry = build_url(&format!("{} ({ip})", address.name), ip, port, token);
            if is_tailscale(ip) {
                tailscale.push(entry);
            } else if ip.is_private() {
                lan.push(entry);
            }
        }
    }

    tailscale.sort_by(|a, b| a.label.cmp(&b.label));
    lan.sort_by(|a, b| a.label.cmp(&b.label));

    let preferred = tailscale
        .first()
        .cloned()
        .or_else(|| lan.first().cloned())
        .unwrap_or_else(|| loopback.clone());

    UrlSet {
        preferred,
        tailscale,
        tailscale_https,
        lan,
        loopback,
    }
}

pub fn is_tailscale(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

fn build_url(label: &str, ip: Ipv4Addr, port: u16, token: &str) -> ConnectionUrl {
    ConnectionUrl {
        label: label.to_string(),
        url: format!("http://{ip}:{port}/?token={token}"),
    }
}

fn discover_tailscale_dns_name() -> Option<String> {
    let output = tailscale_status_output()?;
    let status: TailscaleStatus = serde_json::from_slice(&output).ok()?;
    if !status.current_tailnet.as_ref()?.magic_dns_enabled {
        return None;
    }

    let dns_name = status
        .self_node
        .as_ref()?
        .dns_name
        .trim()
        .trim_end_matches('.')
        .to_string();

    (!dns_name.is_empty()).then_some(dns_name)
}

fn tailscale_status_output() -> Option<Vec<u8>> {
    for candidate in tailscale_command_candidates() {
        let Ok(output) = Command::new(candidate).args(["status", "--json"]).output() else {
            continue;
        };
        if output.status.success() {
            return Some(output.stdout);
        }
    }

    None
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
    #[serde(rename = "Self")]
    self_node: Option<TailscaleNode>,
    #[serde(rename = "CurrentTailnet")]
    current_tailnet: Option<TailscaleTailnet>,
}

#[derive(Debug, Deserialize)]
struct TailscaleNode {
    #[serde(rename = "DNSName")]
    dns_name: String,
}

#[derive(Debug, Deserialize)]
struct TailscaleTailnet {
    #[serde(rename = "MagicDNSEnabled")]
    magic_dns_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::{TailscaleStatus, is_tailscale};
    use std::net::Ipv4Addr;

    #[test]
    fn tailscale_range_detection_matches_cgnat_space() {
        assert!(is_tailscale(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_tailscale(Ipv4Addr::new(100, 127, 255, 254)));
        assert!(!is_tailscale(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_tailscale(Ipv4Addr::new(100, 128, 0, 1)));
        assert!(!is_tailscale(Ipv4Addr::new(192, 168, 1, 10)));
    }

    #[test]
    fn tailscale_status_deserializes_magicdns_hostname() {
        let status: TailscaleStatus = serde_json::from_str(
            r#"{
                "Self": {
                    "DNSName": "sparta.tail359cf9.ts.net."
                },
                "CurrentTailnet": {
                    "MagicDNSEnabled": true
                }
            }"#,
        )
        .expect("status json should deserialize");

        assert_eq!(
            status.self_node.expect("self node").dns_name,
            "sparta.tail359cf9.ts.net."
        );
        assert!(status.current_tailnet.expect("tailnet").magic_dns_enabled);
    }
}
