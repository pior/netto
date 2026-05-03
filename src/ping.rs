use std::process::Command;

pub struct PingTarget {
    pub name: String,
    pub host: String,
}

#[derive(Clone, Debug)]
pub enum PingResult {
    Ok(f64),
    Timeout,
    Error(String),
    Pending,
}

pub fn ping_host(host: &str) -> PingResult {
    let output = Command::new("ping")
        .args(["-c", "1", "-W", "5", host])
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if out.status.success() {
                parse_ping_time(&stdout)
            } else if stdout.contains("100.0% packet loss") || stdout.contains("100% packet loss")
            {
                PingResult::Timeout
            } else {
                PingResult::Error("failed".to_string())
            }
        }
        Err(e) => PingResult::Error(e.to_string()),
    }
}

fn parse_ping_time(output: &str) -> PingResult {
    // macOS ping output: "round-trip min/avg/max/stddev = 1.234/2.345/3.456/0.567 ms"
    for line in output.lines() {
        if line.contains("round-trip") || line.contains("rtt") {
            if let Some(eq_pos) = line.find('=') {
                let stats = &line[eq_pos + 1..];
                let parts: Vec<&str> = stats.trim().split('/').collect();
                if parts.len() >= 2 {
                    if let Ok(avg) = parts[1].trim().parse::<f64>() {
                        return PingResult::Ok(avg);
                    }
                }
            }
        }
    }
    // Fallback: try "time=X.XX ms" from individual ping line
    for line in output.lines() {
        if let Some(time_pos) = line.find("time=") {
            let rest = &line[time_pos + 5..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            if let Ok(ms) = num_str.parse::<f64>() {
                return PingResult::Ok(ms);
            }
        }
    }
    PingResult::Error("parse error".to_string())
}
