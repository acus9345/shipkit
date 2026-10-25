//! Local action log: one JSON object per line, secrets redacted, never sent anywhere.

use crate::redact::redact;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-user state directory for the log and cooldown files.
/// `SHIP_STATE_DIR` overrides it (used by tests).
pub fn state_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("SHIP_STATE_DIR") {
        return Some(PathBuf::from(d));
    }
    if cfg!(windows) {
        if let Some(d) = std::env::var_os("LOCALAPPDATA") {
            return Some(PathBuf::from(d).join("ship"));
        }
    }
    if let Some(d) = std::env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(d).join("ship"));
    }
    home_dir().map(|h| h.join(".local").join("state").join("ship"))
}

pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub struct Logger {
    path: Option<PathBuf>,
    run_id: String,
    step: String,
}

impl Logger {
    pub fn open(step: &str) -> Logger {
        let path = state_dir().and_then(|d| {
            std::fs::create_dir_all(&d).ok()?;
            Some(d.join("ship.log"))
        });
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Logger {
            path,
            run_id: format!("{:x}-{:x}", std::process::id(), nanos & 0xffff_ffff),
            step: step.to_string(),
        }
    }

    /// A logger that writes nowhere (unit tests).
    pub fn null() -> Logger {
        Logger { path: None, run_id: "test".into(), step: "test".into() }
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    /// Append one event. Logging failures never abort a step.
    pub fn event(&self, event: &str, detail: Value) {
        let Some(path) = &self.path else { return };
        let line = json!({
            "ts": rfc3339(SystemTime::now()),
            "run": self.run_id,
            "step": self.step,
            "event": event,
            "detail": detail,
        });
        let text = redact(&line.to_string());
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{text}");
        }
    }
}

/// Format a time as UTC RFC 3339 (`2026-10-24T13:16:00Z`) without a date library.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Today's date as `YYYY-MM-DD` (UTC), for CHANGELOG entries.
pub fn today() -> String {
    rfc3339(SystemTime::now())[..10].to_string()
}

// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn formats_known_instants() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = UNIX_EPOCH + Duration::from_secs(951_782_400); // leap day 2000
        assert_eq!(rfc3339(t), "2000-02-29T00:00:00Z");
        let t = UNIX_EPOCH + Duration::from_secs(1_792_847_045);
        assert_eq!(rfc3339(t), "2026-10-24T13:04:05Z");
    }
}
