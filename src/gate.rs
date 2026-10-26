//! Risk levels and the confirm/dry-run gate. This table is the single source
//! of truth used by the steps and by the Claude Code PreToolUse hook.

use crate::error::{Code, Result, ShipError};
use crate::proc::Cmd;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Reads only (may query git, gh or the registry).
    Low,
    /// Changes local files or local git state; nothing leaves the machine.
    Medium,
    /// Public or irreversible. Refused without `--confirm`.
    High,
}

/// Every step and its risk, in the order they are normally run.
pub const STEPS: &[(&str, Risk)] = &[
    ("preflight", Risk::Low),
    ("check-names", Risk::Low),
    ("scaffold", Risk::Medium),
    ("scan-secrets", Risk::Low),
    ("checks", Risk::Low),
    ("inspect", Risk::Low),
    ("commit", Risk::Medium),
    ("create-repo", Risk::High),
    ("add-workflows", Risk::Medium),
    ("trusted-publishing", Risk::Medium),
    ("bump", Risk::Medium),
    ("release", Risk::High),
    ("watch", Risk::Low),
    ("verify", Risk::Medium),
    ("publish", Risk::High),
];

/// Setup helpers, run only when the user asks: `login` opens the tool's own
/// login in a window; `install-tool` installs software, so it is gated.
pub const HELPERS: &[(&str, Risk)] = &[("login", Risk::Medium), ("install-tool", Risk::High)];

/// Commands that are not publishing steps (plugin plumbing). All read-only.
pub const UTILITIES: &[(&str, Risk)] = &[("hook", Risk::Low), ("self-test", Risk::Low)];

pub fn risk_of(step: &str) -> Option<Risk> {
    STEPS.iter().chain(HELPERS).chain(UTILITIES).find(|(s, _)| *s == step).map(|(_, r)| *r)
}

pub fn is_gated(step: &str) -> bool {
    risk_of(step) == Some(Risk::High)
}

/// Why a gated step needs `--confirm`, for refusals and the hook.
pub fn gate_reason(step: &str) -> &'static str {
    if step == "install-tool" {
        "installs software on this machine"
    } else {
        "is public or irreversible"
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub confirm: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Execute,
    DryRun,
    Refuse,
}

/// `--dry-run` always wins, even next to `--confirm`.
pub fn decide(risk: Risk, flags: Flags) -> Decision {
    if flags.dry_run {
        Decision::DryRun
    } else if risk == Risk::High && !flags.confirm {
        Decision::Refuse
    } else {
        Decision::Execute
    }
}

/// One thing a step will do. A plan is shown verbatim for `--dry-run` and in
/// the `E_CONFIRM_REQUIRED` refusal, so it must describe every effect.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Run a command.
    Run { cmd: String, purpose: String },
    /// Create a file that does not exist yet (existing files are never overwritten).
    CreateFile { path: String, bytes: usize },
    /// Change an existing file in place (e.g. the version line of a manifest).
    EditFile { path: String, change: String },
    /// Open a URL in the default browser.
    OpenUrl { url: String },
    /// Something that will not happen, and why (already done, not applicable).
    Skip { what: String, reason: String },
}

impl Action {
    pub fn run(cmd: &Cmd, purpose: &str) -> Action {
        Action::Run { cmd: cmd.display(), purpose: purpose.to_string() }
    }

    pub fn skip(what: &str, reason: &str) -> Action {
        Action::Skip { what: what.to_string(), reason: reason.to_string() }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// Facts the plan was built from (versions, names, URLs), shown with it.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub context: Value,
    /// The commands behind the `Run` actions added with [`Plan::run`], in
    /// order. `execute` runs exactly these, so the plan shown is what happens.
    #[serde(skip)]
    pub exec: Vec<Cmd>,
}

impl Plan {
    pub fn push(&mut self, a: Action) {
        self.actions.push(a);
    }

    /// Add a command that `execute` will run.
    pub fn run(&mut self, cmd: Cmd, purpose: &str) {
        self.actions.push(Action::run(&cmd, purpose));
        self.exec.push(cmd);
    }

    pub fn is_noop(&self) -> bool {
        self.actions.iter().all(|a| matches!(a, Action::Skip { .. }))
    }
}

pub fn confirm_required(step: &str, plan: &Plan) -> ShipError {
    ShipError::new(
        Code::ConfirmRequired,
        format!(
            "`{step}` {}. Review the plan, then re-run with --confirm (or --dry-run to preview).",
            gate_reason(step)
        ),
    )
    .with_details(json!({ "plan": plan }))
}

/// Refuse outward steps on a dirty working tree unless explicitly allowed.
pub fn require_clean(dirty_files: &[String], allow_dirty: bool) -> Result<()> {
    if dirty_files.is_empty() || allow_dirty {
        return Ok(());
    }
    Err(ShipError::new(
        Code::DirtyTree,
        "the working tree has uncommitted changes; commit them first or pass --allow-dirty",
    )
    .with_details(json!({ "files": dirty_files })))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_FLAGS: [Flags; 4] = [
        Flags { confirm: false, dry_run: false },
        Flags { confirm: true, dry_run: false },
        Flags { confirm: false, dry_run: true },
        Flags { confirm: true, dry_run: true },
    ];

    #[test]
    fn high_risk_needs_confirm() {
        assert_eq!(decide(Risk::High, Flags::default()), Decision::Refuse);
        assert_eq!(decide(Risk::High, Flags { confirm: true, dry_run: false }), Decision::Execute);
    }

    #[test]
    fn dry_run_always_wins() {
        for r in [Risk::Low, Risk::Medium, Risk::High] {
            assert_eq!(decide(r, Flags { confirm: true, dry_run: true }), Decision::DryRun);
            assert_eq!(decide(r, Flags { confirm: false, dry_run: true }), Decision::DryRun);
        }
    }

    #[test]
    fn low_and_medium_never_refused() {
        for r in [Risk::Low, Risk::Medium] {
            for f in ALL_FLAGS {
                assert_ne!(decide(r, f), Decision::Refuse);
            }
        }
    }

    #[test]
    fn exactly_the_three_public_steps_are_gated() {
        let gated: Vec<&str> = STEPS.iter().filter(|(s, _)| is_gated(s)).map(|(s, _)| *s).collect();
        assert_eq!(gated, ["create-repo", "release", "publish"]);
        assert_eq!(STEPS.len(), 15);
        assert!(risk_of("nope").is_none());
        assert!(is_gated("install-tool"));
        assert!(!is_gated("login"));
    }

    #[test]
    fn plan_json_shape() {
        let mut p = Plan::default();
        p.push(Action::run(&Cmd::new("git", &["push"]), "push main"));
        p.push(Action::skip("tag", "exists"));
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["actions"][0], json!({"kind": "run", "cmd": "git push", "purpose": "push main"}));
        assert_eq!(v["actions"][1]["kind"], "skip");
        assert!(v.get("context").is_none());
        assert!(!p.is_noop());
    }

    #[test]
    fn refusal_carries_plan() {
        let e = confirm_required("publish", &Plan::default());
        assert_eq!(e.code, Code::ConfirmRequired);
        assert!(e.details.unwrap().get("plan").is_some());
    }

    #[test]
    fn dirty_tree_rule() {
        assert!(require_clean(&[], false).is_ok());
        let files = vec!["src/main.rs".to_string()];
        assert_eq!(require_clean(&files, false).unwrap_err().code, Code::DirtyTree);
        assert!(require_clean(&files, true).is_ok());
    }
}
