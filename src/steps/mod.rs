//! Steps: each one builds a [`Plan`], then the runner applies the gate and
//! either returns the plan (dry run), refuses (gated, no `--confirm`) or executes.

pub mod add_workflows;
pub mod bump;
pub mod check_names;
pub mod checks;
pub mod commit;
pub mod create_repo;
pub mod guard;
pub mod inspect;
pub mod install_tool;
pub mod login;
pub mod preflight;
pub mod publish;
pub mod release;
pub mod scaffold;
pub mod scan_secrets;
pub mod trusted_publishing;
pub mod verify;
pub mod watch;

use crate::error::Result;
use crate::gate::{self, Decision, Flags, Plan, Risk};
use crate::log::Logger;
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct Ctx {
    /// Project directory (`--dir`, default: current directory).
    pub dir: PathBuf,
    pub flags: Flags,
    pub log: Logger,
}

pub trait Step {
    const NAME: &'static str;

    /// Describe exactly what `execute` will do. Must not change anything.
    fn plan(&self, ctx: &Ctx) -> Result<Plan>;

    /// Carry out the plan.
    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value>;

    fn risk() -> Risk {
        gate::risk_of(Self::NAME).expect("every step is listed in gate::STEPS or gate::HELPERS")
    }
}

pub struct StepOutcome {
    pub data: Value,
    pub dry_run: bool,
}

pub fn run<S: Step>(step: &S, ctx: &Ctx) -> Result<StepOutcome> {
    let plan = step.plan(ctx)?;
    match gate::decide(S::risk(), ctx.flags) {
        Decision::DryRun => {
            ctx.log.event("dry_run", json!({ "plan": plan }));
            Ok(StepOutcome { data: json!({ "plan": plan }), dry_run: true })
        }
        Decision::Refuse => {
            ctx.log.event("refused", json!({ "reason": "missing --confirm" }));
            Err(gate::confirm_required(S::NAME, &plan))
        }
        Decision::Execute => {
            ctx.log.event("execute", json!({ "plan": plan }));
            let data = step.execute(ctx, plan)?;
            Ok(StepOutcome { data, dry_run: false })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Code;
    use crate::gate::Action;
    use std::cell::Cell;

    /// Pretends to be the gated `publish` step and records whether it executed.
    struct FakePublish {
        executed: Cell<bool>,
    }

    impl Step for FakePublish {
        const NAME: &'static str = "publish";
        fn plan(&self, _ctx: &Ctx) -> Result<Plan> {
            let mut p = Plan::default();
            p.push(Action::skip("everything", "fake"));
            Ok(p)
        }
        fn execute(&self, _ctx: &Ctx, _plan: Plan) -> Result<Value> {
            self.executed.set(true);
            Ok(json!({ "published": true }))
        }
    }

    fn ctx(confirm: bool, dry_run: bool) -> Ctx {
        Ctx { dir: PathBuf::from("."), flags: Flags { confirm, dry_run }, log: Logger::null() }
    }

    #[test]
    fn gated_step_refused_without_confirm() {
        let s = FakePublish { executed: Cell::new(false) };
        let e = run(&s, &ctx(false, false)).err().unwrap();
        assert_eq!(e.code, Code::ConfirmRequired);
        assert_eq!(e.details.unwrap()["plan"]["actions"][0]["kind"], "skip");
        assert!(!s.executed.get());
    }

    #[test]
    fn dry_run_never_executes() {
        let s = FakePublish { executed: Cell::new(false) };
        let out = run(&s, &ctx(true, true)).ok().unwrap();
        assert!(out.dry_run);
        assert!(out.data.get("plan").is_some());
        assert!(!s.executed.get());
    }

    #[test]
    fn confirm_executes() {
        let s = FakePublish { executed: Cell::new(false) };
        let out = run(&s, &ctx(true, false)).ok().unwrap();
        assert!(!out.dry_run);
        assert!(s.executed.get());
    }
}
