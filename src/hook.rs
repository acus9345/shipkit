//! Claude Code PreToolUse hook: blocks any gated `ship` command (create-repo,
//! release, publish, install-tool) that lacks `--confirm`. Everything else gets no opinion,
//! so the user's normal permission rules still apply.

use crate::gate;
use crate::log::Logger;
use crate::readme::split_args;
use serde_json::{json, Value};

/// Characters that separate one shell command from the next. Treating every
/// one of them as a boundary keeps `--confirm` in one command from
/// unlocking a gated `ship` in another.
const SEPARATORS: &[char] = &['\n', ';', '|', '&', '(', ')', '`'];

/// Split on separators outside single and double quotes.
fn quote_aware_segments(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                cur.push(c);
            }
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                cur.push(c);
            }
            None if SEPARATORS.contains(&c) => out.push(std::mem::take(&mut cur)),
            None => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Why `command` must be blocked, or None to let it through. Both a
/// quote-aware and a naive split are checked, and either one can block: the
/// first sees through quoted separators (e.g. in --input JSON), the second
/// catches command substitution hidden inside double quotes.
pub fn decide(command: &str) -> Option<String> {
    let naive = command.split(SEPARATORS).map(str::to_string);
    for segment in quote_aware_segments(command).into_iter().chain(naive) {
        let segment = segment.as_str();
        let tokens = match split_args(segment) {
            Some(t) => t,
            // Unbalanced quotes: fall back to plain whitespace splitting.
            None => segment.split_whitespace().map(str::to_string).collect(),
        };
        if let Some((step, args)) = ship_invocation(&tokens) {
            if gate::is_gated(&step) && !args.iter().any(|a| a == "--confirm" || a == "--dry-run") {
                return Some(format!(
                    "`ship {step}` {}. First run it with --dry-run and show the user the plan, \
                     get their explicit OK in chat, then re-run it with --confirm.",
                    gate::gate_reason(&step)
                ));
            }
        }
    }
    None
}

fn is_ship(token: &str) -> bool {
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token).to_ascii_lowercase();
    base == "ship" || base == "ship.exe"
}

/// Global flags that take a value, so the value is not mistaken for the step.
const VALUE_FLAGS: &[&str] = &["--dir", "--input"];

/// The step name and the arguments after `ship`, if this command runs ship
/// (directly, via a path, or as `cargo run --bin ship -- ...`).
fn ship_invocation(tokens: &[String]) -> Option<(String, Vec<String>)> {
    let start = if let Some(i) = tokens.iter().position(|t| is_ship(t)) {
        i + 1
    } else if tokens.iter().any(|t| t == "cargo") && tokens.iter().any(|t| t == "run") {
        let bin_ship = tokens.windows(2).any(|w| w[0] == "--bin" && w[1] == "ship") || tokens.iter().any(|t| t == "--bin=ship");
        let dashdash = tokens.iter().position(|t| t == "--")?;
        if !bin_ship {
            return None;
        }
        dashdash + 1
    } else {
        return None;
    };
    let args: Vec<String> = tokens[start..].to_vec();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if VALUE_FLAGS.contains(&a.as_str()) {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        return Some((a.to_ascii_lowercase(), args));
    }
    None
}

/// Handle one hook event read from stdin. Returns the JSON decision to print,
/// or None for "no decision" (exit 0 with empty stdout, per Claude Code's hook
/// protocol), which leaves the user's normal permission flow in charge.
pub fn handle(event: &str, stdin: &str, log: &Logger) -> Option<Value> {
    if event != "pre-tool-use" {
        return None;
    }
    let Ok(input) = serde_json::from_str::<Value>(stdin) else {
        log.event("hook", json!({ "error": "stdin is not JSON" }));
        return None;
    };
    let command = input["tool_input"]["command"].as_str().unwrap_or("");
    match decide(command) {
        Some(reason) => {
            log.event("hook", json!({ "decision": "deny", "tool": input["tool_name"], "command": command }));
            Some(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }
            }))
        }
        // No opinion: never "allow", which would skip the user's permission prompts.
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(cmd: &str) -> bool {
        decide(cmd).is_some()
    }

    #[test]
    fn gated_steps_need_confirm() {
        for cmd in ["ship release", "ship publish", "ship create-repo --private", "ship --dir x publish", "ship --input '{}' release"] {
            assert!(blocked(cmd), "{cmd}");
        }
        for cmd in ["ship release --confirm", "ship publish --dry-run", "ship --confirm create-repo"] {
            assert!(!blocked(cmd), "{cmd}");
        }
        assert!(decide("ship install-tool uv").unwrap().contains("installs software"));
        assert!(!blocked("ship install-tool uv --confirm"));
        assert!(!blocked("ship login npm"));
    }

    #[test]
    fn other_commands_pass() {
        for cmd in ["ship preflight", "ship bump --level patch", "git push", "cargo publish", "ls"] {
            assert!(!blocked(cmd), "{cmd}");
        }
        // Conservative: a mention that merely looks like an invocation is blocked too.
        assert!(blocked("echo ship release"));
    }

    #[test]
    fn quoted_separators_cannot_split_a_command() {
        assert!(blocked(r#"ship --input '{"notes":"a;b"}' publish"#));
        assert!(!blocked(r#"ship --input '{"notes":"a;b"}' publish --confirm"#));
        assert!(blocked(r#"echo "$(ship publish)" --confirm"#));
    }

    #[test]
    fn paths_and_exe_are_recognised() {
        assert!(blocked("/c/tools/ship.exe release"));
        assert!(blocked(r#"& "C:\Program Files\ship\ship.exe" publish"#));
        assert!(blocked("SHIP_X=1 ./target/release/ship publish"));
        assert!(!blocked("./shipyard publish"));
    }

    #[test]
    fn confirm_must_be_in_the_same_command() {
        assert!(blocked("ship preflight --confirm && ship publish"));
        assert!(blocked("ship publish; echo --confirm"));
        assert!(blocked("echo ok | ship release"));
        assert!(blocked("ship commit -m \"x\" && ship release"));
    }

    #[test]
    fn quoted_confirm_is_not_a_flag() {
        assert!(blocked("ship publish \"--confirm \""));
    }

    #[test]
    fn cargo_run_of_ship_is_covered() {
        assert!(blocked("cargo run --bin ship -- publish"));
        assert!(!blocked("cargo run --bin ship -- publish --confirm"));
        assert!(!blocked("cargo run -- publish"));
    }

    #[test]
    fn hook_json() {
        let log = Logger::null();
        let deny = handle("pre-tool-use", r#"{"tool_name":"Bash","tool_input":{"command":"ship release"}}"#, &log).unwrap();
        assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(deny["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        let pass = handle("pre-tool-use", r#"{"tool_name":"Bash","tool_input":{"command":"ship release --confirm"}}"#, &log);
        assert_eq!(pass, None);
        assert_eq!(handle("pre-tool-use", "garbage", &log), None);
    }
}
