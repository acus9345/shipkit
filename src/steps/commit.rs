//! `ship commit`: git init (branch main) if needed, refuse when secrets or
//! .env files would be committed or the registry dry run fails, then stage
//! everything and commit.

use super::{inspect, scan_secrets, Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::proc;
use crate::project::{self, git};
use crate::registry;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Commit {
    /// Commit message (default for the first commit: "Initial commit").
    #[arg(long, short)]
    pub message: Option<String>,
    #[arg(long)]
    pub registry: Option<String>,
}

struct Facts {
    is_repo: bool,
    /// None only when there is nothing to commit and no message was given.
    message: Option<String>,
    clean: bool,
    dry_run_needed: bool,
}

impl Commit {
    fn facts(&self, ctx: &Ctx) -> Result<Facts> {
        if !ctx.dir.is_dir() {
            return Err(ShipError::new(Code::NotFound, format!("{} does not exist", ctx.dir.display())));
        }
        let is_repo = project::is_git_root(&ctx.dir, &ctx.log);
        let has_commits = is_repo && project::has_commits(&ctx.dir, &ctx.log);
        let clean = has_commits && project::dirty_files(&ctx.dir, &ctx.log)?.is_empty();
        let message = match (&self.message, has_commits) {
            (Some(m), _) if !m.trim().is_empty() => Some(m.clone()),
            (_, false) => Some("Initial commit".to_string()),
            _ if clean => None,
            _ => return Err(ShipError::new(Code::Input, "--message is required after the first commit")),
        };
        if project::git_identity(&ctx.dir, &ctx.log).is_none() {
            return Err(ShipError::new(
                Code::Precondition,
                "git identity is not set; run `git config --global user.name` and `user.email` yourself",
            ));
        }
        // The secret scan always runs first, so even --dry-run reports a refusal.
        scan_secrets::require_clean(&scan_secrets::scan_committable(ctx)?)?;
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        let dry_run_needed = ctx.dir.join(reg.manifest()).is_file() && reg.project_info(&ctx.dir)?.publishable;
        Ok(Facts { is_repo, message, clean, dry_run_needed })
    }
}

impl Step for Commit {
    const NAME: &'static str = "commit";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let f = self.facts(ctx)?;
        let mut plan = Plan::default();
        if f.is_repo {
            plan.push(Action::skip("git init", "already a git repository"));
        } else {
            plan.push(Action::run(&git(&ctx.dir, &["init", "-b", "main"]), "create the repository on branch main"));
        }
        let Some(message) = f.message.as_deref().filter(|_| !f.clean) else {
            plan.push(Action::skip("commit", "nothing to commit; the working tree is clean"));
            return Ok(plan);
        };
        plan.push(Action::skip("secret scan", "passed"));
        if f.dry_run_needed {
            let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
            plan.push(Action::run(&reg.dry_run_cmd(&ctx.dir), "registry dry run must pass before committing"));
        } else {
            plan.push(Action::skip("registry dry run", "no publishable manifest"));
        }
        plan.push(Action::run(&git(&ctx.dir, &["add", "-A"]), "stage all changes"));
        plan.push(Action::run(&git(&ctx.dir, &["commit", "-m", message]), "commit"));
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value> {
        if plan.actions.iter().any(|a| matches!(a, Action::Skip { what, .. } if what == "commit")) {
            return Ok(json!({ "committed": false, "reason": "nothing to commit" }));
        }
        let f = self.facts(ctx)?;
        let message = f.message.clone().unwrap_or_default();
        if f.dry_run_needed {
            let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
            inspect::inspect_package(reg.as_ref(), &ctx.dir, ctx)?;
        }
        if !f.is_repo {
            proc::run_ok(&git(&ctx.dir, &["init", "-b", "main"]), &ctx.log)?;
        }
        proc::run_ok(&git(&ctx.dir, &["add", "-A"]), &ctx.log)?;
        let staged = proc::run(&git(&ctx.dir, &["diff", "--cached", "--quiet"]), &ctx.log)?;
        if staged.success() {
            return Ok(json!({ "committed": false, "reason": "nothing to commit" }));
        }
        let names = proc::run_ok(&git(&ctx.dir, &["diff", "--cached", "--name-only"]), &ctx.log)?;
        let count = names.stdout.lines().filter(|l| !l.is_empty()).count();
        proc::run_ok(&git(&ctx.dir, &["commit", "--quiet", "-m", &message]), &ctx.log)?;
        let sha = proc::run_ok(&git(&ctx.dir, &["rev-parse", "HEAD"]), &ctx.log)?.stdout.trim().to_string();
        let branch = proc::run_ok(&git(&ctx.dir, &["branch", "--show-current"]), &ctx.log)?.stdout.trim().to_string();
        Ok(json!({
            "committed": true,
            "commit": sha,
            "branch": branch,
            "files_changed": count,
            "message": message,
            "registry_dry_run": f.dry_run_needed,
        }))
    }
}
