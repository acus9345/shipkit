//! Semantic versions: parse, compare and bump.

use crate::error::{Code, Result, ShipError};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Pre-release identifier without the leading `-` (e.g. `rc.1`).
    pub pre: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Patch,
    Minor,
    Major,
}

impl Version {
    pub fn parse(s: &str) -> Result<Version> {
        let bad = || ShipError::new(Code::Input, format!("`{s}` is not a semantic version (MAJOR.MINOR.PATCH)"));
        let s = s.trim();
        let core_and_pre = s.split('+').next().unwrap_or(s); // build metadata is ignored
        let (core, pre) = match core_and_pre.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
            Some(_) => return Err(bad()),
            None => (core_and_pre, None),
        };
        let nums: Vec<&str> = core.split('.').collect();
        if nums.len() != 3 {
            return Err(bad());
        }
        let num = |p: &str| -> Result<u64> {
            if p.is_empty() || (p.len() > 1 && p.starts_with('0')) || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            p.parse().map_err(|_| bad())
        };
        Ok(Version { major: num(nums[0])?, minor: num(nums[1])?, patch: num(nums[2])?, pre })
    }

    /// Standard bump. A pre-release bumped by `patch` becomes its release
    /// (`1.2.3-rc.1` -> `1.2.3`).
    pub fn bump(&self, level: Level) -> Version {
        let (major, minor, patch) = match level {
            Level::Major => (self.major + 1, 0, 0),
            Level::Minor => (self.major, self.minor + 1, 0),
            Level::Patch if self.pre.is_some() => (self.major, self.minor, self.patch),
            Level::Patch => (self.major, self.minor, self.patch + 1),
        };
        Version { major, minor, patch, pre: None }
    }

    /// True if `self` sorts after `other` (pre-releases sort before releases).
    pub fn is_newer_than(&self, other: &Version) -> bool {
        let a = (self.major, self.minor, self.patch);
        let b = (other.major, other.minor, other.patch);
        if a != b {
            return a > b;
        }
        match (&self.pre, &other.pre) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x > y,
            _ => false,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parses_and_prints() {
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
        assert_eq!(v("0.1.0-rc.1").to_string(), "0.1.0-rc.1");
        assert_eq!(v("1.0.0+build.5").to_string(), "1.0.0");
        for bad in ["1.2", "1.2.3.4", "01.2.3", "a.b.c", "1.2.3-", ""] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bumps() {
        assert_eq!(v("0.1.0").bump(Level::Patch).to_string(), "0.1.1");
        assert_eq!(v("0.1.9").bump(Level::Minor).to_string(), "0.2.0");
        assert_eq!(v("1.4.2").bump(Level::Major).to_string(), "2.0.0");
        assert_eq!(v("1.2.3-rc.1").bump(Level::Patch).to_string(), "1.2.3");
        assert_eq!(v("1.2.3-rc.1").bump(Level::Minor).to_string(), "1.3.0");
    }

    #[test]
    fn ordering() {
        assert!(v("1.0.1").is_newer_than(&v("1.0.0")));
        assert!(v("1.0.0").is_newer_than(&v("1.0.0-rc.1")));
        assert!(!v("1.0.0").is_newer_than(&v("1.0.0")));
        assert!(!v("0.9.9").is_newer_than(&v("1.0.0")));
    }
}
