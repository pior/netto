use objc2_foundation::{NSString, NSUserDefaults};
use serde::Deserialize;

use crate::ping::PingTarget;

const TARGETS_KEY: &str = "ping_targets";

#[derive(Deserialize)]
struct LegacyStoredTarget {
    host: String,
}

pub fn load_targets() -> Option<Vec<PingTarget>> {
    let defaults = NSUserDefaults::standardUserDefaults();
    let json = defaults.stringForKey(&NSString::from_str(TARGETS_KEY))?;
    let text = json.to_string();

    // Current format: a plain JSON array of host strings.
    let hosts: Vec<String> = if let Ok(hosts) = serde_json::from_str::<Vec<String>>(&text) {
        hosts
    } else if let Ok(legacy) = serde_json::from_str::<Vec<LegacyStoredTarget>>(&text) {
        // Legacy format had {name, host} per entry; keep only the host.
        legacy.into_iter().map(|s| s.host).collect()
    } else {
        return None;
    };

    if hosts.is_empty() {
        return None;
    }
    Some(hosts.into_iter().map(|host| PingTarget { host }).collect())
}

pub fn save_targets(targets: &[PingTarget]) {
    let hosts: Vec<&str> = targets.iter().map(|t| t.host.as_str()).collect();
    let json = serde_json::to_string(&hosts).expect("serializable");
    let defaults = NSUserDefaults::standardUserDefaults();
    unsafe {
        defaults.setObject_forKey(
            Some(&NSString::from_str(&json)),
            &NSString::from_str(TARGETS_KEY),
        );
    }
}
