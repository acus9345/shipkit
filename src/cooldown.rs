//! After a registry rate-limits a publish (HTTP 429), further publishes to it
//! are refused for 24 hours unless `--force` is passed. State lives in the
//! per-user state directory, never in the project.

use crate::log::{rfc3339, state_dir};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);

fn path(registry: &str) -> Option<PathBuf> {
    let safe: String = registry.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    state_dir().map(|d| d.join(format!("cooldown-{safe}.json")))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The active cooldown for `registry`, if any: `{until, until_utc, reason}`.
pub fn active(registry: &str) -> Option<Value> {
    let text = std::fs::read_to_string(path(registry)?).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    (v["until"].as_u64()? > now()).then_some(v)
}

/// Start a cooldown after a 429.
pub fn start(registry: &str, reason: &str) -> Option<Value> {
    let p = path(registry)?;
    std::fs::create_dir_all(p.parent()?).ok()?;
    let until = now() + COOLDOWN.as_secs();
    let v = json!({
        "registry": registry,
        "until": until,
        "until_utc": rfc3339(UNIX_EPOCH + Duration::from_secs(until)),
        "reason": reason,
    });
    std::fs::write(p, v.to_string()).ok()?;
    Some(v)
}
