//! `ship login <tool>`: open the tool's own login in a visible window and wait
//! for the user to finish it there. ship never sees what the user types or
//! pastes, never reads the stored credential, and skips tools already logged in.

use super::preflight::gh_accounts;
use super::{Ctx, Step};
use crate::error::{Code, Result, ShipError};
use crate::gate::{Action, Plan};
use crate::log::Logger;
use crate::proc::{self, Cmd};
use crate::registry::{crates_io::CratesIo, npm::Npm, pypi::PyPi, Registry};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

/// How long ship waits for the login window to close.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default, PartialEq, clap::Args, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Login {
    /// gh, npm, cargo (crates.io) or pypi.
    #[arg(value_name = "TOOL")]
    pub tool: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    GitHub,
    Npm,
    CratesIo,
    PyPi,
}

impl Service {
    fn parse(tool: Option<&str>) -> Result<Service> {
        let tool = tool.ok_or_else(|| ShipError::new(Code::Input, "name the tool to log in to: gh, npm, cargo or pypi"))?;
        Ok(match tool.to_ascii_lowercase().as_str() {
            "gh" | "github" => Service::GitHub,
            "npm" => Service::Npm,
            "cargo" | "crates.io" | "crates-io" => Service::CratesIo,
            "pypi" | "uv" => Service::PyPi,
            other => {
                return Err(ShipError::new(Code::Input, format!("unknown tool `{other}`; supported: gh, npm, cargo, pypi")))
            }
        })
    }

    fn id(self) -> &'static str {
        match self {
            Service::GitHub => "github",
            Service::Npm => "npm",
            Service::CratesIo => "crates.io",
            Service::PyPi => "pypi",
        }
    }

    /// The official interactive login. GitHub and npm approve in the browser;
    /// crates.io and PyPI have no browser login, so the user creates a token
    /// on the page ship opens and pastes it into the tool's own prompt.
    fn login_cmd(self) -> Cmd {
        match self {
            Service::GitHub => Cmd::new(
                "gh",
                &["auth", "login", "--hostname", "github.com", "--git-protocol", "https", "--web", "--scopes", "workflow"],
            ),
            Service::Npm => Cmd::new(&proc::node_tool("npm"), &["login", "--auth-type=web"]),
            Service::CratesIo => Cmd::new("cargo", &["login"]),
            Service::PyPi => Cmd::new("uv", &["auth", "login", "https://upload.pypi.org/legacy/", "--username", "__token__"]),
        }
    }

    /// Page where the user creates the token the prompt asks for.
    fn token_page(self) -> Option<&'static str> {
        match self {
            Service::CratesIo => Some("https://crates.io/settings/tokens/new"),
            Service::PyPi => Some("https://pypi.org/manage/account/token/"),
            Service::GitHub | Service::Npm => None,
        }
    }

    fn what_you_do(self) -> &'static str {
        match self {
            Service::GitHub => "press Enter in the window, then enter the one-time code it shows on github.com",
            Service::Npm => "press Enter in the window, then approve the login on npmjs.com",
            Service::CratesIo => "create a token with the publish-new and publish-update scopes on the page, then paste it into the window",
            Service::PyPi => "create an API token on the page, then paste it into the window when it asks for a password",
        }
    }

    /// Who is logged in now: (logged in, where the credential lives).
    fn status(self, log: &Logger) -> (bool, Option<String>) {
        let from = |s: crate::registry::LoginStatus| (s.logged_in, s.source);
        match self {
            Service::GitHub => {
                let active = gh_accounts(log).into_iter().find(|a| a.active && a.state == "success");
                (active.is_some(), active.map(|a| format!("gh: {}", a.login)))
            }
            Service::Npm => from(Npm.login_status()),
            Service::CratesIo => from(CratesIo.login_status()),
            Service::PyPi => from(PyPi.login_status()),
        }
    }
}

impl Step for Login {
    const NAME: &'static str = "login";

    fn plan(&self, ctx: &Ctx) -> Result<Plan> {
        let service = Service::parse(self.tool.as_deref())?;
        let (logged_in, source) = service.status(&ctx.log);
        let mut plan = Plan {
            context: json!({ "service": service.id(), "logged_in": logged_in, "source": source }),
            ..Default::default()
        };
        if logged_in {
            plan.push(Action::skip("login", &format!("already logged in ({})", source.as_deref().unwrap_or("found"))));
            return Ok(plan);
        }
        if let Some(url) = service.token_page() {
            plan.push(Action::OpenUrl { url: url.to_string() });
        }
        plan.run(
            service.login_cmd(),
            &format!(
                "open a window running the official login and wait for you to finish: {}. ship never sees what you enter",
                service.what_you_do()
            ),
        );
        Ok(plan)
    }

    fn execute(&self, ctx: &Ctx, plan: Plan) -> Result<Value> {
        let service = Service::parse(self.tool.as_deref())?;
        let mut out = plan.context.clone();
        let Some(cmd) = plan.exec.first() else {
            return Ok(out);
        };
        if let Some(url) = service.token_page() {
            proc::run_ok(&proc::open_url_cmd(url)?, &ctx.log)?;
        }
        let exit = proc::run_in_window(cmd, &ctx.log, LOGIN_TIMEOUT)?;
        let (logged_in, source) = service.status(&ctx.log);
        out["logged_in"] = json!(logged_in);
        out["source"] = json!(source);
        if logged_in {
            return Ok(out);
        }
        Err(match exit {
            None => ShipError::new(Code::Timeout, "the login window is still open; finish it there, then run `ship preflight` to check"),
            Some(c) => ShipError::new(
                Code::CommandFailed,
                format!("the login window closed (exit {c}) but no {} login was found; run `ship login {}` again", service.id(), self.tool.as_deref().unwrap_or_default()),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_parse() {
        assert_eq!(Service::parse(Some("GH")).unwrap(), Service::GitHub);
        assert_eq!(Service::parse(Some("crates.io")).unwrap(), Service::CratesIo);
        assert_eq!(Service::parse(Some("uv")).unwrap(), Service::PyPi);
        assert_eq!(Service::parse(Some("docker")).unwrap_err().code, Code::Input);
        assert_eq!(Service::parse(None).unwrap_err().code, Code::Input);
    }

    #[test]
    fn logins_never_take_a_secret_argument() {
        for s in [Service::GitHub, Service::Npm, Service::CratesIo, Service::PyPi] {
            let args = s.login_cmd().args.join(" ");
            for flag in ["--token", "--password", "--with-token"] {
                assert!(!args.contains(flag), "{args}");
            }
        }
    }

    #[test]
    fn token_pages_are_https() {
        for s in [Service::CratesIo, Service::PyPi] {
            assert!(proc::open_url_cmd(s.token_page().unwrap()).is_ok());
        }
    }
}
