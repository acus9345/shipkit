//! `ship scaffold`: create a standard project. Only creates missing files;
//! anything that already exists is left untouched, so it is safe to re-run.

use super::{Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::github;
use crate::log;
use crate::project;
use crate::registry::{self, NewProject, Registry, Target, WorkflowSpec};
use crate::templates;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Scaffold {
    /// Package name (default: the folder name, made valid).
    #[arg(long)]
    pub name: Option<String>,
    /// One-line description for the manifest and README.
    #[arg(long)]
    pub description: Option<String>,
    /// repo: GitHub only (publishing blocked); package; or both.
    #[arg(long, value_enum)]
    pub target: Option<Target>,
    /// GitHub repository URL. When set, CI and release workflows are added.
    #[arg(long)]
    pub repo_url: Option<String>,
    /// Add a command-line entry point with --help and --version.
    #[arg(long)]
    pub cli: bool,
    /// The repository will be private: CI runs on Ubuntu only.
    #[arg(long)]
    pub private: bool,
    #[arg(long)]
    pub registry: Option<String>,
}

impl Scaffold {
    fn files(&self, ctx: &Ctx) -> Result<(Vec<(String, String)>, Value)> {
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        let existing = reg.project_info(&ctx.dir).ok();
        let name = match (&self.name, &existing) {
            (Some(n), _) => n.clone(),
            (None, Some(info)) => info.name.clone(),
            (None, None) => reg.sanitize_name(&project::folder_name(&ctx.dir)),
        };
        reg.validate_name(&name)
            .map_err(|why| ShipError::new(Code::Input, format!("invalid package name `{name}`: {why}")))?;
        if let Some(url) = &self.repo_url {
            if github::parse_github_url(url).is_none() {
                return Err(ShipError::new(Code::Input, format!("`{url}` is not a GitHub repository URL")));
            }
        }
        let identity = project::git_identity(&ctx.dir, &ctx.log);
        let description = self
            .description
            .clone()
            .or_else(|| existing.as_ref().and_then(|i| i.description.clone()))
            .unwrap_or_else(|| format!("{name}: a small Rust project."))
            .replace(['\r', '\n'], " ");
        let today = log::today();
        let p = NewProject {
            name: name.clone(),
            description,
            author: identity.as_ref().map(|(n, e)| format!("{n} <{e}>")),
            repo_url: self.repo_url.clone().map(|u| u.trim_end_matches(".git").to_string()),
            target: self.target.unwrap_or_default(),
            cli: self.cli,
            private: self.private,
            year: today[..4].to_string(),
            date: today,
        };
        let holder = identity
            .map(|(n, _)| n)
            .or_else(|| p.repo_url.as_deref().and_then(github::parse_github_url).map(|(o, _)| o))
            .unwrap_or_else(|| "the authors".into());

        let mut files = reg.new_project_files(&p);
        files.push(("README.md".into(), templates::readme(&p, reg.as_ref())));
        files.push(("LICENSE".into(), templates::license_mit(&p.year, &holder)));
        files.push(("CHANGELOG.md".into(), templates::changelog(&p.date)));
        files.push((".gitignore".into(), templates::gitignore(reg.as_ref())));
        files.push((".gitattributes".into(), templates::GITATTRIBUTES.into()));
        if p.repo_url.is_some() {
            let w = WorkflowSpec {
                name: name.clone(),
                private: p.private,
                publish: p.target.has_package(),
                bin: p.cli.then(|| name.clone()),
            };
            files.extend(workflow_files(reg.as_ref(), &w, true));
        }
        let context = json!({
            "name": p.name,
            "target": p.target,
            "author": p.author,
            "repo_url": p.repo_url,
            "cli": p.cli,
            "private": p.private,
            "registry": reg.id(),
        });
        Ok((files, context))
    }
}

/// `.github/workflows/ci.yml` and (optionally) `release.yml`.
pub fn workflow_files(reg: &dyn Registry, w: &WorkflowSpec, release: bool) -> Vec<(String, String)> {
    let mut v = vec![(".github/workflows/ci.yml".to_string(), reg.ci_workflow(w))];
    if release {
        v.push((".github/workflows/release.yml".to_string(), reg.release_workflow(w)));
    }
    v
}

/// Plan creation of `files` under `ctx.dir`, skipping any that exist.
pub fn plan_create(ctx: &Ctx, files: &[(String, String)]) -> Plan {
    let mut plan = Plan::default();
    for (path, content) in files {
        if ctx.dir.join(path).exists() {
            plan.push(Action::skip(path, "already exists; never overwritten"));
        } else {
            plan.push(Action::CreateFile { path: path.clone(), bytes: content.len() });
        }
    }
    plan
}

/// Create the files a plan lists, re-checking existence at write time.
pub fn create_planned(ctx: &Ctx, files: &[(String, String)], plan: &Plan) -> Result<Value> {
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    for (path, content) in files {
        let planned = plan.actions.iter().any(|a| matches!(a, Action::CreateFile { path: p, .. } if p == path));
        if planned && project::create_new(&ctx.dir.join(path), content)? {
            created.push(path.clone());
        } else {
            skipped.push(path.clone());
        }
    }
    Ok(json!({ "created": created, "skipped_existing": skipped }))
}

impl Step for Scaffold {
    const NAME: &'static str = "scaffold";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let (files, context) = self.files(ctx)?;
        let mut plan = plan_create(ctx, &files);
        plan.context = context;
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value> {
        let (files, context) = self.files(ctx)?;
        std::fs::create_dir_all(&ctx.dir)?;
        let mut out = create_planned(ctx, &files, &plan)?;
        out["project"] = context;
        Ok(out)
    }
}
