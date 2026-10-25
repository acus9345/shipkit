//! `ship checks`: run the project's lint, typecheck, build and test steps,
//! stopping at the first failure.

use super::{Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::proc;
use crate::registry;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Instant;

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Checks {
    #[arg(long)]
    pub registry: Option<String>,
}

impl Step for Checks {
    const NAME: &'static str = "checks";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let (reg, _) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        let mut plan = Plan::default();
        for (name, cmd) in reg.checks(&ctx.dir) {
            plan.push(Action::run(&cmd, name));
        }
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, _plan: Plan) -> Result<Value> {
        let (reg, _) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        let mut passed = Vec::new();
        for (name, cmd) in reg.checks(&ctx.dir) {
            let started = Instant::now();
            let out = proc::run(&cmd, &ctx.log)?;
            let seconds = (started.elapsed().as_secs_f64() * 10.0).round() / 10.0;
            if !out.success() {
                return Err(ShipError::new(Code::CheckFailed, format!("`{name}` failed: {}", cmd.display()))
                    .with_details(json!({
                        "failed": name,
                        "cmd": cmd.display(),
                        "exit": out.code,
                        "timed_out": out.timed_out,
                        "output": out.tail(80),
                        "passed": passed,
                    })));
            }
            passed.push(json!({ "name": name, "cmd": cmd.display(), "seconds": seconds }));
        }
        Ok(json!({ "passed": passed }))
    }
}
