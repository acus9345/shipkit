//! `ship verify`: in a fresh temp folder, install the published package from
//! the real registry (or clone and build the repo), then run the README's
//! commands one at a time, stopping at the first failure.

use super::{guard, Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::github;
use crate::proc::{self, Cmd};
use crate::project::{git, TempDir};
use crate::readme;
use crate::registry::{self, ProjectInfo, Registry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Install from the package registry.
    Registry,
    /// Clone the GitHub repo and build it.
    Repo,
}

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Verify {
    /// registry (default for publishable packages) or repo.
    #[arg(long, value_enum)]
    pub from: Option<Source>,
    /// Version to verify (default: the manifest version).
    #[arg(long)]
    pub version: Option<String>,
    /// Keep the temp folder afterwards (it is always kept on failure).
    #[arg(long)]
    pub keep: bool,
    #[arg(long)]
    pub registry: Option<String>,
}

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

impl Verify {
    fn setup(&self, ctx: &Ctx) -> Result<(Box<dyn Registry>, ProjectInfo, Source, String)> {
        let (reg, info) = registry::resolve_existing(self.registry.as_deref(), &ctx.dir)?;
        let source = self.from.unwrap_or(if info.publishable { Source::Registry } else { Source::Repo });
        if source == Source::Registry && !info.publishable {
            return Err(ShipError::new(Code::Precondition, "the manifest blocks publishing; use --from repo"));
        }
        let version = self.version.clone().unwrap_or_else(|| info.version.clone());
        Ok((reg, info, source, version))
    }

    fn clone_url(&self, ctx: &Ctx) -> Result<String> {
        let url = github::origin_url(&ctx.dir, &ctx.log)
            .ok_or_else(|| ShipError::new(Code::Precondition, "no GitHub origin; run `ship create-repo` first"))?;
        let (o, r) = github::parse_github_url(&url)
            .ok_or_else(|| ShipError::new(Code::Precondition, format!("origin `{url}` is not a GitHub repository")))?;
        Ok(format!("https://github.com/{o}/{r}"))
    }
}

impl Step for Verify {
    const NAME: &'static str = "verify";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let (reg, info, source, version) = self.setup(ctx)?;
        let tmp = PathBuf::from("<temp>");
        let mut plan = Plan::default();
        plan.push(Action::skip("temp folder", "a new empty folder under the system temp directory"));
        match (source, &info.bin) {
            (Source::Registry, Some(_)) => {
                plan.push(Action::run(&reg.install_cmd(&info.name, &version, &tmp.join("root")), "install from the real registry"))
            }
            (Source::Registry, None) => {
                for c in reg.library_smoke(&info.name, &version, &tmp) {
                    plan.push(Action::run(&c, "use the published library as a dependency"));
                }
            }
            (Source::Repo, _) => {
                let url = self.clone_url(ctx)?;
                plan.push(Action::run(&git(&tmp, &["clone", "--depth", "1", &url, "src"]), "clone the public repo"));
                plan.push(Action::run(&reg.build_release_cmd(&tmp.join("src")), "build it"));
            }
        }
        let readme_text = std::fs::read_to_string(ctx.dir.join("README.md")).unwrap_or_default();
        if let Some(bin) = &info.bin {
            for args in readme::commands(&readme_text, bin).run {
                plan.push(Action::Run { cmd: args.join(" "), purpose: "README command".into() });
            }
        }
        plan.context = json!({ "source": source, "package": info.name, "version": version, "bin": info.bin });
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, _plan: Plan) -> Result<Value> {
        let (reg, info, source, version) = self.setup(ctx)?;
        let tmp = TempDir::new("ship-verify")?;
        let result = run_verify(self, ctx, reg.as_ref(), &info, source, &version, tmp.path());
        match result {
            Ok(mut v) => {
                v["temp_dir"] = if self.keep { json!(tmp.keep()) } else { Value::Null };
                Ok(v)
            }
            Err(mut e) => {
                let kept = tmp.keep();
                let mut d = e.details.take().unwrap_or_else(|| json!({}));
                d["temp_dir"] = json!(kept);
                e.details = Some(d);
                Err(e)
            }
        }
    }
}

fn run_verify(
    step: &Verify,
    ctx: &Ctx,
    reg: &dyn Registry,
    info: &ProjectInfo,
    source: Source,
    version: &str,
    tmp: &Path,
) -> Result<Value> {
    let fail = |stage: &str, cmd: &Cmd, out: &proc::Output| {
        ShipError::new(Code::CheckFailed, format!("verify failed at {stage}: {}", cmd.display()))
            .with_details(json!({ "stage": stage, "cmd": cmd.display(), "exit": out.code, "output": out.tail(60) }))
    };
    let run_stage = |stage: &str, cmd: Cmd| -> Result<()> {
        let out = proc::run(&cmd, &ctx.log)?;
        if out.success() {
            Ok(())
        } else {
            Err(fail(stage, &cmd, &out))
        }
    };

    let mut stages = Vec::new();
    let (bin_path, readme_text) = match source {
        Source::Registry => {
            if !reg.version_exists(&info.name, version, &ctx.log)? {
                return Err(ShipError::new(Code::NotFound, format!("{} {version} is not on {} yet", info.name, reg.id())));
            }
            let readme_text = std::fs::read_to_string(ctx.dir.join("README.md")).unwrap_or_default();
            match &info.bin {
                Some(bin) => {
                    let root = tmp.join("root");
                    std::fs::create_dir_all(&root)?;
                    let cmd = reg.install_cmd(&info.name, version, &root);
                    stages.push(cmd.display());
                    run_stage("install", cmd)?;
                    (Some(reg.installed_bin(&root, bin)), readme_text)
                }
                None => {
                    for cmd in reg.library_smoke(&info.name, version, tmp) {
                        stages.push(cmd.display());
                        run_stage("library smoke test", cmd)?;
                    }
                    (None, readme_text)
                }
            }
        }
        Source::Repo => {
            let url = step.clone_url(ctx)?;
            let tag = format!("v{version}");
            let mut clone = git(tmp, &["clone", "--depth", "1"]);
            if guard::remote_ref(ctx, &format!("refs/tags/{tag}"))?.is_some() {
                clone = clone.arg("--branch").arg(tag);
            }
            let clone = clone.arg(url).arg("src").timeout(Duration::from_secs(300));
            stages.push(clone.display());
            run_stage("clone", clone)?;
            let src = tmp.join("src");
            let build = reg.build_release_cmd(&src);
            stages.push(build.display());
            run_stage("build", build)?;
            let readme_text = std::fs::read_to_string(src.join("README.md")).unwrap_or_default();
            (info.bin.as_ref().map(|b| reg.built_bin(&src, b)), readme_text)
        }
    };

    let mut ran = Vec::new();
    let mut skipped = Vec::new();
    if let (Some(bin), Some(path)) = (&info.bin, &bin_path) {
        let cmds = readme::commands(&readme_text, bin);
        skipped = cmds.skipped;
        let work = tmp.join("work");
        std::fs::create_dir_all(&work)?;
        for args in cmds.run {
            let refs: Vec<&str> = args[1..].iter().map(String::as_str).collect();
            let cmd = Cmd::new(&path.to_string_lossy(), &refs).cwd(&work).timeout(COMMAND_TIMEOUT);
            let out = proc::run(&cmd, &ctx.log)?;
            let shown = args.join(" ");
            if !out.success() {
                return Err(ShipError::new(Code::CheckFailed, format!("README command failed: {shown}"))
                    .with_details(json!({ "stage": "readme", "cmd": shown, "exit": out.code, "output": out.tail(60), "passed": ran })));
            }
            ran.push(json!({ "cmd": shown, "output": out.tail(5) }));
        }
    }
    Ok(json!({
        "verified": true,
        "source": source,
        "package": info.name,
        "version": version,
        "stages": stages,
        "readme_commands": ran,
        "readme_skipped": skipped,
    }))
}
