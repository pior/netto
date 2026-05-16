use std::process::Command;

#[derive(Clone, Debug, Default)]
pub struct NetInfo {
    pub local_v4: Option<String>,
    pub local_v6: Option<String>,
    pub router_v4: Option<String>,
    pub router_v6: Option<String>,
    pub dns_ips: Vec<String>,
    /// `Some("utun4")` when the default route goes through a VPN-style
    /// interface. `None` when traffic exits via a physical interface.
    pub vpn_interface: Option<String>,
}

pub fn collect() -> NetInfo {
    let (interface, router_v4) = default_route_v4();
    let local_v4 = interface.as_deref().and_then(local_v4_for);
    let local_v6 = interface.as_deref().and_then(local_v6_for);
    let router_v6 = default_gateway_v6();
    let dns_ips = dns_servers();
    let vpn_interface = interface.as_deref().filter(|i| is_vpn_iface(i)).map(String::from);
    NetInfo {
        local_v4,
        local_v6,
        router_v4,
        router_v6,
        dns_ips,
        vpn_interface,
    }
}

/// A default route that exits via one of these interface kinds indicates
/// a VPN. macOS uses `utun*` for user-space tunnels (Wireguard, OpenVPN
/// clients, most commercial VPNs), `ipsec*` for native IKEv2/IPsec, and
/// `ppp*` for L2TP/PPTP.
fn is_vpn_iface(name: &str) -> bool {
    name.starts_with("utun") || name.starts_with("ipsec") || name.starts_with("ppp")
}

fn default_route_v4() -> (Option<String>, Option<String>) {
    let output = match Command::new("route").args(["-n", "get", "default"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return (None, None),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gateway = None;
    let mut interface = None;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("gateway:") {
            gateway = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("interface:") {
            interface = Some(rest.trim().to_string());
        }
    }
    (interface, gateway)
}

fn default_gateway_v6() -> Option<String> {
    let output = Command::new("route")
        .args(["-n", "get", "-inet6", "default"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("gateway:") {
            let addr = rest.trim();
            // IPv6 gateways often carry a zone id ("fe80::1%en0"); drop it
            // for display, the route is established via the default interface.
            let addr = addr.split('%').next().unwrap_or(addr);
            if !addr.is_empty() {
                return Some(addr.to_string());
            }
        }
    }
    None
}

fn local_v4_for(interface: &str) -> Option<String> {
    let output = Command::new("ipconfig")
        .args(["getifaddr", interface])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() { None } else { Some(ip) }
}

fn local_v6_for(interface: &str) -> Option<String> {
    // Parse `ifconfig <iface>`, take the first non-link-local inet6 address.
    let output = Command::new("ifconfig").arg(interface).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("inet6 ") else {
            continue;
        };
        let addr = rest.split_whitespace().next()?;
        let addr = addr.split('%').next().unwrap_or(addr);
        if addr.starts_with("fe80:") || addr == "::1" {
            continue;
        }
        // Skip temporary/anonymous addresses (privacy extensions) — the
        // long-lived one is usually shown right after.
        if rest.contains("temporary") {
            continue;
        }
        return Some(addr.to_string());
    }
    None
}

fn dns_servers() -> Vec<String> {
    let output = match Command::new("scutil").arg("--dns").output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut servers = Vec::new();
    let mut in_primary_resolver = false;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("resolver #") {
            in_primary_resolver = rest.trim() == "1";
            continue;
        }
        if !in_primary_resolver {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("nameserver[") {
            if let Some(colon) = rest.find(':') {
                let ip = rest[colon + 1..].trim().to_string();
                if !ip.is_empty() && !servers.contains(&ip) {
                    servers.push(ip);
                }
            }
        }
    }
    servers
}
