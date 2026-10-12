//! One orchestrated Cargo invocation: resolve, apply policy, delegate, record.
//!
//! The delegated Cargo inherits stdin/stdout/stderr, the full environment and
//! the raw argument vector. Haul only adds its own variables (and, when a
//! policy says so, `CARGO_INCREMENTAL`), and records what it did.

use crate::config::Config;
use crate::proc::{self, ExitInfo};
use crate::resolve::{self, Family, Invocation};
use crate::store::{self, WsStore};
use crate::ui::Rendering;
use crate::util::{self, canon, path_str};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Invoked under the name `cargo`.
    Shim,
    /// `haul <cargo-command> …`.
    Explicit,
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub args: Vec<OsString>,
    pub mode: Mode,
    /// Extra environment for the delegated Cargo (benchmarks and experiments only).
    pub env: Vec<(String, String)>,
    /// Scenario label attached to the record (benchmarks and experiments only).
    pub scenario: Option<Value>,
    /// Finalize synchronously regardless of configuration.
    pub force_finalize: bool,
    pub cwd: Option<PathBuf>,
    /// Send the delegated command's stdout to our stderr. Used by management commands
    /// (benchmarks, experiments) whose own stdout carries their result.
    pub stdout_to_stderr: bool,
    /// A private artifact cache (benchmarks and experiments, whose repetitions must not reuse
    /// each other's results); the shared one in `HAUL_HOME` otherwise.
    pub artifact_cache: Option<PathBuf>,
}

impl Spec {
    pub fn new(args: Vec<OsString>, mode: Mode) -> Spec {
        Spec { args, mode, env: Vec::new(), scenario: None, force_finalize: false, cwd: None, stdout_to_stderr: false, artifact_cache: None }
    }
}

pub struct Outcome {
    pub exit: ExitInfo,
    pub session_id: Option<String>,
    pub store: Option<WsStore>,
    pub report: Option<Value>,
    /// Exit status override from a blocking policy on an otherwise successful command.
    pub policy_exit: Option<i32>,
}

/// Environment variables that influence builds and may be persisted (PRIV-003).
const ENV_ALLOW: &[&str] = &[
    "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTDOCFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS", "CARGO_INCREMENTAL", "CARGO_BUILD_TARGET", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", "CARGO_BUILD_JOBS",
    "CARGO_BUILD_RUSTFLAGS", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "RUSTUP_TOOLCHAIN", "RUSTC_BOOTSTRAP", "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "LDFLAGS", "MACOSX_DEPLOYMENT_TARGET", "SDKROOT",
    "PKG_CONFIG_PATH", "CI", "CARGO_NET_OFFLINE", "CARGO_HOME", "MAKEFLAGS",
];
const ENV_ALLOW_PREFIX: &[&str] = &["CARGO_PROFILE_", "CARGO_TARGET_", "CARGO_UNSTABLE_"];

/// Build-relevant environment, with secrets reduced to "set" and paths tokenized (COMP-043, FR-010.5).
pub fn env_fingerprint(tok: &util::Tokenizer, mode: &str) -> Value {
    let mut vars = serde_json::Map::new();
    let mut parts = Vec::new();
    let mut names: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.to_string_lossy().into_owned())))
        .filter(|(k, _)| ENV_ALLOW.contains(&k.as_str()) || ENV_ALLOW_PREFIX.iter().any(|p| k.starts_with(p)) || k.starts_with("SCCACHE_"))
        .collect();
    names.sort();
    for (k, v) in names {
        let digest = util::sha256_hex(v.as_bytes())[..16].to_string();
        parts.push(format!("{}={}", k, digest));
        if mode == "none" {
            continue;
        }
        if util::is_secret_name(&k) {
            vars.insert(k, json!({ "set": true, "value": "<redacted>" }));
        } else {
            vars.insert(k, json!({ "value": tok.apply(&v), "sha": digest }));
        }
    }
    json!({ "digest": util::digest_parts(parts), "vars": vars })
}

fn wrapper_binary() -> Option<PathBuf> {
    let (exe, _) = resolve::self_identity();
    let dir = exe.parent()?;
    util::exe_candidates(dir, "haul-rustc-wrapper").into_iter().find(|p| util::is_executable(p))
}

/// Compact record of one Cargo JSON message, kept for analysis.
pub(crate) fn summarize_message(line: &[u8]) -> Option<Value> {
    if !line.starts_with(b"{\"reason\"") {
        return None;
    }
    let v: Value = serde_json::from_slice(line).ok()?;
    match v["reason"].as_str()? {
        "compiler-artifact" => Some(json!({
            "reason": "compiler-artifact",
            "package_id": v["package_id"],
            "target": v["target"]["name"],
            "kind": v["target"]["kind"],
            "fresh": v["fresh"],
            "features": v["features"],
            "filenames": v["filenames"],
            "executable": v["executable"],
            "manifest_path": v["manifest_path"],
            "opt_level": v["profile"]["opt_level"],
            "test": v["profile"]["test"],
        })),
        "build-script-executed" => Some(json!({ "reason": "build-script-executed", "package_id": v["package_id"], "out_dir": v["out_dir"], "linked_libs": v["linked_libs"] })),
        "build-finished" => Some(json!({ "reason": "build-finished", "success": v["success"] })),
        "compiler-message" => Some(json!({ "reason": "compiler-message", "package_id": v["package_id"], "level": v["message"]["level"] })),
        _ => None,
    }
}

/// Copies Cargo's stdout through byte-for-byte while observing JSON lines (FR-001.2, COMP-005).
fn pump_json(mut from: impl Read, max_records: usize) -> Vec<Value> {
    let mut out = std::io::stdout().lock();
    let mut records = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        // Forward first and unbuffered so consumers see the stream exactly as Cargo wrote it.
        // If our own stdout is gone, drop the pipe so Cargo observes the same broken pipe.
        if out.write_all(&buf[..n]).and_then(|_| out.flush()).is_err() {
            break;
        }
        pending.extend_from_slice(&buf[..n]);
        while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
            if records.len() < max_records {
                if let Some(r) = summarize_message(&pending[..pos]) {
                    records.push(r);
                }
            }
            pending.drain(..=pos);
        }
        // A non-JSON line of unbounded length (program output) must not grow memory.
        if pending.len() > 8 * 1024 * 1024 {
            pending.clear();
        }
    }
    records
}

struct Plan {
    env_set: Vec<(String, String)>,
    env_remove: Vec<String>,
    instrumented: bool,
    events_in_parent: bool,
    events_dir: Option<PathBuf>,
    wrappers: Value,
    policy: Value,
    sccache: Option<String>,
    warnings: Vec<String>,
    /// Parallel-frontend threads the wrapper applies to Cargo's units (fast mode).
    frontend_threads: Option<u32>,
    /// The fast linker in use (fast mode).
    linker: Option<PathBuf>,
    /// Early cut-off is on: the wrapper compiles with Haul's compiler driver when it is built.
    early_cutoff: bool,
}

/// Decides wrapper composition and cache/incremental policy for one session.
fn plan(inv: &Invocation, cfg: &Config, subset: &resolve::CargoConfigSubset, store: &WsStore, id: &str, artifact_cache: &Path, engine: &crate::engine::Engine) -> Result<Plan, String> {
    let mut p = Plan {
        env_set: Vec::new(),
        env_remove: Vec::new(),
        instrumented: false,
        events_in_parent: false,
        events_dir: None,
        wrappers: json!(null),
        policy: json!(null),
        sccache: None,
        warnings: Vec::new(),
        frontend_threads: None,
        linker: None,
        early_cutoff: false,
    };
    let wants = cfg.bool("measurement.enabled") && (inv.family.builds() || (inv.family == Family::External && cfg.str("compatibility.unknown-command") == "instrument"));
    let wrapper = wrapper_binary();
    let existing = resolve::existing_rustc_wrapper(subset);
    let ours = |w: &str| wrapper.as_ref().map(|me| canon(Path::new(w)) == canon(me)).unwrap_or(false);
    let already_ours = existing.as_ref().map(|(w, _)| ours(w)).unwrap_or(false);
    let mut inner: Option<(String, String)> = if already_ours {
        std::env::var(crate::ENV_INNER_WRAPPER).ok().filter(|v| !v.is_empty()).map(|w| (w, "parent session".to_string()))
    } else {
        existing.clone()
    };

    // Cache policy (FR-100). `inherit` and `none` never add a cache; neither removes the user's wrapper.
    let provider = cfg.str("cache.provider");
    let fail_open = cfg.str("cache.fail") == "open";
    let mut cache_state = "disabled";
    let mut cache_detail = String::new();
    if inv.family.builds() && !already_ours {
        match (&inner, provider) {
            (Some((w, _)), _) if crate::cache::is_sccache(w) => {
                cache_state = "local";
                cache_detail = "sccache configured by the user".into();
            }
            (Some((w, src)), "sccache") => {
                cache_state = "conflict";
                cache_detail = format!("cache.provider = sccache, but the existing rustc wrapper {:?} ({}) is preserved instead", w, src);
                p.warnings.push(cache_detail.clone());
            }
            (None, "sccache") if wants && wrapper.is_some() => match util::which("sccache") {
                Some(bin) => {
                    inner = Some((path_str(&bin), "policy cache.provider".into()));
                    cache_state = "local";
                    cache_detail = "sccache chained by cache.provider policy".into();
                }
                None if fail_open => {
                    cache_state = "degraded";
                    cache_detail = "sccache is not installed; compiling without a cache (cache.fail = open)".into();
                    p.warnings.push(cache_detail.clone());
                }
                None => return Err("cache.provider = sccache but sccache is not on PATH (cache.fail = closed)".into()),
            },
            (None, "sccache") => {
                cache_state = "degraded";
                cache_detail = "sccache needs the Haul rustc wrapper, which is unavailable or disabled".into();
                p.warnings.push(cache_detail.clone());
            }
            _ => {}
        }
    }
    let sccache_bin = inner.as_ref().filter(|(w, _)| crate::cache::is_sccache(w)).map(|(w, _)| w.clone());
    let mut before = None;
    if let (Some(bin), true) = (&sccache_bin, inv.family.builds() && !already_ours) {
        // The statistics snapshot doubles as the reachability preflight (FR-100.6).
        match crate::cache::stats(bin) {
            Ok(s) => before = Some(s),
            Err(e) if inner.as_ref().map(|(_, src)| src.starts_with("policy")).unwrap_or(false) => {
                if !fail_open {
                    return Err(format!("sccache is unavailable and cache.fail = closed: {}", e));
                }
                cache_state = "degraded";
                cache_detail = format!("sccache is unavailable ({}); compiling without it (cache.fail = open)", e);
                p.warnings.push(cache_detail.clone());
                inner = None;
            }
            Err(e) => {
                cache_state = "degraded";
                cache_detail = format!("sccache statistics unavailable: {}", e);
            }
        }
    }
    let sccache_active = inner.as_ref().map(|(w, _)| crate::cache::is_sccache(w)).unwrap_or(false);
    if sccache_active {
        p.sccache = sccache_bin;
    }

    // Incremental policy (FR-090): scenario-specific, explicit user choice wins, every change recorded.
    let workload = if util::is_ci() { "ci" } else { "local-dev" };
    let setting = cfg.str(&format!("incremental.{}", workload)).to_string();
    let explicit = std::env::var("CARGO_INCREMENTAL").ok();
    let (decision, applied, rationale) = match (setting.as_str(), &explicit) {
        ("on" | "off", Some(v)) => ("cargo-default", None, format!("explicit CARGO_INCREMENTAL={} is respected over the `{}` policy from {}", v, setting, cfg.origin(&format!("incremental.{}", workload)))),
        ("on", None) if inv.family.builds() => ("force-on", Some("1"), format!("incremental.{} = on ({})", workload, cfg.origin(&format!("incremental.{}", workload)))),
        ("off", None) if inv.family.builds() => ("force-off", Some("0"), format!("incremental.{} = off ({}){}", workload, cfg.origin(&format!("incremental.{}", workload)), if sccache_active { "; lets sccache cache workspace crates" } else { "" })),
        ("experiment", _) => ("experiment", None, "left at Cargo's default; compare alternatives with `haul experiment`".to_string()),
        _ => ("cargo-default", None, "no policy override; Cargo's profile settings decide".to_string()),
    };
    if let Some(v) = applied {
        p.env_set.push(("CARGO_INCREMENTAL".into(), v.into()));
    }
    p.policy = json!({
        "workload": workload,
        "incremental": { "setting": setting, "decision": decision, "applied_env": applied.map(|v| format!("CARGO_INCREMENTAL={}", v)), "rationale": rationale, "explicit_user_value": explicit },
        "cache": { "provider": provider, "state": cache_state, "detail": cache_detail, "fail": cfg.str("cache.fail"), "sccache_active": sccache_active },
    });
    if let Some(b) = before {
        p.policy["cache"]["stats_before"] = b;
    }

    if wants && already_ours {
        // A nested build inside an instrumented session: its rustc events land in the parent's stream.
        p.instrumented = true;
        p.events_in_parent = true;
    } else if wants {
        match &wrapper {
            Some(w) => {
                let dir = store.events_dir(id);
                // Haul's artifact cache: fast mode only, and never behind a wrapper the user
                // configured, which may be a cache with rules of its own.
                let artifacts = cfg.str("mode") == "fast" && cfg.str("cache.artifacts") == "on" && inner.is_none();
                // The parallel frontend and the linker need `-Z` flags (RUSTC_BOOTSTRAP on a stable
                // compiler): not behind a user wrapper, and not when the user decides about
                // RUSTC_BOOTSTRAP themselves.
                let unlockable = cfg.str("mode") == "fast" && inner.is_none() && std::env::var_os("RUSTC_BOOTSTRAP").is_none();
                let threads = if unlockable { crate::config::frontend_threads(cfg.str("fast.frontend-threads")) } else { None };
                let linker = if unlockable && cfg.str("fast.linker") == "auto" { fast_linker(engine) } else { None };
                let settings = crate::wrapper::Settings {
                    artifact_cache: artifacts.then(|| artifact_cache.to_path_buf()),
                    frontend_threads: threads,
                    min_link_store_ns: cfg.int("cache.artifacts-min-link-ms") * 1_000_000,
                    linker_dir: linker.as_ref().map(|(dir, _)| dir.clone()),
                    early_cutoff: artifacts && crate::driver::wanted(cfg),
                    target_dir: engine.target.as_ref().map(|t| t.dir.clone()),
                };
                // The wrapper runs only with its settings in place; otherwise the build is uninstrumented.
                if util::create_private_dir(&dir).and_then(|_| settings.write(&dir)).is_ok() {
                    p.instrumented = true;
                    p.env_set.push(("RUSTC_WRAPPER".into(), path_str(w)));
                    p.env_set.push((crate::ENV_EVENT_DIR.into(), path_str(&dir)));
                    // Only a cache Haul chained itself may be skipped when it cannot start; a wrapper the
                    // user configured fails exactly as it would under upstream Cargo.
                    let skippable = fail_open && inner.as_ref().map(|(_, src)| src.starts_with("policy")).unwrap_or(false);
                    p.env_set.push((crate::ENV_WRAPPER_FAIL.into(), if skippable { "open" } else { "closed" }.into()));
                    match &inner {
                        Some((iw, _)) => p.env_set.push((crate::ENV_INNER_WRAPPER.into(), iw.clone())),
                        None => p.env_remove.push(crate::ENV_INNER_WRAPPER.into()),
                    }
                    p.policy["cache"]["artifacts"] = json!(if artifacts { "on" } else if inner.is_some() { "off: a rustc wrapper is configured" } else { "off" });
                    p.frontend_threads = threads;
                    p.early_cutoff = settings.early_cutoff;
                    p.linker = linker.map(|(_, wild)| wild);
                    p.events_dir = Some(dir);
                }
            }
            None => p.warnings.push("haul-rustc-wrapper was not found next to the haul binary; rustc-level observation is disabled".into()),
        }
    }
    p.wrappers = json!({
        "haul": if p.instrumented { wrapper.as_ref().map(|w| path_str(w)) } else { None },
        "inner": inner.as_ref().map(|(w, _)| w.clone()),
        "inner_source": inner.as_ref().map(|(_, s)| s.clone()),
        "workspace_wrapper": std::env::var("RUSTC_WORKSPACE_WRAPPER").ok().filter(|v| !v.is_empty()),
        "order": "haul-rustc-wrapper → existing RUSTC_WRAPPER → RUSTC_WORKSPACE_WRAPPER (applied by Cargo) → rustc",
    });
    Ok(p)
}

/// The fast linker for this machine: wild in Haul's `bin` or on PATH, for a Linux host with
/// glibc. Returns the directory whose `ld` is wild — handed to the compiler driver with `-B` —
/// and wild itself. `None` when wild is not installed or the link cannot be maintained.
fn fast_linker(engine: &crate::engine::Engine) -> Option<(PathBuf, PathBuf)> {
    let _ = engine;
    if !crate::toolchain::host().ends_with("-linux-gnu") {
        return None;
    }
    let wild = util::exe_candidates(&crate::engine::bin_dir(), "wild").into_iter().find(|p| util::is_executable(p)).or_else(|| util::which("wild"))?;
    let wild = util::canon(&wild);
    let dir = util::haul_home().join("linker");
    let ld = dir.join("ld");
    if std::fs::read_link(&ld).ok().as_deref() != Some(wild.as_path()) {
        // Replaced atomically: a concurrent build sees the old link or the new one.
        let tmp = dir.join(format!(".ld.{}", std::process::id()));
        util::create_private_dir(&dir).ok()?;
        let _ = std::fs::remove_file(&tmp);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&wild, &tmp).ok()?;
        std::fs::rename(&tmp, &ld).ok()?;
    }
    Some((dir, wild))
}

/// Units of this session restored from the artifact cache, from the wrapper's events.
/// Units restored from the artifact cache, and how many of those include build-script code
/// built under another `OUT_DIR`.
fn count_cache_hits(dir: &Path) -> (u64, u64) {
    let hits: Vec<Value> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| util::read_json(&e.path()))
        .filter(|v| v["cache"]["result"] == json!("hit"))
        .collect();
    let foreign = hits.iter().filter(|v| v["cache"]["foreign_out_dir"].is_string()).count() as u64;
    (hits.len() as u64, foreign)
}

/// `args` with Haul's `--config` overlays in the group of `--config` arguments Cargo applies:
/// before the subcommand, or after it when the user put theirs there (Cargo then ignores the
/// ones before it).
fn with_overlays(args: &[OsString], inv: &Invocation, overlays: &[String]) -> Vec<OsString> {
    let at = match (inv.configs_after_command, inv.command_index) {
        (true, Some(ci)) => ci + 1,
        _ => usize::from(inv.toolchain.is_some()),
    }
    .min(args.len());
    let mut out = args[..at].to_vec();
    out.extend(overlays.iter().map(OsString::from));
    out.extend_from_slice(&args[at..]);
    out
}

/// `haul upstream …`: the toolchain's Cargo with no policy and no instrumentation (COMP-013).
/// Haul's configuration is not read: the escape hatch works when it is broken.
pub fn exec_upstream(args: Vec<OsString>) -> ! {
    let inv = resolve::classify(&args);
    if resolve::recursion_detected() {
        fail("refusing to run Cargo: the toolchain's `cargo` is Haul itself");
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = Config::defaults();
    let engine = crate::engine::for_command(&inv, &cwd, &cfg).unwrap_or_else(|e| fail(&e));
    let mut cmd = engine.command(&args, &inv);
    cmd.env(crate::ENV_EXEC_GUARD, format!("{}:upstream", std::process::id()));
    let err = proc::exec(&mut cmd);
    fail(&format!("could not execute Cargo {}: {}", engine.cargo().display(), err))
}

pub fn fail(msg: &str) -> ! {
    eprintln!("haul: error: {}", msg);
    std::process::exit(crate::EXIT_HAUL_ERROR)
}

/// Runs one Cargo invocation. `Err` means Haul could not delegate at all.
/// The triple a `cargo run` builds for: its `--target`, or the host of the compiler.
fn target_triple(inv: &Invocation, engine: &crate::engine::Engine) -> Option<String> {
    if let Some(t) = inv.targets.first() {
        return Some(t.clone());
    }
    let rustc = engine.env.iter().find(|(k, _)| k == "RUSTC").map(|(_, v)| v.clone()).or_else(|| std::env::var("RUSTC").ok().filter(|r| !r.is_empty())).unwrap_or_else(|| path_str(&engine.rustc()));
    let vv = crate::rustc_info::version_verbose(&util::memo_dir("rustc"), &rustc).ok()?;
    vv.lines().find_map(|l| l.strip_prefix("host: ")).map(str::to_string)
}

pub fn execute(spec: &Spec) -> Result<Outcome, String> {
    let preflight = Instant::now();
    let cwd = match &spec.cwd {
        Some(c) => c.clone(),
        None => std::env::current_dir().map_err(|e| format!("cannot determine the current directory: {}", e))?,
    };
    let mut inv = resolve::classify(&spec.args);
    if resolve::recursion_detected() {
        return Err(format!("refusing to run Cargo: the toolchain's `cargo` is Haul itself (session guard {}); a linked toolchain must hold a real Cargo", std::env::var(crate::ENV_EXEC_GUARD).unwrap_or_default()));
    }
    let cfg = Config::load(&cwd).map_err(|e| format!("invalid Haul configuration: {}", e))?;
    // Haul installs programs itself.
    if matches!(inv.command.as_deref(), Some("install" | "uninstall")) && spec.scenario.is_none() {
        let code = crate::install::run(&spec.args, &inv, &cwd, &cfg)?;
        return Ok(Outcome { exit: ExitInfo { code: Some(code), signal: None }, session_id: None, store: None, report: None, policy_exit: None });
    }
    // The disk nearly full: Haul's cache gives back what it can first (for ten seconds at most).
    if let Some(freed) = crate::space::before_command(&cfg) {
        match freed {
            Ok(c) if c.after < c.before => crate::ui::note(&format!("the disk is nearly full: Haul's cache gave back {} ({} → {}){}", util::human_bytes(c.before - c.after), util::human_bytes(c.before), util::human_bytes(c.after), if c.stopped_early { "; more in the background" } else { "" })),
            // Nothing of Haul's left to give: said once an hour, not on every command.
            Ok(c) => crate::ui::notice_hourly("disk-full", &format!("the disk is nearly full, and nothing in Haul's cache ({}) can go now", util::human_bytes(c.after))),
            Err(e) => crate::ui::note(&format!("the disk is nearly full, and Haul's cache could not give back space: {}", e)),
        }
    }
    let engine = crate::engine::for_command(&inv, &cwd, &cfg)?;
    let parent = std::env::var(crate::ENV_SESSION).ok().filter(|v| !v.is_empty());
    let nested_upstream = parent.is_some() && cfg.str("compatibility.nested-cargo") == "upstream";

    let start_dir = crate::builddir::start_dir(&inv, &cwd);
    // The workspace's build directory is in use (Haul's cache leaves it alone) while this runs.
    let _in_use = match (&engine.target, &engine.workspace_root) {
        (Some(t), Some(root)) if t.managed => {
            crate::builddir::mark_used(&t.dir, root);
            Some(crate::builddir::in_use(&t.dir))
        }
        _ => None,
    };
    let subset = if inv.family.builds() || inv.family == Family::External { resolve::cargo_config_subset(&start_dir, &inv.configs) } else { Default::default() };
    resolve::apply_aliases(&mut inv, &subset);

    // Queries, help output and opted-out nested invocations: replace ourselves with Cargo.
    if spec.scenario.is_none() && (inv.command.is_none() || inv.family == Family::Query || nested_upstream) {
        let mut cmd = engine.command(&spec.args, &inv);
        cmd.current_dir(&cwd).env(crate::ENV_EXEC_GUARD, format!("{}:bypass", std::process::id()));
        let err = proc::exec(&mut cmd);
        return Err(format!("could not execute Cargo {}: {}", engine.cargo().display(), err));
    }

    let ws_root = engine.workspace_root.clone();
    let store = WsStore::open(ws_root.as_deref());
    let store_ok = store.ensure().is_ok();
    let id = util::new_id();
    let artifact_cache = spec.artifact_cache.clone().unwrap_or_else(|| util::haul_home().join("cache").join("artifacts"));
    let mut plan = plan(&inv, &cfg, &subset, &store, &id, &artifact_cache, &engine)?;
    if !store_ok {
        plan.instrumented = false;
    }
    // Fast mode: settings that keep program behaviour identical, unless the user made them.
    let mut user_keys = subset.profile_keys.clone();
    if let Some(root) = &ws_root {
        user_keys.extend(resolve::manifest_profile_keys(root));
    }
    user_keys.extend(resolve::env_profile_keys());
    let fast = crate::fast::overlays(&inv, cfg.str("mode"), &user_keys);
    plan.policy["fast"] = fast.record;
    if let Some(applied) = plan.policy["fast"]["applied"].as_array_mut() {
        if let Some(n) = plan.frontend_threads {
            applied.push(json!({ "key": "rustc -Zthreads", "value": n.to_string(), "why": "rustc's parallel frontend speeds up large crates compiled from scratch; applied by the wrapper to Cargo's non-incremental units only, never to compiler probes run by build scripts" }));
        }
        if let Some(wild) = &plan.linker {
            applied.push(json!({ "key": "linker", "value": format!("wild ({})", path_str(wild)), "why": "relinking after an edit is markedly faster than with rustc's bundled LLD; applied to host builds of linked outputs when no linker is configured" }));
        }
    }

    // Haul's own rendering of the command's output (a terminal, nothing the user chose).
    let mut rendering = if !spec.stdout_to_stderr && spec.scenario.is_none() { crate::ui::rendering(&inv, &spec.args, &cfg) } else { Rendering::Off };
    // The command as it reaches Cargo, with fast mode's settings.
    let mut args = with_overlays(&spec.args, &inv, &fast.args);
    if let Rendering::BuildFirst(how) = rendering {
        match target_triple(&inv, &engine) {
            Some(target) => {
                // The command's own arguments (what follows `--` is for the program) and what
                // keeps it from running anything.
                let (flags, configs) = crate::ui::prebuild(how, &resolve::self_identity().0, &target);
                let own: Vec<OsString> = spec.args.iter().take_while(|a| *a != "--").cloned().chain(flags.into_iter().map(OsString::from)).collect();
                args = with_overlays(&own, &inv, &[fast.args.clone(), configs].concat());
            }
            // A compiler that cannot name its host leaves the command to Cargo as it is.
            None => rendering = Rendering::Off,
        }
    }
    let render = rendering != Rendering::Off;
    // A build rendered: Cargo's JSON messages on stdout. A command that does not build: its stderr.
    let render_build = matches!(rendering, Rendering::Build | Rendering::BuildFirst(_));
    if render_build {
        args = crate::ui::with_arg(args, crate::ui::message_format_arg());
    }
    // Cargo's progress (units done of all, crates left to download) for Haul's status line; and
    // its colours for the lines Haul shows as they are, though it writes into a pipe.
    let mut progress_env: Vec<(String, String)> = if render { vec![("CARGO_TERM_PROGRESS_WHEN".into(), "always".into()), ("CARGO_TERM_PROGRESS_WIDTH".into(), "1000".into())] } else { vec![] };
    if rendering == Rendering::Lines && crate::ui::color() {
        progress_env.push(("CARGO_TERM_COLOR".into(), "always".into()));
    }
    let command = |args: &[OsString]| -> Command {
        let mut cmd = engine.command(args, &inv);
        cmd.current_dir(&cwd);
        for (k, v) in plan.env_set.iter().chain(spec.env.iter()).chain(progress_env.iter()) {
            cmd.env(k, v);
        }
        for k in &plan.env_remove {
            cmd.env_remove(k);
        }
        cmd.env(crate::ENV_SESSION, &id);
        cmd.env(crate::ENV_EXEC_GUARD, format!("{}:{}", std::process::id(), id));
        cmd
    };
    let mut cmd = command(&args);

    let tee = !spec.stdout_to_stderr && cfg.str("measurement.cargo-json") == "auto" && inv.json_messages && inv.family.builds() && !std::io::stdout().is_terminal();
    if rendering == Rendering::Lines {
        cmd.stderr(Stdio::piped());
    } else if tee || render {
        cmd.stdout(Stdio::piped());
        if render {
            cmd.stderr(Stdio::piped());
        }
    } else if spec.stdout_to_stderr {
        cmd.stdout(Stdio::from(std::io::stderr()));
    }
    for w in &plan.warnings {
        eprintln!("haul: warning: {}", w);
    }
    let lock = store::register_active(&id, ws_root.as_deref(), &cwd);
    let overhead_pre = preflight.elapsed().as_nanos() as u64;

    // A build Haul renders is skipped when its stamp shows nothing it depends on changed (see
    // prebuilt.rs). For `run` / `test` the command itself still checks freshness; for a build
    // alone Haul's verdict is the answer.
    let judged = render_build;
    let prebuilt_key = (judged && store_ok).then(|| crate::prebuilt::key(&args, &cwd, &engine.cargo(), &engine.env));
    let known_fresh = prebuilt_key.as_deref().and_then(|k| crate::prebuilt::fresh(&store.dir, k));

    let started_wall = util::now_wall_ns();
    let started = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<Value>>();
    let mut live: Option<crate::ui::Live> = None;
    let (mut exit, mut duration_ns) = match &known_fresh {
        Some((units, warnings_line)) => {
            crate::ui::up_to_date(*units, warnings_line.as_deref());
            (ExitInfo { code: Some(0), signal: None }, 0)
        }
        None => {
            let run = proc::run_with(&mut cmd, true, &mut |child| {
                if rendering == Rendering::Lines {
                    if let Some(err) = child.stderr.take() {
                        live = Some(crate::ui::start_lines(err, inv.alias_of.as_deref().or(inv.command.as_deref()).unwrap_or("")));
                    }
                } else if render {
                    if let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) {
                        live = Some(crate::ui::start(out, err, matches!(rendering, Rendering::BuildFirst(_)), inv.alias_of.as_deref().or(inv.command.as_deref()).unwrap_or("")));
                    }
                } else if let Some(out) = child.stdout.take() {
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(pump_json(out, 200_000));
                    });
                }
            });
            let duration_ns = started.elapsed().as_nanos() as u64;
            let (exit, _usage) = run.map_err(|e| format!("could not execute Cargo {}: {}", engine.cargo().display(), e))?;
            (exit, duration_ns)
        }
    };
    // Drain the pipe; a lingering grandchild holding it open must not hang us.
    let mut messages = if tee { rx.recv_timeout(std::time::Duration::from_secs(5)).ok() } else { None };
    if let Some(live) = live {
        let (hits, foreign) = plan.events_dir.as_deref().map(count_cache_hits).unwrap_or((0, 0));
        let fin = live.finish(exit, duration_ns, hits);
        if foreign > 0 {
            crate::ui::notice_foreign_out_dir(foreign);
        }
        if let Some(dir) = &plan.events_dir {
            let mut text = String::new();
            for l in &fin.log {
                text.push_str(&l.to_string());
                text.push('\n');
            }
            let _ = util::write_atomic_fast(&dir.join(crate::ui::OUTPUT_FILE), text.as_bytes());
        }
        if let (Some(key), true) = (&prebuilt_key, exit.success()) {
            let built = crate::prebuilt::Built { messages: &fin.messages, cwd: &cwd, ws_root: ws_root.as_deref(), cargo: &engine.cargo(), units: fin.units, warnings_line: fin.warnings_line.as_deref() };
            crate::prebuilt::record(&store.dir, key, &built);
        }
        messages = Some(fin.messages);
    }
    // Early cut-off needs Haul's compiler driver for this toolchain: built once, in the
    // background, after the build that found it missing (not alongside it, competing for the
    // processor).
    if plan.early_cutoff {
        let rustc = engine.rustc();
        let started = crate::driver::Compiler::of(&path_str(&rustc)).and_then(|c| crate::driver::start_build(&resolve::self_identity().0, &rustc, &c).map(|s| (s, c.version_verbose)));
        match started {
            Ok((true, vv)) if render => crate::ui::notice_driver_build(vv.lines().find_map(|l| l.strip_prefix("release: ")).unwrap_or("this toolchain")),
            Err(e) if render => crate::ui::notice_driver_unavailable(&e),
            _ => {}
        }
    }
    // Once a day, a channel's newer release is looked for in the background.
    if let Some(key) = &engine.channel {
        if let Err(e) = crate::toolchain::start_update(&crate::toolchain::Layout::current(), key, &resolve::self_identity().0, cfg.str("toolchain.auto-update")) {
            if render {
                crate::ui::note(&format!("Haul could not check for a newer Rust release: {}", e));
            }
        }
    }
    // The command itself, once what it needs is built: the program gets the real terminal.
    if let (Rendering::BuildFirst(how), true) = (rendering, exit.success()) {
        // Cargo replays the warnings of fresh units, which Haul has just summarised: they are
        // allowed here (display only: units stay fresh; Cargo versions without the setting
        // ignore it and replay them). `cargo run`'s "Finished … Running …" would repeat what
        // Haul reported; a test run's lines name the test binaries, and `--quiet` would change
        // libtest's output too.
        let quiet_warnings = ["--config".to_string(), "build.warnings=\"allow\"".to_string()];
        let mut second = with_overlays(&spec.args, &inv, &[fast.args.clone(), quiet_warnings.to_vec()].concat());
        if how == crate::ui::Prebuild::Runner {
            second = crate::ui::with_arg(second, "--quiet");
        }
        let again = Instant::now();
        let (e, _) = proc::run(&mut command(&second), true).map_err(|e| format!("could not execute Cargo {}: {}", engine.cargo().display(), e))?;
        exit = e;
        duration_ns += again.elapsed().as_nanos() as u64;
    }
    if let Some(l) = &lock {
        let _ = std::fs::remove_file(l);
    }

    let post = Instant::now();
    let mut outcome = Outcome { exit, session_id: None, store: None, report: None, policy_exit: None };
    if !store_ok {
        // Observation is optional; the build result stands on its own (NFR-020).
        eprintln!("haul: warning: state directory {} is not writable; this run was not recorded", store.dir.display());
        if cfg.bool("measurement.strict") && exit.success() {
            outcome.policy_exit = Some(crate::EXIT_HAUL_ERROR);
        }
        return Ok(outcome);
    }
    let tok = util::Tokenizer::new(ws_root.as_deref(), false);
    let argv: Vec<String> = spec.args.iter().map(|a| util::lossy(a)).collect();
    if let (Some(bin), Some(_)) = (&plan.sccache, plan.policy["cache"].get("stats_before")) {
        if let Ok(after) = crate::cache::stats(bin) {
            plan.policy["cache"]["stats_after"] = after;
        }
    }
    let mut notes: Vec<String> = subset.notes.clone();
    if spec.args.iter().any(|a| a.to_str().is_none()) {
        notes.push("the argument vector contained non-UTF-8 values; they were forwarded unchanged and are shown lossily here".into());
    }
    let messages_file = match (&messages, &plan.events_dir) {
        (Some(m), Some(dir)) => {
            let p = dir.join("cargo-messages.json");
            util::write_atomic_fast(&p, &serde_json::to_vec(m).unwrap_or_default()).ok().map(|_| path_str(&p))
        }
        _ => None,
    };
    let state = if exit.signal.is_some() {
        "cancelled"
    } else if exit.success() {
        "completed"
    } else {
        "failed"
    };
    let record = json!({
        "schema_version": crate::SCHEMA_VERSION,
        "id": id,
        "parent_session_id": parent,
        "state": state,
        "started_wall_ns": started_wall,
        "started_at": util::iso8601(started_wall),
        "duration_ns": duration_ns,
        "command": {
            "family": inv.family.name(),
            "name": inv.command,
            "alias_of": inv.alias_of,
            "entry": if spec.mode == Mode::Shim { "cargo-shim" } else { "haul" },
            "argv_redacted": std::iter::once("cargo".to_string()).chain(util::redact_args(&argv)).map(|a| tok.apply(&a)).collect::<Vec<_>>(),
            "toolchain_override": inv.toolchain,
            "manifest_path": inv.manifest_path.as_ref().map(|m| path_str(m)),
            "targets": inv.targets,
            "release": inv.release,
            "json_messages": inv.json_messages,
        },
        "cwd": path_str(&cwd),
        "workspace_root": ws_root.as_ref().map(|w| path_str(w)),
        "workspace_id": store.id,
        "upstream": { "path": path_str(&engine.cargo()), "canonical": path_str(&canon(&engine.cargo())), "toolchain": engine.toolchain.id, "rule": engine.selected_by.to_string() },
        "result": { "exit_code": exit.code, "signal": exit.signal },
        "instrumented": plan.instrumented,
        "events_in_parent": plan.events_in_parent,
        "events_dir": plan.events_dir.as_ref().map(|d| path_str(d)),
        "cargo_messages_file": messages_file,
        "wrappers": plan.wrappers,
        "policy": plan.policy,
        "env": env_fingerprint(&tok, cfg.str("security.persist-environment")),
        "git_head": ws_root.as_deref().and_then(crate::discover::git_head_fast),
        "scenario": spec.scenario,
        "overhead": { "preflight_ns": overhead_pre, "post_ns": post.elapsed().as_nanos() as u64 },
        "notes": notes,
    });
    let strict = cfg.bool("measurement.strict");
    let mut telemetry_error = None;
    if let Err(e) = util::write_atomic_fast(&store.session_path(&id), &serde_json::to_vec(&record).unwrap_or_default()) {
        telemetry_error = Some(format!("could not record session: {}", e));
    }
    outcome.session_id = Some(id.clone());

    let report_path = cfg.str("measurement.report").to_string();
    let enforce = cfg.bool("ci.enforce-on-build") && inv.family.builds() && exit.success();
    let finalize_now = telemetry_error.is_none() && !plan.events_in_parent && (spec.force_finalize || cfg.str("measurement.finalize") == "sync" || !report_path.is_empty() || enforce);
    if finalize_now {
        // Analysis problems must never change a Cargo result unless strict reporting asks for it (API-004).
        let fin = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::analyze::finalize(&store, &id, &cfg)));
        match fin {
            Ok(Ok(mut report)) => {
                if enforce {
                    let (code, budget) = crate::compare::enforce_on_build(&store, &cfg, &report);
                    report["run"]["result"]["policy"] = budget["result"].clone();
                    report["budget"] = budget;
                    outcome.policy_exit = code;
                }
                if !report_path.is_empty() {
                    let target = cwd.join(report_path.replace("{session}", &id));
                    let exported = crate::analyze::export(&report, ws_root.as_deref(), &cfg);
                    if let Err(e) = util::write_json(&target, &exported) {
                        telemetry_error = Some(format!("could not write report {}: {}", target.display(), e));
                    }
                }
                outcome.report = Some(report);
            }
            Ok(Err(e)) => telemetry_error = Some(format!("analysis failed: {}", e)),
            Err(_) => telemetry_error = Some("analysis failed unexpectedly (internal error)".into()),
        }
    }
    if let Some(e) = &telemetry_error {
        eprintln!("haul: warning: {}", e);
        if strict && exit.success() {
            outcome.policy_exit = Some(crate::EXIT_HAUL_ERROR);
        }
    }
    // Amortised retention: roughly one invocation in 32 pays for pruning.
    if id.as_bytes()[25] % 32 == 0 {
        store.prune(cfg.int("measurement.keep-runs") as usize, cfg.int("measurement.keep-raw-events") as usize);
    }
    // Haul's cache keeps within its space: a collection in the background when one is due.
    if let Err(e) = crate::space::after_command(&cfg, &resolve::self_identity().0) {
        if render {
            crate::ui::note(&format!("Haul could not check the space its cache uses: {}", e));
        }
    }
    outcome.store = Some(store);
    Ok(outcome)
}

/// Entry point for the `cargo` shim and for `haul <cargo-command>`.
pub fn run_and_exit(args: Vec<OsString>, mode: Mode) -> ! {
    match execute(&Spec::new(args, mode)) {
        Ok(out) => {
            if let (Some(code), true) = (out.policy_exit, out.exit.success()) {
                std::process::exit(code);
            }
            proc::propagate(out.exit)
        }
        Err(e) => fail(&e),
    }
}
