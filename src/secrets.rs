//! Secret scanning over a list of files. Findings carry the file, line and
//! kind only; the matched text is never stored or printed.

use crate::redact;
use serde::Serialize;
use std::path::Path;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Add this marker to a line to accept a known false positive.
pub const ALLOW_MARKER: &str = "ship:allow-secret";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    /// 1-based; 0 when the whole file is the problem (e.g. a `.env` file).
    pub line: usize,
    pub kind: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub files_scanned: usize,
    pub findings: Vec<Finding>,
    /// Files not read (binary or too large), with the reason.
    pub skipped: Vec<serde_json::Value>,
}

impl Report {
    pub fn clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Scan `files` (relative to `dir`).
pub fn scan(dir: &Path, files: &[String]) -> Report {
    let mut r = Report::default();
    for f in files {
        if let Some(kind) = risky_filename(f) {
            r.findings.push(Finding { file: f.clone(), line: 0, kind: kind.into() });
        }
        let path = dir.join(f);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.len() > MAX_FILE_BYTES {
            r.skipped.push(serde_json::json!({ "file": f, "reason": "larger than 2 MiB" }));
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            r.skipped.push(serde_json::json!({ "file": f, "reason": "binary" }));
            continue;
        }
        r.files_scanned += 1;
        let text = String::from_utf8_lossy(&bytes);
        for (i, line) in text.lines().enumerate() {
            for kind in scan_line(line) {
                r.findings.push(Finding { file: f.clone(), line: i + 1, kind: kind.into() });
            }
        }
    }
    r
}

/// Kinds of secrets on one line (deduplicated).
pub fn scan_line(line: &str) -> Vec<&'static str> {
    if line.contains(ALLOW_MARKER) {
        return Vec::new();
    }
    let mut kinds: Vec<&'static str> = redact::find(line).into_iter().map(|h| h.kind).collect();
    if kinds.is_empty() && generic_assignment(line) {
        kinds.push("generic_secret_assignment");
    }
    kinds.dedup();
    kinds
}

/// Files that should never be committed, whatever they contain.
pub fn risky_filename(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let env_ok = [".env.example", ".env.sample", ".env.template", ".env.dist"];
    if (name == ".env" || name.starts_with(".env.")) && !env_ok.contains(&name.as_str()) {
        return Some("env_file");
    }
    if matches!(name.as_str(), "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519") {
        return Some("ssh_private_key_file");
    }
    if [".p12", ".pfx", ".jks", ".keystore"].iter().any(|e| name.ends_with(e)) {
        return Some("key_store_file");
    }
    if matches!(name.as_str(), "credentials.toml" | ".pypirc" | ".netrc" | "_netrc" | ".git-credentials") {
        return Some("credentials_file");
    }
    None
}

const SECRET_KEYS: &[&str] = &[
    "password", "passwd", "secret", "token", "api_key", "apikey", "api-key", "access_key",
    "private_key", "client_secret", "auth_token", "_authtoken",
];

const PLACEHOLDERS: &[&str] = &[
    "example", "xxxx", "your", "changeme", "placeholder", "dummy", "redacted", "<", "${", "{{", "***",
    "...", "env(", "secrets.", "process.env", "os.environ", "getenv",
];

/// Assignments of a quoted literal to a secret-sounding key (password, token,
/// api_key, ...) where the literal looks random enough to be a real credential.
fn generic_assignment(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let Some(key_pos) = SECRET_KEYS.iter().filter_map(|k| lower.find(k)).min() else {
        return false;
    };
    let after = &line[key_pos..];
    let Some(op) = after.find(['=', ':']) else { return false };
    let rest = after[op + 1..].trim_start();
    let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
        return false;
    };
    let body = &rest[1..];
    let Some(end) = body.find(q) else { return false };
    let value = &body[..end];
    if value.len() < 12 || value.contains(char::is_whitespace) {
        return false;
    }
    let vl = value.to_ascii_lowercase();
    if PLACEHOLDERS.iter().any(|p| vl.contains(p)) {
        return false;
    }
    let classes = [
        value.chars().any(|c| c.is_ascii_lowercase()),
        value.chars().any(|c| c.is_ascii_uppercase()),
        value.chars().any(|c| c.is_ascii_digit()),
        value.chars().any(|c| !c.is_ascii_alphanumeric()),
    ];
    classes.iter().filter(|b| **b).count() >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_files_flagged_but_examples_allowed() {
        assert_eq!(risky_filename(".env"), Some("env_file"));
        assert_eq!(risky_filename("config/.env.production"), Some("env_file"));
        assert_eq!(risky_filename(".env.example"), None);
        assert_eq!(risky_filename("src/env.rs"), None);
        assert_eq!(risky_filename("home/.ssh/id_ed25519"), Some("ssh_private_key_file"));
    }

    #[test]
    fn generic_assignments() {
        let pw = ["Zq8", "-vT", "4mw", "Lp2", "xR"].concat();
        assert!(generic_assignment(&format!("password = \"{pw}\"")));
        assert!(generic_assignment(&format!("\"client_secret\": '{pw}'")));
        assert!(!generic_assignment("password = \"your-password-here\""));
        assert!(!generic_assignment("token = \"${{ secrets.GITHUB_TOKEN }}\""));
        assert!(!generic_assignment("token_source: Option<String>,"));
        assert!(!generic_assignment("password = \"short\""));
        assert!(!generic_assignment("let token = \"abcdefghijklmnop\";"));
    }

    #[test]
    fn allow_marker_suppresses() {
        let tok = format!("ghp_{}", "Ab1".repeat(12));
        assert_eq!(scan_line(&tok), ["github_token"]);
        assert!(scan_line(&format!("{tok} // {ALLOW_MARKER}")).is_empty());
    }
}
