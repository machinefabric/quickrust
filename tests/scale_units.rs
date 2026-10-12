//! Scale (NFR-SCALE-001, NFR-006): Haul's analysis of a build of 10,000 units. Slow, so outside
//! the default run (`scale_inventory` is its companion; each lives in a test binary of its own,
//! see `common::measure`):
//!
//! ```text
//! cargo test --release --test scale_units --test scale_inventory -- --ignored --nocapture
//! ```
//!
//! A real build makes the session; the units are synthetic compile events with the fields the
//! wrapper records, in a realistic shape (most dependencies on a few popular crates, a share of
//! proc macros that many crates use, a few hundred workspace crates).

// Measured with wait4.
#![cfg(unix)]

mod common;

use common::*;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// Deterministic pseudo-random numbers (xorshift), so every run sees the same graph.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn latest_session(sb: &Sandbox) -> Value {
    sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap()
}

#[test]
#[ignore = "slow: run with --ignored"]
fn analysis_of_a_build_of_ten_thousand_units() {
    const UNITS: usize = 10_000;
    const WORKSPACE: usize = 300;
    let sb = Sandbox::new("scale-units");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    let session = latest_session(&sb);
    let events = Path::new(session["events_dir"].as_str().unwrap()).to_path_buf();
    let start = session["started_wall_ns"].as_u64().unwrap();
    let deps_dir = p.join("target/debug/deps");
    let registry = sb.dir.join("cargo-home/registry/src/fake-0000");

    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    // Unit i: name, hash, whether a proc macro, end time (ns after start) for scheduling.
    let mut units: Vec<(String, String, bool, u64)> = Vec::with_capacity(UNITS);
    let mut latest_end = 0u64;
    let mut artifact_bytes = 0u64;
    for i in 0..UNITS {
        let name = format!("crate_{:05}", i);
        let hash = format!("{:016x}", rng.next());
        let proc_macro = i > 50 && rng.below(100) < 4;
        let local = i >= UNITS - WORKSPACE;
        // Dependencies: popular crates (the first hundred) far more often than others.
        let mut externs: Vec<usize> = Vec::new();
        if i > 0 {
            for _ in 0..rng.below(16) {
                let d = if rng.below(3) == 0 { rng.below(i.min(100) as u64) as usize } else { rng.below(i as u64) as usize };
                if !externs.contains(&d) {
                    externs.push(d);
                }
            }
        }
        let ready = externs.iter().map(|&d| units[d].3).max().unwrap_or(0);
        let dur = 20_000_000 + rng.below(1_500_000_000);
        let begin = ready + rng.below(5_000_000);
        units.push((name.clone(), hash.clone(), proc_macro, begin + dur));
        latest_end = latest_end.max(begin + dur);

        let manifest_dir = if local { p.join(format!("crates/{}", name)) } else { registry.join(format!("{}-1.0.0", name.replace('_', "-"))) };
        let src = if local { format!("crates/{}/src/lib.rs", name) } else { util_path(&manifest_dir.join("src/lib.rs")) };
        let mut args: Vec<String> = vec!["--crate-name".into(), name.clone(), "--edition=2021".into(), src, "--error-format=json".into(), "--json=diagnostic-rendered-ansi,artifacts,future-incompat".into(), "--crate-type".into(), if proc_macro { "proc-macro".into() } else { "lib".into() }, "--emit=dep-info,metadata,link".into(), "-C".into(), "embed-bitcode=no".into(), "-C".into(), "debuginfo=line-tables-only".into(), "-C".into(), format!("metadata={}", hash), "-C".into(), format!("extra-filename=-{}", hash), "--out-dir".into(), util_path(&deps_dir), "-L".into(), format!("dependency={}", util_path(&deps_dir))];
        if local {
            args.extend(["-C".into(), format!("incremental={}", util_path(&p.join("target/debug/incremental")))]);
        }
        for &d in &externs {
            let (dn, dh, dpm, _) = &units[d];
            let file = if *dpm { format!("lib{}-{}.so", dn, dh) } else { format!("lib{}-{}.rmeta", dn, dh) };
            args.extend(["--extern".into(), format!("{}={}", dn, util_path(&deps_dir.join(file)))]);
        }
        artifact_bytes += if proc_macro { 4_000_000 } else { 1_300_000 };
        let outputs = if proc_macro {
            json!([{ "file": format!("lib{}-{}.so", name, hash), "bytes": 4_000_000, "mtime_ns": start + begin + dur }])
        } else {
            json!([{ "file": format!("lib{}-{}.rmeta", name, hash), "bytes": 400_000, "mtime_ns": start + begin + dur / 2 }, { "file": format!("lib{}-{}.rlib", name, hash), "bytes": 900_000, "mtime_ns": start + begin + dur }])
        };
        let inputs = if local {
            json!({ "local": (0..1 + rng.below(12)).map(|f| json!({ "path": format!("crates/{}/src/m{}.rs", name, f), "bytes": 4_000, "mtime_ns": start - 1_000_000_000, "sha": format!("{:016x}", rng.next()) })).collect::<Vec<_>>() })
        } else {
            json!({})
        };
        let ev = json!({
            "v": 1, "session": session["id"], "kind": "compile", "pid": 100_000 + i, "start_wall_ns": start + begin, "dur_ns": dur, "exit_code": 0, "signal": null,
            "rustc": "/toolchain/bin/rustc", "inner_wrapper": null, "cache_fallback": false,
            "rusage": { "user_ns": dur, "sys_ns": dur / 5, "max_rss_bytes": 200_000_000, "scope": "rustc" },
            "rustc_id": { "bytes": 645_952, "mtime_ns": 1 }, "args": args, "args_non_utf8": false, "cwd": util_path(&p),
            "env": { "pkg_name": name.replace('_', "-"), "pkg_version": "1.0.0", "manifest_dir": util_path(&manifest_dir), "primary": local, "out_dir": null, "toolchain": "stable", "jobserver": true },
            "rmeta_ready_ns": dur / 2, "outputs": outputs, "inputs": inputs, "cache": { "result": "stored" },
        });
        fs::write(events.join(format!("{:020}-{}.json", start + begin, 100_000 + i)), serde_json::to_vec(&ev).unwrap()).unwrap();
    }
    // The session lasted as long as its units.
    let record = sb.session_path(session["id"].as_str().unwrap());
    let mut s: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    s["duration_ns"] = json!(latest_end + 1_000_000_000);
    fs::write(&record, serde_json::to_vec_pretty(&s).unwrap()).unwrap();

    let (secs, rss, success, out) = measure(sb.haul(&p).args(["report", "--format", "json"]));
    assert!(success, "the report of a build of {} units failed", UNITS);
    let report: Value = serde_json::from_slice(&out).unwrap();
    let units_reported = report["units"].as_array().map(|u| u.len()).unwrap_or(0);
    let run_bytes: u64 = fs::read_dir(record.parent().unwrap().parent().unwrap().join("runs")).map(|rd| rd.filter_map(|e| e.ok()?.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0);
    eprintln!("scale: {} units analysed in {:.2} s, peak memory {} MiB, stored run {} KiB, report {} KiB", units_reported, secs, rss >> 20, run_bytes >> 10, out.len() >> 10);
    assert!(units_reported >= UNITS, "{} units in the report", units_reported);
    // Budgets, with headroom over what was measured (2.1 s, 483 MiB, 38 MiB on Linux x86_64).
    assert!(secs < 15.0, "analysis took {:.1} s", secs);
    assert!(rss < 1 << 30, "analysis peaked at {} MiB", rss >> 20);
    // NFR-SPACE-001: at most 1% of the build's artifact bytes, and at most 500 MiB.
    let events_bytes: u64 = fs::read_dir(&events).unwrap().filter_map(|e| e.ok()?.metadata().ok()).map(|m| m.len()).sum();
    let metadata = run_bytes + events_bytes;
    assert!(metadata * 100 <= artifact_bytes && metadata <= 500 << 20, "{} MiB of analysis data for {} MiB of artifacts", metadata >> 20, artifact_bytes >> 20);

    // The next build of the workspace is analysed against the unit index the large one left.
    let index = record.parent().unwrap().parent().unwrap().join("index/units.json");
    let index_bytes = fs::metadata(&index).map(|m| m.len()).unwrap_or(0);
    ok(sb.cargo(&p).arg("build"));
    // Only its time: this process now holds the large report, which a forked child counts in its
    // peak memory until it executes (see `common::measure`).
    let (next_secs, _, success, _) = measure(sb.haul(&p).args(["report", "--format", "json"]));
    assert!(success);
    eprintln!("scale: unit index {} KiB; the next small build analysed in {:.2} s", index_bytes >> 10, next_secs);
    // Measured: 0.4 s, of which reading and rewriting the index is about 0.3 s.
    assert!(next_secs < 5.0, "{:.1} s", next_secs);
}

fn util_path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
