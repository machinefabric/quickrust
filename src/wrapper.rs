//! The QuickRust rustc wrapper (`RUSTC_WRAPPER`).
//!
//! Cargo runs `qrust-rustc-wrapper <rustc> <args…>`. The wrapper is the
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
    /// QuickRust's artifact cache, when it is on for this build.
    pub artifact_cache: Option<PathBuf>,
    /// Threads for rustc's parallel frontend in Cargo's units, when it is on.
    pub frontend_threads: Option<u32>,
    /// Linked workspace outputs that compiled faster than this are not stored in the cache.
    pub min_link_store_ns: u64,
    /// Directory whose `ld` is the fast linker, when one is used for this build.
    pub linker_dir: Option<PathBuf>,
    /// Compile Cargo's units with QuickRust's compiler driver, when it is built for the compiler, and
    /// key dependents by the interfaces of the crates they compile against (early cut-off).
    pub early_cutoff: bool,
    /// The build's target directory, which holds generated inputs (build scripts' `OUT_DIR`).
    pub target_dir: Option<PathBuf>,
}

const SETTINGS_FILE: &str = "_wrapper.settings";

impl Settings {
    pub fn write(&self, events_dir: &Path) -> std::io::Result<()> {
        util::write_atomic_fast(&events_dir.join(SETTINGS_FILE), &serde_json::to_vec(self).map_err(std::io::Error::other)?)
    }

    /// `Ok(None)` outside a QuickRust session. A settings file that exists but cannot be read is an
    /// error: the session that wrote it and this wrapper disagree, which must not go unnoticed.
    pub fn read(events_dir: &Path) -> Result<Option<Settings>, String> {
        let path = events_dir.join(SETTINGS_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| format!("{} is not valid wrapper settings ({}); the qrust and qrust-rustc-wrapper binaries may be from different versions", path.display(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {}", path.display(), e)),
        }
    }
}

pub fn main() {
    let mut argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    if argv.is_empty() {
        crate::ui::say(crate::ui::Tag::Fail, "qrust-rustc-wrapper expects `<rustc> [args...]`; Cargo runs it through RUSTC_WRAPPER");
        std::process::exit(crate::EXIT_QRUST_ERROR);
    }
    let rustc = argv.remove(0);
    let inner = std::env::var_os(crate::ENV_INNER_WRAPPER).filter(|v| !v.is_empty());
    let event_dir = std::env::var_os(crate::ENV_EVENT_DIR).filter(|v| !v.is_empty()).map(PathBuf::from);
    let fail_open = std::env::var(crate::ENV_WRAPPER_FAIL).map(|v| v == "open").unwrap_or(false);

    let settings = match event_dir.as_deref().map(Settings::read).transpose() {
        Ok(s) => s.flatten(),
        Err(e) => {
            crate::ui::say(crate::ui::Tag::Fail, &e);
            std::process::exit(crate::EXIT_QRUST_ERROR);
        }
    };
    let cache_root = settings.as_ref().and_then(|s| s.artifact_cache.clone());
    // The compiler's account of itself, for the cache key and the levers' channel check; only
    // asked in a QuickRust session without a user wrapper, where either applies.
    let compiler: Result<String, String> = match (&settings, &inner, rustc.to_str()) {
        (Some(_), None, Some(r)) => rustc_info::version_verbose(&util::memo_dir("rustc"), r),
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

    // Early cut-off (fast mode): QuickRust's compiler driver compiles the units Cargo schedules,
    // recording each library's interface hash; never compiler probes, which must see rustc as
    // it is. Without a driver built for this compiler, rustc compiles them as usual.
    // A unit the driver failed on before is compiled by rustc until the driver changes. Only
    // when the program Cargo runs is rustc itself: a workspace wrapper (`cargo clippy`'s
    // clippy-driver, which describes itself exactly as rustc does) comes first, with rustc as
    // its first argument, and must run as it is.
    let is_rustc = Path::new(&rustc).file_stem().is_some_and(|s| s == "rustc");
    let mut driver: Option<PathBuf> = match (&settings, &inner, &compiler) {
        (Some(s), None, Ok(_)) if s.early_cutoff && is_rustc && is_cargo_unit(&lossy) => rustc.to_str().and_then(|r| crate::driver::Compiler::of(r).ok()).and_then(|c| crate::driver::find(&c)).filter(|d| !crate::driver::failed_before(d, &unit_identity(&lossy))),
        _ => None,
    };

    let start_wall = util::now_wall_ns();
    let started = Instant::now();

    // The global artifact cache (fast mode): only without a user wrapper, which might be a cache itself.
    let args_utf8: Option<Vec<String>> = argv.iter().map(|a| a.to_str().map(str::to_string)).collect();
    let job = match (&inner, &event_dir, &args_utf8, &compiler) {
        (None, Some(_), Some(args), Ok(c)) => artifact_cache::prepare(cache_root.as_deref(), c, driver.as_deref(), args),
        (None, Some(_), Some(_), Err(e)) if cache_root.is_some() => Err(e.clone()),
        _ => Err("off".into()),
    };
    if let Ok(j) = &job {
        if let artifact_cache::Lookup::Hit { stderr, entry, foreign_out_dir } = j.lookup() {
            use std::io::Write;
            if let (Some(root), Some(args)) = (&cache_root, &args_utf8) {
                let _ = artifact_cache::record_link_inputs(root, args);
            }
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(&stderr).and_then(|_| err.flush());
            drop(err);
            let exit = ExitInfo { code: Some(0), signal: None };
            if let Some(dir) = &event_dir {
                let _ = std::panic::catch_unwind(|| {
                    let mut ev = build_event(&rustc, &argv, None, false, start_wall, started.elapsed().as_nanos() as u64, exit, None, settings.as_ref().and_then(|s| s.target_dir.as_deref()));
                    ev["cache"] = json!({ "result": "hit", "entry": entry, "foreign_out_dir": foreign_out_dir });
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
    // unit in a target directory, which has no incremental session yet (measured: QuickRust's own
    // lib 2.7 s → 1.5 s). An incremental rebuild is not: the parallel frontend makes those 2-3x
    // slower on small crates, and turning it on for a session made without it discards the
    // session. Turning it off after the first compilation is free: the session it leaves serves
    // the next edit as well as one made without it. A unit the cache saw compile quickly does
    // not get it either: on a small crate its start-up costs more than it saves (QuickRust's own
    // binaries: 50-66 ms → 79-122 ms). Build scripts also run rustc through this wrapper to
    // probe the compiler; those must see it exactly as it is. Decided after the cache lookup,
    // which is keyed without it.
    // Compiled from scratch: nothing incremental to build on. Only such a compilation tells how
    // long the unit takes: an incremental rebuild of a large one can be quick.
    let from_scratch = codegen_value(&lossy, "incremental").is_none() || !compiled_here_before(&lossy);
    if let (Some(n), None, Some(_)) = (settings.as_ref().and_then(|s| s.frontend_threads), &inner, unlock) {
        if is_cargo_unit(&lossy) && !lossy.iter().any(|a| a.contains("threads=")) {
            let quick = job.as_ref().map(|j| j.known_quick()).unwrap_or(false);
            if from_scratch && !quick {
                argv.push(format!("-Zthreads={}", n).into());
            }
            // RUSTC_BOOTSTRAP is part of an incremental session's identity: a rebuild without it
            // would discard the session the first, parallel compilation left.
            bootstrap = true;
        }
    }
    // Relinking (fast mode, with QuickRust's driver): a binary or test executable's objects are
    // kept when it is compiled. When a later
    // build finds it compiled from the same sources against the same dependency interfaces (an
    // edit below it was cut off, and only what it links changed), it is only linked again: for
    // a large binary a fraction of compiling it (ripgrep's: ~45 ms of ~350 ms). Not for a unit
    // the cache saw compile quickly. The driver keeps the objects when it compiles the unit (it
    // writes the `.rlink` naming them, and they are hard-linked aside before linking deletes
    // them); relinking is rustc's `-Zlink-only`.
    let input = input_index(&lossy);
    let relinkable = matches!((&driver, &job, unlock, input), (Some(_), Ok(j), Some(_), Some(_)) if j.relinkable() && !j.known_quick());
    if relinkable {
        bootstrap = true;
    }
    if bootstrap {
        argv.extend(unlock.unwrap_or_default().iter().map(OsString::from));
    }
    // The command that links the unit from the `.rlink` its last compilation left.
    let link_only: Option<Vec<OsString>> = match (relinkable, &job, input) {
        (true, Ok(j), Some(i)) => {
            let mut a = argv.clone();
            a[i] = j.rlink_path().into_os_string();
            a.push("-Zlink-only".into());
            Some(a)
        }
        _ => None,
    };
    let relink: Option<PathBuf> = match (&link_only, &job) {
        (Some(_), Ok(j)) => j.prepare_relink(),
        _ => None,
    };
    let keep_objects: Option<PathBuf> = match (&link_only, &job) {
        (Some(_), Ok(j)) => Some(j.kept_objects()),
        _ => None,
    };
    let mut relinked: Option<bool> = None;
    let mut early = false;

    let mut captured: Option<Vec<u8>> = None;
    let direct = |rustc: &OsString, argv: &[OsString], driver: &Option<PathBuf>| {
        let mut c = match driver {
            Some(d) => {
                let mut c = Command::new(d);
                c.env(crate::driver::ENV_INTERFACE_HASH, "1");
                match &keep_objects {
                    Some(dir) => c.env(crate::driver::ENV_KEEP_OBJECTS, dir),
                    None => c.env_remove(crate::driver::ENV_KEEP_OBJECTS),
                };
                c
            }
            None => Command::new(rustc),
        };
        c.args(argv);
        if bootstrap {
            // Unlocks `-Z` on a stable compiler for this process only.
            c.env("RUSTC_BOOTSTRAP", "1");
        }
        c
    };
    let mut fallback = false;
    let mut held: Option<(Vec<u8>, bool)> = None;
    let result = match &inner {
        Some(w) => {
            let mut c = Command::new(w);
            c.arg(&rustc).args(&argv);
            match proc::run(&mut c, true) {
                Ok(r) => Ok(r),
                // The cache layer could not even be started: compile locally (FR-100.6).
                Err(e) if fail_open => {
                    fallback = true;
                    crate::ui::say(crate::ui::Tag::Warn, &format!("could not run rustc wrapper {:?} ({}); compiling without it", w, e));
                    proc::run(&mut direct(&rustc, &argv, &None), true)
                }
                Err(e) => Err(e),
            }
        }
        // A cacheable miss, or a compilation by QuickRust's driver: the compiler's stderr streams
        // through and is kept for replay; under the driver, text that is not one of the JSON
        // messages Cargo reads (a crash report) waits until the driver has succeeded.
        // The driver says when its outputs are written (see `run_teed`).
        None if job.is_ok() || driver.is_some() => {
            let hold = if driver.is_some() { Hold::Text } else { Hold::Nothing };
            let compile = || {
                // Only what this compilation keeps may be relinked from later.
                if let (Some(_), Ok(j)) = (&keep_objects, &job) {
                    j.forget_kept_objects();
                }
                match run_teed(&mut direct(&rustc, &argv, &driver), hold, driver.is_some()) {
                    // A driver that cannot even be started (its toolchain gone) fails like one
                    // that crashed: rustc compiles the unit.
                    Err(e) if driver.is_some() => Ok(Teed { exit: ExitInfo { code: Some(127), signal: None }, usage: None, forwarded: Vec::new(), held: format!("cannot run QuickRust's compiler driver: {}\n", e).into_bytes(), artifact_notice: false, early: false }),
                    r => r,
                }
            };
            let ran = match (&link_only, &relink) {
                // Linked again from its last compilation's objects; a failure (they were changed
                // after all) is not shown, and the unit is compiled after all.
                (Some(link_only), Some(_)) => match run_teed(&mut direct(&rustc, link_only, &driver), Hold::All, false) {
                    Ok(t) if t.exit.success() => {
                        use std::io::Write;
                        let _ = std::io::stderr().write_all(&t.held).and_then(|_| std::io::stderr().flush());
                        relinked = Some(true);
                        Ok(Teed { forwarded: t.held, held: Vec::new(), ..t })
                    }
                    _ => compile(),
                },
                _ => compile(),
            };
            ran.map(|t| {
                relinked.get_or_insert(false);
                early = t.early;
                let teed = (t.exit, t.usage);
                captured = Some(t.forwarded);
                held = Some((t.held, t.artifact_notice));
                teed
            })
        }
        None => proc::run(&mut direct(&rustc, &argv, &None), true),
    };
    let (mut exit, mut usage) = match result {
        Ok(r) => r,
        Err(e) => {
            crate::ui::say(crate::ui::Tag::Fail, &format!("could not execute {:?}: {}", inner.as_ref().unwrap_or(&rustc), e));
            std::process::exit(101);
        }
    };
    // QuickRust's driver must never fail a build rustc would pass. When it crashes before any of its
    // metadata reached Cargo (no dependent compiles against it yet), its report is kept aside,
    // the unit is marked to be compiled by rustc from now on, and rustc compiles it now; what
    // rustc builds is not stored under the driver's key. A crash after that is rustc's own
    // (code generation is rustc's) and goes through as it is.
    let mut driver_failure: Option<PathBuf> = None;
    if let (Some(d), Some((text, artifact_notice))) = (driver.clone(), held.take()) {
        if crashed(&exit) && !artifact_notice {
            let report = crate::driver::record_failure(&d, &unit_identity(&lossy), &text);
            let name = arg_value(&lossy, "--crate-name").unwrap_or("?");
            crate::ui::say(crate::ui::Tag::Warn, &format!("QuickRust's compiler driver failed on crate `{}`; compiled with rustc instead ({})", name, report.as_ref().map(|p| format!("report: {}", p.display())).unwrap_or_else(|e| format!("its report could not be kept: {}", e))));
            driver_failure = Some(report.unwrap_or_default());
            driver = None;
            let rerun = match &job {
                Ok(_) => run_teed(&mut direct(&rustc, &argv, &None), Hold::Nothing, false).map(|t| {
                    captured = Some(t.forwarded);
                    (t.exit, t.usage)
                }),
                Err(_) => proc::run(&mut direct(&rustc, &argv, &None), true),
            };
            (exit, usage) = match rerun {
                Ok(r) => r,
                Err(e) => {
                    crate::ui::say(crate::ui::Tag::Fail, &format!("could not execute {:?}: {}", rustc, e));
                    std::process::exit(101);
                }
            };
        } else {
            use std::io::Write;
            let _ = std::io::stderr().write_all(&text);
            if let Some(c) = captured.as_mut() {
                c.extend_from_slice(&text);
            }
        }
    }
    let dur_ns = started.elapsed().as_nanos() as u64;
    // What the driver kept of a compilation QuickRust relinks, recorded for the next relink.
    let not_kept: Option<String> = match (&job, &keep_objects, relinked, &driver_failure) {
        (Ok(j), Some(_), Some(false), None) if exit.success() => j.record_kept_objects(start_wall).err(),
        _ => None,
    };

    // What the rlib was compiled against, for the units that will link it; best effort, as
    // storing: a unit whose dependency went unrecorded is compiled rather than restored.
    if let (Some(root), Some(args), None, true) = (&cache_root, &args_utf8, &inner, exit.success()) {
        if is_cargo_unit(&lossy) {
            let _ = artifact_cache::record_link_inputs(root, args);
        }
    }
    // Storing is best effort and never changes the compiler's outcome.
    let cache = match (&job, &captured) {
        // Keyed for the driver's output, which this is not.
        (Ok(_), _) if driver_failure.is_some() => json!({ "result": "miss", "not_stored": "compiled by rustc after QuickRust's compiler driver failed" }),
        // A freshly linked workspace binary that builds in a moment is not worth a copy on every
        // edit (measured: ~30 ms for a small binary); libraries are cheap to store and form the
        // critical path a clean rebuild waits on.
        (Ok(j), Some(_)) if exit.success() && j.is_local() && j.links() && settings.as_ref().map(|s| dur_ns < s.min_link_store_ns).unwrap_or(false) => {
            // Quick to link again from kept objects is not quick to compile.
            if from_scratch && relinked != Some(true) {
                let _ = j.remember_quick();
            }
            json!({ "result": "miss", "not_stored": "quick to link" })
        }
        (Ok(j), Some(buf)) if exit.success() => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Compiled from scratch and not quick: whatever an earlier compilation suggested.
            if from_scratch {
                j.forget_quick();
            }
            j.store(buf, start_wall)
        })) {
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
            let mut ev = build_event(&rustc, &argv, inner.as_ref(), fallback, start_wall, dur_ns, exit, usage, settings.as_ref().and_then(|s| s.target_dir.as_deref()));
            ev["cache"] = cache;
            if let Some(d) = &driver {
                ev["driver"] = json!(util::path_str(d));
            }
            if let Some(report) = &driver_failure {
                ev["driver_failure"] = json!(util::path_str(report));
            }
            if let (Some(_), Some(r), None) = (&link_only, relinked, &driver_failure) {
                ev["link"] = json!(match (r, not_kept.as_deref()) {
                    (true, _) => "relinked".to_string(),
                    (false, None) => "objects kept".to_string(),
                    (false, Some(why)) => format!("objects not kept: {}", why),
                });
            }
            if early {
                ev["early_exit"] = json!(true);
            }
            if let Ok(bytes) = serde_json::to_vec(&ev) {
                let name = format!("{:020}-{}.json", start_wall, std::process::id());
                let _ = util::write_atomic_fast(&dir.join(name), &bytes);
            }
        });
    }
    proc::propagate(exit)
}

/// How rustc's JSON output marks an `artifact` notice.
const ARTIFACT_NOTICE: &[u8] = b"\"$message_type\":\"artifact\"";

/// What a compiler run through `run_teed` printed.
struct Teed {
    exit: ExitInfo,
    usage: Option<proc::Usage>,
    /// Everything copied through to our stderr, in order.
    forwarded: Vec<u8>,
    /// Lines held back (see `run_teed`).
    held: Vec<u8>,
    /// Whether an `artifact` notice went out: Cargo may have started dependents on it.
    artifact_notice: bool,
    /// The compiler said its outputs were written and was left to finish on its own.
    early: bool,
}

/// What `run_teed` holds back from our stderr for the caller to decide about.
#[derive(Clone, Copy, PartialEq)]
enum Hold {
    Nothing,
    /// Lines that are not JSON messages: Cargo has the compiler print only those; a crash report
    /// is plain text.
    Text,
    /// Everything: an attempt whose failure is not to be seen.
    All,
}

/// Runs `cmd` with its stderr copied through to ours as it arrives (except what `hold` holds
/// back), returning a copy. With `ready` (QuickRust's driver, Unix), the compiler gets a pipe on which
/// it says its outputs are written; once it has, and its stderr has ended, this returns without
/// waiting for it to finish saving its incremental state (`exit` is then success, `usage`
/// unknown, and `early` set).
fn run_teed(cmd: &mut Command, hold: Hold, ready: bool) -> std::io::Result<Teed> {
    use std::io::{Read, Write};
    enum Event {
        Exited(std::io::Result<(ExitInfo, Option<proc::Usage>)>),
        Stderr(Vec<u8>, Vec<u8>, bool),
        // Only Unix compilers get the pipe it comes on.
        #[cfg_attr(not(unix), allow(dead_code))]
        Ready,
    }
    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    #[cfg(unix)]
    let pipe = if ready { Some(ready_pipe()?) } else { None };
    #[cfg(not(unix))]
    let pipe: Option<(i32, i32)> = None;
    let _ = ready;
    match pipe {
        Some((_, w)) => cmd.env(crate::driver::ENV_READY_FD, w.to_string()),
        None => cmd.env_remove(crate::driver::ENV_READY_FD),
    };
    cmd.stderr(std::process::Stdio::piped());
    let stderr_tx = tx.clone();
    let mut on_spawn = move |child: &mut std::process::Child| {
        // Only the compiler holds the pipe's writing end now.
        #[cfg(unix)]
        if let Some((_, w)) = pipe {
            unsafe { libc::close(w) };
        }
        if let Some(mut from) = child.stderr.take() {
            let tx = stderr_tx.clone();
            std::thread::spawn(move || {
                let (mut forwarded, mut held, mut line, mut chunk) = (Vec::new(), Vec::new(), Vec::new(), [0u8; 16 * 1024]);
                let mut artifact_notice = false;
                let mut out = std::io::stderr();
                let mut emit = |bytes: &[u8], forwarded: &mut Vec<u8>| {
                    // Forward at once: Cargo starts dependents on the compiler's `artifact` notices.
                    let _ = out.write_all(bytes).and_then(|_| out.flush());
                    forwarded.extend_from_slice(bytes);
                };
                loop {
                    match from.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) if hold == Hold::Nothing => emit(&chunk[..n], &mut forwarded),
                        Ok(n) if hold == Hold::All => held.extend_from_slice(&chunk[..n]),
                        Ok(n) => {
                            line.extend_from_slice(&chunk[..n]);
                            while let Some(end) = line.iter().position(|&b| b == b'\n') {
                                let rest = line.split_off(end + 1);
                                if line.first() == Some(&b'{') {
                                    artifact_notice |= line.windows(ARTIFACT_NOTICE.len()).any(|w| w == ARTIFACT_NOTICE);
                                    emit(&line, &mut forwarded);
                                } else {
                                    held.extend_from_slice(&line);
                                }
                                line = rest;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                held.extend_from_slice(&line);
                if hold == Hold::All {
                    artifact_notice = held.windows(ARTIFACT_NOTICE.len()).any(|w| w == ARTIFACT_NOTICE);
                }
                let _ = tx.send(Event::Stderr(forwarded, held, artifact_notice));
            });
        }
    };
    #[cfg(unix)]
    if let Some((r, _)) = pipe {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut byte = 0u8;
            let n = loop {
                let n = unsafe { libc::read(r, (&mut byte as *mut u8).cast(), 1) };
                if n >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    break n;
                }
            };
            unsafe { libc::close(r) };
            if n == 1 {
                let _ = tx.send(Event::Ready);
            }
        });
    }
    // The compiler is waited for on a thread of its own: this one may stop waiting for it.
    let mut cmd = std::mem::replace(cmd, Command::new(""));
    std::thread::spawn(move || {
        let r = proc::run_with(&mut cmd, true, &mut on_spawn);
        let _ = tx.send(Event::Exited(r));
    });
    let (mut exited, mut stderr, mut is_ready) = (None, None, false);
    loop {
        match (&exited, &stderr, is_ready) {
            (Some(_), Some(_), _) | (None, Some(_), true) => break,
            _ => {}
        }
        // Once the compiler ended, its stderr ends too unless a grandchild holds it open, which
        // must not hang the build.
        let next = if exited.is_some() { rx.recv_timeout(std::time::Duration::from_secs(5)).ok() } else { rx.recv().ok() };
        match next {
            Some(Event::Exited(r)) => exited = Some(r?),
            Some(Event::Stderr(f, h, a)) => stderr = Some((f, h, a)),
            Some(Event::Ready) => is_ready = true,
            None => break,
        }
    }
    let (forwarded, held, artifact_notice) = stderr.unwrap_or_default();
    Ok(match exited {
        Some((exit, usage)) => Teed { exit, usage, forwarded, held, artifact_notice, early: false },
        None => Teed { exit: ExitInfo { code: Some(0), signal: None }, usage: None, forwarded, held, artifact_notice, early: true },
    })
}

/// A pipe for the driver's ready byte: (reading end, closed on exec; writing end, inherited).
#[cfg(unix)]
fn ready_pipe() -> std::io::Result<(i32, i32)> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    unsafe { libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC) };
    Ok((fds[0], fds[1]))
}

/// The position of the source file rustc compiles among `args`: its one argument that is
/// neither an option nor an option's value.
fn input_index(args: &[String]) -> Option<usize> {
    const TAKES_VALUE: &[&str] = &[
        "--crate-name", "--crate-type", "--edition", "--emit", "--out-dir", "-o", "-L", "-l", "--extern", "-C", "--codegen", "-Z", "--cfg", "--check-cfg", "--cap-lints", "--target", "--error-format", "--json", "--diagnostic-width",
        "--print", "-W", "-A", "-D", "-F", "--warn", "--allow", "--deny", "--forbid", "--force-warn", "--remap-path-prefix", "--explain", "--sysroot", "--color", "--env-set",
    ];
    let mut found = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if TAKES_VALUE.contains(&a) {
            i += 2;
            continue;
        }
        if !a.starts_with('-') && found.replace(i).is_some() {
            return None;
        }
        i += 1;
    }
    found
}

/// Whether a compiler ended by crashing (an internal compiler error, a program that could not be
/// loaded, or a fatal signal other than one asking it to stop) rather than by reporting errors in
/// the code.
fn crashed(exit: &ExitInfo) -> bool {
    // 101: an internal compiler error; 127: the program could not be loaded (a driver whose
    // toolchain's libraries are gone).
    if exit.code == Some(101) || exit.code == Some(127) {
        return true;
    }
    #[cfg(unix)]
    if let Some(sig) = exit.signal {
        return matches!(sig, libc::SIGSEGV | libc::SIGBUS | libc::SIGILL | libc::SIGABRT | libc::SIGFPE);
    }
    false
}

/// A unit as Cargo names it: its output directory and file stem (which carries Cargo's unit hash).
fn unit_identity(args: &[String]) -> String {
    let name = arg_value(args, "--crate-name").unwrap_or("");
    let out = arg_value(args, "--out-dir").map(|d| util::path_str(&std::env::current_dir().unwrap_or_default().join(d))).unwrap_or_default();
    util::digest_parts([out, format!("{}{}", name, codegen_value(args, "extra-filename").unwrap_or(""))])
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
fn build_event(rustc: &OsString, argv: &[OsString], inner: Option<&OsString>, fallback: bool, start_wall: u64, dur_ns: u64, exit: ExitInfo, usage: Option<proc::Usage>, target_dir: Option<&Path>) -> Value {
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
            if let Some(inputs) = read_inputs(&out_dir.join(format!("{}.d", stem)), &cwd, env("OUT_DIR").as_deref(), target_dir) {
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
    let home = util::cargo_home();
    p.starts_with(home.join("registry")) || p.starts_with(home.join("git")) || p.starts_with(util::qrust_home().join("toolchains")) || util::path_str(p).contains("/lib/rustlib/")
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
fn read_inputs(dep_info: &Path, cwd: &Path, out_dir_env: Option<&str>, target_dir: Option<&Path>) -> Option<Value> {
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
    let target_root = target_dir.map(util::path_str);
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
    fn the_input_is_the_one_argument_that_is_no_option_and_no_value() {
        let args = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        let cargo = args("--crate-name app --edition=2021 src/main.rs --error-format=json --crate-type bin --emit=dep-info,link -C debuginfo=2 --cfg feature=\"x\" --check-cfg cfg(test) -C metadata=1a --out-dir target/debug/deps -L dependency=target/debug/deps --extern mid=target/debug/deps/libmid.rlib");
        assert_eq!(input_index(&cargo), Some(3));
        // Two candidates, or none (stdin): not told apart, not compiled apart from linking.
        assert_eq!(input_index(&args("--crate-name app a.rs b.rs")), None);
        assert_eq!(input_index(&args("--crate-name app -")), None);
    }

    #[test]
    fn depinfo_paths_with_spaces() {
        assert_eq!(split_depinfo_paths("src/lib.rs src/my\\ file.rs  build.rs"), vec!["src/lib.rs", "src/my file.rs", "build.rs"]);
    }

    #[test]
    fn settings_round_trip_and_reject_what_this_version_does_not_know() {
        let dir = std::env::temp_dir().join(format!("qrust-settings-{}", util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Settings::read(&dir).unwrap(), None, "no settings file: not in a QuickRust session");
        let s = Settings { artifact_cache: Some(dir.join("c")), frontend_threads: Some(8), min_link_store_ns: 300_000_000, linker_dir: Some(dir.join("l")), early_cutoff: true, target_dir: Some(dir.join("t")) };
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
