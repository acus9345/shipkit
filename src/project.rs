//! Facts about the project folder: git state, the files that would be
//! committed, and safe file creation.

use crate::error::{Code, Result, ShipError};
use crate::log::Logger;
use crate::proc::{self, Cmd};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const GIT_TIMEOUT: Duration = Duration::from_secs(60);

pub fn git(dir: &Path, args: &[&str]) -> Cmd {
    Cmd::new("git", args).cwd(dir).timeout(GIT_TIMEOUT)
}

/// True if `dir` is the top level of a git work tree (not merely inside one).
pub fn is_git_root(dir: &Path, log: &Logger) -> bool {
    if !dir.join(".git").exists() {
        return false;
    }
    proc::run(&git(dir, &["rev-parse", "--is-inside-work-tree"]), log)
        .map(|o| o.success() && o.stdout.trim() == "true")
        .unwrap_or(false)
}

pub fn has_commits(dir: &Path, log: &Logger) -> bool {
    proc::run(&git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]), log)
        .map(|o| o.success())
        .unwrap_or(false)
}

/// Files with uncommitted changes (`git status --porcelain`), empty if not a repo.
pub fn dirty_files(dir: &Path, log: &Logger) -> Result<Vec<String>> {
    if !is_git_root(dir, log) {
        return Ok(Vec::new());
    }
    let out = proc::run_ok(&git(dir, &["status", "--porcelain", "-z", "--untracked-files=all"]), log)?;
    let mut files = Vec::new();
    let mut entries = out.stdout.split('\0').filter(|e| !e.is_empty());
    while let Some(e) = entries.next() {
        if e.len() > 3 {
            files.push(e[3..].to_string());
        }
        // Renames and copies are followed by the original path.
        if e.starts_with('R') || e.starts_with('C') {
            entries.next();
        }
    }
    Ok(files)
}

/// Every file `git add -A` would include: tracked files plus untracked files
/// not excluded by .gitignore. Works in folders that are not yet a git repo by
/// pointing git at a throwaway, empty repository in the temp directory.
pub fn committable_files(dir: &Path, log: &Logger) -> Result<Vec<String>> {
    let out = if is_git_root(dir, log) {
        proc::run_ok(
            &git(dir, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"]),
            log,
        )?
    } else {
        let tmp = TempDir::new("ship-ls")?;
        let git_dir = tmp.path().join("probe.git");
        proc::run_ok(&git(tmp.path(), &["init", "--quiet", "--bare", "probe.git"]), log)?;
        let gd = format!("--git-dir={}", git_dir.display());
        let wt = format!("--work-tree={}", dir.display());
        proc::run_ok(
            &git(dir, &[&gd, &wt, "ls-files", "-z", "--others", "--exclude-standard"]),
            log,
        )?
    };
    let mut files: Vec<String> = out
        .stdout
        .split('\0')
        .filter(|f| !f.is_empty())
        .filter(|f| dir.join(f).is_file())
        .map(str::to_string)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// Create `path` with `contents` only if it does not exist. Returns false
/// (and leaves the file untouched) if it already exists.
pub fn create_new(path: &Path, contents: &str) -> Result<bool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut f) => {
            f.write_all(contents.as_bytes())?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(ShipError::new(Code::Io, format!("cannot create {}: {e}", path.display()))),
    }
}

/// Folder name, used as the default project name.
pub fn folder_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into())
}

/// Forward-slash relative path for JSON output.
pub fn rel(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// A temporary directory removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> Result<TempDir> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let p = std::env::temp_dir().join(format!("{prefix}-{}-{nanos:x}", std::process::id()));
        std::fs::create_dir_all(&p)?;
        Ok(TempDir(p))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Keep the directory (e.g. so the user can inspect a failed verify).
    pub fn keep(self) -> PathBuf {
        let p = self.0.clone();
        std::mem::forget(self);
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Minimal reader/editor for the flat `key = value` subset of TOML used by
/// manifests. Never rewrites anything except the one line it edits.
pub mod toml_lite {
    /// Raw value text of `key` in `[section]` (`[[bin]]` sections are named `bin`;
    /// only the first such table is considered).
    pub fn get_raw<'a>(toml: &'a str, section: &str, key: &str) -> Option<&'a str> {
        let mut current = "";
        let mut seen_tables = std::collections::HashSet::new();
        let mut active = false;
        for raw in toml.lines() {
            let line = strip_comment(raw).trim();
            if line.starts_with('[') {
                current = line.trim_matches(|c| c == '[' || c == ']').trim();
                active = seen_tables.insert(current.to_string());
                continue;
            }
            if current != section || !active {
                continue;
            }
            if let Some(rest) = line.strip_prefix(key) {
                let rest = rest.trim_start();
                if let Some(v) = rest.strip_prefix('=') {
                    return Some(v.trim());
                }
            }
        }
        None
    }

    /// The first key defined in `[section]` (e.g. the first console script in
    /// `[project.scripts]`).
    pub fn first_key(toml: &str, section: &str) -> Option<String> {
        let mut current = "";
        for raw in toml.lines() {
            let line = strip_comment(raw).trim();
            if line.starts_with('[') {
                current = line.trim_matches(|c| c == '[' || c == ']').trim();
                continue;
            }
            if current == section {
                if let Some((k, _)) = line.split_once('=') {
                    let k = k.trim();
                    return Some(unquote(k).unwrap_or_else(|| k.to_string()));
                }
            }
        }
        None
    }

    /// String value of `key` in `[section]`, if it is a plain quoted string.
    pub fn get_str(toml: &str, section: &str, key: &str) -> Option<String> {
        unquote(get_raw(toml, section, key)?)
    }

    pub fn unquote(v: &str) -> Option<String> {
        let v = v.trim();
        for q in ['"', '\''] {
            if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
                return Some(v[1..v.len() - 1].to_string());
            }
        }
        None
    }

    fn strip_comment(line: &str) -> &str {
        // Good enough for manifests: a '#' outside quotes starts a comment.
        let mut in_str: Option<char> = None;
        for (i, c) in line.char_indices() {
            match (c, in_str) {
                ('"' | '\'', None) => in_str = Some(c),
                (c2, Some(q)) if c2 == q => in_str = None,
                ('#', None) => return &line[..i],
                _ => {}
            }
        }
        line
    }

    /// Replace the string value of `key` in `[section]`, keeping the rest of the
    /// file byte-for-byte. Returns None if the key is not a plain string there.
    pub fn set_str(toml: &str, section: &str, key: &str, value: &str) -> Option<String> {
        let mut current = String::new();
        let mut out = String::with_capacity(toml.len());
        let mut done = false;
        for line in toml.split_inclusive('\n') {
            let t = line.trim();
            if t.starts_with('[') {
                current = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            } else if !done && current == section {
                if let Some(rest) = t.strip_prefix(key) {
                    if rest.trim_start().starts_with('=') && unquote(rest.trim_start()[1..].trim()).is_some() {
                        let indent = &line[..line.len() - line.trim_start().len()];
                        let nl = if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" };
                        out.push_str(&format!("{indent}{key} = \"{value}\"{nl}"));
                        done = true;
                        continue;
                    }
                }
            }
            out.push_str(line);
        }
        done.then_some(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const T: &str = "[package]\nname = \"demo\" # the name\nversion = \"0.1.0\"\npublish = false\n\n[[bin]]\nname = \"d\"\n\n[[bin]]\nname = \"e\"\n";

        #[test]
        fn reads_values() {
            assert_eq!(get_str(T, "package", "name").as_deref(), Some("demo"));
            assert_eq!(get_raw(T, "package", "publish"), Some("false"));
            assert_eq!(get_str(T, "bin", "name").as_deref(), Some("d"));
            assert_eq!(get_str(T, "package", "missing"), None);
        }

        #[test]
        fn set_keeps_everything_else() {
            let out = set_str(T, "package", "version", "0.2.0").unwrap();
            assert_eq!(out, T.replace("0.1.0", "0.2.0"));
            assert!(set_str(T, "package", "publish", "x").is_none());
        }

        #[test]
        fn set_keeps_crlf() {
            let t = "[package]\r\nversion = \"1.0.0\"\r\n";
            assert_eq!(set_str(t, "package", "version", "1.0.1").unwrap(), "[package]\r\nversion = \"1.0.1\"\r\n");
        }
    }
}

/// `(name, email)` from git config as seen from `dir`.
pub fn git_identity(dir: &Path, log: &Logger) -> Option<(String, String)> {
    let get = |key: &str| {
        let cwd = if dir.is_dir() { dir } else { Path::new(".") };
        proc::run(&git(cwd, &["config", "--get", key]), log)
            .ok()
            .filter(|o| o.success())
            .map(|o| o.stdout.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Some((get("user.name")?, get("user.email")?))
}

/// Edits one string value in a JSON document without re-serializing it, so
/// key order, indentation and line endings stay exactly as they were.
pub mod json_lite {
    enum Frame {
        Obj { key: Option<String>, expecting_key: bool },
        Arr,
    }

    /// Byte range (inside the quotes) of the string value at `path`, a list of
    /// object keys from the root.
    pub fn string_span(text: &str, path: &[&str]) -> Option<(usize, usize)> {
        let bytes = text.as_bytes();
        let mut stack: Vec<Frame> = Vec::new();
        let mut keys: Vec<String> = Vec::new(); // key of each enclosing container
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'{' | b'[' => {
                    if !stack.is_empty() {
                        keys.push(match stack.last() {
                            Some(Frame::Obj { key: Some(k), .. }) => k.clone(),
                            _ => "[]".to_string(),
                        });
                    }
                    stack.push(if bytes[i] == b'{' { Frame::Obj { key: None, expecting_key: true } } else { Frame::Arr });
                }
                b'}' | b']' => {
                    stack.pop();
                    keys.pop();
                }
                b':' => {
                    if let Some(Frame::Obj { expecting_key, .. }) = stack.last_mut() {
                        *expecting_key = false;
                    }
                }
                b',' => {
                    if let Some(Frame::Obj { key, expecting_key }) = stack.last_mut() {
                        *key = None;
                        *expecting_key = true;
                    }
                }
                b'"' => {
                    let start = i + 1;
                    let mut j = start;
                    while j < bytes.len() && bytes[j] != b'"' {
                        j += if bytes[j] == b'\\' { 2 } else { 1 };
                    }
                    let s = &text[start..j.min(text.len())];
                    match stack.last_mut() {
                        Some(Frame::Obj { key, expecting_key: true }) => *key = Some(s.to_string()),
                        Some(Frame::Obj { key: Some(k), expecting_key: false }) => {
                            let here: Vec<&str> = keys.iter().map(String::as_str).chain([k.as_str()]).collect();
                            if here == path {
                                return Some((start, j));
                            }
                        }
                        _ => {}
                    }
                    i = j;
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Replace the string value at `path`. None if it isn't there.
    pub fn set_string(text: &str, path: &[&str], value: &str) -> Option<String> {
        let (s, e) = string_span(text, path)?;
        Some(format!("{}{}{}", &text[..s], value.replace('\\', "\\\\").replace('"', "\\\""), &text[e..]))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const LOCK: &str = "{\n  \"name\": \"demo\",\n  \"version\": \"0.1.0\",\n  \"lockfileVersion\": 3,\n  \"packages\": {\n    \"\": {\n      \"name\": \"demo\",\n      \"version\": \"0.1.0\"\n    },\n    \"node_modules/x\": {\n      \"version\": \"0.1.0\"\n    }\n  },\n  \"tags\": [\"a\", {\"version\": \"9\"}]\n}\n";

        #[test]
        fn finds_nested_paths_only() {
            let (s, e) = string_span(LOCK, &["version"]).unwrap();
            assert_eq!(&LOCK[s..e], "0.1.0");
            let out = set_string(LOCK, &["version"], "0.2.0").unwrap();
            let out = set_string(&out, &["packages", "", "version"], "0.2.0").unwrap();
            assert_eq!(out.matches("0.2.0").count(), 2);
            assert!(out.contains("\"node_modules/x\": {\n      \"version\": \"0.1.0\""));
            assert!(string_span(LOCK, &["missing"]).is_none());
        }

        #[test]
        fn ignores_keys_inside_strings_and_escapes() {
            let t = r#"{"description": "has \"version\": \"x\" inside", "version": "1.0.0"}"#;
            let (s, e) = string_span(t, &["version"]).unwrap();
            assert_eq!(&t[s..e], "1.0.0");
        }
    }
}
