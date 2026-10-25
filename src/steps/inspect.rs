//! `ship inspect`: run the registry dry run, list the files that would be
//! published, and flag junk or secrets among them.

use super::{Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::proc;
use crate::registry::{self, Registry};
use crate::secrets;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Inspect {
    #[arg(long)]
    pub registry: Option<String>,
}

impl Step for Inspect {
    const NAME: &'static str = "inspect";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let (reg, info) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        let mut plan = Plan::default();
        if !info.publishable {
            plan.push(Action::skip("registry dry run", "the manifest blocks publishing (repo-only project)"));
            return Ok(plan);
        }
        for cmd in reg.package_files_plan(&ctx.dir) {
            plan.push(Action::run(&cmd, "list the files that would be published"));
        }
        plan.push(Action::skip("secret scan", "runs locally over that file list"));
        for cmd in reg.dry_run_cmds(&ctx.dir) {
            plan.push(Action::run(&cmd, "registry dry run (builds the package, uploads nothing)"));
        }
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, _plan: Plan) -> Result<Value> {
        let (reg, info) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        if !info.publishable {
            return Ok(json!({
                "publishable": false,
                "package": info.name,
                "note": "repo-only project: the manifest blocks publishing, so there is no registry dry run",
            }));
        }
        let report = inspect_package(reg.as_ref(), &ctx.dir, ctx)?;
        Ok(json!({
            "publishable": true,
            "package": info.name,
            "version": info.version,
            "registry": reg.id(),
            "inspection": report,
        }))
    }
}

/// File list, junk, secret scan and dry run. Fails on secrets or a failed dry run.
pub fn inspect_package(reg: &dyn Registry, dir: &Path, ctx: &Ctx) -> Result<Value> {
    let files = reg.package_files(dir, &ctx.log).map_err(|e| {
        ShipError::new(Code::CheckFailed, format!("could not list package files: {}", e.message))
            .with_details(e.details.unwrap_or(Value::Null))
    })?;
    let generated = reg.generated_package_files();
    let real: Vec<String> = files
        .iter()
        .filter(|f| !generated.contains(&f.as_str()) && dir.join(f).is_file())
        .cloned()
        .collect();

    let junk: Vec<Value> = real
        .iter()
        .filter_map(|f| junk_reason(f, dir).map(|why| json!({ "file": f, "reason": why })))
        .collect();
    let scan = secrets::scan(dir, &real);
    if !scan.clean() {
        return Err(ShipError::new(Code::SecretsFound, "possible secrets in files that would be published")
            .with_details(json!({ "findings": scan.findings, "files": files })));
    }

    let mut ran = Vec::new();
    let result = (|| {
        for dry_cmd in reg.dry_run_cmds(dir) {
            let dry = proc::run(&dry_cmd, &ctx.log)?;
            if !dry.success() {
                return Err(ShipError::new(Code::CheckFailed, format!("registry dry run failed: {}", dry_cmd.display()))
                    .with_details(json!({ "output": dry.tail(60), "files": files, "junk": junk })));
            }
            ran.push(dry_cmd.display());
        }
        Ok(())
    })();
    remove_scratch(reg, dir, "");
    result?;
    Ok(json!({
        "files": files,
        "file_count": files.len(),
        "junk": junk,
        "dry_run": { "ok": true, "cmds": ran },
    }))
}

/// Delete the registry's temporary build folders, if it made any.
pub fn remove_scratch(reg: &dyn Registry, dir: &Path, version: &str) {
    for d in reg.scratch_dirs(dir, version) {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Why a packaged file looks like it should not be published, if it does.
pub fn junk_reason(path: &str, dir: &Path) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if let Some(kind) = secrets::risky_filename(path) {
        return Some(format!("sensitive file ({kind})"));
    }
    let prefixes = [
        ("target/", "build output"),
        ("node_modules/", "installed dependencies"),
        (".idea/", "editor settings"),
        (".vscode/", "editor settings"),
        (".github/", "CI configuration; not needed in the package"),
        (".claude/", "local Claude Code settings"),
    ];
    for (p, why) in prefixes {
        if lower.starts_with(p) || lower.contains(&format!("/{p}")) {
            return Some(why.into());
        }
    }
    if matches!(name, ".ds_store" | "thumbs.db" | "desktop.ini") {
        return Some("OS metadata".into());
    }
    if [".log", ".tmp", ".swp", ".bak"].iter().any(|e| name.ends_with(e)) || name.ends_with('~') {
        return Some("temporary or log file".into());
    }
    if let Ok(m) = std::fs::metadata(dir.join(path)) {
        if m.len() > 1024 * 1024 {
            return Some(format!("large file ({} KiB)", m.len() / 1024));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_detection() {
        let d = Path::new(".");
        assert!(junk_reason("target/debug/x", d).is_some());
        assert!(junk_reason("src/.DS_Store", d).is_some());
        assert!(junk_reason("debug.log", d).is_some());
        assert!(junk_reason(".env", d).unwrap().contains("env_file"));
        assert!(junk_reason("src/lib.rs", d).is_none());
        assert!(junk_reason("Cargo.toml", d).is_none());
    }
}
