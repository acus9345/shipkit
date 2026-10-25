//! `ship add-workflows`: write ci.yml and (optionally) release.yml. Existing
//! workflow files are never overwritten.

use super::scaffold::{create_planned, plan_create, workflow_files};
use super::{Ctx, Step};
use crate::error::Result;
use crate::gate::{Action, Plan};
use crate::github;
use crate::registry::{self, WorkflowSpec};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AddWorkflows {
    /// Only write ci.yml, not release.yml.
    #[arg(long)]
    pub no_release: bool,
    /// Treat the repo as private (Ubuntu-only runners). Default: read from GitHub.
    #[arg(long)]
    pub private: Option<bool>,
    #[arg(long)]
    pub registry: Option<String>,
}

impl AddWorkflows {
    fn files(&self, ctx: &Ctx) -> Result<(Vec<(String, String)>, Value)> {
        let (reg, info) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        let (private, source) = match self.private {
            Some(p) => (p, "flag"),
            None => match github::origin_url(&ctx.dir, &ctx.log).and_then(|u| github::parse_github_url(&u)) {
                Some((o, r)) => match github::repo(&o, &r, &ctx.log)? {
                    Some(info) => (info.private, "github"),
                    None => (false, "default"),
                },
                None => (false, "default"),
            },
        };
        let w = WorkflowSpec { name: info.name.clone(), private, publish: info.publishable, bin: info.bin.clone() };
        let files = workflow_files(reg.as_ref(), &w, !self.no_release);
        let context = json!({
            "private": private,
            "private_from": source,
            "publish_to_registry": w.publish,
            "registry": reg.id(),
            "executable": w.bin,
            "runners": if private { "ubuntu-latest" } else { "ubuntu-latest, windows-latest" },
        });
        Ok((files, context))
    }
}

impl Step for AddWorkflows {
    const NAME: &'static str = "add-workflows";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let (files, context) = self.files(ctx)?;
        let mut plan = plan_create(ctx, &files);
        // Say when an existing file differs from what would be generated.
        for a in plan.actions.iter_mut() {
            if let Action::Skip { what, reason } = a {
                if let Some((_, content)) = files.iter().find(|(p, _)| p == what) {
                    let current = std::fs::read_to_string(ctx.dir.join(&*what)).unwrap_or_default().replace("\r\n", "\n");
                    if current != *content {
                        *reason = "already exists and differs from the generated version; left unchanged".into();
                    }
                }
            }
        }
        plan.context = context;
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value> {
        let (files, _) = self.files(ctx)?;
        let mut out = create_planned(ctx, &files, &plan)?;
        out["workflows"] = plan.context;
        out["next"] = json!("commit the new files with `ship commit -m \"Add workflows\"`");
        Ok(out)
    }
}
