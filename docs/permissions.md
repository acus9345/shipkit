# Recommended Claude Code permissions for `ship`

Add these to `.claude/settings.json` in a project (or `~/.claude/settings.json` for
all projects). They let Claude run the read-only and local steps without prompting,
make every public or irreversible step ask you first, and stop Claude from doing
the dangerous things `ship` refuses to do by going around it.

```json
{
  "permissions": {
    "allow": [
      "Bash(ship --version)",
      "Bash(ship preflight *)",
      "Bash(ship self-test)",
      "Bash(ship check-names *)",
      "Bash(ship scan-secrets *)",
      "Bash(ship checks *)",
      "Bash(ship inspect *)",
      "Bash(ship watch *)",
      "Bash(ship scaffold *)",
      "Bash(ship add-workflows *)",
      "Bash(ship bump *)",
      "Bash(ship commit *)",
      "Bash(ship verify *)",
      "Bash(ship trusted-publishing *)"
    ],
    "ask": [
      "Bash(ship create-repo *)",
      "Bash(ship release *)",
      "Bash(ship publish *)"
    ],
    "deny": [
      "Bash(cargo publish *)",
      "Bash(cargo yank *)",
      "Bash(cargo owner *)",
      "Bash(gh repo create *)",
      "Bash(gh repo delete *)",
      "Bash(gh repo edit *)",
      "Bash(gh release create *)",
      "Bash(gh release delete *)",
      "Bash(gh secret *)",
      "Bash(git push --force *)",
      "Bash(git push -f *)",
      "Bash(git push --delete *)",
      "Bash(git tag -d *)",
      "Bash(git tag -f *)",
      "Bash(npm publish *)",
      "Bash(npm unpublish *)",
      "Bash(uv publish *)",
      "Bash(twine upload *)"
    ]
  }
}
```

How the layers fit together:

- **`ask` on the three gated steps**: Claude Code asks you before each one, even with `--confirm`.
- **The plugin's PreToolUse hook**: blocks `ship create-repo`, `ship release` and `ship publish` without
  `--confirm` (or `--dry-run`), so Claude has to show you the plan first.
- **`ship` itself**: refuses gated steps without `--confirm`, refuses outward steps on a dirty tree,
  and runs the secret scan and the registry dry run first.
- **`deny`**: keeps Claude from running the raw `cargo publish`, `npm publish`, `uv publish`, `gh repo create`, `gh release create`
  or tag/force-push commands directly. `ship` runs those as child processes, which these rules do not
  affect, so it keeps working.

If you use the PowerShell tool on Windows, repeat the `allow`, `ask` and `deny` entries with
`PowerShell(...)` in place of `Bash(...)`.
