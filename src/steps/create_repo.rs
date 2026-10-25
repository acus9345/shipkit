//! `ship create-repo` (gated): create the GitHub repo and push, or just push
//! when the repo is already this folder's `origin`. Public by default.

use super::{guard, Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::github;
use crate::project::{self, git};
use crate::registry;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CreateRepo {
    /// Repository name (default: the package name).
    #[arg(long)]
    pub name: Option<String>,
    /// Owner (default: the active gh account).
    #[arg(long)]
    pub owner: Option<String>,
    /// Create a private repository (default: public).
    #[arg(long)]
    pub private: bool,
    #[arg(long)]
    pub description: Option<String>,
    /// Repository topics, comma-separated.
    #[arg(long, value_delimiter = ',')]
    pub topics: Vec<String>,
    /// Run even if the working tree has uncommitted changes.
    #[arg(long)]
    pub allow_dirty: bool,
    #[arg(long)]
    pub registry: Option<String>,
}

impl Step for CreateRepo {
    const NAME: &'static str = "create-repo";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        let mut plan = Plan::default();
        let dry_needed = guard::precheck(ctx, reg.as_ref(), self.allow_dirty, &mut plan)?;
        let branch = guard::current_branch(ctx)?;
        let head = guard::head(ctx)?;

        let (owner, name, visibility) = match github::origin_url(&ctx.dir, &ctx.log) {
            Some(url) => {
                let (o, r) = github::parse_github_url(&url).ok_or_else(|| {
                    ShipError::new(Code::Precondition, format!("origin is `{url}`, which is not a GitHub repository"))
                })?;
                let mismatch = |want: &Option<String>, have: &str| want.as_ref().is_some_and(|w| !w.eq_ignore_ascii_case(have));
                if mismatch(&self.owner, &o) || mismatch(&self.name, &r) {
                    return Err(ShipError::new(
                        Code::Precondition,
                        format!("this folder's origin is already {o}/{r}; refusing to point it elsewhere"),
                    ));
                }
                let info = github::repo(&o, &r, &ctx.log)?.ok_or_else(|| {
                    ShipError::new(Code::NotFound, format!("origin points to {o}/{r}, which does not exist or is not visible to your gh account"))
                })?;
                plan.push(Action::skip("create repository", &format!("{} already exists and is this folder's origin", info.full_name)));
                let remote = guard::remote_ref(ctx, &format!("refs/heads/{branch}"))?;
                if remote.as_deref() == Some(head.as_str()) {
                    plan.push(Action::skip("push", &format!("origin/{branch} is already at {}", &head[..7])));
                } else {
                    plan.run(git(&ctx.dir, &["push", "-u", "origin", &branch]), &format!("push {branch} to origin"));
                }
                let vis = if info.private { "private" } else { "public" };
                (o, r, vis.to_string())
            }
            None => {
                let owner = match &self.owner {
                    Some(o) => o.clone(),
                    None => github::active_login(&ctx.log)?,
                };
                let name = match &self.name {
                    Some(n) => n.clone(),
                    None => reg
                        .project_info(&ctx.dir)
                        .map(|i| i.name)
                        .unwrap_or_else(|_| reg.sanitize_name(&project::folder_name(&ctx.dir))),
                };
                github::validate_repo_name(&name).map_err(|why| ShipError::new(Code::Input, why))?;
                if let Some(existing) = github::repo(&owner, &name, &ctx.log)? {
                    return Err(ShipError::new(
                        Code::AlreadyExists,
                        format!(
                            "{} already exists and is not this folder's origin; pick another name, or if it is yours and empty, add it as origin yourself",
                            existing.url
                        ),
                    ));
                }
                let vis = if self.private { "private" } else { "public" };
                let mut cmd = github::gh(&["repo", "create", &format!("{owner}/{name}"), &format!("--{vis}")])
                    .arg("--source=.")
                    .arg("--remote=origin")
                    .arg("--push")
                    .cwd(&ctx.dir);
                if let Some(d) = &self.description {
                    cmd = cmd.arg("--description").arg(d.clone());
                }
                plan.run(cmd, &format!("create {vis} repository {owner}/{name} and push {branch}"));
                (owner, name, vis.to_string())
            }
        };

        if !self.topics.is_empty() {
            let mut cmd = github::gh(&["repo", "edit", &format!("{owner}/{name}")]);
            for t in &self.topics {
                cmd = cmd.arg("--add-topic").arg(t.trim().to_ascii_lowercase());
            }
            plan.run(cmd, "add repository topics");
        }

        plan.context = json!({
            "repo": format!("{owner}/{name}"),
            "url": format!("https://github.com/{owner}/{name}"),
            "visibility": visibility,
            "branch": branch,
            "commit": head,
            "registry_dry_run": dry_needed,
        });
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value> {
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        guard::dry_run(ctx, reg.as_ref(), plan.context["registry_dry_run"] == true)?;
        let ran = guard::run_planned(ctx, &plan)?;
        let mut out = plan.context.clone();
        out["ran"] = json!(ran);
        out["created"] = json!(plan.actions.iter().all(|a| !matches!(a, Action::Skip { what, .. } if what == "create repository")));
        Ok(out)
    }
}
