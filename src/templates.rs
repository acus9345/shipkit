//! Registry-independent project files: README, LICENSE, CHANGELOG, git config.

use crate::registry::{NewProject, Registry};

pub fn readme(p: &NewProject, reg: &dyn Registry) -> String {
    let mut s = format!("# {}\n\n{}\n\n## Install\n\n{}\n", p.name, p.description, reg.install_doc(p));
    s.push_str("## Usage\n\n");
    if p.cli {
        s.push_str(&format!("```bash\n{0} --help\n{0} --version\n{0} Ferris\n```\n\n", p.name));
    } else {
        s.push_str(&format!(
            "```rust\nfn main() {{\n    println!(\"{{}}\", {}::greet(\"Ferris\"));\n}}\n```\n\n",
            p.name.replace('-', "_")
        ));
    }
    s.push_str("## License\n\nMIT. See [LICENSE](LICENSE).\n");
    s
}

pub fn license_mit(year: &str, holder: &str) -> String {
    format!(
        "MIT License

Copyright (c) {year} {holder}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"
    )
}

pub fn changelog(date: &str) -> String {
    format!(
        "# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - {date}

### Added

- Initial release.
"
    )
}

pub fn gitignore(reg: &dyn Registry) -> String {
    format!("{}\n# Never commit secrets\n.env\n.env.*\n!.env.example\n*.pem\n*.key\n\n# Editor and OS files\n.DS_Store\nThumbs.db\n.idea/\n.vscode/\n*.log\n", reg.gitignore().trim_end())
}

pub const GITATTRIBUTES: &str = "* text=auto eol=lf\n*.bat text eol=crlf\n*.cmd text eol=crlf\n*.ps1 text eol=crlf\n";
