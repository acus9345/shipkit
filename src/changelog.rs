//! Keep a Changelog files: read a version's notes, and record a release.

use crate::error::{Code, Result, ShipError};

pub const HEADER: &str = "# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]
";

/// Line ranges of `## ` sections: (title, header line index, end line index).
fn sections(lines: &[&str]) -> Vec<(String, usize, usize)> {
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(rest) = l.strip_prefix("## ") {
            if let Some(last) = out.last_mut() {
                last.2 = i;
            }
            out.push((rest.trim().to_string(), i, lines.len()));
        } else if l.starts_with('[') && l.contains("]: ") {
            // Link reference definitions at the bottom end the last section.
            if let Some(last) = out.last_mut() {
                if last.2 == lines.len() {
                    last.2 = i;
                }
            }
        }
    }
    out
}

fn title_is(title: &str, name: &str) -> bool {
    let t = title.trim_start_matches('[');
    t.strip_prefix(name).is_some_and(|rest| rest.starts_with(']') || rest.is_empty() || rest.starts_with(' '))
}

fn body(lines: &[&str], start: usize, end: usize) -> String {
    lines[start + 1..end].join("\n").trim().to_string()
}

/// Notes recorded for `version`, if the section exists and is not empty.
pub fn notes_for(text: &str, version: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    sections(&lines)
        .into_iter()
        .find(|(t, _, _)| title_is(t, version))
        .map(|(_, s, e)| body(&lines, s, e))
        .filter(|b| !b.is_empty())
}

pub fn has_version(text: &str, version: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    sections(&lines).iter().any(|(t, _, _)| title_is(t, version))
}

/// Record `version` dated `date`. Uses `notes` if given (each plain line
/// becomes a bullet); otherwise moves the Unreleased section's content.
pub fn record_release(text: &str, version: &str, date: &str, notes: Option<&str>) -> Result<String> {
    if has_version(text, version) {
        return Err(ShipError::new(Code::AlreadyExists, format!("CHANGELOG.md already has a section for {version}")));
    }
    let lines: Vec<&str> = text.lines().collect();
    let secs = sections(&lines);
    let unreleased = secs.iter().find(|(t, _, _)| title_is(t, "Unreleased")).cloned();

    let (new_body, clear_unreleased) = match notes {
        Some(n) if !n.trim().is_empty() => (format_notes(n), false),
        _ => match &unreleased {
            Some((_, s, e)) if !body(&lines, *s, *e).is_empty() => (body(&lines, *s, *e), true),
            _ => {
                return Err(ShipError::new(
                    Code::Precondition,
                    "no release notes: pass --notes or add entries under ## [Unreleased] in CHANGELOG.md",
                ))
            }
        },
    };
    let new_section = format!("## [{version}] - {date}\n\n{new_body}\n");

    let mut out: Vec<String> = Vec::new();
    match unreleased {
        Some((_, s, e)) => {
            out.extend(lines[..=s].iter().map(|l| l.to_string()));
            out.push(String::new());
            if !clear_unreleased && !body(&lines, s, e).is_empty() {
                out.push(body(&lines, s, e));
                out.push(String::new());
            }
            out.push(new_section);
            out.extend(lines[e..].iter().map(|l| l.to_string()));
        }
        None => {
            let insert_at = secs.first().map(|(_, s, _)| *s).unwrap_or(lines.len());
            out.extend(lines[..insert_at].iter().map(|l| l.to_string()));
            if out.last().is_some_and(|l| !l.is_empty()) {
                out.push(String::new());
            }
            out.push(new_section);
            out.extend(lines[insert_at..].iter().map(|l| l.to_string()));
        }
    }
    let mut s = out.join("\n");
    while s.contains("\n\n\n") {
        s = s.replace("\n\n\n", "\n\n");
    }
    Ok(s.trim_end().to_string() + "\n")
}

fn format_notes(notes: &str) -> String {
    notes
        .trim()
        .lines()
        .map(|l| {
            let t = l.trim();
            if t.is_empty() || t.starts_with('-') || t.starts_with('*') || t.starts_with('#') {
                t.to_string()
            } else {
                format!("- {t}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "# Changelog\n\nIntro.\n\n## [Unreleased]\n\n### Fixed\n\n- A bug.\n\n## [0.1.0] - 2026-10-25\n\n### Added\n\n- Initial release.\n";

    #[test]
    fn reads_notes() {
        assert_eq!(notes_for(LOG, "0.1.0").unwrap(), "### Added\n\n- Initial release.");
        assert!(notes_for(LOG, "0.2.0").is_none());
        assert!(has_version(LOG, "0.1.0"));
        assert!(!has_version(LOG, "0.1"));
    }

    #[test]
    fn moves_unreleased_into_new_section() {
        let out = record_release(LOG, "0.1.1", "2026-10-26", None).unwrap();
        assert_eq!(
            out,
            "# Changelog\n\nIntro.\n\n## [Unreleased]\n\n## [0.1.1] - 2026-10-26\n\n### Fixed\n\n- A bug.\n\n## [0.1.0] - 2026-10-25\n\n### Added\n\n- Initial release.\n"
        );
        assert_eq!(notes_for(&out, "0.1.1").unwrap(), "### Fixed\n\n- A bug.");
        assert!(notes_for(&out, "Unreleased").is_none());
    }

    #[test]
    fn supplied_notes_keep_unreleased() {
        let out = record_release(LOG, "0.2.0", "2026-10-26", Some("New flag\n- Faster")).unwrap();
        assert_eq!(notes_for(&out, "0.2.0").unwrap(), "- New flag\n- Faster");
        assert_eq!(notes_for(&out, "Unreleased").unwrap(), "### Fixed\n\n- A bug.");
    }

    #[test]
    fn refuses_without_notes_or_duplicates() {
        let empty = "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - x\n\n- y\n";
        assert_eq!(record_release(empty, "0.1.1", "d", None).unwrap_err().code, Code::Precondition);
        assert_eq!(record_release(LOG, "0.1.0", "d", Some("x")).unwrap_err().code, Code::AlreadyExists);
    }

    #[test]
    fn works_without_unreleased_heading() {
        let out = record_release("# Changelog\n\n## [1.0.0] - x\n\n- a\n", "1.0.1", "d", Some("b")).unwrap();
        assert_eq!(out, "# Changelog\n\n## [1.0.1] - d\n\n- b\n\n## [1.0.0] - x\n\n- a\n");
    }
}
