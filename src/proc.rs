//! Running external tools (git, gh, cargo) with captured output, a timeout,
//! interactive prompts disabled, and every invocation logged.

use crate::error::{Code, Result, ShipError};
use crate::log::Logger;
use crate::redact::redact;
use serde::Serialize;
use serde_json::json;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Environment applied to every child so no tool ever stops to prompt
/// (for a login, a credential or an update) or colours its output.
const QUIET_ENV: &[(&str, &str)] = &[
    ("GH_PROMPT_DISABLED", "1"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
    ("GH_SPINNER_DISABLED", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GCM_INTERACTIVE", "never"),
    ("NO_COLOR", "1"),
    ("CLICOLOR", "0"),
    ("CARGO_TERM_COLOR", "never"),
];

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip)]
    pub timeout: Option<Duration>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
}

impl Cmd {
    pub fn new(program: &str, args: &[&str]) -> Cmd {
        Cmd {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            timeout: None,
            env: Vec::new(),
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Cmd {
        self.args.push(a.into());
        self
    }

    pub fn cwd(mut self, dir: &Path) -> Cmd {
        self.cwd = Some(dir.to_path_buf());
        self
    }

    pub fn timeout(mut self, t: Duration) -> Cmd {
        self.timeout = Some(t);
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Cmd {
        self.env.push((k.to_string(), v.to_string()));
        self
    }

    /// Shell-like rendering for plans and logs (display only, never executed).
    pub fn display(&self) -> String {
        let mut s = self.program.clone();
        for a in &self.args {
            s.push(' ');
            if a.is_empty() || a.contains(|c: char| c.is_whitespace() || c == '"') {
                s.push('"');
                s.push_str(&a.replace('"', "\\\""));
                s.push('"');
            } else {
                s.push_str(a);
            }
        }
        redact(&s)
    }
}

#[derive(Debug, Clone)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    /// Last `n` lines of stderr+stdout, redacted and without colour codes, for error reports.
    pub fn tail(&self, n: usize) -> String {
        let all = strip_ansi(&format!("{}\n{}", self.stdout.trim_end(), self.stderr.trim_end()));
        let lines: Vec<&str> = all.trim().lines().collect();
        let start = lines.len().saturating_sub(n);
        redact(&lines[start..].join("\n"))
    }
}

/// Remove ANSI colour sequences, both raw (`ESC[...m`) and caret-escaped (`^[[...m`)
/// as `gh run view --log-failed` prints them.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let raw = rest.find("\u{1b}[").map(|i| (i, 2));
        let caret = rest.find("^[[").map(|i| (i, 3));
        let next = match (raw, caret) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (a, b) => a.or(b),
        };
        let Some((i, skip)) = next else { break };
        out.push_str(&rest[..i]);
        let after = &rest[i + skip..];
        let end = after.find(|c: char| !(c.is_ascii_digit() || c == ';')).unwrap_or(after.len());
        if after[end..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[i..i + skip]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Run a command. Returns `E_TOOL_MISSING` if the program is not installed.
pub fn run(cmd: &Cmd, log: &Logger) -> Result<Output> {
    let started = Instant::now();
    let mut c = Command::new(&cmd.program);
    c.args(&cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = &cmd.cwd {
        c.current_dir(d);
    }
    for (k, v) in QUIET_ENV {
        c.env(k, v);
    }
    for (k, v) in &cmd.env {
        c.env(k, v);
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log.event("run", json!({"cmd": cmd.display(), "error": "not installed"}));
            return Err(ShipError::new(
                Code::ToolMissing,
                format!("`{}` is not installed or not on PATH", cmd.program),
            )
            .with_details(json!({"tool": cmd.program})));
        }
        Err(e) => return Err(e.into()),
    };

    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out_pipe.read_to_end(&mut b);
        b
    });
    let err_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err_pipe.read_to_end(&mut b);
        b
    });

    let deadline = started + cmd.timeout.unwrap_or(DEFAULT_TIMEOUT);
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    let stdout = String::from_utf8_lossy(&out_t.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
    let out = Output {
        code: status.and_then(|s| s.code()),
        stdout,
        stderr,
        timed_out,
    };
    log.event(
        "run",
        json!({
            "cmd": cmd.display(),
            "cwd": cmd.cwd,
            "exit": out.code,
            "timed_out": timed_out,
            "ms": started.elapsed().as_millis() as u64,
        }),
    );
    Ok(out)
}

/// Run and require success; on failure return `E_COMMAND_FAILED` with the output tail.
pub fn run_ok(cmd: &Cmd, log: &Logger) -> Result<Output> {
    let out = run(cmd, log)?;
    if out.timed_out {
        return Err(ShipError::new(Code::Timeout, format!("`{}` timed out", cmd.display()))
            .with_details(json!({"output": out.tail(40)})));
    }
    if !out.success() {
        return Err(ShipError::new(
            Code::CommandFailed,
            format!("`{}` exited with {:?}", cmd.display(), out.code),
        )
        .with_details(json!({"output": out.tail(40)})));
    }
    Ok(out)
}

/// Command that opens an https URL in the default browser. Anything other than
/// a plain https URL is refused, so page or file content can never smuggle in
/// a different program or argument.
pub fn open_url_cmd(url: &str) -> Result<Cmd> {
    let safe = url.starts_with("https://")
        && url.chars().all(|c| c.is_ascii_graphic() && !"\"'`<>|^&;$\\".contains(c));
    if !safe {
        return Err(ShipError::new(Code::Input, format!("refusing to open `{url}`: only plain https URLs are opened")));
    }
    Ok(if cfg!(windows) {
        Cmd::new("rundll32", &["url.dll,FileProtocolHandler", url])
    } else if cfg!(target_os = "macos") {
        Cmd::new("open", &[url])
    } else {
        Cmd::new("xdg-open", &[url])
    })
}

/// First line of `<tool> --version`, or None if the tool is missing or fails.
pub fn version_of(tool: &str, log: &Logger) -> Option<String> {
    let out = run(&Cmd::new(tool, &["--version"]).timeout(Duration::from_secs(20)), log).ok()?;
    if !out.success() {
        return None;
    }
    out.stdout.lines().next().map(|l| l.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_quotes_spaces() {
        let c = Cmd::new("git", &["commit", "-m", "first commit"]);
        assert_eq!(c.display(), "git commit -m \"first commit\"");
    }

    #[test]
    fn strips_colour_codes() {
        assert_eq!(strip_ansi("^[[1m^[[91merror^[[0m: x"), "error: x");
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(strip_ansi("a ^[ b [1m"), "a ^[ b [1m");
    }

    #[test]
    fn only_plain_https_urls_open() {
        assert!(open_url_cmd("https://crates.io/crates/x/settings").is_ok());
        for bad in ["http://x.io", "file:///c:/x", "https://x.io/&calc", "https://x.io/\"a", "javascript:alert(1)"] {
            assert!(open_url_cmd(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn missing_tool_is_reported() {
        let e = run(&Cmd::new("definitely-not-a-real-tool-xyz", &[]), &Logger::null()).unwrap_err();
        assert_eq!(e.code, Code::ToolMissing);
    }

    #[test]
    fn captures_output_of_cargo() {
        let out = run(&Cmd::new("cargo", &["--version"]), &Logger::null()).unwrap();
        assert!(out.success());
        assert!(out.stdout.starts_with("cargo "));
    }
}
