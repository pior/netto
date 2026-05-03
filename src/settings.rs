use objc2_foundation::{NSString, NSUserDefaults};
use serde::{Deserialize, Serialize};

use crate::ping::PingTarget;

const TARGETS_KEY: &str = "ping_targets";

#[derive(Serialize, Deserialize)]
struct StoredTarget {
    name: String,
    host: String,
}

pub fn load_targets() -> Option<Vec<PingTarget>> {
    let defaults = NSUserDefaults::standardUserDefaults();
    let json = defaults.stringForKey(&NSString::from_str(TARGETS_KEY))?;
    let stored: Vec<StoredTarget> = serde_json::from_str(&json.to_string()).ok()?;
    if stored.is_empty() {
        return None;
    }
    Some(
        stored
            .into_iter()
            .map(|s| PingTarget {
                name: s.name,
                host: s.host,
            })
            .collect(),
    )
}

pub fn save_targets(targets: &[PingTarget]) {
    let stored: Vec<StoredTarget> = targets
        .iter()
        .map(|t| StoredTarget {
            name: t.name.clone(),
            host: t.host.clone(),
        })
        .collect();

    let json = serde_json::to_string(&stored).expect("serializable");
    let defaults = NSUserDefaults::standardUserDefaults();
    unsafe {
        defaults.setObject_forKey(
            Some(&NSString::from_str(&json)),
            &NSString::from_str(TARGETS_KEY),
        );
    }
}
