//! GitHub access through the `gh` CLI and its existing login.

use crate::error::{Code, Result, ShipError};
use crate::log::Logger;
use crate::proc::{self, Cmd};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

pub fn gh(args: &[&str]) -> Cmd {
    Cmd::new("gh", args).timeout(Duration::from_secs(60))
}

/// Login of the active gh account.
pub fn active_login(log: &Logger) -> Result<String> {
    let out = proc::run(&gh(&["api", "user", "--jq", ".login"]), log)?;
    if !out.success() {
        return Err(ShipError::new(
            Code::Precondition,
            "gh is not logged in; run `gh auth login` yourself, then retry",
        ));
    }
    Ok(out.stdout.trim().to_string())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoInfo {
    pub full_name: String,
    pub private: bool,
    pub url: String,
    pub default_branch: String,
}

/// Look up `owner/name`. `Ok(None)` means it does not exist (HTTP 404).
pub fn repo(owner: &str, name: &str, log: &Logger) -> Result<Option<RepoInfo>> {
    let path = format!("repos/{owner}/{name}");
    let jq = r#"[.full_name, (.private|tostring), .html_url, .default_branch] | join("\t")"#;
    let out = proc::run(&gh(&["api", &path, "--jq", jq]), log)?;
    if out.success() {
        let parts: Vec<&str> = out.stdout.trim().split('\t').collect();
        if parts.len() == 4 {
            return Ok(Some(RepoInfo {
                full_name: parts[0].into(),
                private: parts[1] == "true",
                url: parts[2].into(),
                default_branch: parts[3].into(),
            }));
        }
        return Err(ShipError::new(Code::CommandFailed, "unexpected `gh api` output"));
    }
    if out.stderr.contains("HTTP 404") || out.stderr.contains("Not Found") {
        return Ok(None);
    }
    Err(ShipError::new(Code::CommandFailed, format!("`gh api {path}` failed"))
        .with_details(serde_json::json!({ "output": out.tail(20) })))
}

/// GitHub repository naming rules.
pub fn validate_repo_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() || name.len() > 100 {
        return Err("repository name must be 1 to 100 characters".into());
    }
    if name == "." || name == ".." {
        return Err("`.` and `..` are not valid repository names".into());
    }
    if let Some(c) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || "-_.".contains(*c))) {
        return Err(format!("repository name contains `{c}`; only letters, digits, `-`, `_` and `.` are allowed"));
    }
    Ok(())
}

/// URL of the `origin` remote as configured (before any `insteadOf` rewriting).
pub fn origin_url(dir: &Path, log: &Logger) -> Option<String> {
    let out = proc::run(&Cmd::new("git", &["config", "--get", "remote.origin.url"]).cwd(dir), log).ok()?;
    out.success().then(|| out.stdout.trim().to_string())
}

/// `(owner, repo)` from an https or ssh GitHub URL.
pub fn parse_github_url(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let want = Some(("o".to_string(), "r".to_string()));
        assert_eq!(parse_github_url("https://github.com/o/r"), want);
        assert_eq!(parse_github_url("https://github.com/o/r.git"), want);
        assert_eq!(parse_github_url("git@github.com:o/r.git"), want);
        assert_eq!(parse_github_url("https://gitlab.com/o/r"), None);
        assert_eq!(parse_github_url("https://github.com/o"), None);
    }

    #[test]
    fn repo_names() {
        assert!(validate_repo_name("ship.kit_2-x").is_ok());
        assert!(validate_repo_name("has space").is_err());
        assert!(validate_repo_name("..").is_err());
    }
}
