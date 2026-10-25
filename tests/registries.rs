//! npm and PyPI projects end to end, offline: scaffold, manifest detection,
//! bump, commit and (for npm, when Node.js is installed) checks.

mod common;

use common::Sandbox;
use std::fs;
use std::process::Command;

fn has(tool: &str) -> bool {
    let program = if cfg!(windows) && tool == "npm" { "npm.cmd" } else { tool };
    Command::new(program).arg("--version").output().is_ok_and(|o| o.status.success())
}

#[test]
fn npm_project_lifecycle() {
    let sb = Sandbox::new("npm");
    let dir = sb.project("hello-npm");
    let (code, v) = sb.ship(&dir, &["scaffold", "--registry", "npm", "--cli", "--target", "repo", "--description", "Says hello"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["project"]["registry"], "npm");
    for f in ["package.json", "index.js", "test/index.test.js", "bin/hello-npm.js", "README.md", ".gitignore"] {
        assert!(dir.join(f).is_file(), "{f}");
    }
    let manifest = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(manifest.contains("\"private\": true"));
    assert!(fs::read_to_string(dir.join(".gitignore")).unwrap().contains("node_modules/"));
    assert!(fs::read_to_string(dir.join("README.md")).unwrap().contains("hello-npm --version"));

    // Later steps detect npm from package.json without --registry.
    let (code, v) = sb.ship(&dir, &["inspect"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["publishable"], false);

    let (code, v) = sb.ship(&dir, &["commit"]);
    assert_eq!(code, 0, "{v}");

    if has("npm") && has("node") {
        // Repo-only, so verify clones the (fake, local) GitHub repo and runs the README commands.
        let url = sb.fake_github_repo("octo", "hello-npm");
        sb.git(&dir, &["remote", "add", "origin", &url]);
        sb.git(&dir, &["push", "--quiet", "origin", "main"]);
        let (code, v) = sb.ship(&dir, &["verify"]);
        assert_eq!(code, 0, "{v}");
        let ran: Vec<&str> = v["data"]["readme_commands"].as_array().unwrap().iter().map(|c| c["cmd"].as_str().unwrap()).collect();
        assert_eq!(ran, ["hello-npm --help", "hello-npm --version", "hello-npm Ferris"]);
    }

    sb.git(&dir, &["tag", "-a", "v0.1.0", "-m", "Release v0.1.0"]);
    let (code, v) = sb.ship(&dir, &["bump", "--level", "minor", "--notes", "Wave"]);
    assert_eq!(code, 0, "{v}");
    assert!(fs::read_to_string(dir.join("package.json")).unwrap().contains("\"version\": \"0.2.0\""));

    if has("npm") && has("node") {
        let (code, v) = sb.ship(&dir, &["checks"]);
        assert_eq!(code, 0, "{v}");
        let names: Vec<_> = v["data"]["passed"].as_array().unwrap().iter().map(|p| p["name"].clone()).collect();
        assert_eq!(names, ["install", "lint", "typecheck", "build", "test"]);
    } else {
        eprintln!("skipping npm checks: node/npm not installed");
    }
}

#[test]
fn pypi_project_lifecycle() {
    let sb = Sandbox::new("pypi");
    let dir = sb.project("hello-py");
    let (code, v) = sb.ship(
        &dir,
        &["scaffold", "--registry", "pypi", "--cli", "--description", "Says hello", "--repo-url", "https://github.com/o/hello-py"],
    );
    assert_eq!(code, 0, "{v}");
    for f in ["pyproject.toml", "hello_py/__init__.py", "hello_py/cli.py", "tests/test_basic.py", ".github/workflows/release.yml"] {
        assert!(dir.join(f).is_file(), "{f}");
    }
    let release = fs::read_to_string(dir.join(".github/workflows/release.yml")).unwrap();
    assert!(release.contains("gh-action-pypi-publish"));
    assert!(fs::read_to_string(dir.join("README.md")).unwrap().contains("pipx install hello-py"));

    let (code, v) = sb.ship(&dir, &["bump", "--version", "0.1.1", "--notes", "Fix"]);
    assert_eq!(code, 0, "{v}");
    let manifest = fs::read_to_string(dir.join("pyproject.toml")).unwrap();
    assert!(manifest.contains("version = \"0.1.1\""));

    // The scaffolded tests pass with plain unittest (no third-party packages).
    let python = if cfg!(windows) { "python" } else { "python3" };
    if has(python) {
        let out = Command::new(python)
            .args(["-m", "unittest", "discover", "-s", "tests", "-t", "."])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn pypi_repo_only_has_no_trusted_publishing() {
    let sb = Sandbox::new("pypitp");
    let dir = sb.project("demo-tp-py");
    sb.ship(&dir, &["scaffold", "--registry", "pypi", "--target", "repo"]);
    let (code, v) = sb.ship(&dir, &["trusted-publishing"]);
    // Repo-only projects carry the Do Not Upload classifier, so there is nothing to connect.
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["status"], "not_applicable");
}

#[test]
fn unknown_registry_is_rejected() {
    let sb = Sandbox::new("badreg");
    let dir = sb.project("x");
    let (code, v) = sb.ship(&dir, &["scaffold", "--registry", "maven"]);
    assert_eq!(code, 2);
    assert_eq!(v["error"]["code"], "E_INPUT");
}
