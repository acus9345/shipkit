//! PyPI via `uv` (build, publish, virtual environments) and `curl` for
//! read-only lookups on the PyPI JSON API.

use super::{workflows, LoginStatus, NewProject, ProjectInfo, Registry, Target, TrustedPublishing, WorkflowSpec, RELEASE_MARKER};
use crate::error::{Code, Result, ShipError};
use crate::http;
use crate::log::{home_dir, Logger};
use crate::proc::{self, Cmd};
use crate::project::{toml_lite, TempDir};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct PyPi;

const LONG: Duration = Duration::from_secs(30 * 60);
/// PyPI rejects uploads carrying this classifier: the standard way to mark a
/// Python project as never-publish (repo-only).
pub const PRIVATE_CLASSIFIER: &str = "Private :: Do Not Upload";

fn uv(args: &[&str], dir: &Path) -> Cmd {
    Cmd::new("uv", args).cwd(dir).timeout(LONG)
}

/// Import name for a distribution name (`my-tool` -> `my_tool`).
fn module_name(name: &str) -> String {
    name.to_ascii_lowercase().replace(['-', '.'], "_")
}

/// Where `uv build` writes for a dry run or publish: unique per `ship` run, so
/// files from another version can never be uploaded by accident.
fn out_dir(dir: &Path, tag: &str) -> PathBuf {
    let base = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    std::env::temp_dir().join(format!("ship-uv-{base}-{tag}-{}", std::process::id()))
}

fn venv_bin(venv: &Path, exe: &str) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join(format!("{exe}.exe"))
    } else {
        venv.join("bin").join(exe)
    }
}

/// Where the local PyPI credential lives, by key names only; values are never read.
/// uv's own sources come first, since `uv publish` is the default upload.
fn credential_source() -> Option<&'static str> {
    let set = |var: &str| std::env::var_os(var).is_some_and(|v| !v.is_empty());
    if set("UV_PUBLISH_TOKEN") {
        return Some("env:UV_PUBLISH_TOKEN");
    }
    if set("UV_PUBLISH_PASSWORD") {
        return Some("env:UV_PUBLISH_PASSWORD");
    }
    if uv_credentials_file().and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|t| uv_store_has_pypi(&t)) {
        return Some("uv auth login");
    }
    if set("TWINE_PASSWORD") {
        return Some("env:TWINE_PASSWORD");
    }
    let pypirc = home_dir().map(|h| h.join(".pypirc"));
    if pypirc
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|t| crate::registry::crates_io::has_key_in_section(&t, "pypi", "password"))
    {
        return Some(".pypirc");
    }
    None
}

fn is_twine_source(src: &str) -> bool {
    src == "env:TWINE_PASSWORD" || src == ".pypirc"
}

/// uv's credential store (`uv auth login`).
fn uv_credentials_file() -> Option<PathBuf> {
    let data = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join(".local").join("share")))
    }?;
    Some(data.join("uv").join("credentials").join("credentials.toml"))
}

/// Whether the store has an entry for PyPI's upload endpoint (`service` lines only).
fn uv_store_has_pypi(toml: &str) -> bool {
    toml.lines().any(|l| {
        l.trim_start()
            .strip_prefix("service")
            .is_some_and(|r| r.trim_start().starts_with('=') && r.contains("upload.pypi.org"))
    })
}

fn read_manifest(dir: &Path) -> Result<String> {
    let path = dir.join("pyproject.toml");
    std::fs::read_to_string(&path).map_err(|e| ShipError::new(Code::NotFound, format!("cannot read {}: {e}", path.display())))
}

impl Registry for PyPi {
    fn id(&self) -> &'static str {
        "pypi"
    }

    fn cli(&self) -> &'static str {
        "uv"
    }

    fn manifest(&self) -> &'static str {
        "pyproject.toml"
    }

    fn login_status(&self) -> LoginStatus {
        match credential_source() {
            Some(src) => LoginStatus { logged_in: true, source: Some(src.into()), note: None },
            None => LoginStatus {
                logged_in: false,
                source: None,
                note: Some(
                    "no local PyPI credential; `ship login pypi` stores one with `uv auth login`, but trusted publishing from CI is recommended and works even before the first release (pending publisher)"
                        .into(),
                ),
            },
        }
    }

    fn validate_name(&self, name: &str) -> std::result::Result<(), String> {
        if name.is_empty() || name.len() > 100 {
            return Err("name must be 1 to 100 characters".into());
        }
        if let Some(c) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || "-._".contains(*c))) {
            return Err(format!("name contains `{c}`; only ASCII letters, digits, `-`, `.` and `_` are allowed"));
        }
        let ends_ok = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
        if !ends_ok(name.chars().next()) || !ends_ok(name.chars().next_back()) {
            return Err("name must start and end with a letter or digit".into());
        }
        Ok(())
    }

    fn sanitize_name(&self, raw: &str) -> String {
        let mut s = String::new();
        for c in raw.trim().chars() {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                s.push(c);
            } else if !s.ends_with('-') {
                s.push('-');
            }
        }
        let s = s.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_string();
        if s.is_empty() {
            "package".into()
        } else {
            s.chars().take(100).collect()
        }
    }

    fn name_lookup_url(&self, name: &str) -> String {
        format!("https://pypi.org/pypi/{name}/json")
    }

    fn name_available(&self, name: &str, log: &Logger) -> Result<bool> {
        // PyPI normalizes names (case, `-`/`_`/`.`) and redirects; curl follows.
        Ok(!http::exists(&self.name_lookup_url(name), log)?)
    }

    fn version_exists(&self, name: &str, version: &str, log: &Logger) -> Result<bool> {
        http::exists(&format!("https://pypi.org/pypi/{name}/{version}/json"), log)
    }

    fn project_info(&self, dir: &Path) -> Result<ProjectInfo> {
        let text = read_manifest(dir)?;
        let name = toml_lite::get_str(&text, "project", "name")
            .ok_or_else(|| ShipError::new(Code::Precondition, "pyproject.toml has no [project] name"))?;
        if toml_lite::get_raw(&text, "project", "dynamic").is_some_and(|d| d.contains("version")) {
            return Err(ShipError::new(Code::Precondition, "dynamic versions are not supported yet; set [project] version"));
        }
        let version = toml_lite::get_str(&text, "project", "version");
        let repository = ["Repository", "Source", "Homepage"]
            .iter()
            .find_map(|k| toml_lite::get_str(&text, "project.urls", k));
        Ok(ProjectInfo {
            publishable: version.is_some() && !text.contains(PRIVATE_CLASSIFIER),
            version: version.unwrap_or_else(|| "0.0.0".into()),
            bin: toml_lite::first_key(&text, "project.scripts"),
            repository,
            description: toml_lite::get_str(&text, "project", "description"),
            name,
        })
    }

    fn new_project_files(&self, p: &NewProject) -> Vec<(String, String)> {
        templates::project_files(p)
    }

    fn gitignore(&self) -> &'static str {
        "__pycache__/\n*.py[cod]\n.venv/\ndist/\nbuild/\n*.egg-info/\n"
    }

    fn install_doc(&self, p: &NewProject) -> String {
        templates::install_doc(p)
    }

    fn usage_doc(&self, p: &NewProject) -> String {
        if p.cli {
            return crate::templates::cli_usage(&p.name);
        }
        format!("```python\nfrom {} import greet\n\nprint(greet(\"Ferris\"))\n```\n", module_name(&p.name))
    }

    fn checks(&self, dir: &Path) -> Vec<(&'static str, Cmd)> {
        vec![
            ("lint", uv(&["run", "python", "-m", "compileall", "-q", "-x", r"(\.venv|build|dist)", "."], dir)),
            ("build", uv(&["build"], dir)),
            ("test", uv(&["run", "python", "-m", "unittest", "discover", "-s", "tests", "-t", "."], dir)),
        ]
    }

    fn package_files_plan(&self, dir: &Path) -> Vec<Cmd> {
        let tmp = Path::new("<temp>");
        vec![
            uv(&["build", "--sdist", "--out-dir", &tmp.to_string_lossy()], dir),
            Cmd::new("tar", &["-tzf", "<temp>/<sdist>.tar.gz"]),
        ]
    }

    /// Builds the sdist into a temp folder and lists it: the sdist is the
    /// superset of what PyPI receives.
    fn package_files(&self, dir: &Path, log: &Logger) -> Result<Vec<String>> {
        let tmp = TempDir::new("ship-sdist")?;
        let out = tmp.path().to_string_lossy().into_owned();
        proc::run_ok(&uv(&["build", "--sdist", "--out-dir", &out], dir), log)?;
        let sdist = std::fs::read_dir(tmp.path())?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.to_string_lossy().ends_with(".tar.gz"))
            .ok_or_else(|| ShipError::new(Code::CommandFailed, "uv build produced no sdist"))?;
        let listed = proc::run_ok(&Cmd::new("tar", &["-tzf", &sdist.to_string_lossy()]).timeout(Duration::from_secs(60)), log)?;
        Ok(parse_sdist_listing(&listed.stdout))
    }

    fn generated_package_files(&self) -> &'static [&'static str] {
        &["PKG-INFO"]
    }

    fn dry_run_cmds(&self, dir: &Path) -> Vec<Cmd> {
        let out = out_dir(dir, "dry-run");
        let glob = format!("{}/*", out.to_string_lossy().replace('\\', "/"));
        vec![
            uv(&["build", "--out-dir", &out.to_string_lossy()], dir),
            uv(&["publish", "--dry-run", "--trusted-publishing", "never", &glob], dir),
        ]
    }

    fn scratch_dirs(&self, dir: &Path, version: &str) -> Vec<PathBuf> {
        vec![out_dir(dir, "dry-run"), out_dir(dir, version)]
    }

    fn set_version(&self, dir: &Path, _info: &ProjectInfo, new: &str) -> Result<Vec<(String, String)>> {
        let text = read_manifest(dir)?;
        let updated = toml_lite::set_str(&text, "project", "version", new)
            .ok_or_else(|| ShipError::new(Code::Precondition, "pyproject.toml has no plain `version = \"...\"` line in [project]"))?;
        Ok(vec![("pyproject.toml".to_string(), updated)])
    }

    fn trusted_publishing(&self, package: &str, owner: &str, repo: &str, workflow: &str, exists: bool) -> TrustedPublishing {
        let mut fields = Vec::new();
        if !exists {
            fields.push(("PyPI Project Name".into(), package.into()));
        }
        fields.extend([
            ("Owner".into(), owner.into()),
            ("Repository name".into(), repo.into()),
            ("Workflow name".into(), workflow.into()),
            ("Environment name".into(), "(leave empty)".into()),
        ]);
        let (settings_url, instructions) = if exists {
            (
                format!("https://pypi.org/manage/project/{package}/settings/publishing/"),
                "Sign in to PyPI, open the project's Publishing settings, add a GitHub publisher with the values exactly, then click Add.",
            )
        } else {
            (
                "https://pypi.org/manage/account/publishing/".to_string(),
                "Sign in to PyPI, open Publishing in your account settings, add a pending GitHub publisher with the values exactly, then click Add. The first tagged release then creates the project.",
            )
        };
        TrustedPublishing { settings_url, fields, needs_existing_package: false, instructions: instructions.into() }
    }

    fn publish_cmds(&self, dir: &Path, version: &str, _allow_dirty: bool) -> Vec<Cmd> {
        let out = out_dir(dir, version);
        let glob = format!("{}/*", out.to_string_lossy().replace('\\', "/"));
        let upload = if credential_source().is_some_and(is_twine_source) {
            // `uv publish` does not read .pypirc or TWINE_*; twine does.
            uv(&["tool", "run", "twine", "upload", "--non-interactive", &glob], dir)
        } else {
            uv(&["publish", "--check-url", "https://pypi.org/simple/", &glob], dir)
        };
        vec![uv(&["build", "--out-dir", &out.to_string_lossy()], dir), upload]
    }

    fn is_rate_limited(&self, output: &str) -> bool {
        let o = output.to_ascii_lowercase();
        o.contains(" 429") || o.contains("too many requests") || o.contains("rate limit")
    }

    fn package_url(&self, name: &str, version: &str) -> String {
        format!("https://pypi.org/project/{name}/{version}/")
    }

    fn install_cmds(&self, name: &str, version: &str, root: &Path) -> Vec<Cmd> {
        let venv = root.join("venv");
        let venv_s = venv.to_string_lossy().into_owned();
        vec![
            uv(&["venv", &venv_s], root),
            uv(&["pip", "install", &format!("{name}=={version}")], root).env("VIRTUAL_ENV", &venv_s),
        ]
    }

    fn installed_bin_cmd(&self, root: &Path, bin: &str) -> Cmd {
        Cmd::new(&venv_bin(&root.join("venv"), bin).to_string_lossy(), &[])
    }

    fn library_smoke(&self, name: &str, version: &str, dir: &Path) -> Vec<Cmd> {
        let venv = dir.join("venv");
        let venv_s = venv.to_string_lossy().into_owned();
        let import = format!("import {0}; print('imported {0}')", module_name(name));
        vec![
            uv(&["venv", &venv_s], dir),
            uv(&["pip", "install", &format!("{name}=={version}")], dir).env("VIRTUAL_ENV", &venv_s),
            Cmd::new(&venv_bin(&venv, "python").to_string_lossy(), &["-c", &import]).cwd(dir),
        ]
    }

    fn build_release_cmds(&self, dir: &Path) -> Vec<Cmd> {
        let venv = dir.join(".ship-venv");
        let venv_s = venv.to_string_lossy().into_owned();
        vec![
            uv(&["venv", &venv_s], dir),
            uv(&["pip", "install", "."], dir).env("VIRTUAL_ENV", &venv_s),
        ]
    }

    fn built_bin_cmd(&self, dir: &Path, bin: &str) -> Cmd {
        Cmd::new(&venv_bin(&dir.join(".ship-venv"), bin).to_string_lossy(), &[])
    }

    fn ci_workflow(&self, w: &WorkflowSpec) -> String {
        templates::ci_workflow(w)
    }

    fn release_workflow(&self, w: &WorkflowSpec) -> String {
        templates::release_workflow(w)
    }
}

/// `tar -tzf` lines like `pkg-0.1.0/src/x.py`, minus the top folder and directories.
fn parse_sdist_listing(stdout: &str) -> Vec<String> {
    let mut files: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().replace('\\', "/"))
        .filter(|l| !l.is_empty() && !l.ends_with('/'))
        .filter_map(|l| l.split_once('/').map(|(_, rest)| rest.to_string()))
        .filter(|l| !l.is_empty())
        .collect();
    files.sort();
    files
}

mod templates {
    use super::*;

    fn py_str(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }

    pub fn project_files(p: &NewProject) -> Vec<(String, String)> {
        let module = module_name(&p.name);
        let mut t = format!(
            "[project]\nname = \"{}\"\nversion = \"0.1.0\"\ndescription = \"{}\"\nreadme = \"README.md\"\nlicense = \"MIT\"\nlicense-files = [\"LICENSE\"]\n",
            p.name,
            py_str(&p.description)
        );
        if let Some(a) = &p.author {
            let (n, e) = a.split_once(" <").map(|(n, e)| (n, e.trim_end_matches('>'))).unwrap_or((a, ""));
            if e.is_empty() {
                t.push_str(&format!("authors = [{{ name = \"{}\" }}]\n", py_str(n)));
            } else {
                t.push_str(&format!("authors = [{{ name = \"{}\", email = \"{}\" }}]\n", py_str(n), py_str(e)));
            }
        }
        t.push_str("requires-python = \">=3.9\"\n");
        if p.target == Target::Repo {
            t.push_str(&format!(
                "# Repository-only project: PyPI rejects uploads with this classifier.\nclassifiers = [\"Programming Language :: Python :: 3\", \"{PRIVATE_CLASSIFIER}\"]\n"
            ));
        } else {
            t.push_str("classifiers = [\"Programming Language :: Python :: 3\"]\n");
        }
        t.push_str("dependencies = []\n");
        if let Some(url) = &p.repo_url {
            t.push_str(&format!("\n[project.urls]\nRepository = \"{url}\"\n"));
        }
        if p.cli {
            t.push_str(&format!("\n[project.scripts]\n{} = \"{module}.cli:main\"\n", p.name));
        }
        t.push_str("\n[build-system]\nrequires = [\"hatchling>=1.27\"]\nbuild-backend = \"hatchling.build\"\n");

        let init = format!(
            "\"\"\"{desc}\"\"\"\n\nfrom importlib.metadata import PackageNotFoundError, version\n\ntry:\n    __version__ = version(\"{name}\")\nexcept PackageNotFoundError:  # running from a source checkout\n    __version__ = \"0.0.0\"\n\n\ndef greet(name: str) -> str:\n    \"\"\"Return a greeting for `name`.\"\"\"\n    return f\"Hello, {{name}}!\"\n",
            desc = py_str(&p.description),
            name = p.name
        );
        let test = format!(
            "import unittest\n\nfrom {module} import greet\n\n\nclass GreetTest(unittest.TestCase):\n    def test_greets_by_name(self):\n        self.assertEqual(greet(\"Python\"), \"Hello, Python!\")\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n"
        );
        let mut files = vec![
            ("pyproject.toml".to_string(), t),
            (format!("{module}/__init__.py"), init),
            ("tests/__init__.py".to_string(), String::new()),
            ("tests/test_basic.py".to_string(), test),
        ];
        if p.cli {
            let cli = format!(
                "\"\"\"Command-line entry point.\"\"\"\n\nimport argparse\n\nfrom . import __version__, greet\n\n\ndef main(argv=None) -> int:\n    parser = argparse.ArgumentParser(prog=\"{name}\", description=\"{desc}\")\n    parser.add_argument(\"name\", nargs=\"?\", default=\"world\", help=\"who to greet (default: world)\")\n    parser.add_argument(\"-V\", \"--version\", action=\"version\", version=f\"%(prog)s {{__version__}}\")\n    args = parser.parse_args(argv)\n    print(greet(args.name))\n    return 0\n",
                name = p.name,
                desc = py_str(&p.description)
            );
            files.push((format!("{module}/cli.py"), cli));
            files.push((format!("{module}/__main__.py"), "from .cli import main\n\nraise SystemExit(main())\n".into()));
        }
        files
    }

    pub fn install_doc(p: &NewProject) -> String {
        let clone = match &p.repo_url {
            Some(url) => format!("git clone {url}\ncd {}\n", p.name),
            None => format!("# clone the repository, then:\ncd {}\n", p.name),
        };
        if p.target.has_package() {
            let cmd = if p.cli { format!("pipx install {}", p.name) } else { format!("pip install {}", p.name) };
            format!("```text\n{cmd}\n```\n\n### From source\n\n```text\n{clone}pip install .\n```\n")
        } else {
            format!("This project is not published to PyPI. Clone it and install it:\n\n```text\n{clone}pip install .\n```\n")
        }
    }

    pub fn ci_workflow(w: &WorkflowSpec) -> String {
        let os = if w.private { "ubuntu-latest" } else { "ubuntu-latest, windows-latest" };
        format!(
            r#"name: CI

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

jobs:
  test:
    name: test (${{{{ matrix.os }}}})
    runs-on: ${{{{ matrix.os }}}}
    strategy:
      fail-fast: false
      matrix:
        os: [{os}]
    steps:
      - uses: actions/checkout@v7
      - uses: astral-sh/setup-uv@v10
      - name: Lint
        run: uv run python -m compileall -q -x "(\.venv|build|dist)" .
      - name: Build
        run: uv build
      - name: Test
        run: uv run python -m unittest discover -s tests -t .
"#
        )
    }

    pub fn release_workflow(w: &WorkflowSpec) -> String {
        let mut jobs = String::from(
            r#"  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: astral-sh/setup-uv@v10
      - name: Tag must match pyproject.toml
        shell: bash
        run: |
          version=$(uv run --no-project python -c "import tomllib; print(tomllib.load(open('pyproject.toml', 'rb'))['project']['version'])")
          if [ "v$version" != "$GITHUB_REF_NAME" ]; then
            echo "::error::tag $GITHUB_REF_NAME does not match pyproject.toml version $version"
            exit 1
          fi
      - run: uv run python -m unittest discover -s tests -t .

  build:
    needs: test
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: astral-sh/setup-uv@v10
      - run: uv build
      - uses: actions/upload-artifact@v7
        with:
          name: python-dist
          path: dist/*
"#,
        );
        let mut needs = vec!["test", "build"];
        if w.publish {
            needs.push("publish");
            jobs.push_str(
                r#"
  publish:
    needs: build
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/download-artifact@v8
        with:
          name: python-dist
          path: dist
      - name: Publish to PyPI (trusted publishing; skips files already uploaded)
        uses: pypa/gh-action-pypi-publish@release/v1
        with:
          skip-existing: true
"#,
            );
        }
        let header = workflows::release_header(
            RELEASE_MARKER,
            "On a v* tag: test, build, publish to PyPI via trusted publishing (no stored token),\n# then create the GitHub release from CHANGELOG.md with the distributions attached.",
        );
        header + &jobs + &workflows::github_release_job(&needs, Some("python-dist"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(target: Target, cli: bool) -> NewProject {
        NewProject {
            name: "hello-tool".into(),
            description: "Says \"hi\"".into(),
            author: Some("A B <a@b.c>".into()),
            repo_url: Some("https://github.com/acme/hello-tool".into()),
            target,
            cli,
            private: false,
            year: "2026".into(),
            date: "2026-10-25".into(),
        }
    }

    #[test]
    fn name_rules() {
        let r = PyPi;
        assert!(r.validate_name("requests").is_ok());
        assert!(r.validate_name("My_Pkg.2").is_ok());
        assert!(r.validate_name("-bad").is_err());
        assert!(r.validate_name("has space").is_err());
        assert_eq!(r.sanitize_name("My Cool Tool!"), "my-cool-tool");
        assert_eq!(module_name("My-Tool.x"), "my_tool_x");
    }

    #[test]
    fn generated_pyproject_reads_back() {
        let dir = TempDir::new("ship-pypi-ut").unwrap();
        for (path, content) in PyPi.new_project_files(&project(Target::Repo, true)) {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let info = PyPi.project_info(dir.path()).unwrap();
        assert_eq!(info.name, "hello-tool");
        assert_eq!(info.version, "0.1.0");
        assert!(!info.publishable, "repo-only carries the Do Not Upload classifier");
        assert_eq!(info.bin.as_deref(), Some("hello-tool"));
        assert_eq!(info.repository.as_deref(), Some("https://github.com/acme/hello-tool"));
        assert!(dir.path().join("hello_tool/cli.py").is_file());
        let files = PyPi.set_version(dir.path(), &info, "0.2.0").unwrap();
        assert!(files[0].1.contains("version = \"0.2.0\""));
        assert!(files[0].1.contains("authors = [{ name = \"A B\", email = \"a@b.c\" }]"));
    }

    #[test]
    fn pending_publisher_before_first_release() {
        let pending = PyPi.trusted_publishing("x", "o", "r", "release.yml", false);
        assert!(!pending.needs_existing_package);
        assert_eq!(pending.settings_url, "https://pypi.org/manage/account/publishing/");
        assert_eq!(pending.fields[0], ("PyPI Project Name".to_string(), "x".to_string()));
        let existing = PyPi.trusted_publishing("x", "o", "r", "release.yml", true);
        assert!(existing.settings_url.contains("/manage/project/x/"));
    }

    #[test]
    fn sdist_listing() {
        let out = "hello-0.1.0/\nhello-0.1.0/PKG-INFO\nhello-0.1.0/hello/__init__.py\nhello-0.1.0/hello/\n";
        assert_eq!(parse_sdist_listing(out), ["PKG-INFO", "hello/__init__.py"]);
    }

    #[test]
    fn release_workflow_shape() {
        let y = PyPi.release_workflow(&WorkflowSpec { name: "x".into(), private: false, publish: true, bin: None });
        assert!(y.contains("pypa/gh-action-pypi-publish@release/v1"));
        assert!(y.contains("id-token: write"));
        assert!(y.contains("needs: [test, build, publish]"));
        assert!(y.contains("pattern: python-dist"));
        assert!(!y.contains("secrets."));
        let repo_only = PyPi.release_workflow(&WorkflowSpec { name: "x".into(), private: false, publish: false, bin: None });
        assert!(!repo_only.contains("gh-action-pypi-publish"));
    }

    #[test]
    fn uv_store_entry_for_pypi() {
        let store = "[[credential]]\nservice = \"https://upload.pypi.org/legacy/\"\nusername = \"__token__\"\n";
        assert!(uv_store_has_pypi(store));
        assert!(!uv_store_has_pypi("[[credential]]\nservice = \"https://example.com/simple/\"\n"));
        assert!(!uv_store_has_pypi("password = \"upload.pypi.org\"\n"));
        assert!(is_twine_source(".pypirc") && !is_twine_source("uv auth login"));
    }
}
