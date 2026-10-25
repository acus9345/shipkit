//! `ship trusted-publishing`: connect the package to the release workflow
//! without stored tokens. crates.io needs a manual step: this opens the
//! settings page and prints the exact values; the user clicks Save.

use super::{guard, Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::github;
use crate::proc;
use crate::registry;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrustedPublishing {
    /// Workflow file that publishes (default: release.yml).
    #[arg(long)]
    pub workflow: Option<String>,
    /// Print the values without opening the browser.
    #[arg(long)]
    pub no_open: bool,
    #[arg(long)]
    pub registry: Option<String>,
}

impl Step for TrustedPublishing {
    const NAME: &'static str = "trusted-publishing";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let (reg, info) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        let mut plan = Plan::default();
        if !info.publishable {
            plan.push(Action::skip("trusted publishing", "the manifest blocks publishing (repo-only project)"));
            plan.context = json!({ "status": "not_applicable" });
            return Ok(plan);
        }
        let (owner, repo) = github::origin_url(&ctx.dir, &ctx.log)
            .and_then(|u| github::parse_github_url(&u))
            .ok_or_else(|| ShipError::new(Code::Precondition, "no GitHub origin; run `ship create-repo` first"))?;
        let workflow = self.workflow.clone().unwrap_or_else(|| "release.yml".into());
        let workflow_present = ctx.dir.join(".github/workflows").join(&workflow).is_file();
        let published = !reg.name_available(&info.name, &ctx.log)?;
        let tp = reg.trusted_publishing(&info.name, &owner, &repo, &workflow, published);
        // Some registries (PyPI) accept a "pending" publisher before the first release.
        let exists = published || !tp.needs_existing_package;

        let status = if !exists {
            plan.push(Action::skip(
                "open settings page",
                &format!("{} {} is not on {} yet; it only accepts a trusted publisher for an existing package", reg.id(), info.name, reg.id()),
            ));
            "package_not_published_yet"
        } else if self.no_open {
            plan.push(Action::skip("open settings page", "--no-open"));
            "values_printed"
        } else {
            plan.push(Action::OpenUrl { url: tp.settings_url.clone() });
            plan.exec.push(proc::open_url_cmd(&tp.settings_url)?);
            "opened"
        };
        let fields: Vec<Value> = tp.fields.iter().map(|(k, v)| json!({ "field": k, "value": v })).collect();
        let mut next = Vec::new();
        if !workflow_present {
            next.push(format!(".github/workflows/{workflow} does not exist yet; run `ship add-workflows`"));
        }
        if !exists {
            next.push("publish the first version from the terminal with `ship publish --confirm`, then run `ship trusted-publishing` again".into());
        } else {
            next.push("enter the values on the settings page and click Save (you do this; ship never submits the form)".into());
        }
        plan.context = json!({
            "status": status,
            "registry": reg.id(),
            "package": info.name,
            "published": published,
            "settings_url": tp.settings_url,
            "values": fields,
            "instructions": tp.instructions,
            "workflow_present": workflow_present,
            "next": next,
        });
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value> {
        guard::run_planned(ctx, &plan)?;
        Ok(plan.context)
    }
}
