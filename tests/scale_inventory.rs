//! Scale (NFR-SCALE-001, NFR-007): the inventory of a build directory of a million files, without
//! keeping file records in memory. Slow, so outside the default run (see `scale_units`).

// Measured with wait4.
#![cfg(unix)]

mod common;

use common::*;
use serde_json::Value;
use std::fs;
use std::time::Instant;

#[test]
#[ignore = "slow: run with --ignored"]
fn inventory_of_a_build_directory_of_a_million_files() {
    let sb = Sandbox::new("scale-files");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    let target = p.join("target/debug");
    let started = Instant::now();
    // Incremental sessions hold most files of a real build directory: 1,000 crates × 1,000 files.
    for c in 0..1_000 {
        let dir = target.join(format!("incremental/crate_{:04}-abcdefghijklm/s-sess-{:04}", c, c));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..1_000 {
            fs::write(dir.join(format!("{:04}.o", f)), b"x").unwrap();
        }
    }
    eprintln!("scale: a million files written in {:.1} s", started.elapsed().as_secs_f64());
    let (secs, rss, success, out) = measure(sb.haul(&p).args(["inventory", "--format", "json"]));
    assert!(success);
    let v: Value = serde_json::from_slice(&out).unwrap();
    let files: u64 = v["build_directories"].as_array().unwrap().iter().map(|d| d["files"].as_u64().unwrap_or(0)).sum();
    eprintln!("scale: inventory of {} files in {:.2} s, peak memory {} MiB", files, secs, rss >> 20);
    assert!(files >= 1_000_000, "{} files", files);
    // NFR-SCALE-001: no file records kept in memory (measured: 5.1 s, 33 MiB).
    assert!(secs < 30.0 && rss < 256 << 20, "{:.1} s, {} MiB", secs, rss >> 20);
}
