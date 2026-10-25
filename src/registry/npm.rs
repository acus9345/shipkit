//! npm via the `npm` CLI (and `curl` for read-only registry lookups).

use super::{workflows, LoginStatus, NewProject, ProjectInfo, Registry, Target, TrustedPublishing, WorkflowSpec, RELEASE_MARKER};
use crate::error::{Code, Result, ShipError};
use crate::http;
use crate::log::{home_dir, Logger};
use crate::proc::{self, Cmd};
use crate::project::json_lite;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

pub struct Npm;

const REGISTRY: &str = "https://registry.npmjs.org";
const LONG: Duration = Duration::from_secs(30 * 60);

/// Names npm refuses: special files and Node.js built-in modules.
const RESERVED: &[&str] = &[
    "node_modules", "favicon.ico", "assert", "buffer", "child_process", "cluster", "crypto", "dns", "events",
    "fs", "http", "http2", "https", "net", "os", "path", "process", "querystring", "readline", "stream",
    "string_decoder", "timers", "tls", "tty", "url", "util", "v8", "vm", "worker_threads", "zlib",
];

fn npm(args: &[&str], dir: &Path) -> Cmd {
    Cmd::new(&proc::node_tool("npm"), args).cwd(dir).timeout(LONG)
}

/// Scoped names (`@scope/name`) need the slash encoded in registry URLs.
fn encoded(name: &str) -> String {
    name.replace('/', "%2F")
}

/// The executable name npm derives from a package name: the part after the scope.
fn unscoped(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn valid_part(part: &str) -> std::result::Result<(), String> {
    if part.is_empty() {
        return Err("name part is empty".into());
    }
    if part.starts_with('.') || part.starts_with('_') {
        return Err("name cannot start with `.` or `_`".into());
    }
    if let Some(c) = part.chars().find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || "-._~".contains(*c))) {
        return Err(format!("name contains `{c}`; only lowercase letters, digits, `-`, `.`, `_` and `~` are allowed"));
    }
    Ok(())
}

fn read_manifest(dir: &Path) -> Result<Value> {
    let path = dir.join("package.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| ShipError::new(Code::NotFound, format!("cannot read {}: {e}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| ShipError::new(Code::Precondition, format!("package.json is not valid JSON: {e}")))
}

fn repo_url(v: &Value) -> Option<String> {
    let raw = match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.get("url")?.as_str()?.to_string(),
        _ => return None,
    };
    let raw = raw.trim_start_matches("git+");
    Some(raw.strip_suffix(".git").unwrap_or(raw).to_string())
}

impl Registry for Npm {
    fn id(&self) -> &'static str {
        "npm"
    }

    fn cli(&self) -> &'static str {
        "npm"
    }

    fn manifest(&self) -> &'static str {
        "package.json"
    }

    fn tool_checks(&self) -> Vec<(&'static str, Cmd)> {
        vec![
            ("node", Cmd::new("node", &["--version"])),
            ("npm", Cmd::new(&proc::node_tool("npm"), &["--version"])),
        ]
    }

    fn login_status(&self) -> LoginStatus {
        for var in ["NODE_AUTH_TOKEN", "NPM_TOKEN"] {
            if std::env::var_os(var).is_some_and(|v| !v.is_empty()) {
                return LoginStatus { logged_in: true, source: Some(format!("env:{var}")), note: None };
            }
        }
        let npmrc = home_dir().map(|h| h.join(".npmrc"));
        if let Some(text) = npmrc.and_then(|p| std::fs::read_to_string(p).ok()) {
            // Key names only; the token value is never read into a variable.
            if text.lines().any(|l| l.trim_start().starts_with("//registry.npmjs.org/:_authToken")) {
                return LoginStatus { logged_in: true, source: Some(".npmrc".into()), note: None };
            }
        }
        LoginStatus {
            logged_in: false,
            source: None,
            note: Some("no npm credential found; run `npm login` yourself, or publish with trusted publishing from CI".into()),
        }
    }

    fn validate_name(&self, name: &str) -> std::result::Result<(), String> {
        if name.is_empty() || name.len() > 214 {
            return Err("name must be 1 to 214 characters".into());
        }
        if RESERVED.contains(&name) {
            return Err("name is reserved (a Node.js built-in module or special file)".into());
        }
        match name.strip_prefix('@') {
            Some(scoped) => {
                let (scope, pkg) = scoped.split_once('/').ok_or("a scoped name must look like @scope/name")?;
                valid_part(scope)?;
                valid_part(pkg)
            }
            None => valid_part(name),
        }
    }

    fn sanitize_name(&self, raw: &str) -> String {
        let mut s = String::new();
        for c in raw.trim().chars() {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || "._~".contains(c) {
                s.push(c);
            } else if !s.ends_with('-') {
                s.push('-');
            }
        }
        let mut s = s.trim_matches(|c| c == '-' || c == '.' || c == '_').to_string();
        if s.is_empty() {
            s = "package".into();
        }
        if RESERVED.contains(&s.as_str()) {
            s.push_str("-js");
        }
        s.chars().take(214).collect()
    }

    fn name_lookup_url(&self, name: &str) -> String {
        format!("{REGISTRY}/{}", encoded(name))
    }

    fn name_available(&self, name: &str, log: &Logger) -> Result<bool> {
        Ok(!http::exists(&self.name_lookup_url(name), log)?)
    }

    fn version_exists(&self, name: &str, version: &str, log: &Logger) -> Result<bool> {
        http::exists(&format!("{REGISTRY}/{}/{version}", encoded(name)), log)
    }

    fn project_info(&self, dir: &Path) -> Result<ProjectInfo> {
        let m = read_manifest(dir)?;
        let name = m["name"]
            .as_str()
            .ok_or_else(|| ShipError::new(Code::Precondition, "package.json has no \"name\""))?
            .to_string();
        let version = m["version"].as_str().map(str::to_string);
        let private = m["private"].as_bool().unwrap_or(false);
        let bin = match &m["bin"] {
            Value::String(_) => Some(unscoped(&name).to_string()),
            Value::Object(o) => o.keys().next().cloned(),
            _ => None,
        };
        Ok(ProjectInfo {
            publishable: version.is_some() && !private,
            version: version.unwrap_or_else(|| "0.0.0".into()),
            bin,
            repository: repo_url(&m["repository"]),
            description: m["description"].as_str().map(str::to_string),
            name,
        })
    }

    fn new_project_files(&self, p: &NewProject) -> Vec<(String, String)> {
        templates::project_files(p)
    }

    fn gitignore(&self) -> &'static str {
        "node_modules/\n*.tgz\n"
    }

    fn install_doc(&self, p: &NewProject) -> String {
        templates::install_doc(p)
    }

    fn usage_doc(&self, p: &NewProject) -> String {
        if p.cli {
            return crate::templates::cli_usage(unscoped(&p.name));
        }
        format!("```js\nimport {{ greet }} from '{}';\n\nconsole.log(greet('Ferris'));\n```\n", p.name)
    }

    fn checks(&self, dir: &Path) -> Vec<(&'static str, Cmd)> {
        vec![
            ("install", npm(&["install", "--no-audit", "--no-fund"], dir)),
            ("lint", npm(&["run", "lint", "--if-present"], dir)),
            ("typecheck", npm(&["run", "typecheck", "--if-present"], dir)),
            ("build", npm(&["run", "build", "--if-present"], dir)),
            ("test", npm(&["run", "test", "--if-present"], dir)),
        ]
    }

    fn package_files_plan(&self, dir: &Path) -> Vec<Cmd> {
        vec![npm(&["pack", "--dry-run", "--json", "--ignore-scripts"], dir)]
    }

    fn package_files(&self, dir: &Path, log: &Logger) -> Result<Vec<String>> {
        let out = proc::run_ok(&self.package_files_plan(dir)[0], log)?;
        parse_pack_json(&out.stdout)
    }

    fn generated_package_files(&self) -> &'static [&'static str] {
        &[]
    }

    fn dry_run_cmds(&self, dir: &Path) -> Vec<Cmd> {
        vec![npm(&["publish", "--dry-run", "--ignore-scripts", "--access", "public"], dir)]
    }

    fn set_version(&self, dir: &Path, _info: &ProjectInfo, new: &str) -> Result<Vec<(String, String)>> {
        let manifest = std::fs::read_to_string(dir.join("package.json"))?;
        let updated = json_lite::set_string(&manifest, &["version"], new)
            .ok_or_else(|| ShipError::new(Code::Precondition, "package.json has no top-level \"version\" string"))?;
        let mut files = vec![("package.json".to_string(), updated)];
        if let Ok(lock) = std::fs::read_to_string(dir.join("package-lock.json")) {
            let mut l = json_lite::set_string(&lock, &["version"], new).unwrap_or(lock);
            if let Some(x) = json_lite::set_string(&l, &["packages", "", "version"], new) {
                l = x;
            }
            files.push(("package-lock.json".to_string(), l));
        }
        Ok(files)
    }

    fn trusted_publishing(&self, package: &str, owner: &str, repo: &str, workflow: &str, _exists: bool) -> TrustedPublishing {
        TrustedPublishing {
            settings_url: format!("https://www.npmjs.com/package/{package}/access"),
            fields: vec![
                ("Publisher".into(), "GitHub Actions".into()),
                ("Organization or user".into(), owner.into()),
                ("Repository".into(), repo.into()),
                ("Workflow filename".into(), workflow.into()),
                ("Environment name".into(), "(leave empty)".into()),
            ],
            needs_existing_package: true,
            instructions: "Sign in to npmjs.com, open the package's Settings page, under Trusted Publisher choose GitHub Actions, enter the values exactly, then save.".into(),
        }
    }

    fn publish_cmds(&self, dir: &Path, _version: &str, _allow_dirty: bool) -> Vec<Cmd> {
        vec![npm(&["publish", "--access", "public"], dir)]
    }

    fn is_rate_limited(&self, output: &str) -> bool {
        let o = output.to_ascii_lowercase();
        o.contains("e429") || o.contains(" 429") || o.contains("too many requests") || o.contains("rate limit")
    }

    fn package_url(&self, name: &str, version: &str) -> String {
        format!("https://www.npmjs.com/package/{name}/v/{version}")
    }

    fn install_cmds(&self, name: &str, version: &str, root: &Path) -> Vec<Cmd> {
        let prefix = root.to_string_lossy().into_owned();
        vec![npm(&["install", "--prefix", &prefix, "--no-audit", "--no-fund", "--no-save", &format!("{name}@{version}")], root)]
    }

    fn installed_bin_cmd(&self, root: &Path, bin: &str) -> Cmd {
        let shim = if cfg!(windows) { format!("{bin}.cmd") } else { bin.to_string() };
        Cmd::new(&root.join("node_modules").join(".bin").join(shim).to_string_lossy(), &[])
    }

    fn library_smoke(&self, name: &str, version: &str, dir: &Path) -> Vec<Cmd> {
        let import = format!("import('{name}').then(() => console.log('loaded {name}'))");
        vec![
            npm(&["init", "-y"], dir),
            npm(&["install", "--no-audit", "--no-fund", &format!("{name}@{version}")], dir),
            Cmd::new("node", &["-e", &import]).cwd(dir).timeout(Duration::from_secs(120)),
        ]
    }

    fn build_release_cmds(&self, dir: &Path) -> Vec<Cmd> {
        vec![npm(&["install", "--no-audit", "--no-fund"], dir), npm(&["run", "build", "--if-present"], dir)]
    }

    fn built_bin_cmd(&self, dir: &Path, bin: &str) -> Cmd {
        let script = read_manifest(dir).ok().and_then(|m| match &m["bin"] {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get(bin).and_then(Value::as_str).map(str::to_string),
            _ => None,
        });
        let script = script.unwrap_or_else(|| format!("bin/{bin}.js"));
        Cmd::new("node", &[&dir.join(script).to_string_lossy()])
    }

    fn ci_workflow(&self, w: &WorkflowSpec) -> String {
        templates::ci_workflow(w)
    }

    fn release_workflow(&self, w: &WorkflowSpec) -> String {
        templates::release_workflow(w)
    }
}

/// File paths from `npm pack --dry-run --json`. npm 11 prints an array of
/// packages; npm 12 prints an object keyed by package name.
fn parse_pack_json(stdout: &str) -> Result<Vec<String>> {
    let start = stdout.find(['[', '{']).unwrap_or(0);
    let bad = |e: String| ShipError::new(Code::CommandFailed, format!("unexpected `npm pack --json` output: {e}"));
    let v: Value = serde_json::Deserializer::from_str(&stdout[start..])
        .into_iter::<Value>()
        .next()
        .ok_or_else(|| bad("no JSON".into()))?
        .map_err(|e| bad(e.to_string()))?;
    let package = match &v {
        Value::Array(a) => a.first(),
        Value::Object(o) => o.values().next(),
        _ => None,
    };
    let mut files: Vec<String> = package
        .and_then(|p| p["files"].as_array())
        .map(|a| a.iter().filter_map(|f| f["path"].as_str().map(|p| p.replace('\\', "/"))).collect())
        .unwrap_or_default();
    files.sort();
    Ok(files)
}

mod templates {
    use super::*;

    fn js_text(s: &str) -> String {
        s.replace('\\', "\\\\").replace('`', "'").replace("${", "$ {")
    }

    fn json_str(s: &str) -> String {
        serde_json::to_string(s).expect("strings serialize")
    }

    pub fn project_files(p: &NewProject) -> Vec<(String, String)> {
        let bin = unscoped(&p.name).to_string();
        let mut m = format!(
            "{{\n  \"name\": {},\n  \"version\": \"0.1.0\",\n  \"description\": {},\n  \"license\": \"MIT\",\n",
            json_str(&p.name),
            json_str(&p.description)
        );
        if let Some(a) = &p.author {
            m.push_str(&format!("  \"author\": {},\n", json_str(a)));
        }
        if let Some(url) = &p.repo_url {
            m.push_str(&format!(
                "  \"repository\": {{\n    \"type\": \"git\",\n    \"url\": {}\n  }},\n",
                json_str(&format!("git+{url}.git"))
            ));
        }
        m.push_str("  \"type\": \"module\",\n  \"main\": \"index.js\",\n  \"exports\": \"./index.js\",\n");
        if p.cli {
            m.push_str(&format!("  \"bin\": {{\n    {}: {}\n  }},\n", json_str(&bin), json_str(&format!("bin/{bin}.js"))));
            m.push_str("  \"files\": [\"index.js\", \"bin/\"],\n");
        } else {
            m.push_str("  \"files\": [\"index.js\"],\n");
        }
        m.push_str("  \"scripts\": {\n    \"test\": \"node --test\"\n  },\n  \"engines\": {\n    \"node\": \">=18\"\n  }");
        if p.target == Target::Repo {
            m.push_str(",\n  \"private\": true");
        }
        m.push_str("\n}\n");

        let index = "/** Returns a greeting for `name`. */\nexport function greet(name) {\n  return `Hello, ${name}!`;\n}\n";
        let test = "import { test } from 'node:test';\nimport assert from 'node:assert/strict';\nimport { greet } from '../index.js';\n\ntest('greets by name', () => {\n  assert.equal(greet('Node'), 'Hello, Node!');\n});\n";
        let mut files = vec![
            ("package.json".to_string(), m),
            ("index.js".to_string(), index.to_string()),
            ("test/index.test.js".to_string(), test.to_string()),
        ];
        if p.cli {
            files.push((format!("bin/{bin}.js"), cli_js(&bin, &p.description)));
        }
        files
    }

    fn cli_js(bin: &str, desc: &str) -> String {
        format!(
            r#"#!/usr/bin/env node
import {{ readFileSync }} from 'node:fs';
import {{ greet }} from '../index.js';

const pkg = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
const HELP = `{bin} - {desc}

Usage: {bin} [OPTIONS] [NAME]

Arguments:
  [NAME]  Who to greet [default: world]

Options:
  -h, --help     Print help
  -V, --version  Print version
`;

const [arg] = process.argv.slice(2);
if (arg === '-h' || arg === '--help') {{
  process.stdout.write(HELP);
}} else if (arg === '-V' || arg === '--version') {{
  console.log(`${{pkg.name}} ${{pkg.version}}`);
}} else if (arg && arg.startsWith('-')) {{
  console.error(`error: unexpected argument '${{arg}}'\n\n${{HELP}}`);
  process.exit(2);
}} else {{
  console.log(greet(arg ?? 'world'));
}}
"#,
            desc = js_text(desc)
        )
    }

    pub fn install_doc(p: &NewProject) -> String {
        let clone = match &p.repo_url {
            Some(url) => format!("git clone {url}\ncd {}\n", unscoped(&p.name)),
            None => format!("# clone the repository, then:\ncd {}\n", unscoped(&p.name)),
        };
        if p.target.has_package() {
            let cmd = if p.cli { format!("npm install -g {}", p.name) } else { format!("npm install {}", p.name) };
            format!("```text\n{cmd}\n```\n\n### From source\n\n```text\n{clone}npm install\n```\n")
        } else {
            format!("This project is not published to npm. Clone it and install its dependencies:\n\n```text\n{clone}npm install\n```\n")
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
      - uses: actions/setup-node@v7
        with:
          node-version: lts/*
      - run: npm install --no-audit --no-fund
      - run: npm run lint --if-present
      - run: npm run typecheck --if-present
      - run: npm run build --if-present
      - run: npm run test --if-present
"#
        )
    }

    pub fn release_workflow(w: &WorkflowSpec) -> String {
        let name = &w.name;
        let mut jobs = String::from(
            r#"  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: actions/setup-node@v7
        with:
          node-version: lts/*
      - name: Tag must match package.json
        shell: bash
        run: |
          version=$(node -p "require('./package.json').version")
          if [ "v$version" != "$GITHUB_REF_NAME" ]; then
            echo "::error::tag $GITHUB_REF_NAME does not match package.json version $version"
            exit 1
          fi
      - run: npm install --no-audit --no-fund
      - run: npm run build --if-present
      - run: npm run test --if-present
"#
        );
        let mut needs = vec!["test"];
        if w.publish {
            needs.push("publish");
            jobs.push_str(&format!(
                r#"
  publish:
    needs: test
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@v7
      - uses: actions/setup-node@v7
        with:
          node-version: 24
          registry-url: https://registry.npmjs.org
      - name: Trusted publishing needs npm 11.5.1 or later
        run: npm install -g npm@latest
      - name: Skip if this version is already on npm
        id: exists
        shell: bash
        run: |
          version=$(node -p "require('./package.json').version")
          if npm view "{name}@$version" version > /dev/null 2>&1; then
            echo "{name} $version is already on npm; skipping publish."
            echo "published=true" >> "$GITHUB_OUTPUT"
          else
            echo "published=false" >> "$GITHUB_OUTPUT"
          fi
      - run: npm install --no-audit --no-fund
        if: steps.exists.outputs.published == 'false'
      - name: Publish (trusted publishing, with provenance; no stored token)
        if: steps.exists.outputs.published == 'false'
        run: npm publish --access public
"#
            ));
        }
        let header = workflows::release_header(
            RELEASE_MARKER,
            "On a v* tag: test, publish to npm via trusted publishing (no stored token),\n# then create the GitHub release from CHANGELOG.md.",
        );
        header + &jobs + &workflows::github_release_job(&needs, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(target: Target, cli: bool) -> NewProject {
        NewProject {
            name: "@acme/hello-tool".into(),
            description: "Says `hi` ${x}".into(),
            author: Some("A <a@b.c>".into()),
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
        let r = Npm;
        assert!(r.validate_name("left-pad").is_ok());
        assert!(r.validate_name("@scope/pkg.js").is_ok());
        assert!(r.validate_name("Upper").is_err());
        assert!(r.validate_name("_private").is_err());
        assert!(r.validate_name("@scope").is_err());
        assert!(r.validate_name("fs").is_err());
        assert_eq!(r.sanitize_name("My Cool Tool"), "my-cool-tool");
        assert_eq!(r.sanitize_name("http"), "http-js");
        assert_eq!(r.name_lookup_url("@a/b"), "https://registry.npmjs.org/@a%2Fb");
    }

    #[test]
    fn generated_package_json_is_valid_and_reads_back() {
        let dir = crate::project::TempDir::new("ship-npm-ut").unwrap();
        for (path, content) in project_files_for(Target::Repo, true) {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let info = Npm.project_info(dir.path()).unwrap();
        assert_eq!(info.name, "@acme/hello-tool");
        assert_eq!(info.version, "0.1.0");
        assert!(!info.publishable, "private: true for repo-only");
        assert_eq!(info.bin.as_deref(), Some("hello-tool"));
        assert_eq!(info.repository.as_deref(), Some("https://github.com/acme/hello-tool"));
        let cli = std::fs::read_to_string(dir.path().join("bin/hello-tool.js")).unwrap();
        assert!(cli.contains("hello-tool - Says 'hi' $ {x}"));
    }

    fn project_files_for(target: Target, cli: bool) -> Vec<(String, String)> {
        Npm.new_project_files(&project(target, cli))
    }

    #[test]
    fn version_bump_touches_only_own_entries() {
        let dir = crate::project::TempDir::new("ship-npm-ut").unwrap();
        std::fs::write(dir.path().join("package.json"), "{\n  \"name\": \"x\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": {\"y\": \"1.0.0\"}\n}\n").unwrap();
        std::fs::write(
            dir.path().join("package-lock.json"),
            "{\n  \"name\": \"x\",\n  \"version\": \"1.0.0\",\n  \"packages\": {\n    \"\": {\"name\": \"x\", \"version\": \"1.0.0\"},\n    \"node_modules/y\": {\"version\": \"1.0.0\"}\n  }\n}\n",
        )
        .unwrap();
        let info = Npm.project_info(dir.path()).unwrap();
        let files = Npm.set_version(dir.path(), &info, "1.1.0").unwrap();
        assert!(files[0].1.contains("\"version\": \"1.1.0\""));
        assert!(files[0].1.contains("\"y\": \"1.0.0\""));
        assert_eq!(files[1].1.matches("1.1.0").count(), 2);
        assert!(files[1].1.contains("\"node_modules/y\": {\"version\": \"1.0.0\"}"));
    }

    #[test]
    fn parses_pack_output() {
        let v11 = "npm notice\n[{\"files\":[{\"path\":\"package.json\"},{\"path\":\"index.js\"}]}]";
        assert_eq!(parse_pack_json(v11).unwrap(), ["index.js", "package.json"]);
        let v12 = "{\n  \"x\": {\n    \"files\": [{\"path\": \"index.js\"}],\n    \"bundled\": []\n  }\n}\n";
        assert_eq!(parse_pack_json(v12).unwrap(), ["index.js"]);
    }

    #[test]
    fn release_uses_trusted_publishing() {
        let w = WorkflowSpec { name: "@acme/x".into(), private: false, publish: true, bin: None };
        let y = Npm.release_workflow(&w);
        assert!(y.starts_with(RELEASE_MARKER));
        assert!(y.contains("id-token: write"));
        assert!(y.contains("npm view \"@acme/x@$version\" version"));
        assert!(y.contains("needs: [test, publish]"));
        assert!(!y.contains("NODE_AUTH_TOKEN") && !y.contains("secrets."));
        let ci = Npm.ci_workflow(&w);
        assert!(ci.contains("name: test (${{ matrix.os }})"));
    }
}
