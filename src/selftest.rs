//! `ship self-test`: offline checks that the safety machinery works on this
//! machine: the gate table, the hook's decisions, redaction, and a secret scan
//! of a throwaway project in a temp folder. No network, nothing published.

use crate::error::{Code, Result, ShipError};
use crate::gate::{self, Flags};
use crate::hook;
use crate::log::Logger;
use crate::project::TempDir;
use crate::redact;
use crate::registry::Target;
use crate::steps::{self, scaffold::Scaffold, scan_secrets, Ctx};
use serde_json::{json, Value};

pub fn run(log: &Logger) -> Result<Value> {
    let mut checks: Vec<Value> = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        checks.push(json!({ "check": name, "ok": ok, "detail": detail }));
    };

    let gated: Vec<&str> = gate::STEPS.iter().filter(|(s, _)| gate::is_gated(s)).map(|(s, _)| *s).collect();
    check("gate table", gated == ["create-repo", "release", "publish"], format!("gated: {}", gated.join(", ")));

    let cases = [
        ("ship release", true),
        ("ship publish", true),
        ("ship create-repo --private", true),
        ("ship preflight --confirm && ship publish", true),
        ("/c/tools/ship.exe release", true),
        ("ship install-tool uv", true),
        ("ship release --confirm", false),
        ("ship publish --dry-run", false),
        ("ship preflight", false),
        ("ship bump --level patch", false),
    ];
    let wrong: Vec<&str> = cases.iter().filter(|(c, block)| hook::decide(c).is_some() != *block).map(|(c, _)| *c).collect();
    check(
        "hook decisions",
        wrong.is_empty(),
        if wrong.is_empty() { format!("{} cases correct", cases.len()) } else { format!("wrong for: {}", wrong.join(" | ")) },
    );

    let fake = format!("ghp_{}", "Sx7".repeat(12));
    let red = redact::redact(&format!("token {fake}"));
    check("redaction", !red.contains(&fake), "a fake GitHub token is replaced with [REDACTED]".into());

    let tmp = TempDir::new("ship-selftest")?;
    let dir = tmp.path().join("selftest-demo");
    std::fs::create_dir_all(&dir)?;
    let ctx = Ctx { dir: dir.clone(), flags: Flags::default(), log: Logger::null() };
    let scaffold = Scaffold { target: Some(Target::Repo), cli: true, ..Default::default() };
    let scaffolded = steps::run(&scaffold, &ctx);
    check("scaffold in temp folder", scaffolded.is_ok(), scaffolded.err().map(|e| e.message).unwrap_or_else(|| "ok".into()));

    match scan_secrets::scan_committable(&ctx) {
        Ok(r) => check("scan of clean project", r.clean(), format!("{} files scanned", r.files_scanned)),
        Err(e) => check("scan of clean project", false, e.message),
    }
    std::fs::write(dir.join("leak.txt"), format!("x\n{fake}\n"))?;
    std::fs::write(dir.join(".env"), "X=1\n")?;
    std::fs::remove_file(dir.join(".gitignore"))?;
    match scan_secrets::scan_committable(&ctx) {
        Ok(r) => {
            let kinds: Vec<String> = r.findings.iter().map(|f| format!("{}:{} {}", f.file, f.line, f.kind)).collect();
            let ok = kinds.iter().any(|k| k == "leak.txt:2 github_token") && kinds.iter().any(|k| k.starts_with(".env:0"));
            check("scan finds planted secrets", ok, kinds.join(", "));
        }
        Err(e) => check("scan finds planted secrets", false, e.message),
    }

    let passed = checks.iter().all(|c| c["ok"] == true);
    log.event("self-test", json!({ "passed": passed }));
    if passed {
        Ok(json!({ "passed": true, "checks": checks }))
    } else {
        Err(ShipError::new(Code::CheckFailed, "self-test failed").with_details(json!({ "checks": checks })))
    }
}
