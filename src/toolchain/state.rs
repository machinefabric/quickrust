//! `toolchains/channels.json`: which installed toolchain each channel currently names, the
//! default toolchain, and linked toolchains.
//!
//! ```json
//! {"format": 1, "default": "stable",
//!  "channels": {"stable-x86_64-unknown-linux-gnu": {"toolchain": "1.99.0-x86_64-unknown-linux-gnu",
//!                "manifest_sha256": "…", "checked_ns": 1791195330123000000}},
//!  "linked": {"dev": "/home/me/rust/build/host/stage1"}}
//! ```
//!
//! Read without a lock (it is replaced atomically); changed only under `toolchains/.state.lock`.

use crate::util;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FORMAT: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelState {
    pub toolchain: String,
    pub manifest_sha256: String,
    pub checked_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub default: String,
    pub channels: BTreeMap<String, ChannelState>,
    pub linked: BTreeMap<String, PathBuf>,
}

impl Default for State {
    fn default() -> Self {
        State { default: "stable".into(), channels: BTreeMap::new(), linked: BTreeMap::new() }
    }
}

pub fn path(toolchains: &Path) -> PathBuf {
    toolchains.join("channels.json")
}

/// The state; the initial one when there is none yet. A file that is not one is an error.
pub fn load(toolchains: &Path) -> Result<State, String> {
    let p = path(toolchains);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(e) => return Err(format!("cannot read {}: {}", p.display(), e)),
    };
    let bad = |why: &str| format!("{} is not Haul's toolchain state ({}); `haul toolchain list` shows what is installed, and removing the file resets the channels", p.display(), why);
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| bad(&e.to_string()))?;
    if v.get("format").and_then(Value::as_u64) != Some(FORMAT) {
        return Err(bad("unknown format"));
    }
    let default = v.get("default").and_then(Value::as_str).ok_or_else(|| bad("no default"))?.to_string();
    let mut channels = BTreeMap::new();
    for (k, c) in v.get("channels").and_then(Value::as_object).ok_or_else(|| bad("no channels"))? {
        let toolchain = c.get("toolchain").and_then(Value::as_str).ok_or_else(|| bad("a channel without a toolchain"))?.to_string();
        let manifest_sha256 = c.get("manifest_sha256").and_then(Value::as_str).unwrap_or("").to_string();
        let checked_ns = c.get("checked_ns").and_then(Value::as_u64).unwrap_or(0);
        channels.insert(k.clone(), ChannelState { toolchain, manifest_sha256, checked_ns });
    }
    let mut linked = BTreeMap::new();
    for (k, d) in v.get("linked").and_then(Value::as_object).ok_or_else(|| bad("no linked"))? {
        linked.insert(k.clone(), PathBuf::from(d.as_str().ok_or_else(|| bad("a linked toolchain without a directory"))?));
    }
    Ok(State { default, channels, linked })
}

/// Changes the state under its lock and saves it.
pub fn update<T>(toolchains: &Path, change: impl FnOnce(&mut State) -> Result<T, String>) -> Result<T, String> {
    let _lock = util::ExclusiveLock::acquire(&toolchains.join(".locks").join("state.lock")).map_err(|e| format!("cannot lock the toolchain state: {}", e))?;
    let mut state = load(toolchains)?;
    let out = change(&mut state)?;
    let channels: Map<String, Value> = state.channels.iter().map(|(k, c)| (k.clone(), json!({"toolchain": c.toolchain, "manifest_sha256": c.manifest_sha256, "checked_ns": c.checked_ns}))).collect();
    let linked: Map<String, Value> = state.linked.iter().map(|(k, d)| (k.clone(), json!(util::path_str(d)))).collect();
    util::write_json(&path(toolchains), &json!({"format": FORMAT, "default": state.default, "channels": channels, "linked": linked})).map_err(|e| format!("cannot save the toolchain state: {}", e))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_foreign_files() {
        let d = std::env::temp_dir().join(format!("haul-tcstate-{}", util::new_id()));
        assert_eq!(load(&d).unwrap(), State::default());
        update(&d, |s| {
            s.default = "nightly".into();
            s.channels.insert("stable-h".into(), ChannelState { toolchain: "1.99.0-h".into(), manifest_sha256: "ab".into(), checked_ns: 7 });
            s.linked.insert("dev".into(), PathBuf::from("/opt/dev"));
            Ok(())
        })
        .unwrap();
        let s = load(&d).unwrap();
        assert_eq!(s.default, "nightly");
        assert_eq!(s.channels["stable-h"].toolchain, "1.99.0-h");
        assert_eq!(s.linked["dev"], PathBuf::from("/opt/dev"));
        std::fs::write(path(&d), "{\"format\": 9}").unwrap();
        assert!(load(&d).unwrap_err().contains("unknown format"));
    }
}
