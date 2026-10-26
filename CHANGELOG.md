# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `ship login gh|npm|cargo|pypi`: opens the tool's official login in a visible window and waits for you to finish it there. ship never sees what you enter, and skips tools already logged in.
- `ship install-tool gh|git|node|uv` (gated): installs a missing tool with winget on Windows or Homebrew on macOS.

### Fixed

- `ship publish` to npm with 2FA: when npm asks for approval, the publish re-runs in a window where you approve it in the browser, instead of failing.
- PyPI terminal publish now finds a `uv auth login` credential, and uploads with twine when the credential is in `.pypirc` or `TWINE_PASSWORD`, which `uv publish` does not read.

## [0.2.0] - 2026-10-25

### Added

- npm support: package.json projects, `npm` checks and dry runs, trusted publishing with provenance in the release workflow, and `verify` for both libraries and CLIs.
- PyPI support through `uv`: pyproject.toml projects (hatchling), `uv build` and `uv publish --dry-run`, a pending trusted publisher so even the first release can publish from CI, and `verify` in a fresh virtual environment.
- `--registry crates.io|npm|pypi` on every step. The registry is detected from the manifest when it is left out.

### Changed

- The release workflows of all registries share one GitHub-release job.
- `add-workflows` reports `executable` in place of `attach_binary`.

## [0.1.0] - 2026-10-25

### Added

- `ship` CLI with 15 steps, from `preflight` to `publish`. Every step prints one JSON object with stable error codes, supports `--dry-run`, and logs locally with secrets redacted.
- Gating: `create-repo`, `release` and `publish` are refused without `--confirm`, refuse a dirty working tree, and run the secret scan and the registry dry run first.
- crates.io support behind a `Registry` trait. Generated release workflows publish through trusted publishing (OIDC) with no stored tokens.
- A 24-hour publish cooldown after a crates.io rate limit (HTTP 429).
- Claude Code plugin: the `ship` skill, a PreToolUse hook that blocks gated commands without `--confirm`, and `/ship-setup`.
- `ship self-test` for an offline check of the safety machinery.
