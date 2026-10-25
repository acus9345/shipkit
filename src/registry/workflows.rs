//! Workflow pieces shared by every registry: the job that creates the GitHub
//! release from CHANGELOG.md, attaching any built artifacts.

/// Steps that download the artifacts uploaded under `pattern` into `dist/`.
fn download_step(pattern: &str) -> String {
    format!(
        "      - uses: actions/download-artifact@v8\n        with:\n          pattern: {pattern}\n          path: dist\n          merge-multiple: true\n"
    )
}

const UPLOAD_EXISTING: &str = r#"            # Attach any missing files; existing assets are never replaced.
            for f in dist/*; do
              gh release upload "$GITHUB_REF_NAME" "$f" || echo "$f is already attached; left unchanged."
            done
"#;

/// The `github-release` job. `artifacts` is the upload-artifact name pattern
/// whose files get attached, if any.
pub fn github_release_job(needs: &[&str], artifacts: Option<&str>) -> String {
    let (download, upload, assets) = match artifacts {
        Some(p) => (download_step(p), UPLOAD_EXISTING, " dist/*"),
        None => (String::new(), "", ""),
    };
    format!(
        r#"
  github-release:
    needs: [{needs}]
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v7
{download}      - name: Release notes from CHANGELOG.md
        shell: bash
        run: |
          version="${{GITHUB_REF_NAME#v}}"
          awk -v v="$version" '
            /^## \[/ {{ if (found) exit; if (index($0, "[" v "]")) {{ found = 1; next }} }}
            found {{ print }}
          ' CHANGELOG.md > notes.md
          [ -s notes.md ] || echo "Release $GITHUB_REF_NAME" > notes.md
      - name: Create the GitHub release (never modifies an existing one)
        shell: bash
        env:
          GH_TOKEN: ${{{{ github.token }}}}
        run: |
          if gh release view "$GITHUB_REF_NAME" > /dev/null 2>&1; then
            echo "Release $GITHUB_REF_NAME already exists."
{upload}          else
            gh release create "$GITHUB_REF_NAME" --verify-tag --title "$GITHUB_REF_NAME" --notes-file notes.md{assets}
          fi
"#,
        needs = needs.join(", ")
    )
}

/// Header of every release workflow: the marker, trigger and permissions.
pub fn release_header(marker: &str, summary: &str) -> String {
    format!("{marker}\n# {summary}\nname: Release\n\non:\n  push:\n    tags: ['v*']\n\npermissions:\n  contents: read\n\njobs:\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_with_and_without_assets() {
        let with = github_release_job(&["test", "build"], Some("dist-*"));
        assert!(with.contains("needs: [test, build]"));
        assert!(with.contains("pattern: dist-*"));
        assert!(with.contains("--notes-file notes.md dist/*"));
        assert!(with.contains("GH_TOKEN: ${{ github.token }}"));
        assert!(with.contains("version=\"${GITHUB_REF_NAME#v}\""));
        let without = github_release_job(&["test"], None);
        assert!(!without.contains("download-artifact"));
        assert!(without.contains("--notes-file notes.md\n"));
    }
}
