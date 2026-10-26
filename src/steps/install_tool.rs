//! `ship install-tool <tool>` (gated): install a tool `ship` needs with the
//! system package manager (winget on Windows, Homebrew on macOS). Skipped if
//! the tool is already installed. Elsewhere it points to the official page.

use super::{Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::proc::{self, Cmd};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

const INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct InstallTool {
    /// gh, git, node or uv.
    #[arg(value_name = "TOOL")]
    pub tool: Option<String>,
}

#[derive(Debug)]
struct Tool {
    name: &'static str,
    /// Program whose `--version` proves it is installed.
    program: &'static str,
    winget_id: &'static str,
    brew_formula: &'static str,
    docs: &'static str,
}

const TOOLS: &[Tool] = &[
    Tool { name: "gh", program: "gh", winget_id: "GitHub.cli", brew_formula: "gh", docs: "https://cli.github.com" },
    Tool { name: "git", program: "git", winget_id: "Git.Git", brew_formula: "git", docs: "https://git-scm.com/downloads" },
    Tool { name: "node", program: "node", winget_id: "OpenJS.NodeJS.LTS", brew_formula: "node", docs: "https://nodejs.org" },
    Tool {
        name: "uv",
        program: "uv",
        winget_id: "astral-sh.uv",
        brew_formula: "uv",
        docs: "https://docs.astral.sh/uv/getting-started/installation/",
    },
];

fn find(tool: Option<&str>) -> Result<&'static Tool> {
    let names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
    let tool = tool.ok_or_else(|| ShipError::new(Code::Input, format!("name the tool to install: {}", names.join(", "))))?;
    let wanted = match tool.to_ascii_lowercase().as_str() {
        "github" | "github-cli" => "gh".to_string(),
        "nodejs" | "npm" => "node".to_string(),
        other => other.to_string(),
    };
    TOOLS
        .iter()
        .find(|t| t.name == wanted)
        .ok_or_else(|| ShipError::new(Code::Input, format!("unknown tool `{tool}`; supported: {}", names.join(", "))))
}

/// The install command for this OS, and the package manager it needs.
fn install_cmd(t: &Tool) -> Option<(&'static str, Cmd)> {
    if cfg!(windows) {
        Some((
            "winget",
            Cmd::new(
                "winget",
                &[
                    "install",
                    "--id",
                    t.winget_id,
                    "--exact",
                    "--source",
                    "winget",
                    "--silent",
                    "--disable-interactivity",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                ],
            ),
        ))
    } else if cfg!(target_os = "macos") {
        Some(("brew", Cmd::new("brew", &["install", t.brew_formula])))
    } else {
        None
    }
}

impl Step for InstallTool {
    const NAME: &'static str = "install-tool";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let t = find(self.tool.as_deref())?;
        let version = proc::version_of(t.program, &ctx.log);
        let mut plan = Plan {
            context: json!({ "tool": t.name, "installed": version.is_some(), "version": version }),
            ..Default::default()
        };
        if let Some(v) = version {
            plan.push(Action::skip("install", &format!("{} is already installed ({v})", t.name)));
            return Ok(plan);
        }
        let Some((manager, cmd)) = install_cmd(t) else {
            return Err(ShipError::new(
                Code::Precondition,
                format!("ship installs tools only on Windows (winget) and macOS (Homebrew); install {} from {}", t.name, t.docs),
            ));
        };
        if proc::version_of(manager, &ctx.log).is_none() {
            return Err(ShipError::new(
                Code::ToolMissing,
                format!("`{manager}` is not available; install {} from {}", t.name, t.docs),
            ));
        }
        let purpose = if manager == "winget" {
            format!(
                "download and install {} ({}) from the winget community repository, accepting its license and the source agreement; Windows may ask you to allow the installer",
                t.name, t.winget_id
            )
        } else {
            format!("download and install {} with Homebrew", t.name)
        };
        plan.run(cmd.timeout(INSTALL_TIMEOUT), &purpose);
        plan.context["manager"] = json!(manager);
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value> {
        let t = find(self.tool.as_deref())?;
        let mut out = plan.context.clone();
        let Some(cmd) = plan.exec.first() else {
            return Ok(out);
        };
        proc::run_ok(cmd, &ctx.log)?;
        // A new PATH entry only reaches processes started after the install.
        let version = proc::version_of(t.program, &ctx.log);
        out["installed"] = json!(true);
        out["version"] = json!(version);
        if version.is_none() {
            out["next"] = json!("installed; open a new terminal (or restart Claude Code) so PATH picks it up, then run `ship preflight`");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_resolve() {
        assert_eq!(find(Some("GH")).unwrap().winget_id, "GitHub.cli");
        assert_eq!(find(Some("npm")).unwrap().name, "node");
        assert_eq!(find(Some("python")).unwrap_err().code, Code::Input);
        assert_eq!(find(None).unwrap_err().code, Code::Input);
    }

    #[test]
    fn winget_install_is_exact_and_quiet() {
        if !cfg!(windows) {
            return;
        }
        let (_, cmd) = install_cmd(find(Some("uv")).unwrap()).unwrap();
        let args = cmd.args.join(" ");
        assert!(args.contains("--id astral-sh.uv --exact --source winget"));
        assert!(args.contains("--disable-interactivity"));
    }

    #[test]
    fn installed_tool_is_skipped() {
        let ctx = Ctx { dir: ".".into(), flags: Default::default(), log: crate::log::Logger::null() };
        let plan = InstallTool { tool: Some("git".into()) }.plan(&ctx).unwrap();
        assert!(plan.is_noop());
        assert_eq!(plan.context["installed"], true);
    }
}
