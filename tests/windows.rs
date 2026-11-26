//! Windows: what a build leaves open behind it, seen from the script that ran it. Each test runs
//! real builds in an isolated sandbox.

#![cfg(windows)]

mod common;

use common::*;
use std::fs;
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ERROR_SHARING_VIOLATION: i32 = 32;

/// Wall-clock nanoseconds now.
fn now_ns() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
}

/// When the last count of the sandbox's cache was made.
fn counted_ns(sb: &Sandbox) -> Option<u64> {
    serde_json::from_slice::<serde_json::Value>(&fs::read(sb.home.join("cache/usage.json")).ok()?).ok()?["scanned_ns"].as_u64()
}

/// The lock at `path` taken as QuickRust takes its locks on Windows (`util::ExclusiveLock`: the
/// file opened without sharing), waiting for whoever holds it until `deadline`.
fn hold(path: &Path, deadline: Instant) -> fs::File {
    loop {
        match fs::OpenOptions::new().create(true).truncate(false).write(true).share_mode(0).open(path) {
            Ok(f) => return f,
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => panic!("cannot take the lock {}: {}", path.display(), e),
        }
    }
}

/// How many QuickRust commands have recorded in `runs` that they run Cargo (`space::CargoRuns`).
fn registered(runs: &Path) -> usize {
    fs::read_dir(runs).map(|rd| rd.filter_map(|e| e.ok()).filter(|e| e.file_name().to_str().is_some_and(|n| n.parse::<u32>().is_ok())).count()).unwrap_or(0)
}

/// The processes working in `cache`: collections a command started, as QuickRust's `cargo`
/// (the shim, or for a command Cargo ran, the one in QuickRust's home). A command starts one when
/// one is due and no collection holds `gc.lock`; one started while another holds it ends at once.
fn collections(cache: &Path) -> Vec<u32> {
    let cache = fs::canonicalize(cache).unwrap();
    quickrust::proc::running().unwrap().into_iter().filter(|p| p.cwd.as_deref() == Some(&cache)).map(|p| p.pid).collect()
}

/// Whether a process holds the lock at `path` (QuickRust's locks are files opened without sharing).
fn held(path: &Path) -> bool {
    match fs::OpenOptions::new().write(true).share_mode(0).open(path) {
        Ok(_) => false,
        Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => true,
        Err(e) => panic!("cannot open the lock {}: {}", path.display(), e),
    }
}

/// The contents of `files`, for a failure's message.
fn contents(files: &[&Path]) -> String {
    files.iter().map(|f| format!("{}:\n{}", f.display(), fs::read_to_string(f).unwrap_or_default())).collect::<Vec<_>>().join("\n")
}

/// Runs `cargo <command>` on the package `p` through `cmd /c`, its streams redirected as
/// `redirect` says, with a collection of the cache due after it. The collection is held running
/// behind the build: once `commands` QuickRust commands have recorded that they run Cargo (the
/// command, and each it runs through Cargo), this test takes the lock a collection waits for before
/// it collects Cargo's downloads (`CargoRuns`), and keeps it until each of `files` has been opened
/// with no sharing (which Windows refuses while any other handle to the file is open), appended to
/// and removed. `round` makes each build compile.
fn redirected_build_leaves_nothing_open(sb: &Sandbox, p: &Path, round: u32, command: &str, commands: usize, redirect: &str, files: &[&Path]) {
    let cache = sb.home.join("cache");
    let runs = sb.home.join("cargo-runs");
    let shim = sb.shim_dir.join("cargo.exe");
    // The last count is two days old: a collection is due after the next command.
    let stale = now_ns() - 2 * 86_400 * 1_000_000_000;
    fs::write(cache.join("usage.json"), serde_json::json!({ "format": 4, "scanned_ns": stale, "total_bytes": 0, "free_at_scan": 0, "kept_by_snapshots": [], "counted": true, "unmet": false, "window_ends_ns": null }).to_string()).unwrap();
    sb.file("p/src/lib.rs", &format!("pub fn add(x: u32, y: u32) -> u32 {{ x + y + {round} - {round} }}\n"));
    assert_eq!(registered(&runs), 0, "a command still records that it runs Cargo");
    // As a script's runner starts a step: `cmd /c "cargo build >> log 2>&1"`.
    let mut cmd = sb.with_shim("cmd", p);
    cmd.raw_arg(format!("/d /s /c \"\"{}\" {} {}\"", shim.display(), command, redirect));
    let mut build = cmd.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(600);
    let lock = loop {
        if registered(&runs) >= commands {
            break hold(&runs.join(".lock"), deadline);
        }
        if let Some(status) = build.try_wait().unwrap() {
            panic!("the build ended ({}) before it ran Cargo:\n{}", status, contents(files));
        }
        assert!(Instant::now() < deadline, "the build did not run Cargo within ten minutes:\n{}", contents(files));
        std::thread::sleep(Duration::from_millis(5));
    };
    let status = build.wait().unwrap();
    assert!(status.success(), "the build failed ({}):\n{}", status, contents(files));
    // The collection is the one process left of those started, holding `gc.lock`.
    let until = Instant::now() + Duration::from_secs(60);
    let gc = loop {
        if let [pid] = collections(&cache)[..] {
            if held(&cache.join("gc.lock")) {
                break pid;
            }
        }
        if Instant::now() >= until {
            panic!("no one collection runs behind `cargo {}` (round {}): {:?} work in the cache, gc.lock {}; its log:\n{}", command, round, collections(&cache), if held(&cache.join("gc.lock")) { "held" } else { "free" }, fs::read_to_string(cache.join("gc.log")).unwrap_or_default());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    for f in files {
        let mut file = fs::OpenOptions::new().append(true).share_mode(0).open(f).unwrap_or_else(|e| panic!("{} is still open behind `cargo {}` (round {}), while its collection (process {}) runs: {}", f.display(), command, round, gc, e));
        writeln!(file, "== next step").unwrap();
        drop(file);
        fs::remove_file(f).unwrap_or_else(|e| panic!("{} cannot be removed while the collection behind `cargo {}` (round {}) runs: {}", f.display(), command, round, e));
    }
    assert!(collections(&cache).contains(&gc), "the collection ended before the files were tried; its log:\n{}", fs::read_to_string(cache.join("gc.log")).unwrap_or_default());
    // Let go, it collects and ends.
    drop(lock);
    let until = Instant::now() + Duration::from_secs(120);
    while !collections(&cache).is_empty() || counted_ns(sb) == Some(stale) {
        assert!(Instant::now() < until, "the collection did not finish; its log:\n{}", fs::read_to_string(cache.join("gc.log")).unwrap_or_default());
        std::thread::sleep(Duration::from_millis(100));
    }
}

// TEST0365: On Windows a collection a build starts behind it holds nothing the build's caller redirected its streams to
#[test]
fn test0365_on_windows_a_collection_a_build_starts_behind_it_holds_nothing_the_builds_caller_redirected_its_streams_to() {
    let sb = Sandbox::new("gc-handles");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build").env("QRUST_CACHE_GC", "off"));
    // Both streams to one log, which the script appends the next step's heading to. The build
    // starts the collection itself.
    let log = sb.dir.join("test.log");
    fs::write(&log, "== build\n").unwrap();
    let both = format!(">> \"{}\" 2>&1", log.display());
    redirected_build_leaves_nothing_open(&sb, &p, 1, "build", 1, &both, &[&log]);
    // Clippy runs its `cargo check` through QuickRust (Cargo hands its subcommands QuickRust as
    // `$CARGO`), and that inner command starts the collection: what it was handed down from the
    // outer command's caller is not among its own standard streams.
    fs::write(&log, "== clippy\n").unwrap();
    redirected_build_leaves_nothing_open(&sb, &p, 2, "clippy", 2, &both, &[&log]);
    // Each stream a file of its own, stdin among them.
    let [input, out, err]: [PathBuf; 3] = ["in.txt", "out.log", "err.log"].map(|n| sb.dir.join(n));
    let each = format!("< \"{}\" >> \"{}\" 2>> \"{}\"", input.display(), out.display(), err.display());
    for (round, command, commands) in [(3, "build", 1), (4, "clippy", 2)] {
        fs::write(&input, "").unwrap();
        redirected_build_leaves_nothing_open(&sb, &p, round, command, commands, &each, &[&input, &out, &err]);
    }
}
