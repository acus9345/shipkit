//! Picking runnable commands out of a README. The README is data: a line is
//! run only if it sits in a `bash`/`sh` code block, starts with the project's
//! own binary, uses no shell syntax and does not look outward or destructive.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub line: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Commands {
    /// Argument lists, the binary name first.
    pub run: Vec<Vec<String>>,
    pub skipped: Vec<Skipped>,
}

const RUNNABLE_LANGS: &[&str] = &["bash", "sh", "shell", "console", "zsh"];
const SHELL_SYNTAX: &[&str] = &["|", ">", "<", "&", ";", "`", "$(", "${"];
const RISKY_WORDS: &[&str] = &[
    "--confirm", "--force", "--yes", "-y", "publish", "push", "delete", "remove", "rm", "uninstall", "release",
    "create-repo", "deploy",
];

pub fn commands(readme: &str, bin: &str) -> Commands {
    let mut out = Commands::default();
    let mut in_block: Option<bool> = None; // Some(runnable?)
    for raw in readme.lines() {
        let line = raw.trim();
        if let Some(fence) = line.strip_prefix("```") {
            in_block = match in_block {
                Some(_) => None,
                None => Some(RUNNABLE_LANGS.contains(&fence.trim().to_ascii_lowercase().as_str())),
            };
            continue;
        }
        if in_block != Some(true) {
            continue;
        }
        let line = line.strip_prefix("$ ").unwrap_or(line).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let first = line.split_whitespace().next().unwrap_or("");
        if first != bin {
            out.skipped.push(skip(line, &format!("does not start with `{bin}`")));
            continue;
        }
        if SHELL_SYNTAX.iter().any(|s| line.contains(s)) {
            out.skipped.push(skip(line, "uses shell syntax"));
            continue;
        }
        let Some(args) = split_args(line) else {
            out.skipped.push(skip(line, "unbalanced quotes"));
            continue;
        };
        if args.iter().skip(1).any(|a| RISKY_WORDS.contains(&a.as_str())) {
            out.skipped.push(skip(line, "looks outward or destructive"));
            continue;
        }
        out.run.push(args);
    }
    out
}

fn skip(line: &str, reason: &str) -> Skipped {
    Skipped { line: line.to_string(), reason: reason.to_string() }
}

/// Split on whitespace, honouring single and double quotes. None if a quote is unclosed.
pub fn split_args(line: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has_token = true;
            }
            (None, c) if c.is_whitespace() => {
                if has_token {
                    args.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            (None, c) => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if has_token {
        args.push(cur);
    }
    Some(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    const README: &str = "# demo\n\n```bash\ncargo install demo\ndemo --help\n$ demo greet \"big world\"\ndemo publish --confirm\ndemo x | grep y\n# comment\n```\n\n```text\ndemo --version\n```\n\n```rust\ndemo::greet();\n```\n";

    #[test]
    fn picks_only_safe_lines_in_shell_blocks() {
        let c = commands(README, "demo");
        assert_eq!(c.run, vec![vec!["demo".to_string(), "--help".into()], vec!["demo".into(), "greet".into(), "big world".into()]]);
        let reasons: Vec<&str> = c.skipped.iter().map(|s| s.reason.as_str()).collect();
        assert_eq!(reasons, ["does not start with `demo`", "looks outward or destructive", "uses shell syntax"]);
    }

    #[test]
    fn splits_quotes() {
        assert_eq!(split_args("a 'b c' \"d\" ''").unwrap(), ["a", "b c", "d", ""]);
        assert!(split_args("a \"b").is_none());
    }
}
