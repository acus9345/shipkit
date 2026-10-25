//! Integration tests for watch, verify and publish, offline: the "GitHub"
//! repo is a local bare repo and nothing is ever published.

mod common;

use common::Sandbox;
use std::fs;
use std::path::PathBuf;

/// A committed repo-only CLI project with main pushed to a fake GitHub origin.
fn pushed_project(sb: &Sandbox, name: &str) -> PathBuf {
    let dir = sb.project(name);
    let url = sb.fake_github_repo("octo", name);
    let (code, v) = sb.ship(&dir, &["scaffold", "--target", "repo", "--cli", "--repo-url", &url]);
    assert_eq!(code, 0, "{v}");
    sb.ship(&dir, &["commit"]);
    sb.git(&dir, &["remote", "add", "origin", &url]);
    sb.git(&dir, &["push", "--quiet", "origin", "main"]);
    dir
}

#[test]
fn watch_needs_a_github_origin() {
    let sb = Sandbox::new("watch");
    let dir = sb.project("demo-watch");
    sb.ship(&dir, &["scaffold", "--target", "repo"]);
    sb.ship(&dir, &["commit"]);
    let (code, v) = sb.ship(&dir, &["watch", "--timeout", "1"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "E_PRECONDITION");
}

#[test]
fn verify_from_repo_clones_builds_and_runs_readme() {
    let sb = Sandbox::new("verify");
    let dir = pushed_project(&sb, "demo-verify");
    let (code, v) = sb.ship(&dir, &["release", "--confirm"]);
    assert_eq!(code, 0, "{v}");

    let (code, v) = sb.ship(&dir, &["verify", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    let (code, v) = sb.ship(&dir, &["verify"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["source"], "repo");
    let ran: Vec<&str> = v["data"]["readme_commands"].as_array().unwrap().iter().map(|c| c["cmd"].as_str().unwrap()).collect();
    assert_eq!(ran, ["demo-verify --help", "demo-verify --version", "demo-verify Ferris"]);
    assert!(v["data"]["stages"][0].as_str().unwrap().contains("--branch v0.1.0"));
    assert!(v["data"]["temp_dir"].is_null(), "temp folder removed on success");
}

#[test]
fn verify_stops_at_first_failing_readme_command() {
    let sb = Sandbox::new("verifyfail");
    let dir = pushed_project(&sb, "demo-vf");
    let readme = fs::read_to_string(dir.join("README.md")).unwrap();
    fs::write(dir.join("README.md"), readme.replace("demo-vf --version\n", "demo-vf --bogus\ndemo-vf --version\n")).unwrap();
    sb.ship(&dir, &["commit", "-m", "Break README"]);
    sb.git(&dir, &["push", "--quiet", "origin", "main"]);

    let (code, v) = sb.ship(&dir, &["verify"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "E_CHECK_FAILED");
    let d = &v["error"]["details"];
    assert_eq!(d["cmd"], "demo-vf --bogus");
    assert_eq!(d["passed"].as_array().unwrap().len(), 1, "only --help ran before it");
    let kept = PathBuf::from(d["temp_dir"].as_str().unwrap());
    assert!(kept.is_dir(), "temp folder kept on failure");
    let _ = fs::remove_dir_all(kept);
}

#[test]
fn publish_refuses_during_cooldown() {
    let sb = Sandbox::new("cooldown");
    let dir = sb.project("demo-cd");
    sb.ship(&dir, &["scaffold"]);
    fs::write(
        sb.root.join("state").join("cooldown-crates_io.json"),
        r#"{"registry":"crates.io","until":99999999999,"until_utc":"5138-11-16T09:46:39Z","reason":"HTTP 429 from publish"}"#,
    )
    .unwrap();
    for args in [&["publish", "--confirm"][..], &["publish", "--dry-run"][..]] {
        let (code, v) = sb.ship(&dir, args);
        assert_eq!(code, 1, "{v}");
        assert_eq!(v["error"]["code"], "E_COOLDOWN");
    }
}

#[test]
fn publish_refuses_repo_only_projects() {
    let sb = Sandbox::new("pubrepo");
    let dir = sb.project("demo-pr");
    sb.ship(&dir, &["scaffold", "--target", "repo"]);
    let (code, v) = sb.ship(&dir, &["publish", "--confirm"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "E_PRECONDITION");
}
