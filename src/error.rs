//! Errors with stable machine-readable codes.

use serde_json::{json, Value};
use std::fmt;

/// Stable error codes. The string form (`E_...`) is part of the public JSON
/// contract: never rename or reuse one, only add new variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Usage,
    Input,
    ConfirmRequired,
    DirtyTree,
    ToolMissing,
    CommandFailed,
    Io,
    NotFound,
    AlreadyExists,
    SecretsFound,
    CheckFailed,
    Precondition,
    RateLimited,
    Cooldown,
    Timeout,
    Internal,
}

impl Code {
    pub const ALL: [Code; 16] = [
        Code::Usage,
        Code::Input,
        Code::ConfirmRequired,
        Code::DirtyTree,
        Code::ToolMissing,
        Code::CommandFailed,
        Code::Io,
        Code::NotFound,
        Code::AlreadyExists,
        Code::SecretsFound,
        Code::CheckFailed,
        Code::Precondition,
        Code::RateLimited,
        Code::Cooldown,
        Code::Timeout,
        Code::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Code::Usage => "E_USAGE",
            Code::Input => "E_INPUT",
            Code::ConfirmRequired => "E_CONFIRM_REQUIRED",
            Code::DirtyTree => "E_DIRTY_TREE",
            Code::ToolMissing => "E_TOOL_MISSING",
            Code::CommandFailed => "E_COMMAND_FAILED",
            Code::Io => "E_IO",
            Code::NotFound => "E_NOT_FOUND",
            Code::AlreadyExists => "E_ALREADY_EXISTS",
            Code::SecretsFound => "E_SECRETS_FOUND",
            Code::CheckFailed => "E_CHECK_FAILED",
            Code::Precondition => "E_PRECONDITION",
            Code::RateLimited => "E_RATE_LIMITED",
            Code::Cooldown => "E_COOLDOWN",
            Code::Timeout => "E_TIMEOUT",
            Code::Internal => "E_INTERNAL",
        }
    }

    /// Process exit code: 2 for bad invocations, 3 for a refused gated step,
    /// 1 for everything else. Success is always 0.
    pub fn exit_code(self) -> i32 {
        match self {
            Code::Usage | Code::Input => 2,
            Code::ConfirmRequired => 3,
            _ => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShipError {
    pub code: Code,
    pub message: String,
    pub details: Option<Value>,
}

impl ShipError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        ShipError {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({ "code": self.code.as_str(), "message": self.message });
        if let Some(d) = &self.details {
            v["details"] = d.clone();
        }
        v
    }
}

impl fmt::Display for ShipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ShipError {}

impl From<std::io::Error> for ShipError {
    fn from(e: std::io::Error) -> Self {
        ShipError::new(Code::Io, e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ShipError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codes_are_unique_and_prefixed() {
        let mut seen = HashSet::new();
        for c in Code::ALL {
            let s = c.as_str();
            assert!(s.starts_with("E_"), "{s}");
            assert!(seen.insert(s), "duplicate code {s}");
        }
    }

    #[test]
    fn exit_codes_are_never_zero() {
        for c in Code::ALL {
            assert_ne!(c.exit_code(), 0);
        }
    }

    #[test]
    fn error_json_shape() {
        let e = ShipError::new(Code::NotFound, "nope");
        assert_eq!(e.to_json(), json!({"code": "E_NOT_FOUND", "message": "nope"}));
        let e = e.with_details(json!({"path": "x"}));
        assert_eq!(e.to_json()["details"]["path"], "x");
    }
}
