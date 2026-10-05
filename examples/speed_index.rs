//! Speed index: how long the same developer loop takes through Haul compared
//! with upstream Cargo.
//!
//!     cargo build --release && cargo run --release --example speed_index -- [--reps N] [--record]
//!
//! The workload is pinned: Haul's own source at a fixed commit, with its real
//! registry dependencies, so the number is comparable from one Haul version to
//! the next. Each variant gets its own checkouts, target directories and state.
//!
//! Variants
//!   cargo        upstream Cargo, no Haul anywhere
//!   haul         `haul <command>` with Haul's default configuration
//!   haul+cache   Haul with `cache.provider = "sccache"` (isolated sccache server and cache)
//!
//! A scenario's speedup is `cargo median / variant median`; the index is the
//! geometric mean over scenarios. 1.00 is parity, above 1 is faster than Cargo.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// The pinned workload: the first commit of this repository.
const WORKLOAD_REV: &str = "b2af135";
const EDIT_FILE: &str = "src/util.rs";

const SCENARIOS: &[(&str, &str)] = &[
    ("cold-build", "cargo build from an empty target directory, nothing cached anywhere"),
    ("no-op", "cargo build again with nothing changed"),
    ("edit-rebuild", "cargo build after a one-line edit to a workspace source file"),
    ("check-after-build", "cargo check in the tree that was just built"),
    ("rebuild-after-clean", "cargo build after the target directory was removed"),
    ("second-checkout", "cargo build in a second checkout of the same commit"),
];

const SCRUB: &[&str] = &["CARGO", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_INCREMENTAL", "RUSTUP_TOOLCHAIN", "CARGO_MAKEFLAGS", "MAKEFLAGS", "CARGO_MANIFEST_DIR", "CARGO_PRIMARY_PACKAGE", "CI"];

struct Variant {
    name: &'static str,
    /// Program and leading arguments placed before the Cargo command.
    program: PathBuf,
    env: Vec<(String, String)>,
    dir: PathBuf,
    sccache: Option<(PathBuf, u32)>,
}

impl Variant {
    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(&self.program);
        c.current_dir(cwd).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        for (k, _) in std::env::vars_os() {
            let k = k.to_string_lossy().into_owned();
            if SCRUB.contains(&k.as_str()) || k.starts_with("HAUL_") || k.starts_with("CARGO_PKG_") || k.starts_with("SCCACHE_") {
                c.env_remove(&k);
            }
        }
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    /// Runs one Cargo command and returns its wall time in nanoseconds.
    fn time(&self, cwd: &Path, args: &[&str]) -> u64 {
        let started = Instant::now();
        let out = self.command(cwd, args).output().unwrap_or_else(|e| fail(&format!("cannot run {:?}: {}", self.program, e)));
        let elapsed = started.elapsed().as_nanos() as u64;
        if !out.status.success() {
            fail(&format!("[{}] `{}` failed in {}:\n{}", self.name, args.join(" "), cwd.display(), String::from_utf8_lossy(&out.stderr)));
        }
        elapsed
    }

    /// Nothing may be cached at the start of a repetition.
    fn reset(&self) {
        for checkout in ["a", "b"] {
            let _ = fs::remove_dir_all(self.dir.join(checkout).join("target"));
        }
        let _ = fs::remove_dir_all(self.dir.join("haul-home"));
        if let Some((cache, port)) = &self.sccache {
            stop_sccache(cache, *port);
            let _ = fs::remove_dir_all(cache);
        }
    }
}

fn stop_sccache(cache: &Path, port: u32) {
    let _ = Command::new("sccache").arg("--stop-server").env("SCCACHE_DIR", cache).env("SCCACHE_SERVER_PORT", port.to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn fail(msg: &str) -> ! {
    eprintln!("speed-index: {}", msg);
    std::process::exit(1)
}

fn capture(program: &str, args: &[&str], cwd: &Path) -> String {
    let out = Command::new(program).args(args).current_dir(cwd).output().unwrap_or_else(|e| fail(&format!("cannot run {}: {}", program, e)));
    if !out.status.success() {
        fail(&format!("`{} {}` failed: {}", program, args.join(" "), String::from_utf8_lossy(&out.stderr)));
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn checkout(repo: &Path, into: &Path) {
    fs::create_dir_all(into).unwrap_or_else(|e| fail(&format!("cannot create {}: {}", into.display(), e)));
    let mut archive = Command::new("git").current_dir(repo).args(["archive", WORKLOAD_REV]).stdout(Stdio::piped()).spawn().unwrap_or_else(|e| fail(&format!("git archive: {}", e)));
    let stream = archive.stdout.take().unwrap_or_else(|| fail("git archive produced no output"));
    let untar = Command::new("tar").arg("-x").arg("-C").arg(into).stdin(stream).status().unwrap_or_else(|e| fail(&format!("tar: {}", e)));
    let archived = archive.wait().unwrap_or_else(|e| fail(&format!("git archive: {}", e)));
    if !archived.success() || !untar.success() {
        fail(&format!("could not extract workload {} (is the full git history available?)", WORKLOAD_REV));
    }
}

fn median(v: &[u64]) -> u64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s[s.len() / 2]
}

fn secs(ns: u64) -> String {
    format!("{:.2}s", ns as f64 / 1e9)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut reps = 3usize;
    let mut record = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reps" => {
                i += 1;
                reps = args.get(i).and_then(|v| v.parse().ok()).filter(|n| *n >= 1).unwrap_or_else(|| fail("--reps needs a number ≥ 1"));
            }
            "--record" => record = true,
            other => fail(&format!("unknown argument {:?} (usage: speed_index [--reps N] [--record])", other)),
        }
        i += 1;
    }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let haul = repo.join("target/release/haul");
    if !haul.is_file() || !repo.join("target/release/haul-rustc-wrapper").is_file() {
        fail("build Haul first: cargo build --release");
    }
    let work = std::env::temp_dir().join(format!("haul-speed-index-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let work = {
        fs::create_dir_all(&work).unwrap_or_else(|e| fail(&format!("cannot create {}: {}", work.display(), e)));
        fs::canonicalize(&work).unwrap_or(work)
    };
    let port = 21_000 + std::process::id() % 20_000;
    let has_sccache = Command::new("sccache").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);

    let mut variants = vec![
        Variant { name: "cargo", program: PathBuf::from("cargo"), env: vec![], dir: work.join("cargo"), sccache: None },
        Variant { name: "haul", program: haul.clone(), env: vec![("HAUL_HOME".into(), work.join("haul/haul-home").display().to_string())], dir: work.join("haul"), sccache: None },
    ];
    if has_sccache {
        let cache = work.join("haul-cache/sccache");
        variants.push(Variant {
            name: "haul+cache",
            program: haul.clone(),
            env: vec![
                ("HAUL_HOME".into(), work.join("haul-cache/haul-home").display().to_string()),
                ("HAUL_CACHE_PROVIDER".into(), "sccache".into()),
                ("SCCACHE_DIR".into(), cache.display().to_string()),
                ("SCCACHE_SERVER_PORT".into(), port.to_string()),
            ],
            dir: work.join("haul-cache"),
            sccache: Some((cache, port)),
        });
    } else {
        eprintln!("speed-index: sccache is not installed; the haul+cache variant is skipped");
    }
    for v in &variants {
        checkout(&repo, &v.dir.join("a"));
        checkout(&repo, &v.dir.join("b"));
    }
    // Dependencies must be on disk before anything is timed.
    variants[0].time(&variants[0].dir.join("a"), &["fetch", "--locked"]);

    let mut samples: Vec<Vec<Vec<u64>>> = vec![vec![Vec::new(); SCENARIOS.len()]; variants.len()];
    for rep in 0..reps {
        // Variants are interleaved within a repetition so slow drift of the machine hits all of them alike.
        for (vi, v) in variants.iter().enumerate() {
            eprintln!("speed-index: repetition {}/{} · {}", rep + 1, reps, v.name);
            v.reset();
            let (a, b) = (v.dir.join("a"), v.dir.join("b"));
            let mut t = Vec::new();
            t.push(v.time(&a, &["build", "--locked"]));
            t.push(v.time(&a, &["build", "--locked"]));
            let file = a.join(EDIT_FILE);
            let original = fs::read(&file).unwrap_or_else(|e| fail(&format!("cannot read {}: {}", file.display(), e)));
            let mut edited = original.clone();
            edited.extend_from_slice(format!("\n// speed-index edit {}\n", rep).as_bytes());
            fs::write(&file, edited).unwrap_or_else(|e| fail(&format!("cannot edit {}: {}", file.display(), e)));
            t.push(v.time(&a, &["build", "--locked"]));
            t.push(v.time(&a, &["check", "--locked"]));
            fs::write(&file, &original).unwrap_or_else(|e| fail(&format!("cannot restore {}: {}", file.display(), e)));
            fs::remove_dir_all(a.join("target")).unwrap_or_else(|e| fail(&format!("cannot remove target: {}", e)));
            t.push(v.time(&a, &["build", "--locked"]));
            t.push(v.time(&b, &["build", "--locked"]));
            for (si, ns) in t.into_iter().enumerate() {
                samples[vi][si].push(ns);
            }
        }
    }
    for v in &variants {
        if let Some((cache, port)) = &v.sccache {
            stop_sccache(cache, *port);
        }
    }

    let medians: Vec<Vec<u64>> = samples.iter().map(|v| v.iter().map(|s| median(s)).collect()).collect();
    let speedup = |vi: usize, si: usize| medians[0][si] as f64 / medians[vi][si] as f64;
    let index = |vi: usize| ((0..SCENARIOS.len()).map(|si| speedup(vi, si).ln()).sum::<f64>() / SCENARIOS.len() as f64).exp();
    let loop_total = |vi: usize| medians[vi].iter().sum::<u64>();

    println!("\nSpeed index — workload: haul@{} · {} repetition(s), medians\n", WORKLOAD_REV, reps);
    print!("{:<22}", "scenario");
    for v in &variants {
        print!("{:>12}", v.name);
    }
    for v in &variants[1..] {
        print!("{:>18}", format!("{} vs cargo", v.name));
    }
    println!();
    for (si, (name, _)) in SCENARIOS.iter().enumerate() {
        print!("{:<22}", name);
        for per_variant in &medians {
            print!("{:>12}", secs(per_variant[si]));
        }
        for vi in 1..variants.len() {
            print!("{:>18}", format!("{:.2}x", speedup(vi, si)));
        }
        println!();
    }
    print!("{:<22}", "whole loop");
    for vi in 0..variants.len() {
        print!("{:>12}", secs(loop_total(vi)));
    }
    for vi in 1..variants.len() {
        print!("{:>18}", format!("{:.2}x", loop_total(0) as f64 / loop_total(vi) as f64));
    }
    println!("\n");
    for vi in 1..variants.len() {
        println!("index ({:<10}) {:.3}   (geometric mean of per-scenario speedups; 1.000 = parity with Cargo)", variants[vi].name, index(vi));
    }

    if record {
        let rev = capture("git", &["rev-parse", "--short", "HEAD"], &repo);
        let dirty = !capture("git", &["status", "--porcelain"], &repo).is_empty();
        let entry = json!({
            "recorded_at_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "haul_rev": rev,
            "haul_dirty": dirty,
            "workload_rev": WORKLOAD_REV,
            "repetitions": reps,
            "machine": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH, "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0) },
            "toolchain": capture("cargo", &["-V"], &repo),
            "scenarios": SCENARIOS.iter().enumerate().map(|(si, (name, _))| {
                let mut row = serde_json::Map::new();
                row.insert("name".into(), json!(name));
                for (vi, v) in variants.iter().enumerate() {
                    row.insert(format!("{}_ns", v.name), json!(medians[vi][si]));
                    row.insert(format!("{}_samples_ns", v.name), json!(samples[vi][si]));
                }
                Value::Object(row)
            }).collect::<Vec<_>>(),
            "index": variants.iter().enumerate().skip(1).map(|(vi, v)| (v.name.to_string(), json!((index(vi) * 1000.0).round() / 1000.0))).collect::<serde_json::Map<_, _>>(),
            "whole_loop_speedup": variants.iter().enumerate().skip(1).map(|(vi, v)| (v.name.to_string(), json!((loop_total(0) as f64 / loop_total(vi) as f64 * 1000.0).round() / 1000.0))).collect::<serde_json::Map<_, _>>(),
        });
        let path = repo.join("benchmarks/speed-index.jsonl");
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        text.push_str(&entry.to_string());
        text.push('\n');
        fs::write(&path, text).unwrap_or_else(|e| fail(&format!("cannot write {}: {}", path.display(), e)));
        println!("\nrecorded in {}", path.display());
    }
    let _ = fs::remove_dir_all(&work);
}
