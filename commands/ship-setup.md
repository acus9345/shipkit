---
description: Check that the ship CLI is installed and ready, then run its offline self-test
allowed-tools: Bash(ship --version) Bash(ship preflight) Bash(ship self-test) Read(${CLAUDE_PLUGIN_ROOT}/docs/permissions.md)
---

Set up and check the `ship` publishing CLI for this machine.

1. Run `ship --version`. If the command is not found, stop and tell the user to install it with one of:
   - `cargo install shipkit`
   - or download the binary for their platform from https://github.com/acus9345/shipkit/releases and put it on PATH

   The plugin's safety hook also needs `ship` on PATH.
2. Run `ship preflight`. It checks git, gh and cargo, the logged-in accounts and the git identity, and starts no logins.
   For each entry in `data.problems` or `data.warnings`, tell the user the exact command to run themselves. Never ask for a token or password.
3. Run `ship self-test`. It checks offline that gated steps are refused without `--confirm`, that the hook blocks them,
   that secrets are redacted, and that the secret scan finds planted secrets in a temp folder.
4. Reply with a short table: tool versions, GitHub account, crates.io login (yes/no), git identity, and self-test result.
5. Read `${CLAUDE_PLUGIN_ROOT}/docs/permissions.md` and offer its recommended permission rules (allow the local steps,
   ask before create-repo/release/publish, deny raw publish/force-push commands). Don't read or change the user's
   settings files unless they ask you to add the rules.
