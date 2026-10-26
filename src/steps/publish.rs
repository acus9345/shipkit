//! `ship publish` (gated): publish from the terminal with the existing
//! registry login. Skipped if the version already exists. After a 429 the
//! registry is put on a 24-hour cooldown that only `--force` overrides. If
//! the registry asks for 2FA, the publish re-runs in a window for the user.

use super::{guard, inspect, Ctx, Step};
use crate::cooldown;
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::proc::{self, Cmd};
use crate::registry::{self, Registry};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Publish {
    /// Run even if the working tree has uncommitted changes.
    #[arg(long)]
    pub allow_dirty: bool,
    /// Ignore an active rate-limit cooldown. Only after the user confirms.
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub registry: Option<String>,
}

impl Step for Publish {
    const NAME: &'static str = "publish";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let (reg, info) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        if let Some(c) = cooldown::active(reg.id()) {
            if !self.force {
                return Err(ShipError::new(
                    Code::Cooldown,
                    format!(
                        "{} rate-limited a publish; refusing to retry until {} unless --force is passed after the user confirms",
                        reg.id(),
                        c["until_utc"].as_str().unwrap_or("later")
                    ),
                )
                .with_details(c));
            }
        }
        if !info.publishable {
            return Err(ShipError::new(Code::Precondition, "the manifest blocks publishing (repo-only project)"));
        }
        let mut plan = Plan::default();
        let dry_needed = guard::precheck(ctx, reg.as_ref(), self.allow_dirty, &mut plan)?;
        let exists = reg.version_exists(&info.name, &info.version, &ctx.log)?;
        if exists {
            plan.actions.retain(|a| !matches!(a, Action::Run { .. }));
            plan.push(Action::skip("publish", &format!("{} {} is already on {}", info.name, info.version, reg.id())));
        } else {
            let mut purpose = format!("publish {} {} to {} using your existing login", info.name, info.version, reg.id());
            if let Some(note) = reg.publish_note() {
                purpose = format!("{purpose}; {note}");
            }
            for cmd in reg.publish_cmds(&ctx.dir, &info.version, self.allow_dirty) {
                plan.run(cmd, &purpose);
            }
        }
        plan.context = json!({
            "registry": reg.id(),
            "package": info.name,
            "version": info.version,
            "already_published": exists,
            "url": reg.package_url(&info.name, &info.version),
            "registry_dry_run": dry_needed && !exists,
            "cooldown_overridden": self.force && cooldown::active(reg.id()).is_some(),
        });
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value> {
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        let mut out = plan.context.clone();
        if plan.exec.is_empty() {
            out["published"] = json!(false);
            out["reason"] = json!("version already exists");
            return Ok(out);
        }
        guard::dry_run(ctx, reg.as_ref(), plan.context["registry_dry_run"] == true)?;
        let result = run_publish(reg.as_ref(), ctx, &plan);
        let version = plan.context["version"].as_str().unwrap_or_default();
        inspect::remove_scratch(reg.as_ref(), &ctx.dir, version);
        let approved_in_window = result?;
        out["published"] = json!(true);
        if approved_in_window {
            out["approved_in_window"] = json!(true);
        }
        out["next"] = json!("run `ship verify` to install it from the registry and try the README commands");
        Ok(out)
    }
}

/// How long a publish window waits for the user to approve (2FA).
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Runs the planned commands. Returns whether the user approved the publish
/// in a window (2FA) after the captured attempt asked for it.
fn run_publish(reg: &dyn Registry, ctx: &Ctx, plan: &Plan) -> Result<bool> {
    let mut approved_in_window = false;
    for cmd in &plan.exec {
        let res = proc::run(cmd, &ctx.log)?;
        if !res.success() {
            let text = format!("{}\n{}", res.stdout, res.stderr);
            if reg.needs_approval(&text) {
                // Nothing was uploaded: the registry refused before accepting
                // the package. The same command runs again where the user can
                // answer the tool's own 2FA prompt; ship never sees the answer.
                ctx.log.event("approval", json!({ "cmd": cmd.display(), "reason": "registry asked for 2FA" }));
                approve_in_window(reg, ctx, cmd, plan)?;
                approved_in_window = true;
                continue;
            }
            if reg.is_rate_limited(&text) {
                let c = cooldown::start(reg.id(), "HTTP 429 from publish");
                return Err(ShipError::new(
                    Code::RateLimited,
                    format!("{} rate-limited the publish; further publishes are refused for 24 hours unless --force", reg.id()),
                )
                .with_details(json!({ "cooldown": c, "output": res.tail(30) })));
            }
            return Err(ShipError::new(Code::CommandFailed, format!("`{}` failed", cmd.display()))
                .with_details(json!({ "exit": res.code, "output": res.tail(60) })));
        }
    }
    Ok(approved_in_window)
}

fn approve_in_window(reg: &dyn Registry, ctx: &Ctx, cmd: &Cmd, plan: &Plan) -> Result<()> {
    let code = proc::run_in_window(cmd, &ctx.log, APPROVAL_TIMEOUT)?;
    let name = plan.context["package"].as_str().unwrap_or_default();
    let version = plan.context["version"].as_str().unwrap_or_default();
    match code {
        Some(0) => Ok(()),
        // The window shows the tool's output; ship captured none of it.
        _ if reg.version_exists(name, version, &ctx.log).unwrap_or(false) => Ok(()),
        None => Err(ShipError::new(
            Code::Timeout,
            "the publish window is still waiting for 2FA approval; finish or close it, then run `ship publish --dry-run` to check",
        )),
        Some(c) => Err(ShipError::new(
            Code::CommandFailed,
            format!("`{}` exited with {c} in the approval window; its output was shown there", cmd.display()),
        )),
    }
}
