//! The JSON envelope every command prints on stdout.
//!
//! Success: `{"v":1,"ok":true,"step":..,"risk":..,"dry_run":..,"data":{..}}`
//! Failure: `{"v":1,"ok":false,"step":..,"error":{"code":"E_..","message":..,"details":..}}`

use crate::error::ShipError;
use crate::gate::Risk;
use crate::redact::redact;
use serde_json::{json, Value};

/// Bumped only on a breaking change to the envelope.
pub const SCHEMA_VERSION: u32 = 1;

pub fn ok_envelope(step: &str, risk: Risk, dry_run: bool, data: Value) -> Value {
    json!({
        "v": SCHEMA_VERSION,
        "ok": true,
        "step": step,
        "risk": risk,
        "dry_run": dry_run,
        "data": data,
    })
}

pub fn err_envelope(step: Option<&str>, err: &ShipError) -> Value {
    json!({
        "v": SCHEMA_VERSION,
        "ok": false,
        "step": step,
        "error": err.to_json(),
    })
}

/// Serialize for stdout. Redaction runs on the final text as a last line of
/// defence; token alphabets contain no JSON metacharacters, so it stays valid.
pub fn render(v: &Value) -> String {
    redact(&serde_json::to_string(v).expect("JSON values always serialize"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Code;

    #[test]
    fn ok_shape() {
        let v = ok_envelope("preflight", Risk::Low, false, json!({"ready": true}));
        assert_eq!(v["v"], 1);
        assert_eq!(v["ok"], true);
        assert_eq!(v["risk"], "low");
        assert_eq!(v["dry_run"], false);
        assert_eq!(v["data"]["ready"], true);
        assert_eq!(v.as_object().unwrap().len(), 6);
    }

    #[test]
    fn err_shape() {
        let e = ShipError::new(Code::ConfirmRequired, "needs --confirm");
        let v = err_envelope(Some("publish"), &e);
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "E_CONFIRM_REQUIRED");
        assert_eq!(v["error"]["message"], "needs --confirm");
        assert!(v.get("data").is_none());
    }

    #[test]
    fn render_redacts_and_stays_valid_json() {
        let tok = format!("ghp_{}", "x1".repeat(18));
        let v = ok_envelope("t", Risk::Low, false, json!({ "leak": tok }));
        let s = render(&v);
        assert!(!s.contains(&tok));
        let back: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["data"]["leak"], "[REDACTED]");
    }
}
