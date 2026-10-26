//! Argument parsing, JSON input merging and the top-level dispatch.

use crate::error::{Code, Result, ShipError};
use crate::gate::{self, Flags};
use crate::log::Logger;
use crate::output;
use crate::steps::{self, Ctx, Step, StepOutcome};
use clap::{Parser, Subcommand};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "ship",
    version,
    about = "Take a project from a local folder to a GitHub repo and a registry release, safely.",
    long_about = "Take a project from a local folder to a GitHub repo and a registry release, safely.\n\n\
                  Every command prints one JSON object. Public or irreversible steps \
                  (create-repo, release, publish) and install-tool are refused unless --confirm is passed; \
                  every step accepts --dry-run to print exactly what it would do."
)]
pub struct Cli {
    /// Project directory (default: current directory).
    #[arg(long, global = true, value_name = "PATH")]
    pub dir: Option<PathBuf>,

    /// Allow a public or irreversible step to run.
    #[arg(long, global = true)]
    pub confirm: bool,

    /// Print exactly what would happen and change nothing.
    #[arg(long = "dry-run", global = true)]
    pub dry_run: bool,

    /// Step arguments as a JSON object: inline `{...}`, a file path, or `-` for stdin.
    /// Cannot be combined with step flags. --confirm/--dry-run are never read from JSON.
    #[arg(long, global = true, value_name = "JSON|FILE|-")]
    pub input: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(flatten)]
    Step(StepCmd),
    /// Claude Code hook entry point: reads the hook event JSON on stdin.
    Hook {
        /// Hook event (only `pre-tool-use` is handled).
        event: String,
    },
    /// Offline self-test of gating, the hook, redaction and secret scanning.
    SelfTest,
}

/// Declares the step subcommands once: the clap enum, each step's name, and dispatch.
macro_rules! step_commands {
    ($($(#[doc = $doc:literal])* $variant:ident($ty:ty),)*) => {
        #[derive(Debug, Subcommand)]
        pub enum StepCmd {
            $($(#[doc = $doc])* $variant($ty),)*
        }

        impl StepCmd {
            pub fn name(&self) -> &'static str {
                match self {
                    $(StepCmd::$variant(_) => <$ty as Step>::NAME,)*
                }
            }
        }

        fn dispatch(cmd: StepCmd, input: Option<&str>, ctx: &Ctx) -> Result<StepOutcome> {
            match cmd {
                $(StepCmd::$variant(a) => steps::run(&merge_input(a, input)?, ctx),)*
            }
        }
    };
}

step_commands! {
    /// Check git, gh and the registry CLI, logged-in accounts and git identity. Run this first.
    Preflight(steps::preflight::Preflight),
    /// Validate the package name and check it and the GitHub repo name are free.
    CheckNames(steps::check_names::CheckNames),
    /// Create a standard project; never overwrites existing files.
    Scaffold(steps::scaffold::Scaffold),
    /// Look for .env files, private keys and tokens in everything that would be committed.
    ScanSecrets(steps::scan_secrets::ScanSecrets),
    /// Run lint, build and test, stopping at the first failure.
    Checks(steps::checks::Checks),
    /// Registry dry run: list files that would be published, flag junk and secrets.
    Inspect(steps::inspect::Inspect),
    /// git init (main) if needed, refuse on secrets or a failed dry run, then commit.
    Commit(steps::commit::Commit),
    /// [gated] Create the GitHub repo and push, or push if it is already origin.
    CreateRepo(steps::create_repo::CreateRepo),
    /// Write ci.yml and release.yml (trusted publishing, no stored tokens).
    AddWorkflows(steps::add_workflows::AddWorkflows),
    /// Open the registry's trusted-publishing page and print the exact values to enter.
    TrustedPublishing(steps::trusted_publishing::TrustedPublishing),
    /// Bump the manifest version and record it in CHANGELOG.md. Does not commit.
    Bump(steps::bump::Bump),
    /// [gated] Tag v<version>, push branch and tag, create the GitHub release.
    Release(steps::release::Release),
    /// Wait (about 45 s per call) for this commit's workflow runs; failing logs on failure.
    Watch(steps::watch::Watch),
    /// Install from the real registry (or clone) in a temp folder and run the README's commands.
    Verify(steps::verify::Verify),
    /// [gated] Publish from the terminal; skipped if the version exists; 24 h cooldown after a 429.
    Publish(steps::publish::Publish),
    /// Open the official login for gh, npm, cargo or pypi in a window; you finish it there.
    Login(steps::login::Login),
    /// [gated] Install gh, git, node or uv with winget (Windows) or Homebrew (macOS).
    InstallTool(steps::install_tool::InstallTool),
}

/// Use `--input` JSON if given, else the parsed flags. Mixing both is an error
/// so there is never doubt about which value won.
pub fn merge_input<T>(flags: T, input: Option<&str>) -> Result<T>
where
    T: DeserializeOwned + Default + PartialEq,
{
    let Some(src) = input else { return Ok(flags) };
    if flags != T::default() {
        return Err(ShipError::new(Code::Input, "use either --input or step flags, not both"));
    }
    let text = if src.trim_start().starts_with('{') {
        src.to_string()
    } else if src == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(src)
            .map_err(|e| ShipError::new(Code::Input, format!("cannot read --input file `{src}`: {e}")))?
    };
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| ShipError::new(Code::Input, format!("--input is not valid JSON: {e}")))?;
    if !value.is_object() {
        return Err(ShipError::new(Code::Input, "--input must be a JSON object"));
    }
    serde_json::from_value(value).map_err(|e| ShipError::new(Code::Input, format!("invalid --input: {e}")))
}

/// Parse `argv`, run one step, print one JSON object, return the exit code.
pub fn main_entry<I, T>(argv: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = match Cli::try_parse_from(argv) {
        Ok(c) => c,
        Err(e) => return usage_error(e),
    };
    let step_cmd = match cli.command {
        Command::Step(s) => s,
        Command::Hook { event } => {
            let mut stdin = String::new();
            let _ = std::io::stdin().read_to_string(&mut stdin);
            let log = Logger::open("hook");
            if let Some(decision) = crate::hook::handle(&event, &stdin, &log) {
                println!("{}", output::render(&decision));
            }
            return 0;
        }
        Command::SelfTest => {
            let log = Logger::open("self-test");
            let (code, v) = match crate::selftest::run(&log) {
                Ok(data) => (0, output::ok_envelope("self-test", gate::Risk::Low, false, data)),
                Err(e) => (e.code.exit_code(), output::err_envelope(Some("self-test"), &e)),
            };
            println!("{}", output::render(&v));
            return code;
        }
    };
    let step = step_cmd.name();
    let dir = match &cli.dir {
        Some(d) if d.is_absolute() => d.clone(),
        Some(d) => std::env::current_dir().map(|c| c.join(d)).unwrap_or_else(|_| d.clone()),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let ctx = Ctx {
        dir,
        flags: Flags { confirm: cli.confirm, dry_run: cli.dry_run },
        log: Logger::open(step),
    };
    ctx.log.event(
        "start",
        json!({ "dir": ctx.dir, "confirm": cli.confirm, "dry_run": cli.dry_run, "input": cli.input.is_some() }),
    );
    let risk = gate::risk_of(step).expect("step listed in gate::STEPS or gate::HELPERS");
    match dispatch(step_cmd, cli.input.as_deref(), &ctx) {
        Ok(out) => {
            ctx.log.event("ok", json!({ "dry_run": out.dry_run }));
            println!("{}", output::render(&output::ok_envelope(step, risk, out.dry_run, out.data)));
            0
        }
        Err(e) => {
            ctx.log.event("error", e.to_json());
            println!("{}", output::render(&output::err_envelope(Some(step), &e)));
            e.code.exit_code()
        }
    }
}

fn usage_error(e: clap::Error) -> i32 {
    use clap::error::ErrorKind;
    match e.kind() {
        // --help and --version stay human-readable.
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            print!("{e}");
            0
        }
        _ => {
            let steps: Vec<_> = gate::STEPS
                .iter()
                .map(|(s, r)| json!({ "step": s, "risk": r }))
                .collect();
            let msg = e.to_string();
            let first = msg.lines().next().unwrap_or("invalid arguments").trim_start_matches("error: ");
            let err = ShipError::new(Code::Usage, first).with_details(json!({ "usage": msg, "steps": steps }));
            println!("{}", output::render(&output::err_envelope(None, &err)));
            Code::Usage.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_and_flags_are_exclusive() {
        let flags = steps::preflight::Preflight { registry: Some("crates.io".into()) };
        let e = merge_input(flags, Some("{}")).unwrap_err();
        assert_eq!(e.code, Code::Input);
    }

    #[test]
    fn inline_input_is_parsed() {
        let p: steps::preflight::Preflight = merge_input(steps::preflight::Preflight::default(), Some(r#"{"registry":"crates.io"}"#)).unwrap();
        assert_eq!(p.registry.as_deref(), Some("crates.io"));
    }

    #[test]
    fn unknown_input_fields_rejected() {
        let e = merge_input(steps::preflight::Preflight::default(), Some(r#"{"confirm":true}"#)).unwrap_err();
        assert_eq!(e.code, Code::Input);
        let e = merge_input(steps::preflight::Preflight::default(), Some("[1]")).unwrap_err();
        assert_eq!(e.code, Code::Input);
    }

    #[test]
    fn no_input_keeps_flags() {
        let p = merge_input(steps::preflight::Preflight { registry: Some("x".into()) }, None).unwrap();
        assert_eq!(p.registry.as_deref(), Some("x"));
    }

    #[test]
    fn every_subcommand_is_in_the_risk_table() {
        use clap::CommandFactory;
        for sub in Cli::command().get_subcommands() {
            assert!(gate::risk_of(sub.get_name()).is_some(), "{}", sub.get_name());
        }
    }
}
