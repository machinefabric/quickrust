//! Builds QuickRust renders in a terminal, skipped when they are known fresh.
//!
//! After a successful build QuickRust records a stamp: every file the build's freshness depends on
//! that QuickRust can name, with its size and modification time — the sources the compiler read,
//! build scripts' declared inputs (every file of the package for a script that declares none),
//! manifests, `Cargo.lock`, Cargo configuration, toolchain files and the compiler, the outputs.
//! The key holds the rest: arguments, directory, environment, QuickRust's and Cargo's identity. The
//! next identical command whose stamped files are all unchanged is known fresh.
//!
//! - `cargo run` / `cargo test` (built first, then run; see `ui::Rendering`): the build phase is
//!   skipped and the command runs at once. Cargo still checks freshness in it, so a stamp that
//!   missed an input only means that build is shown in Cargo's words.
//! - A build alone: QuickRust's verdict is the answer and Cargo is not started. The toolchain's Cargo
//!   is in the key, and the files that choose the toolchain are stamped. A test checks QuickRust's
//!   verdict against Cargo's after every kind of change.

use crate::util;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

const FORMAT: &str = "qrust-prebuilt-1";
/// Stamps kept per workspace: one per distinct command line, the least recently written go.
const KEEP: usize = 32;

/// Shell bookkeeping that changes without changing anything a build reads.
const VOLATILE_ENV: [&str; 2] = ["OLDPWD", "_"];

/// The key of a build phase: everything besides files that decides what it builds.
pub fn key(args: &[OsString], cwd: &Path, cargo: &Path, cargo_env: &[(String, String)]) -> String {
    let identity = |p: &Path| fs::metadata(p).map(|m| format!("{}:{}:{}", util::path_str(p), m.len(), util::mtime_ns(&m))).unwrap_or_else(|_| format!("{}:absent", util::path_str(p)));
    let mut parts = vec![FORMAT.to_string(), identity(&crate::resolve::self_identity().0), identity(cargo), util::path_str(cwd)];
    parts.extend(args.iter().map(|a| format!("arg={}", util::lossy(a))));
    parts.extend(cargo_env.iter().map(|(k, v)| format!("cargo-env={}={}", k, v)));
    let mut env: Vec<String> = std::env::vars_os().filter(|(k, _)| !VOLATILE_ENV.iter().any(|v| k == v)).map(|(k, v)| format!("env={}={}", k.to_string_lossy(), v.to_string_lossy())).collect();
    env.sort();
    parts.extend(env);
    util::digest_parts(parts)
}

fn stamp_path(store_dir: &Path, key: &str) -> PathBuf {
    store_dir.join("prebuilt").join(format!("{}.json", key))
}

/// `[path, bytes, mtime]`, or `[path, null, null]` for a file that does not exist.
fn file_state(p: &Path) -> Value {
    match fs::metadata(p) {
        Ok(m) => json!([util::path_str(p), m.len(), util::mtime_ns(&m)]),
        Err(_) => json!([util::path_str(p), null, null]),
    }
}

/// The stamp of the build phase with this key, when every file it names is as it was recorded.
/// Returns the stamp's summary: units and the warnings line QuickRust printed for that build.
pub fn fresh(store_dir: &Path, key: &str) -> Option<(u64, Option<String>)> {
    let stamp = util::read_json(&stamp_path(store_dir, key))?;
    if stamp["format"] != FORMAT {
        return None;
    }
    for f in stamp["files"].as_array()? {
        if file_state(Path::new(f[0].as_str()?)) != *f {
            return None;
        }
    }
    Some((stamp["units"].as_u64()?, stamp["warnings_line"].as_str().map(str::to_string)))
}

/// Local source files a unit read, from rustc's dep-info next to its outputs. Its paths are
/// relative to where Cargo runs rustc: the workspace root.
fn unit_inputs(filename: &Path, base: &Path, out: &mut BTreeSet<PathBuf>) {
    let (Some(dir), Some(name)) = (filename.parent(), filename.file_name()) else { return };
    let dep_info = dir.join(format!("{}.d", crate::inventory::artifact_stem(&name.to_string_lossy())));
    // The dep-info itself is Cargo's to rewrite; what it lists are the inputs.
    let Ok(text) = fs::read_to_string(&dep_info) else { return };
    if let Some(rule) = text.lines().find(|l| !l.starts_with('#') && l.contains(": ")) {
        out.extend(crate::wrapper::split_depinfo_paths(rule.split_once(": ").map(|(_, b)| b).unwrap_or("")).into_iter().map(|f| base.join(f)));
    }
}

/// What decides whether Cargo reruns a local package's build script: the paths it declared
/// (`cargo::rerun-if-changed`, a directory meaning everything beneath it), or, when it declared
/// none, every file of the package.
fn rerun_if_changed(out_dir: &Path, package_dir: &Path, out: &mut BTreeSet<PathBuf>) {
    // Cargo's classic layout keeps the script's output next to `out/`; the newer one in `run/`.
    let Some(unit) = out_dir.parent() else { return };
    for output in [unit.join("output"), unit.join("run").join("output")] {
        let Ok(text) = fs::read_to_string(&output) else { continue };
        out.insert(output);
        let mut declared = false;
        for l in text.lines() {
            if let Some(p) = l.strip_prefix("cargo::rerun-if-changed=").or_else(|| l.strip_prefix("cargo:rerun-if-changed=")) {
                declared = true;
                files_beneath(&package_dir.join(p), out);
            }
            declared |= l.starts_with("cargo::rerun-if-env-changed=") || l.starts_with("cargo:rerun-if-env-changed=");
        }
        if !declared {
            files_beneath(package_dir, out);
        }
    }
}

/// `path` itself, or, for a directory, every file beneath it except build directories (they
/// hold a `CACHEDIR.TAG`) and version control.
fn files_beneath(path: &Path, out: &mut BTreeSet<PathBuf>) {
    if !path.is_dir() {
        out.insert(path.to_path_buf());
        return;
    }
    if path.join("CACHEDIR.TAG").exists() || path.file_name().is_some_and(|n| n == ".git") {
        return;
    }
    for e in fs::read_dir(path).into_iter().flatten().flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => files_beneath(&p, out),
            _ => {
                out.insert(p);
            }
        }
    }
}

/// The directory of a local package from its Cargo package id (`path+file:///dir#name@1.0`).
fn local_package_dir(package_id: &str) -> Option<PathBuf> {
    let rest = package_id.strip_prefix("path+file://")?;
    Some(PathBuf::from(rest.split('#').next()?))
}

/// A successful build, as recorded in a stamp.
pub struct Built<'a> {
    /// Cargo's messages, compact (see `session::summarize_message`).
    pub messages: &'a [Value],
    pub cwd: &'a Path,
    pub ws_root: Option<&'a Path>,
    /// The toolchain Cargo that ran it.
    pub cargo: &'a Path,
    /// What QuickRust summarised: units, and the warnings line.
    pub units: u64,
    pub warnings_line: Option<&'a str>,
}

/// Records the stamp of a successful build from its Cargo messages.
pub fn record(store_dir: &Path, key: &str, built: &Built) {
    let Built { messages, cwd, ws_root, cargo, units, warnings_line } = *built;
    let immutable: Vec<PathBuf> = vec![util::cargo_home().join("registry"), util::cargo_home().join("git")];
    let base = ws_root.unwrap_or(cwd);
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    for m in messages {
        match m["reason"].as_str() {
            Some("compiler-artifact") => {
                for f in m["filenames"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    files.insert(PathBuf::from(f));
                    unit_inputs(Path::new(f), base, &mut files);
                }
                if let Some(e) = m["executable"].as_str() {
                    files.insert(PathBuf::from(e));
                }
                if let Some(manifest) = m["manifest_path"].as_str() {
                    files.insert(PathBuf::from(manifest));
                }
            }
            Some("build-script-executed") => {
                if let (Some(out_dir), Some(dir)) = (m["out_dir"].as_str(), m["package_id"].as_str().and_then(local_package_dir)) {
                    rerun_if_changed(Path::new(out_dir), &dir, &mut files);
                }
            }
            _ => {}
        }
    }
    if let Some(root) = ws_root {
        files.insert(root.join("Cargo.toml"));
        files.insert(root.join("Cargo.lock"));
    }
    // Cargo's configuration: present or not, in every directory it is read from.
    // And the toolchain: the files that choose it, which release each channel names, the compiler.
    for dir in cwd.ancestors() {
        files.insert(dir.join(".cargo/config.toml"));
        files.insert(dir.join(".cargo/config"));
        files.insert(dir.join("rust-toolchain.toml"));
        files.insert(dir.join("rust-toolchain"));
    }
    files.insert(crate::toolchain::state::path(&crate::toolchain::Layout::current().toolchains()));
    files.insert(cargo.with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX)));
    files.insert(util::cargo_home().join("config.toml"));
    files.insert(util::cargo_home().join("config"));
    let files: Vec<Value> = files.iter().filter(|f| !immutable.iter().any(|i| f.starts_with(i))).map(|f| file_state(f)).collect();
    let stamp = json!({ "format": FORMAT, "units": units, "warnings_line": warnings_line, "files": files });
    let dir = store_dir.join("prebuilt");
    if util::create_private_dir(&dir).and_then(|_| util::write_json_fast(&stamp_path(store_dir, key), &stamp)).is_err() {
        return;
    }
    // A stamp per distinct command line: keep the most recent few.
    let mut stamps: Vec<(u64, PathBuf)> = fs::read_dir(&dir).into_iter().flatten().flatten().filter_map(|e| Some((util::mtime_ns(&e.metadata().ok()?), e.path()))).collect();
    if stamps.len() > KEEP {
        stamps.sort();
        for (_, p) in &stamps[..stamps.len() - KEEP] {
            let _ = fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_holds_until_any_file_it_names_changes_appears_or_goes() {
        let base = std::env::temp_dir().join(format!("qrust-prebuilt-{}", util::new_id()));
        let (ws, store) = (base.join("ws"), base.join("store"));
        fs::create_dir_all(ws.join("src")).unwrap();
        fs::create_dir_all(ws.join("target/debug/deps")).unwrap();
        fs::write(ws.join("Cargo.toml"), "[package]\nname = \"app\"\n").unwrap();
        fs::write(ws.join("Cargo.lock"), "").unwrap();
        fs::write(ws.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(ws.join("src/util.rs"), "\n").unwrap();
        // rustc's dep-info: paths relative to the workspace root, where Cargo runs rustc.
        fs::write(ws.join("target/debug/deps/app-0123.d"), "/x/target/debug/deps/app-0123: src/main.rs src/util.rs\n\nsrc/main.rs:\nsrc/util.rs:\n").unwrap();
        fs::write(ws.join("target/debug/deps/app-0123"), "binary").unwrap();
        let exe = util::path_str(&ws.join("target/debug/deps/app-0123"));
        let messages = vec![json!({ "reason": "compiler-artifact", "filenames": [exe], "executable": exe, "manifest_path": util::path_str(&ws.join("Cargo.toml")) })];
        let cargo = ws.join("bin/cargo");
        let built = |warnings_line| Built { messages: &messages, cwd: &ws, ws_root: Some(&ws), cargo: &cargo, units: 3, warnings_line };
        record(&store, "k", &built(Some("1 warning in p: unused_variables x1")));
        assert_eq!(fresh(&store, "k"), Some((3, Some("1 warning in p: unused_variables x1".to_string()))));
        assert_eq!(fresh(&store, "another command"), None);

        let edit = |p: &Path, text: &str| {
            std::thread::sleep(std::time::Duration::from_millis(5));
            fs::write(p, text).unwrap();
        };
        // A source the unit read.
        edit(&ws.join("src/util.rs"), "pub fn f() {}\n");
        assert_eq!(fresh(&store, "k"), None);
        record(&store, "k", &built(None));
        assert!(fresh(&store, "k").is_some());
        // Cargo configuration that did not exist.
        fs::create_dir_all(ws.join(".cargo")).unwrap();
        fs::write(ws.join(".cargo/config.toml"), "[build]\njobs = 1\n").unwrap();
        assert_eq!(fresh(&store, "k"), None);
        record(&store, "k", &built(None));
        // The lockfile, and the program itself going away.
        edit(&ws.join("Cargo.lock"), "# changed\n");
        assert_eq!(fresh(&store, "k"), None);
        record(&store, "k", &built(None));
        fs::remove_file(ws.join("target/debug/deps/app-0123")).unwrap();
        assert_eq!(fresh(&store, "k"), None);
        fs::remove_dir_all(&base).ok();
    }
}
