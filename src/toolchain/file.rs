//! Toolchain files: `rust-toolchain.toml`, or the older `rust-toolchain` (the same TOML, or a
//! single line naming the toolchain), found in the directory a command runs in or the nearest
//! directory above it.
//!
//! ```toml
//! [toolchain]
//! channel = "1.98"            # or `path = "../my-toolchain"`, never both
//! components = ["rust-src", "llvm-tools"]
//! targets = ["wasm32-unknown-unknown"]
//! profile = "minimal"
//! ```
//!
//! A key the format does not have is an error rather than something to ignore, and so is a
//! directory holding both files.

use super::spec::{is_triple, Spec};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// rustc, cargo and the standard library.
    Minimal,
    /// `minimal` with clippy, rustfmt and the standard library's sources.
    Default,
    /// Every component the release has for the host.
    Complete,
}

impl Profile {
    pub fn parse(s: &str) -> Result<Profile, String> {
        match s {
            "minimal" => Ok(Profile::Minimal),
            "default" => Ok(Profile::Default),
            "complete" => Ok(Profile::Complete),
            _ => Err(format!("`{}` is not a profile: expected minimal, default or complete", s)),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Minimal => "minimal",
            Profile::Default => "default",
            Profile::Complete => "complete",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolchainFile {
    pub path: PathBuf,
    pub spec: Spec,
    pub components: Vec<String>,
    pub targets: Vec<String>,
    pub profile: Option<Profile>,
}

/// The toolchain file governing `start`: in it or the nearest directory above it.
pub fn find(start: &Path) -> Result<Option<ToolchainFile>, String> {
    for dir in start.ancestors() {
        let legacy = dir.join("rust-toolchain");
        let toml = dir.join("rust-toolchain.toml");
        match (legacy.is_file(), toml.is_file()) {
            (true, true) => return Err(format!("{} holds both rust-toolchain and rust-toolchain.toml: keep one", dir.display())),
            (true, false) => return read(&legacy).map(Some),
            (false, true) => return read(&toml).map(Some),
            (false, false) => {}
        }
    }
    Ok(None)
}

pub fn read(path: &Path) -> Result<ToolchainFile, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    parse(path, &text).map_err(|e| format!("{}: {}", path.display(), e))
}

fn parse(path: &Path, text: &str) -> Result<ToolchainFile, String> {
    let trimmed = text.trim();
    let legacy = path.file_name().is_some_and(|n| n == "rust-toolchain");
    if legacy && !trimmed.is_empty() && !trimmed.contains('\n') && !trimmed.contains('=') && !trimmed.starts_with('[') {
        return Ok(ToolchainFile { path: path.to_path_buf(), spec: Spec::parse(trimmed)?, components: vec![], targets: vec![], profile: None });
    }
    let doc: toml::Table = toml::from_str(text).map_err(|e| format!("not a toolchain file: {}", e.message()))?;
    let mut table = None;
    for (k, v) in doc {
        match (k.as_str(), v) {
            ("toolchain", toml::Value::Table(t)) => table = Some(t),
            (k, _) => return Err(format!("unexpected `{}`: a toolchain file has one table, [toolchain]", k)),
        }
    }
    let table = table.ok_or("no [toolchain] table")?;
    let mut spec = None;
    let mut components = vec![];
    let mut targets = vec![];
    let mut profile = None;
    let strings = |key: &str, v: toml::Value| -> Result<Vec<String>, String> {
        match v {
            toml::Value::Array(items) => items.into_iter().map(|i| i.as_str().map(str::to_string).ok_or_else(|| format!("`{}` holds strings", key))).collect(),
            _ => Err(format!("`{}` is a list of strings", key)),
        }
    };
    for (k, v) in table {
        match k.as_str() {
            "channel" | "path" if spec.is_some() => return Err("names its toolchain twice: `channel` and `path` exclude each other".into()),
            "channel" => spec = Some(Spec::parse(v.as_str().ok_or("`channel` is a string")?)?),
            "path" => {
                let p = Path::new(v.as_str().ok_or("`path` is a string")?);
                let base = path.parent().unwrap_or(Path::new("."));
                spec = Some(Spec::Path(if p.is_absolute() { p.to_path_buf() } else { base.join(p) }));
            }
            "components" => components = strings("components", v)?,
            "targets" => {
                targets = strings("targets", v)?;
                if let Some(t) = targets.iter().find(|t| !is_triple(t)) {
                    return Err(format!("`{}` is not a target triple", t));
                }
            }
            "profile" => profile = Some(Profile::parse(v.as_str().ok_or("`profile` is a string")?)?),
            other => return Err(format!("unexpected key `{}` in [toolchain]", other)),
        }
    }
    let spec = spec.ok_or("[toolchain] names no toolchain: `channel` or `path`")?;
    if matches!(spec, Spec::Path(_)) && (!components.is_empty() || !targets.is_empty() || profile.is_some()) {
        return Err("a toolchain given by `path` is used as it is: `components`, `targets` and `profile` do not apply".into());
    }
    Ok(ToolchainFile { path: path.to_path_buf(), spec, components, targets, profile })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("haul-tcfile-{}", crate::util::new_id()));
        fs::create_dir_all(d.join("a/b")).unwrap();
        d
    }

    #[test]
    fn the_nearest_file_governs_and_both_kinds_are_read() {
        let d = tmp();
        fs::write(d.join("rust-toolchain.toml"), "[toolchain]\nchannel = \"1.98\"\ncomponents = [\"rust-src\"]\ntargets = [\"wasm32-unknown-unknown\"]\nprofile = \"minimal\"\n").unwrap();
        let f = find(&d.join("a/b")).unwrap().unwrap();
        assert_eq!(f.spec, Spec::parse("1.98").unwrap());
        assert_eq!((f.components, f.targets, f.profile), (vec!["rust-src".to_string()], vec!["wasm32-unknown-unknown".to_string()], Some(Profile::Minimal)));
        fs::write(d.join("a/rust-toolchain"), "nightly-2026-10-11\n").unwrap();
        let f = find(&d.join("a/b")).unwrap().unwrap();
        assert_eq!(f.spec, Spec::parse("nightly-2026-10-11").unwrap());
        assert_eq!(f.path, d.join("a/rust-toolchain"));
        fs::write(d.join("a/b/rust-toolchain"), "[toolchain]\npath = \"../tc\"\n").unwrap();
        assert_eq!(find(&d.join("a/b")).unwrap().unwrap().spec, Spec::Path(d.join("a/b/../tc")));
        fs::write(d.join("a/b/rust-toolchain.toml"), "[toolchain]\nchannel = \"stable\"\n").unwrap();
        assert!(find(&d.join("a/b")).unwrap_err().contains("both"));
    }

    #[test]
    fn rejects_what_the_format_does_not_have() {
        let p = Path::new("/x/rust-toolchain.toml");
        for (text, why) in [
            ("[toolchain]\nchannel = \"stable\"\ncomponent = [\"x\"]\n", "unexpected key `component`"),
            ("[toolchain]\nchannel = \"stable\"\npath = \"/t\"\n", "twice"),
            ("[toolchain]\ncomponents = []\n", "names no toolchain"),
            ("[toolchain]\nchannel = \"1.98.1.4\"\n", "not a toolchain"),
            ("[toolchain]\nchannel = \"stable\"\nprofile = \"full\"\n", "not a profile"),
            ("[toolchain]\nchannel = \"stable\"\ntargets = [\"wasm32\"]\n", "not a target triple"),
            ("[toolchain]\npath = \"/t\"\ntargets = [\"wasm32-wasip1\"]\n", "used as it is"),
            ("channel = \"stable\"\n", "one table"),
            ("stable\n", "not a toolchain file"),
        ] {
            let e = parse(p, text).unwrap_err();
            assert!(e.contains(why), "{:?}: {}", text, e);
        }
    }
}
