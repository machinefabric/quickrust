//! On-disk layout of Haul's local state. Nothing here is required for a build
//! to succeed; every caller treats failures as loss of observation only.
//!
//! ```text
//! $HAUL_HOME/
//!   config.toml
//!   setup.json
//!   active/<session>.json                 live-session locks (GC safety)
//!   workspaces/<id>/
//!     workspace.json                      root path, last seen
//!     sessions/<session>.json             raw session record
//!     events/<session>/*.json             raw rustc-wrapper events
//!     runs/<session>.json                 finalized, immutable report
//!     index/units.json                    last known unit fingerprints
//!     baselines/ benchmarks/ remediations/
//! ```

use crate::util::{self, canon, path_str};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub fn workspace_id(root: &Path) -> String {
    util::sha256_hex(path_str(&canon(root)).as_bytes())[..16].to_string()
}

#[derive(Debug, Clone)]
pub struct WsStore {
    pub id: String,
    pub dir: PathBuf,
    pub root: Option<PathBuf>,
}

impl WsStore {
    /// Store for the workspace rooted at `root`, or the shared bucket for commands run outside one.
    pub fn open(root: Option<&Path>) -> WsStore {
        let id = root.map(workspace_id).unwrap_or_else(|| "_none".to_string());
        let dir = util::haul_home().join("workspaces").join(&id);
        WsStore { id, dir, root: root.map(|r| r.to_path_buf()) }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        util::create_private_dir(&self.dir.join("sessions"))?;
        let marker = self.dir.join("workspace.json");
        if !marker.exists() {
            if let Some(root) = &self.root {
                util::write_json(&marker, &json!({ "root": path_str(&canon(root)), "created_at": util::iso8601(util::now_wall_ns()) }))?;
            }
        }
        Ok(())
    }

    pub fn session_path(&self, id: &str) -> PathBuf {
        self.dir.join("sessions").join(format!("{}.json", id))
    }
    pub fn events_dir(&self, id: &str) -> PathBuf {
        self.dir.join("events").join(id)
    }
    pub fn run_path(&self, id: &str) -> PathBuf {
        self.dir.join("runs").join(format!("{}.json", id))
    }
    pub fn index_path(&self) -> PathBuf {
        self.dir.join("index").join("units.json")
    }
    pub fn baselines_dir(&self) -> PathBuf {
        self.dir.join("baselines")
    }
    pub fn benchmarks_dir(&self) -> PathBuf {
        self.dir.join("benchmarks")
    }
    pub fn remediations_dir(&self) -> PathBuf {
        self.dir.join("remediations")
    }

    /// Session ids, oldest first (ids are time-sortable).
    pub fn session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = fs::read_dir(self.dir.join("sessions"))
            .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string)).collect())
            .unwrap_or_default();
        ids.sort();
        ids
    }

    pub fn load_session(&self, id: &str) -> Option<Value> {
        util::read_json(&self.session_path(id))
    }

    pub fn load_run(&self, id: &str) -> Option<Value> {
        util::read_json(&self.run_path(id))
    }

    /// Resolves `latest`, `latest~N`, a full id or a unique id prefix/suffix.
    pub fn resolve_run_ref(&self, reference: &str, instrumented_only: bool) -> Result<String, String> {
        let ids = self.session_ids();
        let usable: Vec<&String> = ids
            .iter()
            .filter(|id| !instrumented_only || self.load_session(id).map(|s| s["instrumented"] == json!(true) && s["events_in_parent"] != json!(true)).unwrap_or(false))
            .collect();
        if let Some(rest) = reference.strip_prefix("latest") {
            let back: usize = if rest.is_empty() { 0 } else { rest.strip_prefix('~').and_then(|n| n.parse().ok()).ok_or_else(|| format!("invalid run reference {:?}", reference))? };
            return usable.iter().rev().nth(back).map(|s| s.to_string()).ok_or_else(|| {
                if usable.is_empty() {
                    "no recorded build runs for this workspace yet; run a build through `haul` or Haul's `cargo` first".to_string()
                } else {
                    format!("only {} recorded build run(s); {:?} does not exist", usable.len(), reference)
                }
            });
        }
        let upper = reference.to_ascii_uppercase();
        let matches: Vec<&String> = ids.iter().filter(|id| id.starts_with(&upper) || id.ends_with(&upper)).collect();
        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => Err(format!("no run matches {:?}", reference)),
            n => Err(format!("{:?} is ambiguous ({} runs match)", reference, n)),
        }
    }

    /// Bounded retention (NFR-SPACE-001, DATA-009, R-014).
    pub fn prune(&self, keep_runs: usize, keep_raw: usize) {
        let ids = self.session_ids();
        let n = ids.len();
        for (i, id) in ids.iter().enumerate() {
            let age = n - i;
            if age > keep_raw.max(1) && self.run_path(id).exists() {
                let _ = fs::remove_dir_all(self.events_dir(id));
            }
            if age > keep_runs.max(1) {
                let _ = fs::remove_dir_all(self.events_dir(id));
                let _ = fs::remove_file(self.session_path(id));
                let _ = fs::remove_file(self.run_path(id));
            }
        }
    }
}

/// Every workspace Haul has seen on this machine, with its recorded root.
pub fn known_workspaces() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(util::haul_home().join("workspaces")) {
        for e in rd.filter_map(|e| e.ok()) {
            if let Some(v) = util::read_json(&e.path().join("workspace.json")) {
                if let Some(root) = v["root"].as_str() {
                    out.push((e.file_name().to_string_lossy().into_owned(), PathBuf::from(root)));
                }
            }
        }
    }
    out.sort();
    out
}

fn active_dir() -> PathBuf {
    util::haul_home().join("active")
}

/// Registers a live session so lifecycle tooling can see who owns a build directory.
pub fn register_active(session: &str, workspace: Option<&Path>, cwd: &Path) -> Option<PathBuf> {
    let dir = active_dir();
    util::create_private_dir(&dir).ok()?;
    let p = dir.join(format!("{}.json", session));
    let v = json!({
        "session": session,
        "pid": std::process::id(),
        "workspace_root": workspace.map(|w| path_str(&canon(w))),
        "cwd": path_str(cwd),
        "started_at": util::iso8601(util::now_wall_ns()),
    });
    util::write_atomic_fast(&p, &serde_json::to_vec(&v).ok()?).ok()?;
    Some(p)
}

/// Live sessions; records whose process is gone are removed as a side effect.
pub fn active_sessions() -> Vec<Value> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(active_dir()) {
        for e in rd.filter_map(|e| e.ok()) {
            match util::read_json(&e.path()) {
                Some(v) if crate::proc::pid_alive(v["pid"].as_u64().unwrap_or(0) as u32) => out.push(v),
                _ => {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
    out
}
