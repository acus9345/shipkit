//! Package registries. Steps talk only to the [`Registry`] trait, so npm and
//! PyPI can be added as new implementations without touching the steps.

pub mod crates_io;

use crate::error::{Code, Result, ShipError};
use crate::log::Logger;
use crate::proc::Cmd;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

    /// Lint, typecheck, build and test commands, in order.
    fn checks(&self, dir: &Path) -> Vec<(&'static str, Cmd)>;
    /// Command that lists the files a publish would upload.
    fn package_list_cmd(&self, dir: &Path) -> Cmd;
    /// Parse the output of [`Registry::package_list_cmd`].
    fn parse_package_list(&self, stdout: &str) -> Vec<String>;
    /// Files the registry tool adds itself; not flagged as junk.
    fn generated_package_files(&self) -> &'static [&'static str];
    /// The registry's own publish dry run.
    fn dry_run_cmd(&self, dir: &Path) -> Cmd;

    /// Files to rewrite so the project's version becomes `new`, as
    /// (relative path, full new content). Nothing is written here.
    fn set_version(&self, dir: &Path, info: &ProjectInfo, new: &str) -> Result<Vec<(String, String)>>;
    /// Values connecting `owner/repo`'s `workflow` to the package without stored tokens.
    fn trusted_publishing(&self, package: &str, owner: &str, repo: &str, workflow: &str) -> TrustedPublishing;

    /// Publish from the terminal with the existing login (never a pasted token).
    fn publish_cmd(&self, dir: &Path, allow_dirty: bool) -> Cmd;
    /// Whether publish output means the registry rate-limited us (HTTP 429).
    fn is_rate_limited(&self, output: &str) -> bool;
    /// Public page of one version.
    fn package_url(&self, name: &str, version: &str) -> String;
    /// Install `name@version` from the real registry into `root`.
    fn install_cmd(&self, name: &str, version: &str, root: &Path) -> Cmd;
    /// Where [`Registry::install_cmd`] puts executable `bin`.
    fn installed_bin(&self, root: &Path, bin: &str) -> PathBuf;
    /// Commands, run in order inside the empty folder `dir`, proving a library
    /// version resolves and builds as a dependency.
    fn library_smoke(&self, name: &str, version: &str, dir: &Path) -> Vec<Cmd>;
    /// Release build of a source checkout.
    fn build_release_cmd(&self, dir: &Path) -> Cmd;
    /// Where [`Registry::build_release_cmd`] puts executable `bin`.
    fn built_bin(&self, dir: &Path, bin: &str) -> PathBuf;

    fn ci_workflow(&self, w: &WorkflowSpec) -> String;
    fn release_workflow(&self, w: &WorkflowSpec) -> String;
}

pub fn by_id(id: &str) -> Result<Box<dyn Registry>> {
    match id.to_ascii_lowercase().as_str() {
        "crates.io" | "crates-io" | "cratesio" | "cargo" | "rust" => Ok(Box::new(crates_io::CratesIo)),
        other => Err(ShipError::new(
            Code::Input,
            format!("unknown registry `{other}`; supported: crates.io"),
        )),
    }
}

/// Pick the registry from `--registry`, else from the manifest in `dir`, else crates.io.
pub fn resolve(explicit: Option<&str>, dir: &Path) -> Result<Box<dyn Registry>> {
    if let Some(id) = explicit {
        return by_id(id);
    }
    let all: Vec<Box<dyn Registry>> = vec![Box::new(crates_io::CratesIo)];
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
        assert_eq!(by_id("npm").err().unwrap().code, Code::Input);
    }

    #[test]
    fn target_flags() {
        assert!(!Target::Repo.has_package());
        assert!(Target::Both.has_package() && Target::Both.has_repo());
        assert!(!Target::Package.has_repo());
    }
}
