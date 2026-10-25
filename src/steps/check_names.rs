//! `ship check-names`: is the package name valid and free on the registry, and
//! is the GitHub repo name free? Suggests the best free alternative if not.

use super::{Ctx, Step};
use crate::error::Result;
use crate::gate::{Action, Plan};
use crate::github;
use crate::project;
use crate::registry::{self, Registry, Target};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CheckNames {
    /// Package name (default: from the manifest, else the folder name).
    #[arg(long)]
    pub name: Option<String>,
    /// GitHub repository name (default: the package name).
    #[arg(long)]
    pub repo: Option<String>,
    /// GitHub owner (default: the active gh account).
    #[arg(long)]
    pub owner: Option<String>,
    /// What will be shipped: repo, package or both.
    #[arg(long, value_enum)]
    pub target: Option<Target>,
    #[arg(long)]
    pub registry: Option<String>,
}

/// At most this many alternatives are looked up, to bound network calls.
const MAX_SUGGESTION_LOOKUPS: usize = 6;

impl CheckNames {
    fn resolve(&self, ctx: &Ctx, reg: &dyn Registry) -> (String, String) {
        let name = self.name.clone().unwrap_or_else(|| {
            reg.project_info(&ctx.dir)
                .map(|i| i.name)
                .unwrap_or_else(|_| reg.sanitize_name(&project::folder_name(&ctx.dir)))
        });
        let repo = self.repo.clone().unwrap_or_else(|| name.clone());
        (name, repo)
    }
}

impl Step for CheckNames {
    const NAME: &'static str = "check-names";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        let target = self.target.unwrap_or_default();
        let (name, repo) = self.resolve(ctx, reg.as_ref());
        let mut plan = Plan::default();
        if target.has_package() {
            plan.push(Action::Run {
                cmd: format!("GET https://crates.io/api/v1/crates/{name}"),
                purpose: format!("is `{name}` free on {}", reg.id()),
            });
        }
        if target.has_repo() {
            let owner = self.owner.clone().unwrap_or_else(|| "<active gh account>".into());
            plan.push(Action::run(&github::gh(&["api", &format!("repos/{owner}/{repo}")]), "is the repo name free"));
        }
        plan.push(Action::skip("alternatives", &format!("looked up only if a name is taken (at most {MAX_SUGGESTION_LOOKUPS})")));
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, _plan: Plan) -> Result<Value> {
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        let target = self.target.unwrap_or_default();
        let (name, repo) = self.resolve(ctx, reg.as_ref());
        let owner = if target.has_repo() {
            match &self.owner {
                Some(o) => o.clone(),
                None => github::active_login(&ctx.log)?,
            }
        } else {
            String::new()
        };
        let origin = github::origin_url(&ctx.dir, &ctx.log).and_then(|u| github::parse_github_url(&u));

        let lookups = std::cell::Cell::new(0usize);
        let pkg_free = |n: &str| -> Result<std::result::Result<bool, String>> {
            if let Err(why) = reg.validate_name(n) {
                return Ok(Err(why));
            }
            lookups.set(lookups.get() + 1);
            Ok(Ok(reg.name_available(n, &ctx.log)?))
        };
        let package = if target.has_package() {
            let r = pkg_free(&name)?;
            Some(json!({
                "name": name,
                "registry": reg.id(),
                "valid": r.is_ok(),
                "problem": r.as_ref().err(),
                "available": r.unwrap_or(false),
            }))
        } else {
            None
        };

        let is_origin = |o: &str, r: &str| {
            origin.as_ref().is_some_and(|(oo, rr)| oo.eq_ignore_ascii_case(o) && rr.eq_ignore_ascii_case(r))
        };
        let repo_free = |r: &str| -> Result<std::result::Result<bool, String>> {
            if let Err(why) = github::validate_repo_name(r) {
                return Ok(Err(why));
            }
            Ok(Ok(github::repo(&owner, r, &ctx.log)?.is_none() || is_origin(&owner, r)))
        };
        let repo_json = if target.has_repo() {
            let r = repo_free(&repo)?;
            Some(json!({
                "owner": owner,
                "name": repo,
                "url": format!("https://github.com/{owner}/{repo}"),
                "valid": r.is_ok(),
                "problem": r.as_ref().err(),
                "available": r.unwrap_or(false),
                "is_this_folders_origin": is_origin(&owner, &repo),
            }))
        } else {
            None
        };

        let ok = |v: &Option<Value>| v.as_ref().map_or(true, |j| j["available"] == true);
        let all_free = ok(&package) && ok(&repo_json);

        let mut suggestion = Value::Null;
        if !all_free {
            let cands = candidates(&reg.sanitize_name(&name), &owner);
            for cand in cands.into_iter().filter(|c| *c != name || *c != repo) {
                if lookups.get() >= MAX_SUGGESTION_LOOKUPS {
                    break;
                }
                let p_ok = !target.has_package() || matches!(pkg_free(&cand)?, Ok(true));
                if !p_ok {
                    continue;
                }
                let r_ok = !target.has_repo() || matches!(repo_free(&cand)?, Ok(true));
                if r_ok {
                    suggestion = json!({
                        "package": target.has_package().then(|| cand.clone()),
                        "repo": target.has_repo().then(|| cand.clone()),
                    });
                    break;
                }
            }
        }

        Ok(json!({
            "all_free": all_free,
            "target": target,
            "package": package,
            "repo": repo_json,
            "suggestion": suggestion,
        }))
    }
}

/// Alternatives in order of preference. The sanitized name comes first so an
/// invalid name gets its closest valid form.
pub fn candidates(base: &str, owner: &str) -> Vec<String> {
    let stem = base.trim_end_matches("-rs").trim_end_matches("-cli");
    let mut v = vec![
        base.to_string(),
        format!("{stem}-cli"),
        format!("{stem}-rs"),
        format!("{stem}kit"),
        format!("{stem}-tool"),
    ];
    if !owner.is_empty() {
        v.push(format!("{}-{stem}", owner.to_ascii_lowercase()));
    }
    let mut seen = std::collections::HashSet::new();
    v.retain(|c| seen.insert(c.clone()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_order() {
        assert_eq!(candidates("ship", "Octo"), ["ship", "ship-cli", "ship-rs", "shipkit", "ship-tool", "octo-ship"]);
        assert_eq!(candidates("ship-rs", "")[..3], ["ship-rs", "ship-cli", "shipkit"]);
    }
}
