//! Detection of well-known credential formats, used both to redact anything
//! `ship` prints or logs and (later) by `scan-secrets`.
//!
//! No regex dependency: each pattern is a literal prefix followed by a run of
//! characters from a fixed alphabet, with length bounds and a word boundary.

pub const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, Copy)]
struct Pattern {
    prefix: &'static str,
    kind: &'static str,
    alphabet: fn(char) -> bool,
    /// Bounds on the length of the part after the prefix.
    min: usize,
    max: usize,
}

fn alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}
fn alnum_us(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
fn alnum_dash_us(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}
fn upper_digit(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}
fn token68(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~+/=".contains(c)
}

const PATTERNS: &[Pattern] = &[
    Pattern { prefix: "github_pat_", kind: "github_token", alphabet: alnum_us, min: 30, max: 255 },
    Pattern { prefix: "ghp_", kind: "github_token", alphabet: alnum, min: 30, max: 255 },
    Pattern { prefix: "gho_", kind: "github_token", alphabet: alnum, min: 30, max: 255 },
    Pattern { prefix: "ghu_", kind: "github_token", alphabet: alnum, min: 30, max: 255 },
    Pattern { prefix: "ghs_", kind: "github_token", alphabet: alnum, min: 30, max: 255 },
    Pattern { prefix: "ghr_", kind: "github_token", alphabet: alnum, min: 30, max: 255 },
    Pattern { prefix: "glpat-", kind: "gitlab_token", alphabet: alnum_dash_us, min: 20, max: 255 },
    Pattern { prefix: "cio", kind: "crates_io_token", alphabet: alnum, min: 32, max: 32 },
    Pattern { prefix: "npm_", kind: "npm_token", alphabet: alnum, min: 36, max: 36 },
    Pattern { prefix: "pypi-", kind: "pypi_token", alphabet: alnum_dash_us, min: 50, max: 400 },
    Pattern { prefix: "sk-ant-", kind: "anthropic_key", alphabet: alnum_dash_us, min: 30, max: 255 },
    Pattern { prefix: "sk-", kind: "openai_style_key", alphabet: alnum_dash_us, min: 32, max: 255 },
    Pattern { prefix: "sk_live_", kind: "stripe_key", alphabet: alnum, min: 16, max: 255 },
    Pattern { prefix: "rk_live_", kind: "stripe_key", alphabet: alnum, min: 16, max: 255 },
    Pattern { prefix: "xoxb-", kind: "slack_token", alphabet: alnum_dash_us, min: 10, max: 255 },
    Pattern { prefix: "xoxp-", kind: "slack_token", alphabet: alnum_dash_us, min: 10, max: 255 },
    Pattern { prefix: "xoxa-", kind: "slack_token", alphabet: alnum_dash_us, min: 10, max: 255 },
    Pattern { prefix: "AKIA", kind: "aws_access_key_id", alphabet: upper_digit, min: 16, max: 16 },
    Pattern { prefix: "ASIA", kind: "aws_access_key_id", alphabet: upper_digit, min: 16, max: 16 },
    Pattern { prefix: "AIza", kind: "google_api_key", alphabet: alnum_dash_us, min: 35, max: 35 },
    Pattern { prefix: "Bearer ", kind: "bearer_token", alphabet: token68, min: 20, max: 4096 },
];

/// A credential found in a piece of text: its kind and byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub kind: &'static str,
    pub start: usize,
    pub end: usize,
}

/// Find well-known credentials in `text`. Ranges never overlap.
pub fn find(text: &str) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    for p in PATTERNS {
        for (start, _) in text.match_indices(p.prefix) {
            // Word boundary before the prefix.
            if let Some(prev) = text[..start].chars().next_back() {
                if prev.is_ascii_alphanumeric() || prev == '_' {
                    continue;
                }
            }
            let body_start = start + p.prefix.len();
            let body_len: usize = text[body_start..]
                .chars()
                .take_while(|c| (p.alphabet)(*c))
                .map(char::len_utf8)
                .sum();
            let chars = text[body_start..body_start + body_len].chars().count();
            if chars < p.min || chars > p.max {
                continue;
            }
            let end = body_start + body_len;
            if hits.iter().any(|h| start < h.end && h.start < end) {
                continue;
            }
            hits.push(Hit { kind: p.kind, start, end });
        }
    }
    if let Some(h) = private_key_header(text) {
        if !hits.iter().any(|x| h.start < x.end && x.start < h.end) {
            hits.push(h);
        }
    }
    hits.sort_by_key(|h| h.start);
    hits
}

/// A PEM private key header: five dashes, `BEGIN `, an uppercase label ending
/// in `PRIVATE KEY` (e.g. `RSA PRIVATE KEY`), then five dashes.
fn private_key_header(text: &str) -> Option<Hit> {
    const HEAD: &str = "-----BEGIN ";
    for (start, _) in text.match_indices(HEAD) {
        let rest = &text[start + HEAD.len()..];
        let label_len = rest
            .find(|c: char| !(c.is_ascii_uppercase() || c == ' '))
            .unwrap_or(rest.len());
        if rest[..label_len].ends_with("PRIVATE KEY") && rest[label_len..].starts_with("-----") {
            let end = start + HEAD.len() + label_len + "-----".len();
            return Some(Hit { kind: "private_key", start, end });
        }
    }
    None
}

/// Replace every credential in `text` with `[REDACTED]`.
pub fn redact(text: &str) -> String {
    let hits = find(text);
    if hits.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for h in hits {
        out.push_str(&text[pos..h.start]);
        out.push_str(REDACTED);
        pos = h.end;
    }
    out.push_str(&text[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built at runtime so this file never contains a literal token.
    fn fake(prefix: &str, n: usize) -> String {
        format!("{prefix}{}", "a1B2".repeat(n).chars().take(n).collect::<String>())
    }

    #[test]
    fn redacts_github_tokens() {
        let t = fake("ghp_", 36);
        let s = format!("token={t} more");
        assert_eq!(redact(&s), "token=[REDACTED] more");
        assert_eq!(find(&s)[0].kind, "github_token");
    }

    #[test]
    fn respects_word_boundary() {
        // "precious" followed by 32 alnum must not match the crates.io pattern.
        let s = fake("precio", 35);
        assert!(find(&s).is_empty());
        let s = format!(" {}", fake("cio", 32));
        assert_eq!(find(&s)[0].kind, "crates_io_token");
    }

    #[test]
    fn length_bounds() {
        assert!(find(&fake("AKIA", 15)).is_empty());
        assert_eq!(find(&fake("AKIA", 16).to_uppercase()).len(), 1);
        assert!(find("ghp_short").is_empty());
    }

    #[test]
    fn private_key_header_detected() {
        let s = format!("x\n-----BEGIN {}PRIVATE KEY-----\nabc", "RSA ");
        let h = find(&s);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].kind, "private_key");
    }

    #[test]
    fn private_key_header_needs_a_real_label() {
        assert!(find("-----BEGIN {}PRIVATE KEY-----").is_empty());
        assert_eq!(find(&format!("-----BEGIN {}-----", "PRIVATE KEY")).len(), 1);
    }

    #[test]
    fn no_overlap_between_sk_patterns() {
        let s = fake("sk-ant-", 40);
        let h = find(&s);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].kind, "anthropic_key");
    }

    #[test]
    fn plain_text_untouched() {
        let s = "nothing secret here: gh auth status, cargo publish --dry-run";
        assert_eq!(redact(s), s);
    }
}
