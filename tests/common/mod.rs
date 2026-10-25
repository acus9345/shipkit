//! Test sandbox: a temp folder with its own git identity and ship state dir,
//! so tests never touch the user's config, log or network-facing accounts.

#![allow(dead_code)]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Sandbox {
    pub root: PathBuf,
}

impl Sandbox {
    pub fn new(name: &str) -> Sandbox {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("ship-it-{name}-{}-{nanos:x}", std::process::id()));
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::create_dir_all(root.join("remotes")).unwrap();
        // https://github.com/<owner>/<repo> is rewritten to a local bare repo,
        // so pushes and ls-remote never leave the machine.
        // `file:///` + path without its leading slash works for both `C:/...` and `/tmp/...`.
        let remotes = root.join("remotes").to_string_lossy().replace('\\', "/");
        let remotes = remotes.trim_start_matches('/');
        std::fs::write(
            root.join("gitconfig"),
            format!(
                "[user]\n\tname = Test User\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n[core]\n\tautocrlf = false\n[url \"file:///{remotes}/\"]\n\tinsteadOf = https://github.com/\n"
            ),
        )
        .unwrap();
        Sandbox { root }
    }

    /// A bare repo standing in for https://github.com/<owner>/<name>.
    pub fn fake_github_repo(&self, owner: &str, name: &str) -> String {
        let path = self.root.join("remotes").join(owner).join(name);
        std::fs::create_dir_all(&path).unwrap();
        self.git(&path, &["init", "--bare", "--quiet"]);
        format!("https://github.com/{owner}/{name}")
    }

    /// A fresh project folder inside the sandbox.
    pub fn project(&self, name: &str) -> PathBuf {
        let p = self.root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Run `ship` in `dir`; returns (exit code, parsed stdout JSON).
    pub fn ship(&self, dir: &Path, args: &[&str]) -> (i32, Value) {
        let out = Command::new(env!("CARGO_BIN_EXE_ship"))
            .args(args)
            .current_dir(dir)
            .env("SHIP_STATE_DIR", self.root.join("state"))
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("CARGO_REGISTRY_TOKEN")
            // gh gets a dummy token (never the user's keyring) and a dead proxy,
            // so any GitHub API call fails fast without touching the network.
            .env("GH_TOKEN", "test-dummy")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("NO_PROXY", "")
            .output()
            .expect("run ship");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let json: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}):\n{stdout}"));
        (out.status.code().unwrap_or(-1), json)
    }

    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A fake GitHub token built at runtime, so no source file contains one.
pub fn fake_token() -> String {
    format!("ghp_{}", "Zx9".repeat(12))
}
