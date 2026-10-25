//! Integration tests for bump, release, add-workflows, trusted-publishing and
//! create-repo, fully offline: GitHub URLs are rewritten to local bare repos
//! and gh's API calls fail fast through a dead proxy.

mod common;

use common::{fake_token, Sandbox};
use std::fs;
use std::path::PathBuf;

/// A committed repo-only CLI project whose origin is a fake GitHub repo.
fn released_project(sb: &Sandbox, name: &str) -> PathBuf {
    let dir = sb.project(name);
    let url = sb.fake_github_repo("octo", name);
    let (code, v) = sb.ship(&dir, &["scaffold", "--target", "repo", "--cli", "--private", "--repo-url", &url]);
    assert_eq!(code, 0, "{v}");
    let (code, v) = sb.ship(&dir, &["commit"]);
    assert_eq!(code, 0, "{v}");
    sb.git(&dir, &["remote", "add", "origin", &url]);
    dir
}

#[test]
fn release_is_gated_then_idempotent() {
    let sb = Sandbox::new("release");
    let dir = released_project(&sb, "demo-rel");
    let head = sb.git(&dir, &["rev-parse", "HEAD"]);

    // No --confirm: refused, with the full plan, and nothing pushed.
    let (code, v) = sb.ship(&dir, &["release"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error"]["code"], "E_CONFIRM_REQUIRED");
    let actions = v["error"]["details"]["plan"]["actions"].as_array().unwrap();
    let cmds: Vec<&str> = actions.iter().filter_map(|a| a["cmd"].as_str()).collect();
    assert!(cmds.contains(&"git push origin main"));
    assert!(cmds.contains(&"git tag -a v0.1.0 -m \"Release v0.1.0\""));
    assert!(cmds.contains(&"git push origin refs/tags/v0.1.0"));
    assert!(actions.iter().any(|a| a["what"] == "GitHub release"), "release.yml creates it");
    assert_eq!(sb.git(&dir, &["tag", "-l"]), "");

    // --dry-run, even with --confirm, changes nothing.
    let (code, v) = sb.ship(&dir, &["release", "--confirm", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["dry_run"], true);
    assert_eq!(sb.git(&dir, &["tag", "-l"]), "");

    // --confirm: branch and tag pushed.
    let (code, v) = sb.ship(&dir, &["release", "--confirm"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["tag"], "v0.1.0");
    let remote = sb.root.join("remotes/octo/demo-rel");
    assert_eq!(sb.git(&remote, &["rev-parse", "main"]), head);
    assert_eq!(sb.git(&remote, &["rev-parse", "v0.1.0^{commit}"]), head);

    // Again: everything already done, nothing runs.
    let (code, v) = sb.ship(&dir, &["release", "--confirm"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["ran"].as_array().unwrap().len(), 0);
}

#[test]
fn release_never_moves_a_tag() {
    let sb = Sandbox::new("tagmove");
    let dir = released_project(&sb, "demo-tag");
    sb.git(&dir, &["tag", "-a", "v0.1.0", "-m", "old"]);
    fs::write(dir.join("notes.txt"), "more\n").unwrap();
    let (code, v) = sb.ship(&dir, &["commit", "-m", "More"]);
    assert_eq!(code, 0, "{v}");

    let (code, v) = sb.ship(&dir, &["release", "--confirm"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "E_PRECONDITION");
    assert!(v["error"]["message"].as_str().unwrap().contains("never moved"));
}

#[test]
fn outward_steps_refuse_dirty_trees_and_secrets() {
    let sb = Sandbox::new("dirty");
    let dir = released_project(&sb, "demo-dirty");
    fs::write(dir.join("wip.txt"), "uncommitted\n").unwrap();
    for step in ["release", "create-repo"] {
        let (code, v) = sb.ship(&dir, &[step, "--confirm"]);
        assert_eq!(code, 1, "{step}: {v}");
        assert_eq!(v["error"]["code"], "E_DIRTY_TREE", "{step}");
    }
    // --allow-dirty gets past the tree check, but secrets still block.
    fs::write(dir.join("wip.txt"), format!("{}\n", fake_token())).unwrap();
    let (code, v) = sb.ship(&dir, &["release", "--confirm", "--allow-dirty"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "E_SECRETS_FOUND");
}

#[test]
fn create_repo_needs_a_commit_first() {
    let sb = Sandbox::new("nocommit");
    let dir = sb.project("demo-nc");
    sb.ship(&dir, &["scaffold", "--target", "repo"]);
    let (code, v) = sb.ship(&dir, &["create-repo", "--confirm"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "E_PRECONDITION");
}

#[test]
fn bump_records_changelog_and_is_idempotent() {
    let sb = Sandbox::new("bump");
    let dir = released_project(&sb, "demo-bump");

    // 0.1.0 has no tag yet: bumping again would skip a version.
    let (code, v) = sb.ship(&dir, &["bump", "--level", "patch"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["bumped"], false);

    sb.git(&dir, &["tag", "-a", "v0.1.0", "-m", "Release v0.1.0"]);
    let (code, v) = sb.ship(&dir, &["bump", "--level", "patch", "--notes", "x", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["plan"]["context"]["to"], "0.1.1");
    assert!(fs::read_to_string(dir.join("Cargo.toml")).unwrap().contains("version = \"0.1.0\""));

    let (code, v) = sb.ship(&dir, &["bump", "--level", "patch", "--notes", "Fix greeting\nAdd --loud"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["to"], "0.1.1");
    let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("version = \"0.1.1\""));
    let log = fs::read_to_string(dir.join("CHANGELOG.md")).unwrap();
    assert!(log.contains("## [0.1.1] - "));
    assert!(log.contains("- Fix greeting\n- Add --loud"));
    assert!(log.find("[0.1.1]").unwrap() < log.find("[0.1.0]").unwrap());
    assert_eq!(sb.git(&dir, &["log", "-1", "--format=%s"]), "Initial commit", "bump does not commit");

    // Running it again does not skip to 0.1.2.
    let (code, v) = sb.ship(&dir, &["bump", "--level", "patch"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["bumped"], false);
    let (code, v) = sb.ship(&dir, &["bump", "--version", "0.1.1"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["bumped"], false);

    let (code, v) = sb.ship(&dir, &["bump", "--version", "0.0.9"]);
    assert_eq!(code, 2);
    assert_eq!(v["error"]["code"], "E_INPUT");
}

#[test]
fn bump_without_notes_needs_unreleased_entries() {
    let sb = Sandbox::new("bumpnotes");
    let dir = released_project(&sb, "demo-bn");
    let (code, v) = sb.ship(&dir, &["bump", "--version", "0.2.0"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "E_PRECONDITION");

    let log = fs::read_to_string(dir.join("CHANGELOG.md")).unwrap();
    fs::write(dir.join("CHANGELOG.md"), log.replace("## [Unreleased]\n", "## [Unreleased]\n\n### Added\n\n- A flag.\n")).unwrap();
    let (code, v) = sb.ship(&dir, &["bump", "--version", "0.2.0"]);
    assert_eq!(code, 0, "{v}");
    let log = fs::read_to_string(dir.join("CHANGELOG.md")).unwrap();
    assert!(log.contains("## [Unreleased]\n\n## [0.2.0] - "));
}

#[test]
fn add_workflows_never_overwrites() {
    let sb = Sandbox::new("wf");
    let dir = sb.project("demo-wf");
    sb.ship(&dir, &["scaffold", "--target", "repo", "--cli"]);
    let (code, v) = sb.ship(&dir, &["add-workflows", "--private", "true"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["created"].as_array().unwrap().len(), 2);
    let release = fs::read_to_string(dir.join(".github/workflows/release.yml")).unwrap();
    assert!(!release.contains("cargo publish"), "repo-only projects never publish");
    assert!(release.contains("bin=\"demo-wf\""), "binary attached");

    fs::write(dir.join(".github/workflows/ci.yml"), "# mine\n").unwrap();
    let (code, v) = sb.ship(&dir, &["add-workflows", "--private", "true"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["created"].as_array().unwrap().len(), 0);
    assert_eq!(fs::read_to_string(dir.join(".github/workflows/ci.yml")).unwrap(), "# mine\n");
}

#[test]
fn trusted_publishing_not_applicable_for_repo_only() {
    let sb = Sandbox::new("tp");
    let dir = released_project(&sb, "demo-tp");
    let (code, v) = sb.ship(&dir, &["trusted-publishing"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["status"], "not_applicable");
}

#[test]
fn generated_files_use_lf_line_endings() {
    let sb = Sandbox::new("lf");
    let dir = sb.project("demo-lf");
    sb.ship(&dir, &["scaffold", "--cli", "--repo-url", "https://github.com/o/demo-lf"]);
    for f in ["Cargo.toml", "src/main.rs", "README.md", ".github/workflows/release.yml"] {
        assert!(!fs::read(dir.join(f)).unwrap().contains(&b'\r'), "{f} has CR");
    }
}
