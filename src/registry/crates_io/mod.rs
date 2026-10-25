//! crates.io via `cargo` (and `curl` for read-only API lookups).

mod templates;

use super::{LoginStatus, NewProject, ProjectInfo, Registry, TrustedPublishing, WorkflowSpec};
use crate::error::{Code, Result, ShipError};
use crate::log::{home_dir, Logger};
use crate::proc::{self, Cmd};
use crate::project::toml_lite;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct CratesIo;

const API: &str = "https://crates.io/api/v1/crates";
const USER_AGENT: &str = concat!("shipkit/", env!("CARGO_PKG_VERSION"), " (+https://github.com/acus9345/shipkit)");
const LONG: Duration = Duration::from_secs(30 * 60);

/// Names crates.io refuses (Windows device names and a few reserved crate names).
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
    "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "std", "core", "alloc",
    "proc_macro", "proc-macro", "test", "rustc", "cargo",
];

impl Registry for CratesIo {
    fn id(&self) -> &'static str {
        "crates.io"
    }

    fn cli(&self) -> &'static str {
        "cargo"
    }

    fn manifest(&self) -> &'static str {
        "Cargo.toml"
    }

    fn login_status(&self) -> LoginStatus {
        if std::env::var_os("CARGO_REGISTRY_TOKEN").is_some_and(|v| !v.is_empty()) {
            return LoginStatus {
                logged_in: true,
                source: Some("env:CARGO_REGISTRY_TOKEN".into()),
                note: None,
            };
        }
        let Some(home) = cargo_home() else {
            return LoginStatus { logged_in: false, source: None, note: Some("cannot locate CARGO_HOME".into()) };
        };
        for name in ["credentials.toml", "credentials"] {
            if let Ok(text) = std::fs::read_to_string(home.join(name)) {
                if has_key_in_section(&text, "registry", "token") {
                    return LoginStatus { logged_in: true, source: Some(name.into()), note: None };
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(home.join("config.toml")) {
            if has_key_in_section(&text, "registry", "global-credential-providers") {
                return LoginStatus {
                    logged_in: false,
                    source: Some("credential-provider".into()),
                    note: Some("a cargo credential provider is configured; its login state cannot be read offline".into()),
                };
            }
        }
        LoginStatus {
            logged_in: false,
            source: None,
            note: Some("no crates.io credential found; run `cargo login` yourself, or publish via trusted publishing from CI".into()),
        }
    }

    fn validate_name(&self, name: &str) -> std::result::Result<(), String> {
        if name.is_empty() {
            return Err("name is empty".into());
        }
        if name.chars().count() > 64 {
            return Err("name is longer than 64 characters".into());
        }
        if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return Err("name must start with an ASCII letter".into());
        }
        if let Some(c) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_')) {
            return Err(format!("name contains `{c}`; only ASCII letters, digits, `-` and `_` are allowed"));
        }
        if RESERVED.contains(&name.to_ascii_lowercase().as_str()) {
            return Err("name is reserved on crates.io".into());
        }
        Ok(())
    }

    fn sanitize_name(&self, raw: &str) -> String {
        let mut s = String::new();
        for c in raw.trim().chars() {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '_' {
                s.push(c);
            } else if !s.ends_with('-') {
                s.push('-');
            }
        }
        let mut s = s.trim_matches('-').to_string();
        if !s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            s = format!("rs-{s}").trim_end_matches('-').to_string();
        }
        if RESERVED.contains(&s.as_str()) {
            s.push_str("-rs");
        }
        s.chars().take(64).collect::<String>().trim_end_matches('-').to_string()
    }

    fn name_available(&self, name: &str, log: &Logger) -> Result<bool> {
        // crates.io matches names case-insensitively and treats `-` and `_` as equal.
        Ok(api_status(&format!("{API}/{name}"), log)? == 404)
    }

    fn version_exists(&self, name: &str, version: &str, log: &Logger) -> Result<bool> {
        Ok(api_status(&format!("{API}/{name}/{version}"), log)? == 200)
    }

    fn project_info(&self, dir: &Path) -> Result<ProjectInfo> {
        let path = dir.join("Cargo.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| ShipError::new(Code::NotFound, format!("cannot read {}: {e}", path.display())))?;
        if toml_lite::get_raw(&text, "workspace", "members").is_some() && toml_lite::get_raw(&text, "package", "name").is_none() {
            return Err(ShipError::new(Code::Precondition, "Cargo workspaces without a root package are not supported yet"));
        }
        let name = toml_lite::get_str(&text, "package", "name")
            .ok_or_else(|| ShipError::new(Code::Precondition, "Cargo.toml has no [package] name"))?;
        let raw_version = toml_lite::get_raw(&text, "package", "version");
        if raw_version.is_some_and(|v| v.contains("workspace")) || toml_lite::get_raw(&text, "package", "version.workspace").is_some() {
            return Err(ShipError::new(Code::Precondition, "workspace-inherited versions are not supported yet"));
        }
        let version = raw_version.and_then(toml_lite::unquote);
        let publish = toml_lite::get_raw(&text, "package", "publish").map(|v| v.replace(' ', ""));
        let publishable = version.is_some() && !matches!(publish.as_deref(), Some("false") | Some("[]"));
        let bin = toml_lite::get_str(&text, "bin", "name")
            .or_else(|| dir.join("src").join("main.rs").is_file().then(|| name.clone()));
        Ok(ProjectInfo {
            version: version.unwrap_or_else(|| "0.0.0".into()),
            publishable,
            bin,
            repository: toml_lite::get_str(&text, "package", "repository"),
            description: toml_lite::get_str(&text, "package", "description"),
            name,
        })
    }

    fn new_project_files(&self, p: &NewProject) -> Vec<(String, String)> {
        templates::project_files(p)
    }

    fn gitignore(&self) -> &'static str {
        "/target\n"
    }

    fn install_doc(&self, p: &NewProject) -> String {
        templates::install_doc(p)
    }

    fn checks(&self, dir: &Path) -> Vec<(&'static str, Cmd)> {
        let c = |args: &[&str]| Cmd::new("cargo", args).cwd(dir).timeout(LONG);
        vec![
            ("lint", c(&["clippy", "--all-targets", "--", "-D", "warnings"])),
            ("build", c(&["build", "--all-targets"])),
            ("test", c(&["test"])),
        ]
    }

    fn package_list_cmd(&self, dir: &Path) -> Cmd {
        Cmd::new("cargo", &["package", "--list", "--allow-dirty"]).cwd(dir).timeout(LONG)
    }

    fn parse_package_list(&self, stdout: &str) -> Vec<String> {
        stdout
            .lines()
            .map(|l| l.trim().replace('\\', "/"))
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn generated_package_files(&self) -> &'static [&'static str] {
        &["Cargo.toml.orig", ".cargo_vcs_info.json", "Cargo.lock"]
    }

    fn dry_run_cmd(&self, dir: &Path) -> Cmd {
        Cmd::new("cargo", &["publish", "--dry-run", "--allow-dirty"]).cwd(dir).timeout(LONG)
    }

    fn set_version(&self, dir: &Path, info: &ProjectInfo, new: &str) -> Result<Vec<(String, String)>> {
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml"))?;
        let updated = toml_lite::set_str(&manifest, "package", "version", new)
            .ok_or_else(|| ShipError::new(Code::Precondition, "Cargo.toml has no plain `version = \"...\"` line in [package]"))?;
        let mut files = vec![("Cargo.toml".to_string(), updated)];
        if let Ok(lock) = std::fs::read_to_string(dir.join("Cargo.lock")) {
            if let Some(l) = set_lock_version(&lock, &info.name, &info.version, new) {
                files.push(("Cargo.lock".to_string(), l));
            }
        }
        Ok(files)
    }

    fn trusted_publishing(&self, package: &str, owner: &str, repo: &str, workflow: &str) -> TrustedPublishing {
        TrustedPublishing {
            settings_url: format!("https://crates.io/crates/{package}/settings"),
            fields: vec![
                ("Publisher".into(), "GitHub".into()),
                ("Repository owner".into(), owner.into()),
                ("Repository name".into(), repo.into()),
                ("Workflow filename".into(), workflow.into()),
                ("Environment name".into(), "(leave empty)".into()),
            ],
            needs_existing_package: true,
            instructions: "Sign in to crates.io, open the crate's Settings page, choose \"Add\" under Trusted Publishing, enter the values exactly, then click Save.".into(),
        }
    }

    fn publish_cmd(&self, dir: &Path, allow_dirty: bool) -> Cmd {
        let mut c = Cmd::new("cargo", &["publish"]).cwd(dir).timeout(LONG);
        if allow_dirty {
            c = c.arg("--allow-dirty");
        }
        c
    }

    fn is_rate_limited(&self, output: &str) -> bool {
        let o = output.to_ascii_lowercase();
        o.contains("429") || o.contains("too many requests") || o.contains("rate limit") || o.contains("published too many")
    }

    fn package_url(&self, name: &str, version: &str) -> String {
        format!("https://crates.io/crates/{name}/{version}")
    }

    fn install_cmd(&self, name: &str, version: &str, root: &Path) -> Cmd {
        let root_s = root.to_string_lossy().into_owned();
        Cmd::new("cargo", &["install", name, "--version", &format!("={version}"), "--root", &root_s])
            .cwd(root)
            .timeout(LONG)
    }

    fn installed_bin(&self, root: &Path, bin: &str) -> PathBuf {
        root.join("bin").join(format!("{bin}{}", std::env::consts::EXE_SUFFIX))
    }

    fn library_smoke(&self, name: &str, version: &str, dir: &Path) -> Vec<Cmd> {
        let c = |args: &[&str]| Cmd::new("cargo", args).cwd(dir).timeout(LONG);
        vec![
            c(&["init", "--vcs", "none", "--name", "ship-verify-probe"]),
            c(&["add", &format!("{name}@={version}")]),
            c(&["build"]),
        ]
    }

    fn build_release_cmd(&self, dir: &Path) -> Cmd {
        Cmd::new("cargo", &["build", "--release"]).cwd(dir).timeout(LONG)
    }

    fn built_bin(&self, dir: &Path, bin: &str) -> PathBuf {
        dir.join("target").join("release").join(format!("{bin}{}", std::env::consts::EXE_SUFFIX))
    }

    fn ci_workflow(&self, w: &WorkflowSpec) -> String {
        templates::ci_workflow(w)
    }

    fn release_workflow(&self, w: &WorkflowSpec) -> String {
        templates::release_workflow(w)
    }
}

/// HTTP status of a read-only crates.io API GET. 429 maps to `E_RATE_LIMITED`.
fn api_status(url: &str, log: &Logger) -> Result<u16> {
    let cmd = Cmd::new("curl", &["-s", "-o", null_device(), "-w", "%{http_code}", "--max-time", "20", "-A", USER_AGENT, url])
        .timeout(Duration::from_secs(30));
    let out = proc::run_ok(&cmd, log)?;
    let code: u16 = out.stdout.trim().parse().unwrap_or(0);
    match code {
        200 | 404 => Ok(code),
        429 => Err(ShipError::new(Code::RateLimited, "crates.io answered 429 (rate limited); try again later")),
        _ => Err(ShipError::new(Code::CommandFailed, format!("crates.io API returned HTTP {code} for {url}"))),
    }
}

/// Update this package's entry in Cargo.lock, keeping everything else.
fn set_lock_version(lock: &str, name: &str, old: &str, new: &str) -> Option<String> {
    let needle = format!("name = \"{name}\"\nversion = \"{old}\"\n");
    let norm = lock.replace("\r\n", "\n");
    if !norm.contains(&needle) {
        return None;
    }
    let out = norm.replacen(&needle, &format!("name = \"{name}\"\nversion = \"{new}\"\n"), 1);
    Some(if lock.contains("\r\n") { out.replace('\n', "\r\n") } else { out })
}

fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".cargo")))
}

/// True if `key = ...` appears inside `[section]`. Only key names are
/// inspected; the value is never read into a variable.
pub(crate) fn has_key_in_section(toml: &str, section: &str, key: &str) -> bool {
    let mut current = String::new();
    for raw in toml.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            current = line.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        if current != section {
            continue;
        }
        if let Some(rest) = line.strip_prefix(key) {
            if rest.trim_start().starts_with('=') {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_token_key_in_registry_section() {
        assert!(has_key_in_section("[registry]\ntoken = \"x\"\n", "registry", "token"));
        assert!(!has_key_in_section("[other]\ntoken = \"x\"\n", "registry", "token"));
        assert!(!has_key_in_section("[registry]\ntokens = 1\n", "registry", "token"));
        assert!(!has_key_in_section("", "registry", "token"));
    }

    #[test]
    fn lock_version_update() {
        let lock = "[[package]]\nname = \"dep\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"me\"\nversion = \"0.1.0\"\n";
        let out = set_lock_version(lock, "me", "0.1.0", "0.2.0").unwrap();
        assert!(out.contains("name = \"dep\"\nversion = \"0.1.0\""));
        assert!(out.contains("name = \"me\"\nversion = \"0.2.0\""));
        assert!(set_lock_version(lock, "other", "0.1.0", "0.2.0").is_none());
    }

    #[test]
    fn name_rules() {
        let r = CratesIo;
        assert!(r.validate_name("shipkit").is_ok());
        assert!(r.validate_name("my_crate-2").is_ok());
        assert!(r.validate_name("2fast").is_err());
        assert!(r.validate_name("has space").is_err());
        assert!(r.validate_name("nul").is_err());
        assert!(r.validate_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn sanitizes_folder_names() {
        let r = CratesIo;
        assert_eq!(r.sanitize_name("might be useful"), "might-be-useful");
        assert_eq!(r.sanitize_name("My.Cool  Tool!"), "my-cool-tool");
        assert_eq!(r.sanitize_name("42"), "rs-42");
        assert_eq!(r.sanitize_name("std"), "std-rs");
        for raw in ["might be useful", "My.Cool  Tool!", "42", "std", "--x--"] {
            assert!(r.validate_name(&r.sanitize_name(raw)).is_ok(), "{raw}");
        }
    }

    #[test]
    fn reads_project_info() {
        let dir = crate::project::TempDir::new("ship-ut").unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.2.3\"\npublish = false\n\n[[bin]]\nname = \"dm\"\npath = \"src/main.rs\"\n",
        )
        .unwrap();
        let info = CratesIo.project_info(dir.path()).unwrap();
        assert_eq!(info.name, "demo");
        assert_eq!(info.version, "1.2.3");
        assert!(!info.publishable);
        assert_eq!(info.bin.as_deref(), Some("dm"));

        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"w\"\nversion.workspace = true\n").unwrap();
        assert_eq!(CratesIo.project_info(dir.path()).unwrap_err().code, Code::Precondition);
    }
}
