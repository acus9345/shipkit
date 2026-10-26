//! Integration tests for the local steps. Nothing here uses the network:
//! projects are repo-only (no registry dry run) and check-names is not called.

mod common;

use common::{fake_token, Sandbox};
use std::fs;

#[test]
fn usage_errors_are_json() {
    let sb = Sandbox::new("usage");
    let (code, v) = sb.ship(&sb.root, &["no-such-step"]);
    assert_eq!(code, 2);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "E_USAGE");
    assert_eq!(v["error"]["details"]["steps"].as_array().unwrap().len(), 15);
}

#[test]
fn scaffold_dry_run_changes_nothing() {
    let sb = Sandbox::new("dry");
    let dir = sb.project("demo-dry");
    let (code, v) = sb.ship(&dir, &["scaffold", "--target", "repo", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["dry_run"], true);
    assert!(v["data"]["plan"]["actions"].as_array().unwrap().len() >= 8);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn scaffold_never_overwrites_and_is_idempotent() {
    let sb = Sandbox::new("idem");
    let dir = sb.project("demo-idem");
    fs::write(dir.join("README.md"), "my own readme\n").unwrap();

    let (code, v) = sb.ship(&dir, &["scaffold", "--target", "repo", "--cli"]);
    assert_eq!(code, 0, "{v}");
    let created = v["data"]["created"].as_array().unwrap();
    assert!(created.iter().any(|f| f == "Cargo.toml"));
    assert!(v["data"]["skipped_existing"].as_array().unwrap().iter().any(|f| f == "README.md"));
    assert_eq!(fs::read_to_string(dir.join("README.md")).unwrap(), "my own readme\n");

    let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("name = \"demo-idem\""));
    assert!(manifest.contains("version = \"0.1.0\""));
    assert!(manifest.contains("authors = [\"Test User <test@example.com>\"]"));
    assert!(manifest.contains("publish = false"));
    assert!(dir.join("src/main.rs").is_file());
    assert!(!dir.join(".github").exists(), "no workflows without a repo URL");

    let (code, v) = sb.ship(&dir, &["scaffold", "--target", "repo", "--cli"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["created"].as_array().unwrap().len(), 0);
}

#[test]
fn scaffold_private_repo_gets_ubuntu_only_ci() {
    let sb = Sandbox::new("priv");
    let dir = sb.project("demo-priv");
    let (code, v) = sb.ship(
        &dir,
        &["scaffold", "--input", r#"{"repo_url":"https://github.com/o/demo-priv","private":true,"cli":true}"#],
    );
    assert_eq!(code, 0, "{v}");
    let ci = fs::read_to_string(dir.join(".github/workflows/ci.yml")).unwrap();
    assert!(ci.contains("os: [ubuntu-latest]"));
    let release = fs::read_to_string(dir.join(".github/workflows/release.yml")).unwrap();
    assert!(release.contains("crates-io-auth-action"));
    assert!(!release.contains("windows-latest"));
    let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("repository = \"https://github.com/o/demo-priv\""));
}

#[test]
fn input_and_flags_cannot_mix() {
    let sb = Sandbox::new("mix");
    let dir = sb.project("demo-mix");
    let (code, v) = sb.ship(&dir, &["scaffold", "--cli", "--input", "{}"]);
    assert_eq!(code, 2);
    assert_eq!(v["error"]["code"], "E_INPUT");
}

#[test]
fn scan_reports_file_and_line_but_never_the_secret() {
    let sb = Sandbox::new("scan");
    let dir = sb.project("demo-scan");
    let token = fake_token();
    fs::write(dir.join("notes.txt"), format!("hello\nkey: {token}\n")).unwrap();
    fs::write(dir.join(".env"), "X=1\n").unwrap();

    let (code, v) = sb.ship(&dir, &["scan-secrets"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "E_SECRETS_FOUND");
    let findings = v["error"]["details"]["findings"].as_array().unwrap();
    assert!(findings.iter().any(|f| f["file"] == "notes.txt" && f["line"] == 2 && f["kind"] == "github_token"));
    assert!(findings.iter().any(|f| f["file"] == ".env" && f["kind"] == "env_file"));
    assert!(!v.to_string().contains(&token));

    let log = fs::read_to_string(sb.root.join("state").join("ship.log")).unwrap();
    assert!(!log.contains(&token));
}

#[test]
fn gitignored_files_are_not_scanned() {
    let sb = Sandbox::new("ignored");
    let dir = sb.project("demo-ignored");
    fs::write(dir.join(".gitignore"), "secret.txt\n").unwrap();
    fs::write(dir.join("secret.txt"), fake_token()).unwrap();
    let (code, v) = sb.ship(&dir, &["scan-secrets"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["clean"], true);
}

#[test]
fn commit_refuses_env_files_even_in_dry_run() {
    let sb = Sandbox::new("env");
    let dir = sb.project("demo-env");
    fs::write(dir.join("a.txt"), "hi\n").unwrap();
    fs::write(dir.join(".env.production"), "X=1\n").unwrap();
    for args in [&["commit", "--dry-run"][..], &["commit"][..]] {
        let (code, v) = sb.ship(&dir, args);
        assert_eq!(code, 1);
        assert_eq!(v["error"]["code"], "E_SECRETS_FOUND");
    }
    assert!(!dir.join(".git").exists());
}

#[test]
fn commit_inits_main_and_is_idempotent() {
    let sb = Sandbox::new("commit");
    let dir = sb.project("demo-commit");
    let (code, _) = sb.ship(&dir, &["scaffold", "--target", "repo"]);
    assert_eq!(code, 0);

    let (code, v) = sb.ship(&dir, &["commit"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["committed"], true);
    assert_eq!(v["data"]["branch"], "main");
    assert_eq!(v["data"]["registry_dry_run"], false);
    assert_eq!(sb.git(&dir, &["log", "--format=%s"]), "Initial commit");

    let (code, v) = sb.ship(&dir, &["commit"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["committed"], false);

    fs::write(dir.join("extra.txt"), "x\n").unwrap();
    let (code, v) = sb.ship(&dir, &["commit"]);
    assert_eq!(code, 2);
    assert_eq!(v["error"]["code"], "E_INPUT");
    let (code, v) = sb.ship(&dir, &["commit", "-m", "Add extra"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["files_changed"], 1);
}

#[test]
fn inspect_skips_dry_run_for_repo_only() {
    let sb = Sandbox::new("inspect");
    let dir = sb.project("demo-inspect");
    sb.ship(&dir, &["scaffold", "--target", "repo"]);
    let (code, v) = sb.ship(&dir, &["inspect"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["publishable"], false);
}

#[test]
fn steps_needing_a_manifest_say_so() {
    let sb = Sandbox::new("nomanifest");
    let dir = sb.project("empty");
    for step in ["checks", "inspect"] {
        let (code, v) = sb.ship(&dir, &[step]);
        assert_eq!(code, 1);
        assert_eq!(v["error"]["code"], "E_NOT_FOUND");
    }
}

#[test]
fn checks_pass_then_stop_at_first_failure() {
    let sb = Sandbox::new("checks");
    let dir = sb.project("demo-checks");
    sb.ship(&dir, &["scaffold", "--target", "repo", "--cli"]);

    let (code, v) = sb.ship(&dir, &["checks"]);
    assert_eq!(code, 0, "{v}");
    let names: Vec<_> = v["data"]["passed"].as_array().unwrap().iter().map(|p| p["name"].clone()).collect();
    assert_eq!(names, ["lint", "build", "test"]);

    // An unused variable is a clippy warning, denied with -D warnings.
    let lib = fs::read_to_string(dir.join("src/lib.rs")).unwrap();
    fs::write(dir.join("src/lib.rs"), lib + "\npub fn f() { let unused = 1; }\n").unwrap();
    let (code, v) = sb.ship(&dir, &["checks"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "E_CHECK_FAILED");
    assert_eq!(v["error"]["details"]["failed"], "lint");
    assert!(v["error"]["details"]["output"].as_str().unwrap().contains("unused"));
    assert_eq!(v["error"]["details"]["passed"].as_array().unwrap().len(), 0);
}

#[test]
fn helpers_validate_input_and_install_tool_is_gated() {
    let sb = Sandbox::new("helpers");
    for args in [&["login", "docker"][..], &["login"][..], &["install-tool", "python"][..]] {
        let (code, v) = sb.ship(&sb.root, args);
        assert_eq!(code, 2, "{args:?}: {v}");
        assert_eq!(v["error"]["code"], "E_INPUT");
    }
    // git is always present here, so the plan is a skip, but the gate still applies.
    let (code, v) = sb.ship(&sb.root, &["install-tool", "git"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error"]["code"], "E_CONFIRM_REQUIRED");
    assert!(v["error"]["message"].as_str().unwrap().contains("installs software"));
    let (code, v) = sb.ship(&sb.root, &["install-tool", "git", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["plan"]["actions"][0]["kind"], "skip");
}
