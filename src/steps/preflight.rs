//! `ship preflight`: which tools are installed, who is logged in, and the git
//! identity. Read-only, and never starts a login.

use super::{Ctx, Step};
use crate::error::Result;
use crate::gate::{Action, Plan};
use crate::log::Logger;
use crate::proc::{self, Cmd};
use crate::registry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Preflight {
    /// Registry to check (default: detected from the project, else crates.io).
    #[arg(long)]
    pub registry: Option<String>,
}

impl Step for Preflight {
    const NAME: &'static str = "preflight";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        let mut plan = Plan::default();
        for tool in ["git", "gh"] {
            plan.push(Action::run(&Cmd::new(tool, &["--version"]), "is it installed"));
        }
        for (_, cmd) in reg.tool_checks() {
            plan.push(Action::run(&cmd, "is it installed"));
        }
        for key in ["user.name", "user.email"] {
            plan.push(Action::run(
                &Cmd::new("git", &["config", "--show-scope", "--get", key]),
                "git identity",
            ));
        }
        plan.push(Action::run(
            &Cmd::new("gh", &["auth", "status", "--json", "hosts"]),
            "GitHub accounts (gh validates its token over the network)",
        ));
        plan.push(Action::Skip {
            what: format!("{} login", reg.id()),
            reason: "checked offline by looking for credential key names; values are never read".into(),
        });
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, _plan: Plan) -> Result<Value> {
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        let mut problems: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        let mut tools = BTreeMap::new();
        let mut checks = vec![("git", Cmd::new("git", &["--version"])), ("gh", Cmd::new("gh", &["--version"]))];
        checks.extend(reg.tool_checks());
        for (tool, cmd) in checks {
            let version = proc::first_line_of(&cmd.timeout(Duration::from_secs(30)), &ctx.log);
            if version.is_none() {
                problems.push(format!("`{tool}` is not installed or not on PATH"));
            }
            tools.insert(tool, json!({ "installed": version.is_some(), "version": version }));
        }

        let git_dir = if ctx.dir.is_dir() { ctx.dir.as_path() } else { Path::new(".") };
        let name = git_config(git_dir, "user.name", &ctx.log);
        let email = git_config(git_dir, "user.email", &ctx.log);
        if name.is_none() || email.is_none() {
            problems.push(
                "git identity is incomplete; set it with `git config --global user.name` / `user.email`".into(),
            );
        }

        let github = if tools["gh"]["installed"] == true {
            let accounts = gh_accounts(&ctx.log);
            if !accounts.iter().any(|a| a.active && a.state == "success") {
                problems.push("not logged in to GitHub; run `gh auth login` yourself".into());
            }
            for a in accounts.iter().filter(|a| a.active) {
                if !a.scopes.is_empty() && !a.scopes.iter().any(|s| s == "workflow") {
                    warnings.push(format!(
                        "gh token for {} lacks the `workflow` scope, needed to push workflow files; run `gh auth refresh -s workflow` yourself",
                        a.login
                    ));
                }
            }
            json!({ "accounts": accounts })
        } else {
            json!({ "accounts": [] })
        };

        let login = reg.login_status();
        if !login.logged_in {
            warnings.push(format!(
                "no local {} credential; publishing from the terminal will fail until you log in, but trusted publishing from CI does not need one",
                reg.id()
            ));
        }

        Ok(json!({
            "ready": problems.is_empty(),
            "problems": problems,
            "warnings": warnings,
            "tools": tools,
            "git_identity": { "name": name, "email": email },
            "github": github,
            "registry": { "id": reg.id(), "cli": reg.cli(), "login": login },
            "ship": {
                "version": env!("CARGO_PKG_VERSION"),
                "os": std::env::consts::OS,
                "log": ctx.log.path(),
            },
        }))
    }
}

/// Value and scope (`local`/`global`/`system`) of a git config key.
fn git_config(dir: &Path, key: &str, log: &Logger) -> Option<Value> {
    let cmd = Cmd::new("git", &["config", "--show-scope", "--get", key])
        .cwd(dir)
        .timeout(Duration::from_secs(20));
    let out = proc::run(&cmd, log).ok()?;
    if !out.success() {
        return None;
    }
    let line = out.stdout.lines().next()?.trim().to_string();
    let (scope, value) = line.split_once('\t').unwrap_or(("unknown", line.as_str()));
    Some(json!({ "value": value, "scope": scope }))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GhAccount {
    pub host: String,
    pub login: String,
    pub active: bool,
    pub state: String,
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_source: Option<String>,
}

#[derive(Deserialize)]
struct GhStatus {
    hosts: BTreeMap<String, Vec<GhHost>>,
}

// Deliberately has no `token` field: gh never prints it without --show-token,
// and if it ever did, serde would drop it here.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhHost {
    #[serde(default)]
    state: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    host: String,
    #[serde(default)]
    login: String,
    token_source: Option<String>,
    scopes: Option<String>,
}

fn gh_accounts(log: &Logger) -> Vec<GhAccount> {
    let cmd = Cmd::new("gh", &["auth", "status", "--json", "hosts"]).timeout(Duration::from_secs(30));
    match proc::run(&cmd, log) {
        Ok(out) => parse_gh_status(&out.stdout),
        Err(_) => Vec::new(),
    }
}

pub fn parse_gh_status(stdout: &str) -> Vec<GhAccount> {
    let Ok(status) = serde_json::from_str::<GhStatus>(stdout) else {
        return Vec::new();
    };
    status
        .hosts
        .into_values()
        .flatten()
        .map(|h| GhAccount {
            host: h.host,
            login: h.login,
            active: h.active,
            state: h.state,
            scopes: h
                .scopes
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            token_source: h.token_source,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gh_status_json() {
        let s = r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"octo","tokenSource":"keyring","scopes":"repo, workflow","gitProtocol":"https"}]}}"#;
        let a = parse_gh_status(s);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].login, "octo");
        assert_eq!(a[0].scopes, ["repo", "workflow"]);
        assert!(a[0].active);
    }

    #[test]
    fn bad_gh_output_means_no_accounts() {
        assert!(parse_gh_status("not json").is_empty());
        assert!(parse_gh_status(r#"{"hosts":{}}"#).is_empty());
    }
}
