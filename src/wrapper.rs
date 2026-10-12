//! The Haul rustc wrapper (`RUSTC_WRAPPER`).
//!
//! Cargo runs `haul-rustc-wrapper <rustc> <args…>`. The wrapper is the
//! outermost link of the chain (FR-100.5): it runs the user's own wrapper if
//! one was configured, otherwise rustc, with untouched arguments, environment
//! and stdio, and afterwards drops one small event file. Each invocation writes
//! its own file, so compiler processes never contend on a shared lock (NFR-003).
//!
//! Observation is strictly fail-open: no telemetry problem may change the
//! compiler's result (NFR-020).

use crate::artifact_cache;
use crate::rustc_info;
use crate::proc::{self, ExitInfo};
use crate::util;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// Files larger than this are fingerprinted by size and mtime only.
const HASH_LIMIT: u64 = 8 * 1024 * 1024;

/// What a session tells the wrapper about its build, written once into the session's event
/// directory. A file rather than more environment variables: programs run by `cargo run` and
/// `cargo test` inherit Cargo's environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Haul's artifact cache, when it is on for this build.
    pub artifact_cache: Option<PathBuf>,
    /// Threads for rustc's parallel frontend in Cargo's units, when it is on.
    pub frontend_threads: Option<u32>,
    /// Linked workspace outputs that compiled faster than this are not stored in the cache.
    pub min_link_store_ns: u64,
    /// Directory whose `ld` is the fast linker, when one is used for this build.
    pub linker_dir: Option<PathBuf>,
}

const SETTINGS_FILE: &str = "_wrapper.settings";

impl Settings {
    pub fn write(&self, events_dir: &Path) -> std::io::Result<()> {
        util::write_atomic_fast(&events_dir.join(SETTINGS_FILE), &serde_json::to_vec(self).map_err(std::io::Error::other)?)
    }

    /// `Ok(None)` outside a Haul session. A settings file that exists but cannot be read is an
    /// error: the session that wrote it and this wrapper disagree, which must not go unnoticed.
    pub fn read(events_dir: &Path) -> Result<Option<Settings>, String> {
        let path = events_dir.join(SETTINGS_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| format!("{} is not valid wrapper settings ({}); the haul and haul-rustc-wrapper binaries may be from different versions", path.display(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {}", path.display(), e)),
        }
    }
}

pub fn main() {
    let mut argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    if argv.is_empty() {
        eprintln!("haul-rustc-wrapper: expected `<rustc> [args…]`; this binary is run by Cargo through RUSTC_WRAPPER");
        std::process::exit(crate::EXIT_HAUL_ERROR);
    }
    let rustc = argv.remove(0);
    let inner = std::env::var_os(crate::ENV_INNER_WRAPPER).filter(|v| !v.is_empty());
    let event_dir = std::env::var_os(crate::ENV_EVENT_DIR).filter(|v| !v.is_empty()).map(PathBuf::from);
    let fail_open = std::env::var(crate::ENV_WRAPPER_FAIL).map(|v| v == "open").unwrap_or(false);

    let settings = match event_dir.as_deref().map(Settings::read).transpose() {
        Ok(s) => s.flatten(),
        Err(e) => {
            eprintln!("haul: error: {}", e);
            std::process::exit(crate::EXIT_HAUL_ERROR);
        }
    };
    let cache_root = settings.as_ref().and_then(|s| s.artifact_cache.clone());
    // The compiler's account of itself, for the cache key and the levers' channel check; only
    // asked in a Haul session without a user wrapper, where either applies.
    let compiler: Result<String, String> = match (&settings, &inner, rustc.to_str()) {
        (Some(_), None, Some(r)) => rustc_info::version_verbose(&util::haul_home().join("rustc"), r),
        (Some(_), None, None) => Err("the compiler's path is not UTF-8".into()),
        _ => Err("off".into()),
    };
    // The levers below need `-Z` flags. On a stable or beta compiler they unlock them with
    // RUSTC_BOOTSTRAP, which would also accept `#![feature]`; `-Zallow-features=` keeps stable's
    // rejection of it. No lever applies when the user decides about RUSTC_BOOTSTRAP themselves
    // (also through Cargo's `[env]`, which the session cannot see), nor for a compiler that
    // cannot describe itself: rustc then reports its own trouble.
    let unlock: Option<&[&str]> = match compiler.as_deref().ok().and_then(rustc_info::is_unstable_channel) {
        _ if std::env::var_os("RUSTC_BOOTSTRAP").is_some() => None,
        Some(true) => Some(&[]),
        Some(false) => Some(&["-Zallow-features="]),
        None => None,
    };

    let mut bootstrap = false;
    let lossy: Vec<String> = argv.iter().map(|a| util::lossy(a)).collect();
    // Fast mode: link with wild instead of rustc's bundled LLD (measured: relinking ripgrep
    // 22% faster, a small binary 34%). Only for host builds, and only when no linker was chosen
    // by the user (a `-C linker`, link arguments selecting one, or linker features).
    if let (Some(dir), None, Some(_)) = (settings.as_ref().and_then(|s| s.linker_dir.clone()), &inner, unlock) {
        if is_cargo_unit(&lossy) && links_output(&lossy) && arg_value(&lossy, "--target").is_none() && !chooses_linker(&lossy) {
            for a in ["-Zunstable-options", "-Clink-self-contained=-linker", "-Clinker-features=-lld"] {
                argv.push(a.into());
            }
            argv.push(format!("-Clink-arg=-B{}", util::path_str(&dir)).into());
            bootstrap = true;
        }
    }

    let start_wall = util::now_wall_ns();
    let started = Instant::now();

    // The global artifact cache (fast mode): only without a user wrapper, which might be a cache itself.
    let args_utf8: Option<Vec<String>> = argv.iter().map(|a| a.to_str().map(str::to_string)).collect();
    let job = match (&inner, &event_dir, &args_utf8, &compiler) {
        (None, Some(_), Some(args), Ok(c)) => artifact_cache::prepare(cache_root.as_deref(), c, args),
        (None, Some(_), Some(_), Err(e)) if cache_root.is_some() => Err(e.clone()),
        _ => Err("off".into()),
    };
    if let Ok(j) = &job {
        if let artifact_cache::Lookup::Hit { stderr, entry } = j.lookup() {
            use std::io::Write;
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(&stderr).and_then(|_| err.flush());
            drop(err);
            let exit = ExitInfo { code: Some(0), signal: None };
            if let Some(dir) = &event_dir {
                let _ = std::panic::catch_unwind(|| {
                    let mut ev = build_event(&rustc, &argv, None, false, start_wall, started.elapsed().as_nanos() as u64, exit, None);
                    ev["cache"] = json!({ "result": "hit", "entry": entry });
                    if let Ok(bytes) = serde_json::to_vec(&ev) {
                        let _ = util::write_atomic_fast(&dir.join(format!("{:020}-{}.json", start_wall, std::process::id())), &bytes);
                    }
                });
            }
            proc::propagate(exit)
        }
    }
    // Fast mode: rustc's parallel frontend for the units Cargo schedules that are compiled from
    // scratch: dependencies, release and CI builds, and the first compilation of a workspace
    // unit in a target directory, which has no incremental session yet (measured: Haul's own
    // lib 2.7 s → 1.5 s). An incremental rebuild is not: the parallel frontend makes those 2-3x
    // slower on small crates, and turning it on for a session made without it discards the
    // session. Turning it off after the first compilation is free: the session it leaves serves
    // the next edit as well as one made without it. A unit the cache saw compile quickly does
    // not get it either: on a small crate its start-up costs more than it saves (Haul's own
    // binaries: 50-66 ms → 79-122 ms). Build scripts also run rustc through this wrapper to
    // probe the compiler; those must see it exactly as it is. Decided after the cache lookup,
    // which is keyed without it.
    if let (Some(n), None, Some(_)) = (settings.as_ref().and_then(|s| s.frontend_threads), &inner, unlock) {
        if is_cargo_unit(&lossy) && !lossy.iter().any(|a| a.contains("threads=")) {
            let incremental = codegen_value(&lossy, "incremental").is_some();
            let quick = job.as_ref().map(|j| j.known_quick()).unwrap_or(false);
            if (!incremental || !compiled_here_before(&lossy)) && !quick {
                argv.push(format!("-Zthreads={}", n).into());
            }
            // RUSTC_BOOTSTRAP is part of an incremental session's identity: a rebuild without it
            // would discard the session the first, parallel compilation left.
            bootstrap = true;
        }
    }
    if bootstrap {
        argv.extend(unlock.unwrap_or_default().iter().map(OsString::from));
    }

    let mut captured: Option<Vec<u8>> = None;
    let direct = |rustc: &OsString, argv: &[OsString]| {
        let mut c = Command::new(rustc);
        c.args(argv);
        if bootstrap {
            // Unlocks `-Z` on a stable compiler for this process only.
            c.env("RUSTC_BOOTSTRAP", "1");
        }
        c
    };
    let mut fallback = false;
    let result = match &inner {
        Some(w) => {
            let mut c = Command::new(w);
            c.arg(&rustc).args(&argv);
            match proc::run(&mut c, true) {
                Ok(r) => Ok(r),
                // The cache layer could not even be started: compile locally (FR-100.6).
                Err(e) if fail_open => {
                    fallback = true;
                    eprintln!("haul: warning: could not run rustc wrapper {:?} ({}); compiling without it", w, e);
                    proc::run(&mut direct(&rustc, &argv), true)
                }
                Err(e) => Err(e),
            }
        }
        // A cacheable miss: rustc's stderr streams through unchanged and is kept for replay.
        None if job.is_ok() => run_teed(&mut direct(&rustc, &argv)).map(|(e, u, buf)| {
            captured = Some(buf);
            (e, u)
        }),
        None => proc::run(&mut direct(&rustc, &argv), true),
    };
    let (exit, usage) = match result {
        Ok(r) => r,
        Err(e) => {
            eprintln!("haul: error: could not execute {:?}: {}", inner.as_ref().unwrap_or(&rustc), e);
            std::process::exit(101);
        }
    };
    let dur_ns = started.elapsed().as_nanos() as u64;

    // Storing is best effort and never changes the compiler's outcome.
    let cache = match (&job, &captured) {
        // A freshly linked workspace binary that builds in a moment is not worth a copy on every
        // edit (measured: ~30 ms for a small binary); libraries are cheap to store and form the
        // critical path a clean rebuild waits on.
        (Ok(j), Some(_)) if exit.success() && j.is_local() && j.links() && settings.as_ref().map(|s| dur_ns < s.min_link_store_ns).unwrap_or(false) => {
            let _ = j.remember_quick();
            json!({ "result": "miss", "not_stored": "quick to link" })
        }
        (Ok(j), Some(buf)) if exit.success() => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| j.store(buf, start_wall))) {
            Ok(Ok(())) => json!({ "result": "stored" }),
            Ok(Err(e)) => json!({ "result": "miss", "not_stored": e }),
            Err(_) => json!({ "result": "miss", "not_stored": "internal error" }),
        },
        (Ok(_), _) => json!({ "result": "miss", "not_stored": "the compilation failed" }),
        (Err(why), _) => json!({ "result": "off", "reason": why }),
    };
    if let Some(dir) = event_dir {
        // Telemetry must never alter the compiler's outcome.
        let _ = std::panic::catch_unwind(|| {
            let mut ev = build_event(&rustc, &argv, inner.as_ref(), fallback, start_wall, dur_ns, exit, usage);
            ev["cache"] = cache;
            if let Ok(bytes) = serde_json::to_vec(&ev) {
                let name = format!("{:020}-{}.json", start_wall, std::process::id());
                let _ = util::write_atomic_fast(&dir.join(name), &bytes);
            }
        });
    }
    proc::propagate(exit)
}

/// Runs `cmd` with its stderr copied through to ours as it arrives, returning a copy.
fn run_teed(cmd: &mut Command) -> std::io::Result<(ExitInfo, Option<proc::Usage>, Vec<u8>)> {
    use std::io::{Read, Write};
    cmd.stderr(std::process::Stdio::piped());
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let r = proc::run_with(cmd, true, &mut |child| {
        if let Some(mut from) = child.stderr.take() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let (mut buf, mut chunk) = (Vec::new(), [0u8; 16 * 1024]);
                let mut out = std::io::stderr();
                loop {
                    match from.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            // Forward first: Cargo starts dependents on rustc's `artifact` notices.
                            let _ = out.write_all(&chunk[..n]).and_then(|_| out.flush());
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = tx.send(buf);
            });
        }
    });
    let (exit, usage) = r?;
    // A grandchild holding the pipe open must not hang the build.
    let buf = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or_default();
    Ok((exit, usage, buf))
}

/// The unit produces something a linker builds: a binary, test, proc-macro or dynamic library.
fn links_output(args: &[String]) -> bool {
    args.iter().any(|a| a == "--test")
        || args.iter().enumerate().any(|(i, a)| {
            let v = if a == "--crate-type" { args.get(i + 1).map(String::as_str) } else { a.strip_prefix("--crate-type=") };
            v.map(|v| v.split(',').any(|t| matches!(t, "bin" | "proc-macro" | "cdylib" | "dylib"))).unwrap_or(false)
        })
}

/// Whether the user (through Cargo configuration or RUSTFLAGS) already decided how to link.
fn chooses_linker(args: &[String]) -> bool {
    codegen_value(args, "linker").is_some()
        || codegen_value(args, "link-self-contained").is_some()
        || codegen_value(args, "linker-features").is_some()
        || codegen_value(args, "linker-flavor").is_some()
        || args.iter().any(|a| a.contains("-fuse-ld=") || a.contains("--ld-path="))
}

/// A compilation Cargo scheduled, as opposed to a compiler probe run by a build script:
/// Cargo passes `-C metadata` to every unit it builds.
fn is_cargo_unit(args: &[String]) -> bool {
    let named = arg_value(args, "--crate-name").map(|n| n != "___").unwrap_or(false);
    named && codegen_value(args, "metadata").is_some() && !args.iter().any(|a| a.starts_with("--print"))
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().map(String::as_str);
        }
        if let Some(v) = a.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            return Some(v);
        }
    }
    None
}

/// Whether this unit was compiled in its output directory before: its dep-info is there.
/// (Cargo passes rustc the target's shared incremental directory; the unit's own session
/// directory inside it is named by a hash only rustc computes.)
fn compiled_here_before(args: &[String]) -> bool {
    let (Some(name), Some(out)) = (arg_value(args, "--crate-name"), arg_value(args, "--out-dir")) else { return false };
    Path::new(out).join(format!("{}{}.d", name, codegen_value(args, "extra-filename").unwrap_or(""))).is_file()
}

fn codegen_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    let mut it = args.iter();
    let want = format!("{}=", key);
    while let Some(a) = it.next() {
        let opt = if a == "-C" { it.next().map(String::as_str) } else { a.strip_prefix("-C").filter(|r| !r.is_empty()) };
        if let Some(v) = opt.and_then(|o| o.strip_prefix(want.as_str())) {
            return Some(v);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn build_event(rustc: &OsString, argv: &[OsString], inner: Option<&OsString>, fallback: bool, start_wall: u64, dur_ns: u64, exit: ExitInfo, usage: Option<proc::Usage>) -> Value {
    let args = expand_argfiles(argv.iter().map(|a| util::lossy(a)).collect());
    let non_utf8 = argv.iter().any(|a| a.to_str().is_none());
    let cwd = std::env::current_dir().unwrap_or_default();
    let env = |k: &str| std::env::var(k).ok();
    let crate_name = arg_value(&args, "--crate-name").map(str::to_string);
    let is_compile = crate_name.as_deref().map(|n| n != "___").unwrap_or(false) && !args.iter().any(|a| a.starts_with("--print"));

    let mut ev = json!({
        "v": 1,
        "session": env(crate::ENV_SESSION),
        "kind": if is_compile { "compile" } else { "probe" },
        "pid": std::process::id(),
        "start_wall_ns": start_wall,
        "dur_ns": dur_ns,
        "exit_code": exit.code,
        "signal": exit.signal,
        "rustc": util::lossy(rustc),
        "inner_wrapper": inner.map(|w| util::lossy(w)),
        "cache_fallback": fallback,
    });
    if let Some(u) = usage {
        ev["rusage"] = json!({ "user_ns": u.user_ns, "sys_ns": u.sys_ns, "max_rss_bytes": u.max_rss_bytes, "scope": if inner.is_some() && !fallback { "wrapper-client" } else { "rustc" } });
    }
    if let Ok(md) = std::fs::metadata(rustc) {
        ev["rustc_id"] = json!({ "bytes": md.len(), "mtime_ns": util::mtime_ns(&md) });
    }
    if !is_compile {
        return ev;
    }
    ev["args"] = json!(util::redact_args(&args));
    ev["args_non_utf8"] = json!(non_utf8);
    ev["cwd"] = json!(util::path_str(&cwd));
    ev["env"] = json!({
        "pkg_name": env("CARGO_PKG_NAME"),
        "pkg_version": env("CARGO_PKG_VERSION"),
        "manifest_dir": env("CARGO_MANIFEST_DIR"),
        "primary": env("CARGO_PRIMARY_PACKAGE").is_some(),
        "out_dir": env("OUT_DIR"),
        "toolchain": env("RUSTUP_TOOLCHAIN"),
        "jobserver": env("CARGO_MAKEFLAGS").is_some(),
    });

    let out_dir = arg_value(&args, "--out-dir").map(|d| cwd.join(d));
    let extra = codegen_value(&args, "extra-filename").unwrap_or("");
    if let (Some(out_dir), Some(name)) = (&out_dir, &crate_name) {
        let stem = format!("{}{}", name, extra);
        let mut outputs = Vec::new();
        let candidates = [
            format!("lib{}.rmeta", stem),
            format!("lib{}.rlib", stem),
            format!("lib{}.dylib", stem),
            format!("lib{}.so", stem),
            format!("lib{}.a", stem),
            format!("{}.dll", stem),
            format!("{}.exe", stem),
            format!("{}.wasm", stem),
            stem.clone(),
            format!("{}.d", stem),
        ];
        for file in candidates {
            if let Ok(md) = std::fs::metadata(out_dir.join(&file)) {
                if !md.is_file() {
                    continue;
                }
                let mtime = util::mtime_ns(&md);
                // Skip leftovers from an earlier build of the same unit.
                if mtime + 2_000_000_000 < start_wall {
                    continue;
                }
                if file.ends_with(".rmeta") && mtime >= start_wall {
                    // When metadata became available: what unblocks pipelined dependents.
                    ev["rmeta_ready_ns"] = json!((mtime - start_wall).min(dur_ns));
                }
                outputs.push(json!({ "file": file, "bytes": md.len(), "mtime_ns": mtime }));
            }
        }
        ev["outputs"] = json!(outputs);
        let local = env("CARGO_MANIFEST_DIR").map(|d| !is_external_source(Path::new(&d))).unwrap_or(false);
        if local && exit.success() {
            if let Some(inputs) = read_inputs(&out_dir.join(format!("{}.d", stem)), &cwd, env("OUT_DIR").as_deref(), out_dir) {
                ev["inputs"] = inputs;
            }
        }
    }
    ev
}

/// Cargo passes long command lines as `@argfile` (one argument per line). The file
/// is gone by analysis time, so it is expanded while it still exists.
fn expand_argfiles(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        match a.strip_prefix('@').and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(text) => out.extend(text.lines().map(str::to_string)),
            None => out.push(a),
        }
    }
    out
}

/// Registry, git-checkout and toolchain sources are immutable for our purposes.
pub fn is_external_source(p: &Path) -> bool {
    let s = util::path_str(p);
    let under = |root: Option<PathBuf>, sub: &str| root.map(|r| s.starts_with(&util::path_str(&r.join(sub)))).unwrap_or(false);
    under(util::cargo_home(), "registry") || under(util::cargo_home(), "git") || s.contains("/.rustup/toolchains/") || s.contains("/lib/rustlib/")
}

/// Splits one Makefile-style dep-info rule body into paths (`\ ` escapes a space).
pub fn split_depinfo_paths(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                cur.push(' ');
                chars.next();
            }
            ' ' | '\t' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Reads rustc's dep-info for a local crate and fingerprints what it actually read.
fn read_inputs(dep_info: &Path, cwd: &Path, out_dir_env: Option<&str>, artifact_dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(dep_info).ok()?;
    let mut files: Vec<String> = Vec::new();
    let mut env_deps = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# env-dep:") {
            let (name, value) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (rest, None),
            };
            // Values are never stored verbatim; secret-looking names keep no digest at all.
            let digest = if util::is_secret_name(name) { None } else { value.map(|v| util::sha256_hex(v.as_bytes())[..16].to_string()) };
            env_deps.push(json!({ "name": name, "set": value.is_some(), "value_sha": digest }));
        } else if files.is_empty() && !line.starts_with('#') {
            if let Some((_, body)) = line.split_once(": ") {
                files = split_depinfo_paths(body);
            }
        }
    }
    // The target directory holds generated inputs (OUT_DIR lives inside it).
    let target_root = artifact_dir.ancestors().nth(2).map(util::path_str);
    let mut local = Vec::new();
    let mut generated = Vec::new();
    let mut external = 0u64;
    for f in files {
        let p = cwd.join(&f);
        if is_external_source(&p) {
            external += 1;
            continue;
        }
        let Ok(md) = std::fs::metadata(&p) else { continue };
        let sha = if md.len() <= HASH_LIMIT { util::sha256_file(&p).ok().map(|h| h[..16].to_string()) } else { None };
        let ps = util::path_str(&p);
        let is_generated = out_dir_env.map(|o| ps.starts_with(o)).unwrap_or(false) || target_root.as_deref().map(|t| ps.starts_with(t)).unwrap_or(false);
        let rec = json!({ "path": f, "bytes": md.len(), "mtime_ns": util::mtime_ns(&md), "sha": sha });
        if is_generated {
            generated.push(rec);
        } else {
            local.push(rec);
        }
    }
    Some(json!({ "local": local, "generated": generated, "external_count": external, "env_deps": env_deps }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depinfo_paths_with_spaces() {
        assert_eq!(split_depinfo_paths("src/lib.rs src/my\\ file.rs  build.rs"), vec!["src/lib.rs", "src/my file.rs", "build.rs"]);
    }

    #[test]
    fn settings_round_trip_and_reject_what_this_version_does_not_know() {
        let dir = std::env::temp_dir().join(format!("haul-settings-{}", util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Settings::read(&dir).unwrap(), None, "no settings file: not in a Haul session");
        let s = Settings { artifact_cache: Some(dir.join("c")), frontend_threads: Some(8), min_link_store_ns: 300_000_000, linker_dir: Some(dir.join("l")) };
        s.write(&dir).unwrap();
        assert_eq!(Settings::read(&dir).unwrap(), Some(s));
        std::fs::write(dir.join(SETTINGS_FILE), r#"{"artifact_cache":null,"frontend_threads":null,"min_link_store_ns":1,"linker_dir":null,"from_the_future":true}"#).unwrap();
        assert!(Settings::read(&dir).unwrap_err().contains("different versions"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_fast_linker_is_for_linked_outputs_without_a_linker_of_the_users() {
        let a = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
        assert!(links_output(&a(&["--crate-type", "bin"])) && links_output(&a(&["--crate-type=proc-macro"])) && links_output(&a(&["--test"])));
        assert!(!links_output(&a(&["--crate-type", "lib"])) && !links_output(&a(&["--crate-type", "rlib"])));
        assert!(chooses_linker(&a(&["-C", "linker=clang"])) && chooses_linker(&a(&["-C", "link-arg=-fuse-ld=mold"])) && chooses_linker(&a(&["-Clinker-features=-lld"])));
        assert!(!chooses_linker(&a(&["-C", "opt-level=0", "-C", "link-arg=-Wl,--as-needed"])));
    }

    #[test]
    fn only_cargo_units_get_the_parallel_frontend() {
        let unit: Vec<String> = ["--crate-name", "foo", "-C", "metadata=abc", "src/lib.rs"].iter().map(|s| s.to_string()).collect();
        let probe: Vec<String> = ["--crate-name", "probe", "probe.rs"].iter().map(|s| s.to_string()).collect();
        let info: Vec<String> = ["--crate-name", "___", "-C", "metadata=x", "--print=cfg"].iter().map(|s| s.to_string()).collect();
        assert!(is_cargo_unit(&unit));
        assert!(!is_cargo_unit(&probe) && !is_cargo_unit(&info));
    }

    #[test]
    fn finds_flag_values() {
        let a: Vec<String> = ["--crate-name", "foo", "-C", "extra-filename=-abc", "-Cmetadata=1", "--out-dir=/x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(arg_value(&a, "--crate-name"), Some("foo"));
        assert_eq!(arg_value(&a, "--out-dir"), Some("/x"));
        assert_eq!(codegen_value(&a, "extra-filename"), Some("-abc"));
        assert_eq!(codegen_value(&a, "metadata"), Some("1"));
        assert_eq!(codegen_value(&a, "lto"), None);
    }
}
