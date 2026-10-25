# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-10-25

### Added

- `ship` CLI with 15 steps, from `preflight` to `publish`. Every step prints one JSON object with stable error codes, supports `--dry-run`, and logs locally with secrets redacted.
- Gating: `create-repo`, `release` and `publish` are refused without `--confirm`, refuse a dirty working tree, and run the secret scan and the registry dry run first.
- crates.io support behind a `Registry` trait. Generated release workflows publish through trusted publishing (OIDC) with no stored tokens.
- A 24-hour publish cooldown after a crates.io rate limit (HTTP 429).
- Claude Code plugin: the `ship` skill, a PreToolUse hook that blocks gated commands without `--confirm`, and `/ship-setup`.
- `ship self-test` for an offline check of the safety machinery.
