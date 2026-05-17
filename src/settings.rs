use objc2_foundation::{NSString, NSUserDefaults};
use serde::{Deserialize, Serialize};

use crate::ping::PingTarget;

const TARGETS_KEY: &str = "ping_targets";
const PREFS_KEY: &str = "preferences";

// Discrete step values for each slider. The slider UI is a 0..N-1 integer
// index; these arrays map index → real value.
pub const SLOW_STEPS: &[f64] = &[2.0, 5.0, 10.0, 30.0, 60.0];
pub const FAST_STEPS: &[f64] = &[0.25, 0.5, 1.0, 2.0, 5.0, 10.0];
/// Latency (ms) below which sparkline cells stay fully green. Lower =
/// stricter (turns yellow sooner); higher = more tolerant.
pub const TOLERANCE_STEPS: &[f64] = &[20.0, 30.0, 60.0, 100.0, 200.0];

pub const SLOW_DEFAULT: f64 = 10.0;
pub const FAST_DEFAULT: f64 = 1.0;
pub const TOLERANCE_DEFAULT: f64 = 30.0;

/// Snap an arbitrary value to the closest entry in `steps`.
pub fn snap_to_steps(steps: &[f64], v: f64) -> f64 {
    steps
        .iter()
        .copied()
        .min_by(|a, b| {
            (a - v)
                .abs()
                .partial_cmp(&(b - v).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(v)
}

#[derive(Clone, Copy, Debug)]
pub struct AppPrefs {
    pub slow_secs: f64,
    pub fast_secs: f64,
    pub tolerance_ms: f64,
}

impl Default for AppPrefs {
    fn default() -> Self {
        Self {
            slow_secs: SLOW_DEFAULT,
            fast_secs: FAST_DEFAULT,
            tolerance_ms: TOLERANCE_DEFAULT,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoredPrefs {
    slow_secs: Option<f64>,
    fast_secs: Option<f64>,
    tolerance_ms: Option<f64>,
}

pub fn load_prefs() -> AppPrefs {
    let defaults = NSUserDefaults::standardUserDefaults();
    let stored: StoredPrefs = defaults
        .stringForKey(&NSString::from_str(PREFS_KEY))
        .and_then(|s| serde_json::from_str(&s.to_string()).ok())
        .unwrap_or_default();
    AppPrefs {
        slow_secs: snap_to_steps(SLOW_STEPS, stored.slow_secs.unwrap_or(SLOW_DEFAULT)),
        fast_secs: snap_to_steps(FAST_STEPS, stored.fast_secs.unwrap_or(FAST_DEFAULT)),
        tolerance_ms: snap_to_steps(
            TOLERANCE_STEPS,
            stored.tolerance_ms.unwrap_or(TOLERANCE_DEFAULT),
        ),
    }
}

pub fn save_prefs(p: &AppPrefs) {
    let stored = StoredPrefs {
        slow_secs: Some(p.slow_secs),
        fast_secs: Some(p.fast_secs),
        tolerance_ms: Some(p.tolerance_ms),
    };
    let json = serde_json::to_string(&stored).expect("serializable");
    let defaults = NSUserDefaults::standardUserDefaults();
    unsafe {
        defaults.setObject_forKey(
            Some(&NSString::from_str(&json)),
            &NSString::from_str(PREFS_KEY),
        );
    }
}

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
