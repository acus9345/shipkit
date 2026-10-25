//! `ship release` (gated): tag v<version> from the manifest, push the branch
//! and the tag, and create the GitHub release from the CHANGELOG unless the
//! release workflow does it. Tags are never moved or deleted.

use super::{guard, Ctx, Step};
use crate::changelog;
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::github;
use crate::proc;
use crate::project::git;
use crate::registry::{self, RELEASE_MARKER};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Release {
    /// Run even if the working tree has uncommitted changes.
    #[arg(long)]
    pub allow_dirty: bool,
    #[arg(long)]
    pub registry: Option<String>,
}

impl Step for Release {
    const NAME: &'static str = "release";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let (reg, info) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        let mut plan = Plan::default();
        let dry_needed = guard::precheck(ctx, reg.as_ref(), self.allow_dirty, &mut plan)?;
        let (owner, repo) = github::origin_url(&ctx.dir, &ctx.log)
            .and_then(|u| github::parse_github_url(&u))
            .ok_or_else(|| ShipError::new(Code::Precondition, "no GitHub origin; run `ship create-repo` first"))?;
        let version = info.version.clone();
        let tag = format!("v{version}");
        let log_text = std::fs::read_to_string(ctx.dir.join("CHANGELOG.md")).unwrap_or_default();
        let notes = changelog::notes_for(&log_text, &version).ok_or_else(|| {
            ShipError::new(Code::Precondition, format!("CHANGELOG.md has no notes for {version}; run `ship bump` or add a `## [{version}]` section"))
        })?;
        let branch = guard::current_branch(ctx)?;
        let head = guard::head(ctx)?;
        let short = &head[..7];

        // 1. Branch.
        match guard::remote_ref(ctx, &format!("refs/heads/{branch}"))? {
            Some(sha) if sha == head => plan.push(Action::skip("push branch", &format!("origin/{branch} is already at {short}"))),
            _ => plan.run(git(&ctx.dir, &["push", "origin", &branch]), &format!("push {branch} (never forced)")),
        }

        // 2. Local tag.
        let local = proc::run(&git(&ctx.dir, &["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}^{{commit}}")]), &ctx.log)?;
        let local_sha = local.success().then(|| local.stdout.trim().to_string());
        match &local_sha {
            Some(sha) if *sha != head => {
                return Err(ShipError::new(
                    Code::Precondition,
                    format!("tag {tag} already exists at {}, not at HEAD ({short}); tags are never moved, so bump the version instead", &sha[..7]),
                ))
            }
            Some(_) => plan.push(Action::skip("create tag", &format!("{tag} already points at {short}"))),
            None => plan.run(
                git(&ctx.dir, &["tag", "-a", &tag, "-m", &format!("Release {tag}")]),
                &format!("create annotated tag {tag} at {short}"),
            ),
        }

        // 3. Remote tag.
        match guard::remote_ref(ctx, &format!("refs/tags/{tag}"))? {
            Some(sha) if sha == head => plan.push(Action::skip("push tag", &format!("origin already has {tag} at {short}"))),
            Some(sha) => {
                return Err(ShipError::new(
                    Code::Precondition,
                    format!("origin already has {tag} at {}, not at HEAD ({short}); tags are never moved", &sha[..7.min(sha.len())]),
                ))
            }
            None => plan.run(git(&ctx.dir, &["push", "origin", &format!("refs/tags/{tag}")]), &format!("push tag {tag} (starts the release workflow, if any)")),
        }

        // 4. GitHub release.
        let workflow = std::fs::read_to_string(ctx.dir.join(".github/workflows/release.yml")).unwrap_or_default();
        let by_workflow = workflow.contains(RELEASE_MARKER);
        let slug = format!("{owner}/{repo}");
        let exists = proc::run(&github::gh(&["release", "view", &tag, "-R", &slug, "--json", "tagName"]), &ctx.log)?.success();
        if exists {
            plan.push(Action::skip("GitHub release", &format!("{tag} already has a release")));
        } else if by_workflow {
            plan.push(Action::skip("GitHub release", "release.yml creates it from CHANGELOG.md with the binaries attached"));
        } else {
            plan.run(
                github::gh(&["release", "create", &tag, "-R", &slug, "--verify-tag", "--title", &tag, "--notes", &notes]),
                &format!("create the GitHub release for {tag} from CHANGELOG.md"),
            );
        }

        plan.context = json!({
            "repo": slug,
            "tag": tag,
            "commit": head,
            "branch": branch,
            "notes": notes,
            "release_by_workflow": by_workflow && !exists,
            "release_url": format!("https://github.com/{owner}/{repo}/releases/tag/{tag}"),
            "registry_dry_run": dry_needed,
        });
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value> {
        let reg = registry::resolve(self.registry.as_deref(), &ctx.dir)?;
        guard::dry_run(ctx, reg.as_ref(), plan.context["registry_dry_run"] == true)?;
        let ran = guard::run_planned(ctx, &plan)?;
        let mut out = plan.context.clone();
        if let Some(o) = out.as_object_mut() {
            o.remove("notes");
        }
        out["ran"] = json!(ran);
        out["next"] = json!("run `ship watch` until the workflow run for this commit finishes");
        Ok(out)
    }
}
