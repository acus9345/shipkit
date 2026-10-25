//! `ship scan-secrets`: look for .env files, private keys and tokens in
//! everything that would be committed. Reports file and line, never the secret.

use super::{Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::project;
use crate::secrets::{self, Report};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ScanSecrets {}

impl Step for ScanSecrets {
    const NAME: &'static str = "scan-secrets";

    fn plan(&self, _ctx: &Ctx) -> Result<Plan> {
        let mut plan = Plan::default();
        plan.push(Action::Run {
            cmd: "git ls-files --cached --others --exclude-standard".into(),
            purpose: "list every file that would be committed".into(),
        });
        plan.push(Action::skip("network", "the scan is local only"));
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, _plan: Plan) -> Result<Value> {
        let report = scan_committable(ctx)?;
        require_clean(&report)?;
        Ok(json!({ "clean": true, "files_scanned": report.files_scanned, "skipped": report.skipped }))
    }
}

pub fn scan_committable(ctx: &Ctx) -> Result<Report> {
    let files = project::committable_files(&ctx.dir, &ctx.log)?;
    Ok(secrets::scan(&ctx.dir, &files))
}

/// `E_SECRETS_FOUND` listing file, line and kind for every finding.
pub fn require_clean(report: &Report) -> Result<()> {
    if report.clean() {
        return Ok(());
    }
    Err(ShipError::new(
        Code::SecretsFound,
        format!(
            "{} possible secret(s) in files that would be committed; remove them, add the file to .gitignore, or mark a false positive with `{}`",
            report.findings.len(),
            secrets::ALLOW_MARKER
        ),
    )
    .with_details(json!({ "findings": report.findings, "files_scanned": report.files_scanned })))
}
