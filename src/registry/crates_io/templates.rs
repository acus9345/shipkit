//! File templates for Rust projects. Placeholders are `@@NAME@@` so they never
//! clash with GitHub Actions' `${{ }}` syntax.

use crate::registry::{workflows, NewProject, Target, WorkflowSpec, RELEASE_MARKER};

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut s = template.to_string();
    for (k, v) in vars {
        s = s.replace(&format!("@@{k}@@"), v);
    }
    s
}

fn crate_ident(name: &str) -> String {
    name.replace('-', "_")
}

fn toml_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn project_files(p: &NewProject) -> Vec<(String, String)> {
    let mut manifest = format!(
        "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"{}\"\nlicense = \"MIT\"\nreadme = \"README.md\"\n",
        p.name,
        toml_str(&p.description)
    );
    if let Some(a) = &p.author {
        manifest.push_str(&format!("authors = [\"{}\"]\n", toml_str(a)));
    }
    if let Some(url) = &p.repo_url {
        manifest.push_str(&format!("repository = \"{}\"\n", toml_str(url)));
    }
    if p.target == Target::Repo {
        manifest.push_str("# Repository-only project: never published to crates.io.\npublish = false\n");
    }
    manifest.push_str("\n[dependencies]\n");

    let ident = crate_ident(&p.name);
    let lib = fill(
        r#"//! @@DESC@@

/// Returns a greeting for `name`.
///
/// ```
/// assert_eq!(@@IDENT@@::greet("world"), "Hello, world!");
/// ```
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}
"#,
        &[("DESC", &p.description), ("IDENT", &ident)],
    );
    let test = fill(
        r#"#[test]
fn greets_by_name() {
    assert_eq!(@@IDENT@@::greet("Rust"), "Hello, Rust!");
}
"#,
        &[("IDENT", &ident)],
    );

    let mut files = vec![
        ("Cargo.toml".to_string(), manifest),
        ("src/lib.rs".to_string(), lib),
        ("tests/basic.rs".to_string(), test),
    ];
    if p.cli {
        files.push(("src/main.rs".to_string(), main_rs(p, &ident)));
    }
    files
}

fn main_rs(p: &NewProject, ident: &str) -> String {
    fill(
        r#"use std::process::ExitCode;

const HELP: &str = "\
@@NAME@@ - @@DESC@@

Usage: @@NAME@@ [OPTIONS] [NAME]

Arguments:
  [NAME]  Who to greet [default: world]

Options:
  -h, --help     Print help
  -V, --version  Print version
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(flag) if flag.starts_with('-') => {
            eprintln!("error: unexpected argument '{flag}'\n\n{HELP}");
            ExitCode::from(2)
        }
        name => {
            println!("{}", @@IDENT@@::greet(name.unwrap_or("world")));
            ExitCode::SUCCESS
        }
    }
}
"#,
        &[("NAME", &p.name), ("DESC", &p.description.replace('\\', "\\\\").replace('"', "\\\"")), ("IDENT", ident)],
    )
}

pub fn install_doc(p: &NewProject) -> String {
    let clone = match &p.repo_url {
        Some(url) => format!("git clone {url}\ncd {}\n", p.name),
        None => format!("# clone the repository, then:\ncd {}\n", p.name),
    };
    let mut s = String::new();
    if p.target.has_package() {
        if p.cli {
            s.push_str(&format!("```bash\ncargo install {}\n```\n\n", p.name));
        } else {
            s.push_str(&format!("```bash\ncargo add {}\n```\n\n", p.name));
        }
        s.push_str("### Build from source\n\n");
    } else {
        s.push_str("This project is not published to a package registry. Clone it and build it with Cargo:\n\n");
    }
    s.push_str(&format!("```\n{clone}cargo build --release\n```\n"));
    if p.cli {
        s.push_str(&format!("\nThe binary is written to `target/release/{}` (`.exe` on Windows).\n", p.name));
    }
    s
}

pub fn ci_workflow(w: &WorkflowSpec) -> String {
    let os = if w.private { "ubuntu-latest" } else { "ubuntu-latest, windows-latest" };
    fill(
        r#"name: CI

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

jobs:
  test:
    name: test (${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [@@OS@@]
    steps:
      - uses: actions/checkout@v7
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - name: Lint
        run: cargo clippy --all-targets -- -D warnings
      - name: Build
        run: cargo build --all-targets
      - name: Test
        run: cargo test
"#,
        &[("OS", os)],
    )
}

pub fn release_workflow(w: &WorkflowSpec) -> String {
    let mut needs = vec!["test"];
    let mut jobs = fill(TEST_JOB, &[("NAME", &w.name)]);
    if let Some(bin) = &w.bin {
        needs.push("build");
        let matrix = if w.private {
            "          - os: ubuntu-latest\n            target: x86_64-unknown-linux-musl\n"
        } else {
            "          - os: windows-latest\n            target: x86_64-pc-windows-msvc\n          - os: ubuntu-latest\n            target: x86_64-unknown-linux-musl\n"
        };
        jobs.push_str(&fill(BUILD_JOB, &[("MATRIX", matrix.trim_end_matches('\n')), ("BIN", bin)]));
    }
    if w.publish {
        needs.push("publish");
        jobs.push_str(&fill(PUBLISH_JOB, &[("NAME", &w.name)]));
    }
    let header = workflows::release_header(
        RELEASE_MARKER,
        "On a v* tag: test, build, publish via trusted publishing (no stored token),\n# then create the GitHub release from CHANGELOG.md.",
    );
    header + &jobs + &workflows::github_release_job(&needs, w.bin.is_some().then_some("dist-*"))
}

const TEST_JOB: &str = r#"  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - name: Tag must match Cargo.toml
        shell: bash
        run: |
          version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "@@NAME@@") | .version')
          if [ "v$version" != "$GITHUB_REF_NAME" ]; then
            echo "::error::tag $GITHUB_REF_NAME does not match Cargo.toml version $version"
            exit 1
          fi
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test
"#;

const BUILD_JOB: &str = r#"
  build:
    needs: test
    name: build (${{ matrix.target }})
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
@@MATRIX@@
    steps:
      - uses: actions/checkout@v7
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - name: Install musl tools
        if: contains(matrix.target, 'musl')
        run: sudo apt-get update && sudo apt-get install -y musl-tools
      - name: Build
        run: cargo build --release --target ${{ matrix.target }}
      - name: Package
        shell: bash
        run: |
          mkdir dist
          bin="@@BIN@@"
          archive="$bin-$GITHUB_REF_NAME-${{ matrix.target }}"
          if [[ "${{ matrix.target }}" == *windows* ]]; then
            cp "target/${{ matrix.target }}/release/$bin.exe" .
            7z a "dist/$archive.zip" "$bin.exe" README.md LICENSE
          else
            cp "target/${{ matrix.target }}/release/$bin" .
            tar czf "dist/$archive.tar.gz" "$bin" README.md LICENSE
          fi
      - uses: actions/upload-artifact@v7
        with:
          name: dist-${{ matrix.target }}
          path: dist/*
"#;

const PUBLISH_JOB: &str = r#"
  publish:
    needs: test
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@v7
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
      - name: Skip if this version is already on crates.io
        id: exists
        shell: bash
        run: |
          version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "@@NAME@@") | .version')
          code=$(curl -s -o /dev/null -w '%{http_code}' -A "release-workflow (github.com/$GITHUB_REPOSITORY)" "https://crates.io/api/v1/crates/@@NAME@@/$version")
          if [ "$code" = "200" ]; then
            echo "@@NAME@@ $version is already on crates.io; skipping publish."
            echo "published=true" >> "$GITHUB_OUTPUT"
          else
            echo "published=false" >> "$GITHUB_OUTPUT"
          fi
      - name: Get a short-lived crates.io token (trusted publishing)
        if: steps.exists.outputs.published == 'false'
        id: auth
        uses: rust-lang/crates-io-auth-action@v1
      - name: Publish
        if: steps.exists.outputs.published == 'false'
        run: cargo publish
        env:
          CARGO_REGISTRY_TOKEN: ${{ steps.auth.outputs.token }}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(private: bool, publish: bool, bin: Option<&str>) -> WorkflowSpec {
        WorkflowSpec { name: "demo".into(), private, publish, bin: bin.map(String::from) }
    }

    #[test]
    fn private_ci_is_ubuntu_only() {
        assert!(ci_workflow(&spec(true, true, None)).contains("os: [ubuntu-latest]\n"));
        assert!(ci_workflow(&spec(false, true, None)).contains("windows-latest"));
    }

    #[test]
    fn release_has_no_stored_tokens() {
        let y = release_workflow(&spec(false, true, Some("demo")));
        assert!(y.starts_with(RELEASE_MARKER));
        assert!(y.contains("id-token: write"));
        assert!(y.contains("crates-io-auth-action"));
        assert!(!y.contains("secrets."));
        assert!(y.contains("needs: [test, build, publish]"));
        assert!(y.contains("--notes-file notes.md dist/*"));
        assert!(!y.contains("@@"));
    }

    #[test]
    fn repo_only_release_skips_publish() {
        let y = release_workflow(&spec(false, false, None));
        assert!(!y.contains("cargo publish"));
        assert!(!y.contains("download-artifact"));
        assert!(y.contains("needs: [test]"));
    }

    fn project(target: Target, cli: bool) -> NewProject {
        NewProject {
            name: "my-tool".into(),
            description: "A \"quoted\" tool".into(),
            author: Some("A <a@b.c>".into()),
            repo_url: Some("https://github.com/o/my-tool".into()),
            target,
            cli,
            private: false,
            year: "2026".into(),
            date: "2026-10-25".into(),
        }
    }

    #[test]
    fn repo_only_manifest_blocks_publishing() {
        let files = project_files(&project(Target::Repo, false));
        let manifest = &files[0].1;
        assert!(manifest.contains("publish = false"));
        assert!(manifest.contains("description = \"A \\\"quoted\\\" tool\""));
        assert!(install_doc(&project(Target::Repo, false)).contains("not published"));
    }

    #[test]
    fn cli_adds_main() {
        let files = project_files(&project(Target::Both, true));
        assert!(files.iter().any(|(p, c)| p == "src/main.rs" && c.contains("my_tool::greet")));
        assert!(!files[0].1.contains("publish = false"));
    }
}
