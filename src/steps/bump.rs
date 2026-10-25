//! `ship bump`: raise the manifest version (patch/minor/major or an explicit
//! version) and record it in CHANGELOG.md. Does not commit.
//!
//! Idempotent: a version that has not been released yet (no `v<version>` tag)
//! is not bumped again, so running the step twice cannot skip a version.

use super::{Ctx, Step};
use crate::changelog;
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::log;
use crate::proc;
use crate::project::{self, git};
use crate::registry;
use crate::version::{Level, Version};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Bump {
    /// patch, minor or major.
    #[arg(long, value_enum)]
    pub level: Option<Level>,
    /// Exact new version instead of --level.
    #[arg(long)]
    pub version: Option<String>,
    /// Release notes (one item per line). Default: the CHANGELOG's Unreleased section.
    #[arg(long)]
    pub notes: Option<String>,
    #[arg(long)]
    pub registry: Option<String>,
}

struct Edits {
    from: String,
    to: String,
    files: Vec<(String, String)>,
    noop: Option<String>,
}

impl Bump {
    fn edits(&self, ctx: &Ctx) -> Result<Edits> {
        let (reg, info) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        let current = Version::parse(&info.version)?;
        let log_path = ctx.dir.join("CHANGELOG.md");
        let text = std::fs::read_to_string(&log_path).unwrap_or_else(|_| changelog::HEADER.to_string());
        let noop = |to: &Version, why: String| Edits { from: current.to_string(), to: to.to_string(), files: vec![], noop: Some(why) };

        let target = match (&self.level, &self.version) {
            (Some(l), None) => {
                if !tag_exists(ctx, &format!("v{current}")) {
                    return Ok(noop(&current, format!(
                        "{current} has not been released yet (no tag v{current}); release it first, or pass --version to pick a different version"
                    )));
                }
                current.bump(*l)
            }
            (None, Some(v)) => {
                let v = Version::parse(v)?;
                if v == current && changelog::has_version(&text, &v.to_string()) {
                    return Ok(noop(&v, format!("already at {v}")));
                }
                v
            }
            (None, None) => return Err(ShipError::new(Code::Input, "pass --level patch|minor|major or --version X.Y.Z")),
            (Some(_), Some(_)) => return Err(ShipError::new(Code::Input, "pass either --level or --version, not both")),
        };
        if target != current && !target.is_newer_than(&current) {
            return Err(ShipError::new(Code::Input, format!("{target} is not newer than the current version {current}")));
        }
        let to = target.to_string();
        let mut files = if target != current { reg.set_version(&ctx.dir, &info, &to)? } else { vec![] };
        let new_log = changelog::record_release(&text, &to, &log::today(), self.notes.as_deref())?;
        files.push(("CHANGELOG.md".to_string(), new_log));
        Ok(Edits { from: current.to_string(), to, files, noop: None })
    }
}

fn tag_exists(ctx: &Ctx, tag: &str) -> bool {
    project::is_git_root(&ctx.dir, &ctx.log)
        && proc::run(&git(&ctx.dir, &["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")]), &ctx.log)
            .map(|o| o.success())
            .unwrap_or(false)
}

impl Step for Bump {
    const NAME: &'static str = "bump";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let e = self.edits(ctx)?;
        let mut plan = Plan::default();
        if let Some(why) = &e.noop {
            plan.push(Action::skip("bump", why));
        }
        for (path, _) in &e.files {
            if ctx.dir.join(path).exists() {
                let change = if path == "CHANGELOG.md" {
                    format!("add a `## [{}]` section", e.to)
                } else {
                    format!("version {} -> {}", e.from, e.to)
                };
                plan.push(Action::EditFile { path: path.clone(), change });
            } else {
                plan.push(Action::CreateFile { path: path.clone(), bytes: 0 });
            }
        }
        plan.context = json!({ "from": e.from, "to": e.to });
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, _plan: Plan) -> Result<Value> {
        let e = self.edits(ctx)?;
        if let Some(why) = e.noop {
            return Ok(json!({ "bumped": false, "version": e.from, "reason": why }));
        }
        let mut written = Vec::new();
        for (path, content) in &e.files {
            write_atomic(&ctx.dir.join(path), content)?;
            written.push(path.clone());
        }
        Ok(json!({
            "bumped": true,
            "from": e.from,
            "to": e.to,
            "files": written,
            "next": format!("review, then `ship commit -m \"Release v{}\"`", e.to),
        }))
    }
}

/// Replace a file via a temp file + rename, so a crash never leaves it half-written.
fn write_atomic(path: &std::path::Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("ship-tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ShipError::new(Code::Io, format!("cannot replace {}: {e}", path.display()))
    })
}
