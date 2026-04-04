use if_addrs::{IfAddr, get_if_addrs};
use std::{collections::BTreeSet, net::Ipv4Addr};

#[derive(Debug, Clone)]
pub struct ConnectionUrl {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct UrlSet {
    pub preferred: ConnectionUrl,
    pub tailscale: Vec<ConnectionUrl>,
    pub lan: Vec<ConnectionUrl>,
    pub loopback: ConnectionUrl,
}

pub fn discover_urls(port: u16, token: &str) -> UrlSet {
    let loopback = build_url("Local loopback", Ipv4Addr::LOCALHOST, port, token);
    let mut seen = BTreeSet::new();
    let mut tailscale = Vec::new();
    let mut lan = Vec::new();

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

#[cfg(test)]
mod tests {
    use super::is_tailscale;
    use std::net::Ipv4Addr;

    #[test]
    fn tailscale_range_detection_matches_cgnat_space() {
        assert!(is_tailscale(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_tailscale(Ipv4Addr::new(100, 127, 255, 254)));
        assert!(!is_tailscale(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_tailscale(Ipv4Addr::new(100, 128, 0, 1)));
        assert!(!is_tailscale(Ipv4Addr::new(192, 168, 1, 10)));
    }
}
