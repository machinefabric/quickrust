//! Skipping rustup's proxy (fast mode).
//!
//! `cargo` on `PATH` is usually rustup's proxy: it works out which toolchain applies
//! (`+toolchain`, `RUSTUP_TOOLCHAIN`, directory overrides, `rust-toolchain.toml`, the default),
//! sets a few variables and executes that toolchain's Cargo. That costs ~5 ms on every
//! command. Haul learns the outcome once, by running a probe *through* the proxy (an external
//! subcommand that is Haul itself and reports its environment) and the same probe directly
//! against the toolchain's Cargo; the difference is exactly what rustup sets. Later commands
//! with the same inputs go straight to that Cargo with those variables.
//!
//! The key holds everything rustup's choice depends on: the proxy binary, the working
//! directory, every `rust-toolchain(.toml)` above it, rustup's settings file (default and
//! directory overrides), `+toolchain`, and the `RUSTUP_*`, `CARGO_HOME` and library-path
//! variables. Anything unexpected means using the proxy as before.

use crate::util;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Name under which the Haul binary reports its environment when Cargo runs it as a subcommand.
pub const PROBE_NAME: &str = "cargo-haul-env-probe";

/// Environment that decides rustup's choice or that rustup changes.
fn relevant(k: &str) -> bool {
    k.starts_with("RUSTUP_") || matches!(k, "CARGO_HOME" | "PATH" | "LD_LIBRARY_PATH" | "DYLD_LIBRARY_PATH" | "DYLD_FALLBACK_LIBRARY_PATH")
}

fn digest_file(p: &Path) -> Option<String> {
    util::sha256_file(p).ok().map(|h| h[..32].to_string())
}

fn key(proxy: &Path, toolchain: Option<&str>, cwd: &Path) -> Option<String> {
    let md = std::fs::metadata(proxy).ok()?;
    let mut parts = vec!["haul-toolchain-2".to_string(), util::path_str(&util::canon(proxy)), md.len().to_string(), util::mtime_ns(&md).to_string()];
    parts.push(format!("cwd={}", util::path_str(cwd)));
    parts.push(format!("+{}", toolchain.unwrap_or("")));
    let mut env: Vec<String> = std::env::vars().filter(|(k, _)| relevant(k)).map(|(k, v)| format!("{}={}", k, v)).collect();
    env.sort();
    parts.extend(env);
    let rustup_home = std::env::var_os("RUSTUP_HOME").map(PathBuf::from).or_else(|| util::home_dir().map(|h| h.join(".rustup")))?;
    parts.push(format!("settings={}", digest_file(&rustup_home.join("settings.toml")).unwrap_or_default()));
    for dir in cwd.ancestors() {
        for name in ["rust-toolchain.toml", "rust-toolchain"] {
            let p = dir.join(name);
            if p.is_file() {
                parts.push(format!("{}={}", util::path_str(&p), digest_file(&p).unwrap_or_default()));
            }
        }
    }
    Some(util::digest_parts(parts))
}

fn store_path() -> PathBuf {
    util::haul_home().join("toolchains.json")
}

/// The toolchain's Cargo and the variables rustup would set for it, if known for these inputs.
pub fn resolve(proxy: &Path, toolchain: Option<&str>, cwd: &Path) -> Option<(PathBuf, Vec<(String, String)>)> {
    if cfg!(not(unix)) {
        return None;
    }
    let k = key(proxy, toolchain, cwd)?;
    let known = util::read_json(&store_path()).unwrap_or_else(|| json!({}));
    let entry = match known.get(&k) {
        Some(e) => e.clone(),
        None => {
            let e = probe(proxy, toolchain, cwd)?;
            let mut all = known.as_object().cloned().unwrap_or_default();
            // A small bounded map: old resolutions simply get learned again.
            if all.len() >= 256 {
                all.clear();
            }
            all.insert(k, e.clone());
            let _ = util::create_private_dir(&util::haul_home()).and_then(|_| util::write_atomic_fast(&store_path(), &serde_json::to_vec(&Value::Object(all)).unwrap_or_default()));
            e
        }
    };
    // Learned: the `cargo` found is Cargo itself, not a proxy; there is nothing to skip.
    if entry["proxy"] == json!(false) {
        return None;
    }
    let cargo = PathBuf::from(entry["cargo"].as_str()?);
    // An uninstalled or replaced toolchain: back to the proxy, which may install it.
    if !util::is_executable(&cargo) {
        return None;
    }
    let mut env: Vec<(String, String)> = entry["env"].as_object()?.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect();
    // rustup counts nested proxy invocations; continue the count as it would.
    let depth = std::env::var("RUST_RECURSION_COUNT").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    env.push(("RUST_RECURSION_COUNT".into(), (depth + 1).to_string()));
    Some((cargo, env))
}

fn run_probe(program: &Path, toolchain: Option<&str>, cwd: &Path, bin: &Path) -> Option<BTreeMap<String, String>> {
    let path = std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default())).ok()?;
    let mut cmd = Command::new(program);
    if let Some(t) = toolchain {
        cmd.arg(format!("+{}", t));
    }
    let out = cmd.arg(&PROBE_NAME["cargo-".len()..]).current_dir(cwd).env("PATH", path).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Runs the probe through the proxy and directly; records the toolchain's Cargo and rustup's variables.
fn probe(proxy: &Path, toolchain: Option<&str>, cwd: &Path) -> Option<Value> {
    let bin = util::haul_home().join("probe-bin");
    let link = bin.join(PROBE_NAME);
    if !link.exists() {
        util::create_private_dir(&bin).ok()?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(crate::resolve::self_identity().0, &link).ok()?;
    }
    let via = run_probe(proxy, toolchain, cwd, &bin)?;
    let cargo = PathBuf::from(via.get("CARGO")?);
    if !cargo.is_absolute() {
        return None;
    }
    if util::canon(&cargo) == util::canon(proxy) {
        // Not a proxy: remembered, so that later commands do not probe again.
        return Some(json!({ "proxy": false, "learned_at": util::iso8601(util::now_wall_ns()) }));
    }
    let direct = run_probe(&cargo, None, cwd, &bin)?;
    let mut env = Map::new();
    for k in via.keys().chain(direct.keys()) {
        if k == "CARGO" || k == "RUST_RECURSION_COUNT" {
            continue;
        }
        match (via.get(k), direct.get(k)) {
            (Some(a), Some(b)) if a == b => {}
            // rustup may prepend `$CARGO_HOME/bin` to PATH; the probe's own directory is not
            // part of what it set. PATH is in the key, so the rest is exact for later commands.
            (Some(a), _) if k == "PATH" => {
                let kept: Vec<PathBuf> = std::env::split_paths(a).filter(|p| p != &bin).collect();
                env.insert(k.clone(), json!(util::path_str(Path::new(&std::env::join_paths(kept).ok()?))));
            }
            (Some(a), _) => {
                env.insert(k.clone(), json!(a));
            }
            // The proxy removed a variable: not something we can reproduce exactly.
            (None, Some(_)) => return None,
            (None, None) => {}
        }
    }
    Some(json!({ "proxy": true, "cargo": util::path_str(&cargo), "env": env, "learned_at": util::iso8601(util::now_wall_ns()) }))
}

/// Entry point when Cargo runs this binary as the probe subcommand: print the environment.
pub fn report_environment() -> ! {
    let vars: BTreeMap<String, String> = std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect();
    println!("{}", serde_json::to_string(&vars).unwrap_or_default());
    std::process::exit(0)
}
