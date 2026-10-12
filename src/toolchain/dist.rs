//! Rust's release manifests (`dist/[<date>/]channel-rust-<channel>.toml` on the dist server):
//! which release a channel names, and where each of its components for each target is, with the
//! SHA-256 of its archive.
//!
//! A manifest is trusted when it matches the `.sha256` published beside it (over HTTPS); each
//! component archive must then match the hash the manifest gives for it.

use super::file::Profile;
use crate::net;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Where the manifests name their files; a configured dist server (a mirror, or a test's) serves
/// the same paths.
pub const UPSTREAM: &str = "https://static.rust-lang.org";

#[derive(Deserialize)]
struct Raw {
    #[serde(rename = "manifest-version")]
    manifest_version: String,
    date: String,
    pkg: BTreeMap<String, RawPkg>,
    #[serde(default)]
    renames: BTreeMap<String, RawRename>,
    #[serde(default)]
    profiles: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct RawPkg {
    version: String,
    #[serde(default)]
    target: BTreeMap<String, RawTarget>,
}

#[derive(Deserialize)]
struct RawTarget {
    available: bool,
    xz_url: Option<String>,
    xz_hash: Option<String>,
}

#[derive(Deserialize)]
struct RawRename {
    to: String,
}

/// One archive to install: a package of the release, built for one target (`*`: for any).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub pkg: String,
    pub target: String,
    pub url: String,
    pub sha256: String,
}

impl Component {
    /// The archive's top-level directory: its file name without `.tar.xz`.
    pub fn root(&self) -> &str {
        let name = self.url.rsplit('/').next().unwrap_or(&self.url);
        name.strip_suffix(".tar.xz").unwrap_or(name)
    }

    /// How a person names it: `clippy`, `rust-std (wasm32-unknown-unknown)`.
    pub fn label(&self, host: &str) -> String {
        let name = self.pkg.strip_suffix("-preview").unwrap_or(&self.pkg);
        if self.target == "*" || self.target == host {
            name.to_string()
        } else {
            format!("{} ({})", name, self.target)
        }
    }
}

/// Why a release cannot provide what was asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Missing {
    /// The release has no such component at all.
    Unknown(String),
    /// The release has the component, but not for this target.
    NoTarget { pkg: String, target: String },
    /// This build of the release did not produce it (nightlies, mostly).
    Unavailable { pkg: String, target: String },
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Missing::Unknown(c) => write!(f, "Rust has no component `{}`", c),
            Missing::NoTarget { pkg, target } => write!(f, "{} is not provided for {}", pkg, target),
            Missing::Unavailable { pkg, target } => write!(f, "{} for {} is missing from this release", pkg, target),
        }
    }
}

pub struct Manifest {
    raw: Raw,
    /// The manifest file as published, kept with an installed toolchain so that components added
    /// later come from the same release.
    pub bytes: Vec<u8>,
}

impl Manifest {
    pub fn parse(bytes: Vec<u8>) -> Result<Manifest, String> {
        let text = std::str::from_utf8(&bytes).map_err(|_| "the release manifest is not UTF-8".to_string())?;
        let raw: Raw = toml::from_str(text).map_err(|e| format!("not a release manifest: {}", e.message()))?;
        if raw.manifest_version != "2" {
            return Err(format!("release manifest version {} (QuickRust reads version 2)", raw.manifest_version));
        }
        if !raw.pkg.contains_key("rustc") {
            return Err("the release manifest has no rustc".into());
        }
        Ok(Manifest { raw, bytes })
    }

    pub fn date(&self) -> &str {
        &self.raw.date
    }

    /// The release: `1.98.1`, `1.99.0-beta.3`, `1.100.0-nightly`.
    pub fn version(&self) -> &str {
        self.raw.pkg["rustc"].version.split_whitespace().next().unwrap_or("")
    }

    /// The package a component name means (`clippy` → `clippy-preview`).
    pub fn package(&self, name: &str) -> String {
        match self.raw.renames.get(name) {
            Some(r) => r.to.clone(),
            None => name.to_string(),
        }
    }

    /// The archive of `pkg` for `target` (a package built for any target is found under `*`).
    pub fn component(&self, pkg: &str, target: &str) -> Result<Component, Missing> {
        let p = self.raw.pkg.get(pkg).ok_or_else(|| Missing::Unknown(pkg.to_string()))?;
        let (t, entry) = p.target.get_key_value("*").or_else(|| p.target.get_key_value(target)).ok_or_else(|| Missing::NoTarget { pkg: pkg.to_string(), target: target.to_string() })?;
        let unavailable = || Missing::Unavailable { pkg: pkg.to_string(), target: target.to_string() };
        match (entry.available, &entry.xz_url, &entry.xz_hash) {
            (true, Some(url), Some(hash)) => Ok(Component { pkg: pkg.to_string(), target: t.clone(), url: url.clone(), sha256: hash.clone() }),
            _ => Err(unavailable()),
        }
    }

    /// What a new toolchain for `host` gets: the profile's packages that exist for the host,
    /// `extra` components, and the standard library for `targets`. A component asked for by
    /// name, and the profile's rustc, cargo and standard library, must be available.
    pub fn plan(&self, host: &str, profile: Profile, extra: &[String], targets: &[String]) -> Result<Vec<Component>, Missing> {
        let mut out: Vec<Component> = vec![];
        let mut add = |c: Component| {
            if !out.contains(&c) {
                out.push(c);
            }
        };
        let base: Vec<String> = match profile {
            Profile::Complete => self.raw.profiles.get("complete").cloned().unwrap_or_default(),
            _ => {
                let mut v = self.raw.profiles.get("minimal").cloned().unwrap_or_else(|| vec!["rustc".into(), "cargo".into(), "rust-std".into()]);
                if profile == Profile::Default {
                    v.extend(["clippy", "rustfmt", "rust-src"].iter().map(|n| self.package(n)));
                }
                v
            }
        };
        const ESSENTIAL: &[&str] = &["rustc", "cargo", "rust-std"];
        for pkg in &base {
            match self.component(pkg, host) {
                Ok(c) => add(c),
                Err(Missing::NoTarget { .. } | Missing::Unknown(_)) if !ESSENTIAL.contains(&pkg.as_str()) => {}
                Err(Missing::Unavailable { .. }) if profile == Profile::Complete && !ESSENTIAL.contains(&pkg.as_str()) => {}
                Err(e) => return Err(e),
            }
        }
        for name in extra {
            add(self.component(&self.package(name), host)?);
        }
        for t in targets {
            add(self.component("rust-std", t)?);
        }
        Ok(out)
    }
}

/// The manifest's URL for `channel` (`stable`, `nightly`, `1.98.1`) on `date`, if dated.
pub fn manifest_url(server: &str, channel: &str, date: Option<&str>) -> String {
    match date {
        Some(d) => format!("{}/dist/{}/channel-rust-{}.toml", server.trim_end_matches('/'), d, channel),
        None => format!("{}/dist/channel-rust-{}.toml", server.trim_end_matches('/'), channel),
    }
}

/// A component's URL on `server`: the manifests name files on the upstream server.
pub fn on_server(url: &str, server: &str) -> String {
    match url.strip_prefix(UPSTREAM) {
        Some(rest) if server.trim_end_matches('/') != UPSTREAM => format!("{}{}", server.trim_end_matches('/'), rest),
        _ => url.to_string(),
    }
}

/// Fetches the manifest at `url` and checks it against its published SHA-256. `Ok(None)`: there
/// is no such manifest (an unknown release, a day without a nightly).
pub fn fetch(client: &net::Client, url: &str) -> Result<Option<(Manifest, String)>, String> {
    let bytes = match client.get_bytes(url, &[]) {
        Ok(b) => b,
        Err(net::Error::Status { status: 404, .. }) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let published = client.get_bytes(&format!("{}.sha256", url), &[]).map_err(|e| e.to_string())?;
    let published = String::from_utf8_lossy(&published);
    let expected = published.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    let actual = crate::util::sha256_hex(&bytes);
    if expected != actual {
        return Err(format!("{} does not match its published SHA-256 ({} published, {} received)", url, expected, actual));
    }
    Ok(Some((Manifest::parse(bytes).map_err(|e| format!("{}: {}", url, e))?, actual)))
}

#[cfg(test)]
pub mod testing {
    //! Manifests in the dist server's format, for tests.

    /// `(pkg, target, available)`; each available archive at `<server>/dist/<date>/<pkg>-<version>-<target>.tar.xz`
    /// with the hash from `hash(pkg, target)`.
    pub fn manifest(server: &str, date: &str, version: &str, pkgs: &[(&str, &str, bool)], hash: &dyn Fn(&str, &str) -> String) -> String {
        let mut s = format!("manifest-version = \"2\"\ndate = \"{}\"\n", date);
        let mut names: Vec<&str> = pkgs.iter().map(|p| p.0).collect();
        names.dedup();
        for name in names {
            s.push_str(&format!("\n[pkg.{}]\nversion = \"{} (0123456789 {})\"\n", name, version, date));
            for (pkg, target, available) in pkgs.iter().filter(|p| p.0 == name) {
                s.push_str(&format!("\n[pkg.{}.target.\"{}\"]\navailable = {}\n", pkg, target, available));
                if *available {
                    let file = if *target == "*" { format!("{}-{}", pkg, version) } else { format!("{}-{}-{}", pkg, version, target) };
                    s.push_str(&format!("xz_url = \"{}/dist/{}/{}.tar.xz\"\nxz_hash = \"{}\"\n", server, date, file, hash(pkg, target)));
                }
            }
        }
        s.push_str("\n[renames.clippy]\nto = \"clippy-preview\"\n\n[renames.rustfmt]\nto = \"rustfmt-preview\"\n");
        s.push_str("\n[profiles]\nminimal = [\"rustc\", \"cargo\", \"rust-std\", \"rust-mingw\"]\ndefault = [\"rustc\", \"cargo\", \"rust-std\", \"rust-mingw\", \"rust-docs\", \"rustfmt-preview\", \"clippy-preview\"]\ncomplete = [\"rustc\", \"cargo\", \"rust-std\", \"rust-mingw\", \"rust-docs\", \"rustfmt-preview\", \"clippy-preview\", \"rust-src\", \"miri-preview\"]\n");
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "x86_64-unknown-linux-gnu";

    fn m(pkgs: &[(&str, &str, bool)]) -> Manifest {
        Manifest::parse(testing::manifest(UPSTREAM, "2026-10-01", "1.99.0", pkgs, &|p, t| crate::util::sha256_hex(format!("{}{}", p, t).as_bytes())).into_bytes()).unwrap()
    }

    fn names(cs: &[Component]) -> Vec<String> {
        cs.iter().map(|c| c.label(HOST)).collect()
    }

    const RELEASE: &[(&str, &str, bool)] = &[
        ("rustc", HOST, true),
        ("cargo", HOST, true),
        ("rust-std", HOST, true),
        ("rust-std", "wasm32-unknown-unknown", true),
        ("rust-mingw", "x86_64-pc-windows-gnu", true),
        ("rust-docs", HOST, true),
        ("clippy-preview", HOST, true),
        ("rustfmt-preview", HOST, false),
        ("rust-src", "*", true),
        ("miri-preview", HOST, false),
    ];

    #[test]
    fn the_real_stable_manifest_reads() {
        // A trimmed copy of the published manifest for 1.99.0 (2026-10-01).
        let m = Manifest::parse(include_bytes!("../../tests/fixtures/channel-rust-1.99.0-trimmed.toml").to_vec()).unwrap();
        assert_eq!((m.version(), m.date()), ("1.99.0", "2026-10-01"));
        let plan = m.plan(HOST, Profile::Default, &["llvm-tools".into()], &["wasm32-unknown-unknown".into()]).unwrap();
        assert_eq!(names(&plan), ["rustc", "cargo", "rust-std", "clippy", "rustfmt", "rust-src", "llvm-tools", "rust-std (wasm32-unknown-unknown)"]);
        assert_eq!(plan[0].root(), "rustc-1.99.0-x86_64-unknown-linux-gnu");
        assert_eq!(plan[0].sha256, "77171ba2a0345fdf2abc4fedda55d6de078dae7a68527c28be8c77dcc9604bd5");
        assert_eq!(plan[5].root(), "rust-src-1.99.0");
    }

    #[test]
    fn profiles_take_what_exists_for_the_host_and_requests_must_be_there() {
        let m = m(RELEASE);
        assert_eq!(names(&m.plan(HOST, Profile::Minimal, &[], &[]).unwrap()), ["rustc", "cargo", "rust-std"]);
        // rustfmt is in the profile but this build lacks it: only an explicit request fails on that.
        assert_eq!(m.plan(HOST, Profile::Default, &[], &[]).unwrap_err(), Missing::Unavailable { pkg: "rustfmt-preview".into(), target: HOST.into() });
        assert_eq!(names(&m.plan(HOST, Profile::Complete, &[], &["wasm32-unknown-unknown".into()]).unwrap()), ["rustc", "cargo", "rust-std", "rust-docs", "clippy", "rust-src", "rust-std (wasm32-unknown-unknown)"]);
        assert_eq!(m.plan(HOST, Profile::Minimal, &["rustfmt".into()], &[]).unwrap_err(), Missing::Unavailable { pkg: "rustfmt-preview".into(), target: HOST.into() });
        assert_eq!(m.plan(HOST, Profile::Minimal, &["rust-analyzer".into()], &[]).unwrap_err(), Missing::Unknown("rust-analyzer".into()));
        assert_eq!(m.plan(HOST, Profile::Minimal, &[], &["riscv64gc-unknown-none-elf".into()]).unwrap_err(), Missing::NoTarget { pkg: "rust-std".into(), target: "riscv64gc-unknown-none-elf".into() });
        let no_rustc = [("rustc", HOST, false), ("cargo", HOST, true), ("rust-std", HOST, true)];
        assert!(matches!(self::m(&no_rustc).plan(HOST, Profile::Complete, &[], &[]), Err(Missing::Unavailable { .. })));
    }

    #[test]
    fn urls_follow_the_configured_server() {
        assert_eq!(manifest_url("https://mirror.example/rust/", "nightly", Some("2026-10-11")), "https://mirror.example/rust/dist/2026-10-11/channel-rust-nightly.toml");
        assert_eq!(on_server("https://static.rust-lang.org/dist/2026-10-01/x.tar.xz", "http://127.0.0.1:9/"), "http://127.0.0.1:9/dist/2026-10-01/x.tar.xz");
        assert_eq!(on_server("https://static.rust-lang.org/dist/x", UPSTREAM), "https://static.rust-lang.org/dist/x");
    }
}
