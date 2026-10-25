# shipkit

Take a project from a local folder to a public GitHub repo and a crates.io, npm or PyPI release, safely.

`ship` is one small CLI. It runs every step of the release as a separate command that prints one
JSON object, so Claude Code (or a script, or you) can drive it one step at a time. The Claude Code
plugin adds a skill that knows the order of the steps, a hook that blocks the risky ones until you
say yes, and a `/ship-setup` command. There is no MCP server.

- **Safe by default.** Creating a repo, tagging a release and publishing are refused without
  `--confirm`. Every step has a `--dry-run` that prints exactly what it would run.
- **No tokens.** `ship` uses your existing `gh`, `cargo`, `npm` or `uv` logins. Generated release
  workflows publish with trusted publishing (OIDC), so the repo stores no secrets.
- **Never overwrites.** `scaffold` and `add-workflows` only create missing files. Tags are never
  moved or deleted. Running any step twice is safe.
- **Checks before anything leaves your machine.** A secret scan and the registry dry run must pass
  before a commit, repo creation, release or publish.
- **No telemetry.** The only network calls are the ones a step needs (GitHub and the registry). Every
  action is logged locally, with secrets redacted.

## Install

With Rust installed:

```text
cargo install shipkit
```

Or download a prebuilt binary (Windows x64, Linux x64) from the
[releases page](https://github.com/acus9345/shipkit/releases), unpack it, and put `ship`
(`ship.exe` on Windows) on your `PATH`.

Check it:

```bash
ship --version
ship --help
ship self-test
```

`ship` needs `git` and the GitHub CLI `gh` (logged in with `gh auth login`), plus the tool for
your registry. `ship preflight` checks all of them and never starts a login itself.

## Registries

The registry is detected from the manifest (`Cargo.toml`, `package.json` or `pyproject.toml`).
For a new project, pass `--registry crates.io|npm|pypi` to `scaffold`.

| Registry | Tool | Checks | Dry run | Trusted publishing |
|---|---|---|---|---|
| crates.io | `cargo` | clippy, build, test | `cargo publish --dry-run` | Needs the crate to exist: publish the first version with `ship publish`. |
| npm | `node`, `npm` | install, `lint`/`typecheck`/`build`/`test` scripts if present | `npm publish --dry-run` | Needs the package to exist: publish the first version with `ship publish`. |
| PyPI | `uv` | compile, `uv build`, unittest | `uv build` + `uv publish --dry-run` | Works before the first release (pending publisher), so CI can publish 0.1.0. |

Repo-only projects block publishing the registry's own way: `publish = false` in `Cargo.toml`,
`"private": true` in `package.json`, or the `Private :: Do Not Upload` classifier in
`pyproject.toml`.

### Claude Code plugin

```text
/plugin marketplace add acus9345/shipkit
/plugin install shipkit@shipkit
/shipkit:ship-setup
```

`/ship-setup` runs `ship preflight` and `ship self-test` and offers the recommended permission
rules from [docs/permissions.md](docs/permissions.md). With those rules, Claude runs the local
steps without asking, asks you before `create-repo`, `release` and `publish`, and can't run raw
`cargo publish`, `npm publish` or force-push.

Then ask Claude something like *"ship this project to GitHub and crates.io"* (or npm, or PyPI).

## Steps

Run them in order. Each one stops at the first problem and explains it.

| Step | Risk | What it does |
|---|---|---|
| `preflight` | low | Are git, gh and the registry's tools installed? Which accounts are logged in? What is the git identity? Run this first. |
| `check-names` | low | Is the package name valid and free on the registry, and is the repo name free on GitHub? Suggests the best free alternative if not. |
| `scaffold` | medium | Creates a standard project: sources, tests, README, LICENSE, CHANGELOG, `.gitignore`, `.gitattributes`, and a manifest at 0.1.0 with your git identity. `--cli` adds `--help`/`--version`. `--target repo` blocks publishing. `--repo-url` adds workflows. `--private` runs CI on Ubuntu only. |
| `scan-secrets` | low | Finds `.env` files, private keys and tokens in everything that would be committed. Reports file and line, never the secret. |
| `checks` | low | The registry's lint, typecheck, build and test steps (see Registries); stops at the first failure and returns its output. |
| `inspect` | low | The registry's publish dry run, the list of files that would be published, and any junk or secrets among them. |
| `commit` | medium | `git init` on `main` if needed, refuses if secrets or `.env` files would be committed, then commits. |
| `create-repo` | **high** | Creates the GitHub repo (public unless `--private`) and pushes, or just pushes if it is already `origin`. `--topics` adds topics. |
| `add-workflows` | medium | `ci.yml` (lint, build, test on push and PR) and `release.yml` (on `v*` tags: test, build, publish by trusted publishing unless the version exists, GitHub release from the CHANGELOG with the binaries attached). |
| `trusted-publishing` | medium | Opens the registry's trusted-publishing settings page and prints the exact values to enter. You click Save. |
| `bump` | medium | Bumps the version (patch/minor/major) and records it in the CHANGELOG from `--notes` or the Unreleased section. Does not commit. |
| `release` | **high** | Tags `v<version>`, pushes the branch and the tag, and creates the GitHub release unless `release.yml` does. |
| `watch` | low | Waits up to about 45 s for the workflow runs of the current commit; on failure, returns the failing log. Call it again while runs are in progress. |
| `verify` | medium | In a fresh temp folder, installs the package from the real registry (or clones the repo) and runs the README's commands one at a time. |
| `publish` | **high** | Publishes from your terminal (`cargo publish`, `npm publish` or `uv publish`); skipped if the version exists. After a rate limit (HTTP 429) it refuses to retry for 24 hours unless `--force`. |

crates.io and npm accept a trusted publisher only for a package that already exists, so there the
first version goes out with `ship publish`. PyPI accepts a pending publisher, so `ship release`
can publish even the first version. After that, `ship release` pushes a tag and `release.yml`
publishes.

## Demo

A typical first release, with the JSON shortened:

```text
$ ship preflight
{"v":1,"ok":true,"step":"preflight","risk":"low","dry_run":false,"data":{"ready":true,"problems":[],
 "tools":{"cargo":{"installed":true,...},"gh":{...},"git":{...}},"github":{"accounts":[{"login":"you",...}]},...}}

$ ship check-names --name ship
{"v":1,"ok":true,...,"data":{"all_free":false,"package":{"name":"ship","available":false,...},
 "repo":{"name":"ship","available":true,...},"suggestion":{"package":"ship-cli","repo":"ship-cli"}}}

$ ship scaffold --name hello-ship --cli --description "Says hello" --repo-url https://github.com/you/hello-ship
{"v":1,"ok":true,...,"data":{"created":["Cargo.toml","src/lib.rs","tests/basic.rs","src/main.rs","README.md",
 "LICENSE","CHANGELOG.md",".gitignore",".gitattributes",".github/workflows/ci.yml",".github/workflows/release.yml"]}}

$ ship scan-secrets && ship checks && ship inspect && ship commit
...

$ ship create-repo
{"v":1,"ok":false,"step":"create-repo","error":{"code":"E_CONFIRM_REQUIRED",
 "message":"`create-repo` is public or irreversible. Review the plan, then re-run with --confirm ...",
 "details":{"plan":{"actions":[
   {"kind":"skip","what":"secret scan","reason":"passed"},
   {"kind":"run","cmd":"gh repo create you/hello-ship --public --source=. --remote=origin --push",...}]}}}}

$ ship create-repo --confirm
{"v":1,"ok":true,"step":"create-repo","risk":"high",...,"data":{"repo":"you/hello-ship","visibility":"public","created":true,...}}

$ ship publish --confirm          # first version only
$ ship trusted-publishing         # opens the registry's settings; you click Save
$ ship release --confirm          # tag v0.1.0; release.yml builds and publishes the GitHub release
$ ship watch                      # call until "done": true
$ ship verify                     # install from the registry in a temp folder and run the README commands
```

## How it talks

Every command prints exactly one JSON object on stdout.

```text
success  {"v":1,"ok":true,"step":"<step>","risk":"low|medium|high","dry_run":false,"data":{...}}
failure  {"v":1,"ok":false,"step":"<step>","error":{"code":"E_...","message":"...","details":{...}}}
```

- Exit codes: `0` success, `1` failure, `2` bad usage or input, `3` refused for lack of `--confirm`.
- Error codes are stable: `E_USAGE`, `E_INPUT`, `E_CONFIRM_REQUIRED`, `E_DIRTY_TREE`,
  `E_TOOL_MISSING`, `E_COMMAND_FAILED`, `E_IO`, `E_NOT_FOUND`, `E_ALREADY_EXISTS`,
  `E_SECRETS_FOUND`, `E_CHECK_FAILED`, `E_PRECONDITION`, `E_RATE_LIMITED`, `E_COOLDOWN`,
  `E_TIMEOUT`, `E_INTERNAL`.
- Step arguments come from flags, or from one JSON object with `--input '{"name":"x"}'`
  (also a file path, or `-` for stdin). Unknown fields are rejected. `--confirm` and `--dry-run`
  are only ever read from the command line, so a hook can always see them.
- `--dir <path>` runs a step on another folder.
- Actions are logged as JSON lines to `%LOCALAPPDATA%\ship\ship.log` on Windows, or
  `$XDG_STATE_HOME/ship/ship.log` (default `~/.local/state/ship/ship.log`) elsewhere.

## Safety model

Several separate layers, so no single mistake publishes anything:

1. **`ship` itself** refuses `create-repo`, `release` and `publish` without `--confirm`. Before any
   of them, it refuses a dirty working tree (unless `--allow-dirty`) and runs the secret scan and
   the registry dry run.
2. **The plugin's PreToolUse hook** (on Bash and PowerShell) blocks those three commands without
   `--confirm` or `--dry-run`, so Claude has to show you the plan first. `--confirm` only counts in
   the same command; `ship preflight --confirm && ship publish` is still blocked.
3. **The skill** tells Claude to dry-run, show you the plan, and wait for your yes before each
   gated step.
4. **Your permission rules** ([docs/permissions.md](docs/permissions.md)) make Claude Code ask
   before each gated step and deny raw `cargo publish`, `npm publish`, `uv publish`, `gh repo create` and force-pushes.

READMEs, CI logs and web pages are treated as data. `verify` only runs README lines in
`bash`/`sh` blocks that start with the project's own binary, use no shell syntax, and don't look
outward (no `--confirm`, `publish`, `push`, `delete`, ...). It runs them without a shell.

## Development

```text
cargo clippy --all-targets -- -D warnings
cargo test
```

Integration tests run entirely offline in temp folders. GitHub URLs are rewritten to local bare
repositories, `gh` gets a dummy token and a dead proxy, and nothing is ever published.

The registry layer is the `Registry` trait in `src/registry/`, implemented for crates.io, npm
and PyPI. Adding another registry means implementing it; the steps don't change.

## License

MIT. See [LICENSE](LICENSE).
