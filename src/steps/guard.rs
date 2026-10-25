//! Shared preconditions for outward steps (create-repo, release, publish):
//! a clean working tree, a clean secret scan and a passing registry dry run.

use super::{inspect, scan_secrets, Ctx};
use crate::error::{Code, Result, ShipError};
use crate::gate::{self, Action, Plan};
use crate::proc;
use crate::project;
use crate::registry::Registry;
use serde_json::{json, Value};

/// Fast checks, run while planning so `--dry-run` reports a refusal too.
/// Returns whether the (slow) registry dry run must run before executing.
pub fn precheck(ctx: &Ctx, reg: &dyn Registry, allow_dirty: bool, plan: &mut Plan) -> Result<bool> {
    if !project::is_git_root(&ctx.dir, &ctx.log) || !project::has_commits(&ctx.dir, &ctx.log) {
        return Err(ShipError::new(Code::Precondition, "no commits yet; run `ship commit` first"));
    }
    gate::require_clean(&project::dirty_files(&ctx.dir, &ctx.log)?, allow_dirty)?;
    scan_secrets::require_clean(&scan_secrets::scan_committable(ctx)?)?;
    plan.push(Action::skip("secret scan", "passed"));
    let needed = ctx.dir.join(reg.manifest()).is_file() && reg.project_info(&ctx.dir)?.publishable;
    if needed {
        for cmd in reg.dry_run_cmds(&ctx.dir) {
            plan.push(Action::run(&cmd, "registry dry run must pass first"));
        }
    } else {
        plan.push(Action::skip("registry dry run", "the manifest blocks publishing"));
    }
    Ok(needed)
}

/// The registry dry run, if needed. Run at the start of `execute`.
pub fn dry_run(ctx: &Ctx, reg: &dyn Registry, needed: bool) -> Result<()> {
    if needed {
        inspect::inspect_package(reg, &ctx.dir, ctx)?;
    }
    Ok(())
}

/// Run the plan's commands in order, stopping at the first failure.
pub fn run_planned(ctx: &Ctx, plan: &Plan) -> Result<Vec<Value>> {
    let mut done = Vec::new();
    for cmd in &plan.exec {
        proc::run_ok(cmd, &ctx.log).map_err(|e| {
            let mut details = e.details.clone().unwrap_or_else(|| json!({}));
            details["completed"] = json!(done);
            ShipError { details: Some(details), ..e }
        })?;
        done.push(json!(cmd.display()));
    }
    Ok(done)
}

pub fn head(ctx: &Ctx) -> Result<String> {
    Ok(proc::run_ok(&project::git(&ctx.dir, &["rev-parse", "HEAD"]), &ctx.log)?.stdout.trim().to_string())
}

pub fn current_branch(ctx: &Ctx) -> Result<String> {
    let b = proc::run_ok(&project::git(&ctx.dir, &["branch", "--show-current"]), &ctx.log)?.stdout.trim().to_string();
    if b.is_empty() {
        return Err(ShipError::new(Code::Precondition, "HEAD is detached; check out a branch first"));
    }
    Ok(b)
}

/// Commit a remote ref points at (peeling annotated tags), if it exists.
pub fn remote_ref(ctx: &Ctx, refname: &str) -> Result<Option<String>> {
    let peeled = format!("{refname}^{{}}");
    let out = proc::run_ok(&project::git(&ctx.dir, &["ls-remote", "origin", refname, &peeled]), &ctx.log)?;
    let mut plain = None;
    for line in out.stdout.lines() {
        if let Some((sha, name)) = line.split_once('\t') {
            if name == peeled {
                return Ok(Some(sha.to_string()));
            }
            if name == refname {
                plain = Some(sha.to_string());
            }
        }
    }
    Ok(plain)
}
