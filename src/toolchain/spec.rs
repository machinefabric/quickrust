//! Toolchain names: what `+<name>`, `RUSTUP_TOOLCHAIN`, a toolchain file's `channel` and
//! `qrust toolchain` accept.
//!
//! - a release channel: `stable`, `beta`, `nightly`, optionally dated (`nightly-2026-10-11`) and
//!   for a host (`stable-x86_64-unknown-linux-gnu`);
//! - a release: `1.98` (its latest patch) or `1.98.1`, optionally for a host;
//! - the name of a linked toolchain (`qrust toolchain link <name> <dir>`);
//! - an absolute path to a toolchain directory (what rust-analyzer passes).
//!
//! Anything else is rejected here, before any download is attempted.

use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
    Nightly,
    /// `1.98` or `1.98.1`.
    Version(String),
}

impl Channel {
    /// The name in dist manifest file names (`channel-rust-<name>.toml`).
    pub fn dist_name(&self) -> &str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
            Channel::Nightly => "nightly",
            Channel::Version(v) => v,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spec {
    Dist { channel: Channel, date: Option<String>, host: Option<String> },
    /// A linked toolchain.
    Named(String),
    /// A toolchain directory.
    Path(PathBuf),
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn is_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    (parts.len() == 2 || parts.len() == 3) && parts.iter().all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|c| c.is_ascii_digit()))
}

/// A target triple: at least two `-`-separated parts of lowercase letters, digits, `_` and `.`,
/// starting with a letter (the architecture).
pub fn is_triple(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() >= 2 && s.bytes().next().is_some_and(|c| c.is_ascii_lowercase()) && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'.'))
}

/// A name `qrust toolchain link` accepts.
pub fn is_link_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().next().is_some_and(|c| c.is_ascii_alphanumeric()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

impl Spec {
    pub fn parse(s: &str) -> Result<Spec, String> {
        let bad = |why: &str| format!("`{}` is not a toolchain: {}", s, why);
        if s.is_empty() {
            return Err(bad("the name is empty"));
        }
        let path = std::path::Path::new(s);
        if path.is_absolute() {
            return Ok(Spec::Path(path.to_path_buf()));
        }
        if s.contains('/') || s.contains('\\') {
            return Err(bad("a toolchain directory is named by its absolute path"));
        }
        let (head, rest) = match s.find('-') {
            Some(i) => (&s[..i], Some(&s[i + 1..])),
            None => (s, None),
        };
        let channel = match head {
            "stable" => Channel::Stable,
            "beta" => Channel::Beta,
            "nightly" => Channel::Nightly,
            v if is_version(v) => Channel::Version(v.to_string()),
            _ if is_link_name(s) && !head.bytes().all(|c| c.is_ascii_digit() || c == b'.') => return Ok(Spec::Named(s.to_string())),
            _ => return Err(bad("expected stable, beta, nightly, a release such as 1.98 or 1.98.1, a linked toolchain's name, or an absolute path")),
        };
        let mut date = None;
        let mut host = None;
        if let Some(rest) = rest {
            let after_date = if rest.len() >= 10 && is_date(&rest[..10]) && (rest.len() == 10 || rest.as_bytes()[10] == b'-') {
                if matches!(channel, Channel::Version(_)) {
                    return Err(bad("a release is not dated"));
                }
                date = Some(rest[..10].to_string());
                if rest.len() == 10 {
                    None
                } else {
                    Some(&rest[11..])
                }
            } else {
                Some(rest)
            };
            if let Some(triple) = after_date {
                if !is_triple(triple) {
                    return Err(bad(&format!("`{}` is neither a date (YYYY-MM-DD) nor a target triple", triple)));
                }
                host = Some(triple.to_string());
            }
        }
        Ok(Spec::Dist { channel, date, host })
    }

    /// The toolchain this names can move to a newer release (`stable`, `nightly`, `1.98`).
    pub fn floating(&self) -> bool {
        match self {
            Spec::Dist { date: Some(_), .. } => false,
            Spec::Dist { channel: Channel::Version(v), .. } => v.split('.').count() == 2,
            Spec::Dist { .. } => true,
            _ => false,
        }
    }
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Spec::Dist { channel, date, host } => {
                write!(f, "{}", channel.dist_name())?;
                if let Some(d) = date {
                    write!(f, "-{}", d)?;
                }
                if let Some(h) = host {
                    write!(f, "-{}", h)?;
                }
                Ok(())
            }
            Spec::Named(n) => write!(f, "{}", n),
            Spec::Path(p) => write!(f, "{}", p.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(channel: Channel, date: Option<&str>, host: Option<&str>) -> Spec {
        Spec::Dist { channel, date: date.map(str::to_string), host: host.map(str::to_string) }
    }

    #[test]
    fn channels_releases_dates_and_hosts() {
        assert_eq!(Spec::parse("stable").unwrap(), dist(Channel::Stable, None, None));
        assert_eq!(Spec::parse("nightly-2026-10-11").unwrap(), dist(Channel::Nightly, Some("2026-10-11"), None));
        assert_eq!(Spec::parse("nightly-2026-10-11-aarch64-apple-darwin").unwrap(), dist(Channel::Nightly, Some("2026-10-11"), Some("aarch64-apple-darwin")));
        assert_eq!(Spec::parse("stable-x86_64-unknown-linux-gnu").unwrap(), dist(Channel::Stable, None, Some("x86_64-unknown-linux-gnu")));
        assert_eq!(Spec::parse("1.98").unwrap(), dist(Channel::Version("1.98".into()), None, None));
        assert_eq!(Spec::parse("1.98.1-x86_64-pc-windows-msvc").unwrap(), dist(Channel::Version("1.98.1".into()), None, Some("x86_64-pc-windows-msvc")));
        assert_eq!(Spec::parse("my-dev.1").unwrap(), Spec::Named("my-dev.1".into()));
        assert!(matches!(Spec::parse(if cfg!(windows) { "C:\\tc" } else { "/opt/tc" }).unwrap(), Spec::Path(_)));
        for s in ["stable", "nightly-2026-10-11-aarch64-apple-darwin", "1.98.1-x86_64-pc-windows-msvc", "my-dev.1"] {
            assert_eq!(Spec::parse(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn what_floats() {
        assert!(Spec::parse("stable").unwrap().floating());
        assert!(Spec::parse("1.98").unwrap().floating());
        assert!(!Spec::parse("1.98.1").unwrap().floating());
        assert!(!Spec::parse("nightly-2026-10-11").unwrap().floating());
        assert!(!Spec::parse("dev").unwrap().floating());
    }

    #[test]
    fn rejects_what_names_nothing() {
        for s in ["", "1.98.1-2026-01-01", "stable-2026-13", "stable-Linux", "1.98.1.4", "1", "relative/dir", "-x", "stable-x86_64--gnu", "nightly-2026-10-11-"] {
            assert!(Spec::parse(s).is_err(), "{:?} accepted", s);
        }
    }
}
