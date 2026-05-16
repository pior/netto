use std::process::Command;

#[derive(Clone, Debug, Default)]
pub struct NetInfo {
    pub local_ip: Option<String>,
    pub router_ip: Option<String>,
    pub dns_ips: Vec<String>,
}

pub fn collect() -> NetInfo {
    let (interface, router_ip) = default_route();
    let local_ip = interface.as_deref().and_then(local_ip_for);
    let dns_ips = dns_servers();
    NetInfo {
        local_ip,
        router_ip,
        dns_ips,
    }
}

fn default_route() -> (Option<String>, Option<String>) {
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

fn local_ip_for(interface: &str) -> Option<String> {
    let output = Command::new("ipconfig")
        .args(["getifaddr", interface])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() {
        None
    } else {
        Some(ip)
    }
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
