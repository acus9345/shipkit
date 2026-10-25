//! Package registries. Steps talk only to the [`Registry`] trait, so npm and
//! PyPI can be added as new implementations without touching the steps.

pub mod crates_io;
pub mod npm;
pub mod pypi;
pub mod workflows;

use crate::error::{Code, Result, ShipError};
use crate::log::Logger;
use crate::proc::Cmd;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Local, offline view of the registry login. Never contains a secret.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LoginStatus {
    pub logged_in: bool,
    /// Where the credential lives (e.g. `env:CARGO_REGISTRY_TOKEN`, `credentials.toml`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What a project is shipped as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// A GitHub repository only; the manifest blocks registry publishing.
    Repo,
    /// A registry package only.
    Package,
    /// Both.
    #[default]
    Both,
}

impl Target {
    pub fn has_package(self) -> bool {
        self != Target::Repo
    }
    pub fn has_repo(self) -> bool {
        self != Target::Package
    }
}

/// Facts read from an existing manifest.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectInfo {
    pub name: String,
    pub version: String,
    /// False when the manifest blocks publishing (repo-only projects).
    pub publishable: bool,
    /// Name of the executable, if the project builds one.
    pub bin: Option<String>,
    pub repository: Option<String>,
    pub description: Option<String>,
}

/// Inputs for generating a new project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProject {
    pub name: String,
    pub description: String,
    /// `Name <email>` from git config.
    pub author: Option<String>,
    pub repo_url: Option<String>,
    pub target: Target,
    pub cli: bool,
    pub private: bool,
    pub year: String,
    pub date: String,
}

/// Inputs for generating CI and release workflows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSpec {
    pub name: String,
    /// Private repos run on Ubuntu only to save paid minutes.
    pub private: bool,
    /// Whether the release workflow publishes to the registry.
    pub publish: bool,
    /// Executable to build and attach to the GitHub release.
    pub bin: Option<String>,
}

/// What to enter on the registry's trusted-publishing settings page.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TrustedPublishing {
    pub settings_url: String,
    /// Field label and exact value, in the order the form shows them.
    pub fields: Vec<(String, String)>,
    /// The registry only accepts a trusted publisher for a package that exists.
    pub needs_existing_package: bool,
    pub instructions: String,
}

/// Marker in a generated release workflow meaning "this workflow creates the
/// GitHub release itself", so `ship release` must not.
pub const RELEASE_MARKER: &str = "# ship:creates-github-release";

pub trait Registry {
    /// Stable id used in JSON (`crates.io`).
    fn id(&self) -> &'static str;
    /// The command-line tool that talks to this registry (`cargo`).
    fn cli(&self) -> &'static str;
    /// Manifest file that marks a project for this registry (`Cargo.toml`).
    fn manifest(&self) -> &'static str;
    /// Whether a credential is configured locally. Reads key names only, never values.
    fn login_status(&self) -> LoginStatus;

    /// Registry naming rules. `Err` carries a human-readable reason.
    fn validate_name(&self, name: &str) -> std::result::Result<(), String>;
    /// Closest valid name to `raw` (e.g. a folder name).
    fn sanitize_name(&self, raw: &str) -> String;
    /// Network: is `name` unclaimed on the registry?
    fn name_available(&self, name: &str, log: &Logger) -> Result<bool>;
    /// Network: does `name@version` already exist on the registry?
    fn version_exists(&self, name: &str, version: &str, log: &Logger) -> Result<bool>;

    /// Read the manifest in `dir`.
    fn project_info(&self, dir: &Path) -> Result<ProjectInfo>;
    /// Language files for a new project: manifest, sources, tests.
    fn new_project_files(&self, p: &NewProject) -> Vec<(String, String)>;
    /// Lines for `.gitignore`.
    fn gitignore(&self) -> &'static str;
    /// Markdown for the README's install section.
    fn install_doc(&self, p: &NewProject) -> String;
    /// Markdown for the README's usage section (code blocks included).
    fn usage_doc(&self, p: &NewProject) -> String;

    /// Tools `preflight` checks, as (name, version command).
    fn tool_checks(&self) -> Vec<(&'static str, Cmd)> {
        vec![(self.cli(), Cmd::new(self.cli(), &["--version"]))]
    }
    /// The public URL `check-names` queries, for plans.
    fn name_lookup_url(&self, name: &str) -> String;

    /// Lint, typecheck, build and test commands, in order.
    fn checks(&self, dir: &Path) -> Vec<(&'static str, Cmd)>;
    /// Commands `package_files` runs, for plans.
    fn package_files_plan(&self, dir: &Path) -> Vec<Cmd>;
    /// The files a publish would upload, relative to `dir`.
    fn package_files(&self, dir: &Path, log: &Logger) -> Result<Vec<String>>;
    /// Files the registry tool adds itself; not flagged as junk.
    fn generated_package_files(&self) -> &'static [&'static str];
    /// The registry's own publish dry run, run in order.
    fn dry_run_cmds(&self, dir: &Path) -> Vec<Cmd>;
    /// Temporary build folders the dry run or publish commands create, removed afterwards.
    fn scratch_dirs(&self, _dir: &Path, _version: &str) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    /// Files to rewrite so the project's version becomes `new`, as
    /// (relative path, full new content). Nothing is written here.
    fn set_version(&self, dir: &Path, info: &ProjectInfo, new: &str) -> Result<Vec<(String, String)>>;
    /// Values connecting `owner/repo`'s `workflow` to the package without stored
    /// tokens. `exists` says whether the package is already on the registry.
    fn trusted_publishing(&self, package: &str, owner: &str, repo: &str, workflow: &str, exists: bool) -> TrustedPublishing;

    /// Publish from the terminal with the existing login (never a pasted token).
    fn publish_cmds(&self, dir: &Path, version: &str, allow_dirty: bool) -> Vec<Cmd>;
    /// Whether publish output means the registry rate-limited us (HTTP 429).
    fn is_rate_limited(&self, output: &str) -> bool;
    /// Public page of one version.
    fn package_url(&self, name: &str, version: &str) -> String;
    /// Install `name@version` from the real registry into the empty folder `root`.
    fn install_cmds(&self, name: &str, version: &str, root: &Path) -> Vec<Cmd>;
    /// Command that runs executable `bin` after [`Registry::install_cmds`].
    fn installed_bin_cmd(&self, root: &Path, bin: &str) -> Cmd;
    /// Commands, run in order inside the empty folder `dir`, proving a library
    /// version resolves and loads as a dependency.
    fn library_smoke(&self, name: &str, version: &str, dir: &Path) -> Vec<Cmd>;
    /// Build a fresh source checkout so its executable can run.
    fn build_release_cmds(&self, dir: &Path) -> Vec<Cmd>;
    /// Command that runs executable `bin` after [`Registry::build_release_cmds`].
    fn built_bin_cmd(&self, dir: &Path, bin: &str) -> Cmd;

    fn ci_workflow(&self, w: &WorkflowSpec) -> String;
    fn release_workflow(&self, w: &WorkflowSpec) -> String;
}

pub fn by_id(id: &str) -> Result<Box<dyn Registry>> {
    match id.to_ascii_lowercase().as_str() {
        "crates.io" | "crates-io" | "cratesio" | "cargo" | "rust" => Ok(Box::new(crates_io::CratesIo)),
        "npm" | "npmjs" | "node" | "javascript" | "js" => Ok(Box::new(npm::Npm)),
        "pypi" | "python" | "pip" | "uv" => Ok(Box::new(pypi::PyPi)),
        other => Err(ShipError::new(
            Code::Input,
            format!("unknown registry `{other}`; supported: crates.io, npm, pypi"),
        )),
    }
}

/// Pick the registry from `--registry`, else from the manifest in `dir`, else crates.io.
pub fn resolve(explicit: Option<&str>, dir: &Path) -> Result<Box<dyn Registry>> {
    if let Some(id) = explicit {
        return by_id(id);
    }
    let all: Vec<Box<dyn Registry>> = vec![Box::new(crates_io::CratesIo), Box::new(npm::Npm), Box::new(pypi::PyPi)];
    let mut found = all.into_iter().filter(|r| dir.join(r.manifest()).is_file());
    Ok(found.next().unwrap_or_else(|| Box::new(crates_io::CratesIo)))
}

/// Like [`resolve`] but requires the manifest to exist.
pub fn resolve_existing(explicit: Option<&str>, dir: &Path) -> Result<(Box<dyn Registry>, ProjectInfo)> {
    let reg = resolve(explicit, dir)?;
    if !dir.join(reg.manifest()).is_file() {
        return Err(ShipError::new(
            Code::NotFound,
            format!("no {} in {}; run `ship scaffold` first", reg.manifest(), dir.display()),
        ));
    }
    let info = reg.project_info(dir)?;
    Ok((reg, info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve() {
        for id in ["crates.io", "crates-io", "Cargo"] {
            assert_eq!(by_id(id).unwrap().id(), "crates.io");
        }
        assert_eq!(by_id("npm").unwrap().id(), "npm");
        assert_eq!(by_id("Python").unwrap().id(), "pypi");
        assert_eq!(by_id("maven").err().unwrap().code, Code::Input);
    }

    #[test]
    fn target_flags() {
        assert!(!Target::Repo.has_package());
        assert!(Target::Both.has_package() && Target::Both.has_repo());
        assert!(!Target::Package.has_repo());
    }
}
