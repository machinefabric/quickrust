//! Speed index: how long the same developer loop takes through Haul compared with upstream Cargo.
//!
//!     cargo build --release --workspace
//!     target/release/haul-bench fetch                # once; the only step that uses the network
//!     target/release/haul-bench run --quick          # ~minutes, while iterating
//!     target/release/haul-bench run --full --record  # the recorded measurement
//!
//! See benchmarks/README.md for the method.

mod measure;
mod sealed;
mod verify;
mod workload;

use measure::Sample;
use sealed::Sealed;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use workload::Workload;

const SCENARIOS: &[(&str, &str)] = &[
    ("cold-build", "first `cargo build`: empty target directory and Haul state"),
    ("no-op", "`cargo build` with nothing changed (mean of 10 runs per sample)"),
    ("edit-leaf", "body change in a crate nothing depends on, then `cargo build`"),
    ("edit-core", "body change in a crate others build on, then `cargo build`"),
    ("check-after-edit", "another leaf edit, then `cargo check`"),
    ("test-build", "`cargo test --no-run`"),
    ("rebuild-after-clean", "`cargo clean`, then `cargo build`"),
    ("second-checkout", "`cargo build` in a second checkout of the same commit"),
];
const QUICK_SCENARIOS: &[&str] = &["cold-build", "no-op", "edit-leaf", "edit-core", "rebuild-after-clean", "second-checkout"];
const NOOP_RUNS: u64 = 10;

fn fail(msg: &str) -> ! {
    eprintln!("haul-bench: error: {}", msg);
    std::process::exit(1)
}

#[derive(Debug, Clone, PartialEq)]
enum Base {
    Cargo,
    Haul,
}

#[derive(Debug, Clone)]
struct Variant {
    name: String,
    base: Base,
    env: Vec<(String, String)>,
    /// `cargo --config` overlays placed before the subcommand.
    config: Vec<String>,
}

fn builtin_variants(haul_has_mode: bool) -> Vec<Variant> {
    let v = |name: &str, base: Base, env: &[(&str, &str)], config: &[&str]| Variant {
        name: name.into(),
        base,
        env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        config: config.iter().map(|s| s.to_string()).collect(),
    };
    let mut out = vec![
        v("cargo", Base::Cargo, &[], &[]),
        // Cargo's own build-performance guide, written by hand: what a user gets without Haul.
        v("cargo-tuned", Base::Cargo, &[], &["profile.dev.debug=\"line-tables-only\"", "profile.dev.package.\"*\".debug=false"]),
        v("haul", Base::Haul, &[], &[]),
    ];
    if haul_has_mode {
        out.push(v("haul-compat", Base::Haul, &[("HAUL_MODE", "compat")], &[]));
    }
    out
}

/// `NAME=BASE[;env:K=V][;config:K=V]…`, e.g. `try-lld=haul;env:HAUL_X=1`.
fn parse_define(spec: &str) -> Result<Variant, String> {
    let (name, rest) = spec.split_once('=').ok_or_else(|| format!("--define {:?}: expected NAME=BASE[;env:K=V][;config:K=V]", spec))?;
    let mut parts = rest.split(';');
    let base = match parts.next() {
        Some("cargo") => Base::Cargo,
        Some("haul") => Base::Haul,
        other => return Err(format!("--define {:?}: base must be `cargo` or `haul`, got {:?}", spec, other)),
    };
    let mut v = Variant { name: name.to_string(), base, env: Vec::new(), config: Vec::new() };
    for p in parts {
        if let Some(kv) = p.strip_prefix("env:") {
            let (k, val) = kv.split_once('=').ok_or_else(|| format!("--define: {:?} is not K=V", kv))?;
            v.env.push((k.into(), val.into()));
        } else if let Some(c) = p.strip_prefix("config:") {
            v.config.push(c.into());
        } else {
            return Err(format!("--define: unknown part {:?} (use env:K=V or config:K=V)", p));
        }
    }
    Ok(v)
}

struct Ctx {
    work: PathBuf,
    sealed: Sealed,
    haul: PathBuf,
}

impl Ctx {
    fn variant_dir(&self, v: &Variant, w: &Workload) -> PathBuf {
        self.work.join("runs").join(&v.name).join(&w.name)
    }

    /// A Cargo command for `v`, run in checkout `co` with its own target directory and Haul state.
    fn command(&self, v: &Variant, w: &Workload, co: &Path, args: &[&str], workload_args: bool) -> std::process::Command {
        let program = match v.base {
            Base::Cargo => self.sealed.cargo.clone(),
            Base::Haul => self.haul.clone(),
        };
        let mut c = self.sealed.command(&program, co, &co.join("target"), false);
        if v.base == Base::Haul {
            c.env("HAUL_HOME", self.variant_dir(v, w).join("haul-home"));
        }
        for (k, val) in &v.env {
            c.env(k, val);
        }
        for o in &v.config {
            c.arg("--config").arg(o);
        }
        c.args(args);
        if workload_args {
            c.args(&w.args);
        }
        c
    }

    fn time(&self, v: &Variant, w: &Workload, co: &Path, args: &[&str]) -> Sample {
        measure::run(&mut self.command(v, w, co, args, true)).unwrap_or_else(|e| fail(&format!("[{} / {}] cargo {}: {}", w.name, v.name, args.join(" "), e)))
    }
}

/// One measured repetition for one variant: the whole developer loop from nothing built.
fn repetition(ctx: &Ctx, v: &Variant, w: &Workload, selected: &[&str], originals: &[(String, String)], edit_n: &mut u64) -> BTreeMap<String, Sample> {
    let dir = ctx.variant_dir(v, w);
    let (a, b) = (dir.join("a"), dir.join("b"));
    // Nothing may be cached at the start: build output, Haul's state, edited sources.
    for p in [a.join("target"), b.join("target"), dir.join("haul-home")] {
        let _ = fs::remove_dir_all(p);
    }
    for (file, text) in originals {
        fs::write(a.join(file), text).unwrap_or_else(|e| fail(&format!("cannot restore {}: {}", file, e)));
    }
    let original = |file: &str| originals.iter().find(|(f, _)| f == file).map(|(_, t)| t.as_str()).unwrap_or("");
    let mut edit = |e: &workload::Edit| {
        *edit_n += 1;
        e.apply(&a, original(&e.file), *edit_n).unwrap_or_else(|err| fail(&err));
    };
    let sel = |s: &str| selected.contains(&s);
    let mut out = BTreeMap::new();
    let mut put = |name: &str, mut s: Sample, co: &Path| {
        s.target_bytes = measure::allocated_bytes(&co.join("target"));
        out.insert(name.to_string(), s);
    };

    let cold = ctx.time(v, w, &a, &["build"]);
    if sel("cold-build") {
        put("cold-build", cold, &a);
    }
    if sel("no-op") {
        let mut acc = Sample::default();
        for _ in 0..NOOP_RUNS {
            let s = ctx.time(v, w, &a, &["build"]);
            acc.wall_ns += s.wall_ns;
            acc.cpu_ns += s.cpu_ns;
            acc.units_run += s.units_run;
            acc.max_rss_bytes = acc.max_rss_bytes.max(s.max_rss_bytes);
            acc.cpu_pressure = s.cpu_pressure;
            acc.io_pressure = s.io_pressure;
        }
        acc.wall_ns /= NOOP_RUNS;
        acc.cpu_ns /= NOOP_RUNS;
        put("no-op", acc, &a);
    }
    if sel("edit-leaf") {
        edit(&w.leaf);
        put("edit-leaf", ctx.time(v, w, &a, &["build"]), &a);
    }
    if sel("edit-core") {
        edit(&w.core);
        put("edit-core", ctx.time(v, w, &a, &["build"]), &a);
    }
    if sel("check-after-edit") {
        edit(&w.leaf);
        put("check-after-edit", ctx.time(v, w, &a, &["check"]), &a);
    }
    if sel("test-build") {
        put("test-build", ctx.time(v, w, &a, &["test", "--no-run"]), &a);
    }
    if sel("rebuild-after-clean") {
        measure::run(&mut ctx.command(v, w, &a, &["clean"], false)).unwrap_or_else(|e| fail(&format!("[{} / {}] cargo clean: {}", w.name, v.name, e)));
        put("rebuild-after-clean", ctx.time(v, w, &a, &["build"]), &a);
    }
    if sel("second-checkout") {
        put("second-checkout", ctx.time(v, w, &b, &["build"]), &b);
    }
    out
}

fn median(v: &[u64]) -> u64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    match s.len() {
        0 => 0,
        n if n % 2 == 1 => s[n / 2],
        n => (s[n / 2 - 1] + s[n / 2]) / 2,
    }
}

fn geomean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    (xs.iter().map(|x| x.ln()).sum::<f64>() / xs.len() as f64).exp()
}

fn secs(ns: u64) -> String {
    let s = ns as f64 / 1e9;
    if s < 1.0 {
        format!("{:.0}ms", s * 1e3)
    } else {
        format!("{:.2}s", s)
    }
}

fn capture(dir: &Path, program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program).current_dir(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn machine() -> Value {
    let cpu = fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| t.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string()));
    let mem = fs::read_to_string("/proc/meminfo").ok().and_then(|t| t.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|k| k.parse::<u64>().ok())).map(|k| k * 1024);
    json!({ "os": std::env::consts::OS, "arch": std::env::consts::ARCH, "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0), "cpu_model": cpu, "memory_bytes": mem })
}

struct Opts {
    work: PathBuf,
    toolchain: String,
    quick: bool,
    full: bool,
    reps: Option<usize>,
    /// Untimed repetitions first; default 1 for full runs, none for quick ones.
    warmups: Option<usize>,
    note: Option<String>,
    workloads: Option<Vec<String>>,
    variants: Option<Vec<String>>,
    only: Option<Vec<String>>,
    defines: Vec<Variant>,
    aa: bool,
    record: bool,
    haul: Option<PathBuf>,
    max_cpu_pressure: f64,
    /// Every option given, to reject those the chosen command does not use.
    given: Vec<String>,
}

fn list(v: &str) -> Vec<String> {
    v.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn parse_opts(args: &[String]) -> Opts {
    let mut o = Opts {
        work: PathBuf::from("/mnt/shared/nvme2p3/haul-bench"),
        toolchain: "stable".into(),
        quick: false,
        full: false,
        reps: None,
        warmups: None,
        note: None,
        workloads: None,
        variants: None,
        only: None,
        defines: Vec::new(),
        aa: false,
        record: false,
        haul: None,
        max_cpu_pressure: 10.0,
        given: Vec::new(),
    };
    if let Some(w) = std::env::var_os("HAUL_BENCH_DIR") {
        o.work = PathBuf::from(w);
    }
    let mut i = 0;
    let val = |i: &mut usize, flag: &str| -> String {
        *i += 1;
        args.get(*i).cloned().unwrap_or_else(|| fail(&format!("{} needs a value", flag)))
    };
    while i < args.len() {
        o.given.push(args[i].clone());
        match args[i].as_str() {
            "--work-dir" => o.work = PathBuf::from(val(&mut i, "--work-dir")),
            "--toolchain" => o.toolchain = val(&mut i, "--toolchain"),
            "--quick" => o.quick = true,
            "--full" => o.full = true,
            "--reps" => o.reps = Some(val(&mut i, "--reps").parse().ok().filter(|n| *n >= 1).unwrap_or_else(|| fail("--reps needs a number ≥ 1"))),
            "--warmups" => o.warmups = Some(val(&mut i, "--warmups").parse().unwrap_or_else(|_| fail("--warmups needs a number"))),
            "--note" => o.note = Some(val(&mut i, "--note")),
            "--workload" => o.workloads = Some(list(&val(&mut i, "--workload"))),
            "--variant" => o.variants = Some(list(&val(&mut i, "--variant"))),
            "--only" => o.only = Some(list(&val(&mut i, "--only"))),
            "--define" => o.defines.push(parse_define(&val(&mut i, "--define")).unwrap_or_else(|e| fail(&e))),
            "--aa" => o.aa = true,
            "--record" => o.record = true,
            "--haul" => o.haul = Some(PathBuf::from(val(&mut i, "--haul"))),
            "--max-cpu-pressure" => o.max_cpu_pressure = val(&mut i, "--max-cpu-pressure").parse().unwrap_or_else(|_| fail("--max-cpu-pressure needs a number")),
            other => fail(&format!("unknown argument {:?}; see benchmarks/README.md", other)),
        }
        i += 1;
    }
    if o.quick && o.full {
        fail("--quick and --full exclude each other");
    }
    o
}

const USAGE: &str = "usage: haul-bench fetch [--work-dir DIR] [--workload a,b] [--toolchain T]
       haul-bench run [--quick | --full] [--reps N] [--warmups N] [--workload a,b] [--variant a,b]
                      [--only scenario,…] [--define NAME=BASE;env:K=V;config:K=V] [--aa]
                      [--toolchain T] [--record] [--note TEXT] [--work-dir DIR] [--haul PATH]
       haul-bench verify [--workload a,b] [--define …] [--toolchain T] [--record] [--work-dir DIR] [--haul PATH]";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first() else { fail(USAGE) };
    let opts = parse_opts(&argv[1..]);
    let accepted: &[&str] = match cmd.as_str() {
        "fetch" => &["--work-dir", "--toolchain", "--workload"],
        "verify" => &["--work-dir", "--toolchain", "--workload", "--define", "--record", "--haul"],
        _ => &[],
    };
    if let Some(unused) = opts.given.iter().find(|g| !accepted.is_empty() && !accepted.contains(&g.as_str())) {
        fail(&format!("`haul-bench {}` does not take {}\n{}", cmd, unused, USAGE));
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap_or(Path::new(".")).to_path_buf();
    let all = workload::load(&repo.join("benchmarks/workloads.toml")).unwrap_or_else(|e| fail(&e));
    let chosen: Vec<Workload> = match &opts.workloads {
        Some(names) => names.iter().map(|n| all.iter().find(|w| &w.name == n).cloned().unwrap_or_else(|| fail(&format!("unknown workload {:?}", n)))).collect(),
        None if opts.quick => all.iter().filter(|w| w.quick).cloned().collect(),
        None => all.clone(),
    };
    fs::create_dir_all(&opts.work).unwrap_or_else(|e| fail(&format!("cannot create {}: {}", opts.work.display(), e)));
    let work = fs::canonicalize(&opts.work).unwrap_or(opts.work.clone());
    sealed::check_no_ancestor_config(&work).unwrap_or_else(|e| fail(&e));
    let sealed = Sealed::new(&work, &opts.toolchain).unwrap_or_else(|e| fail(&e));
    match cmd.as_str() {
        "fetch" => {
            for w in &chosen {
                workload::fetch(&work, &repo, w, &sealed).unwrap_or_else(|e| fail(&e));
                eprintln!("haul-bench: {} @ {} ready", w.name, w.rev);
            }
        }
        "run" => run(&opts, &repo, work, sealed, &chosen),
        "verify" => verify_command(&opts, &repo, work, sealed, &chosen),
        _ => fail(USAGE),
    }
}

/// The binaries under test and fetched workloads, or a hard error saying what is missing.
fn context(opts: &Opts, work: &Path, sealed: Sealed, workloads: &[Workload]) -> Ctx {
    let haul = opts.haul.clone().unwrap_or_else(|| std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("haul"))).unwrap_or_default());
    if !haul.is_file() || !haul.with_file_name("haul-rustc-wrapper").is_file() {
        fail(&format!("{} (and haul-rustc-wrapper next to it) not found: build with `cargo build --release --workspace`, or pass --haul", haul.display()));
    }
    for w in workloads {
        if !workload::is_fetched(work, w) {
            fail(&format!("workload {} is not fetched; run `haul-bench fetch` first", w.name));
        }
    }
    Ctx { work: work.to_path_buf(), sealed, haul }
}

/// Git revision and dirtiness of the Haul checkout; a record without them is worthless.
fn provenance(repo: &Path) -> (String, bool) {
    let rev = capture(repo, "git", &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| fail(&format!("cannot read the Haul revision in {} (git rev-parse failed)", repo.display())));
    let status = capture(repo, "git", &["status", "--porcelain"]).unwrap_or_else(|| fail(&format!("cannot read the state of {} (git status failed)", repo.display())));
    (rev, !status.is_empty())
}

/// `haul-bench verify`: the correctness gate (see verify.rs).
fn verify_command(opts: &Opts, repo: &Path, work: PathBuf, sealed: Sealed, workloads: &[Workload]) {
    let (haul_rev, haul_dirty) = provenance(repo);
    let ctx = context(opts, &work, sealed, workloads);
    let mut variants: Vec<Variant> = builtin_variants(false).into_iter().filter(|v| v.name == "cargo" || v.name == "haul").collect();
    variants.extend(opts.defines.iter().cloned());
    let (report, agree) = verify::verify(&ctx, &variants, workloads);
    let entry = json!({
        "recorded_at_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "haul_rev": haul_rev, "haul_dirty": haul_dirty, "toolchain": ctx.sealed.toolchain, "machine": machine(),
        "variants": variants.iter().map(|v| json!({ "name": v.name, "env": v.env.iter().map(|(k, val)| format!("{}={}", k, val)).collect::<Vec<_>>(), "config": v.config })).collect::<Vec<_>>(),
        "agree": agree, "workloads": report,
    });
    println!("
{}", if agree { "VERIFIED: every variant agrees with upstream Cargo." } else { "NOT VERIFIED: some test outcomes differ from upstream Cargo." });
    if opts.record {
        let path = repo.join("benchmarks/verify.jsonl");
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        text.push_str(&entry.to_string());
        text.push('\n');
        fs::write(&path, text).unwrap_or_else(|e| fail(&format!("cannot write {}: {}", path.display(), e)));
        println!("recorded in {}", path.display());
    }
    if !agree {
        std::process::exit(1);
    }
}

fn run(opts: &Opts, repo: &Path, work: PathBuf, sealed: Sealed, workloads: &[Workload]) {
    let ctx = context(opts, &work, sealed, workloads);
    // Haul's effective configuration is part of what is measured.
    let probe = work.join("probe");
    let _ = fs::create_dir_all(&probe);
    let policy: Value = ctx
        .sealed
        .command(&ctx.haul, &probe, &probe.join("target"), false)
        .env("HAUL_HOME", work.join("probe/haul-home"))
        .args(["policy", "explain", "--format", "json"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or(Value::Null);
    let settings: Vec<String> = policy["settings"].as_array().into_iter().flatten().map(|s| format!("{}={}", s["key"].as_str().unwrap_or(""), s["value"].as_str().unwrap_or(""))).collect();
    let has_mode = settings.iter().any(|s| s.starts_with("mode="));

    let mut variants = builtin_variants(has_mode);
    variants.extend(opts.defines.iter().cloned());
    if opts.aa {
        variants.push(Variant { name: "cargo-aa".into(), base: Base::Cargo, env: vec![], config: vec![] });
    }
    let wanted: Option<Vec<String>> = opts.variants.clone().or_else(|| opts.quick.then(|| vec!["cargo".into(), "haul".into()]));
    if let Some(names) = &wanted {
        for n in names {
            if !variants.iter().any(|v| &v.name == n) {
                fail(&format!("unknown variant {:?} (available: {})", n, variants.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(", ")));
            }
        }
        variants.retain(|v| names.contains(&v.name) || v.name == "cargo" || (opts.aa && v.name == "cargo-aa") || opts.defines.iter().any(|d| d.name == v.name));
    }
    if !has_mode {
        eprintln!("haul-bench: this Haul has no `mode` setting yet; haul-compat is identical to haul and is skipped");
    }
    let selected: Vec<&str> = match &opts.only {
        Some(names) => names.iter().map(|n| SCENARIOS.iter().find(|(s, _)| s == n).map(|(s, _)| *s).unwrap_or_else(|| fail(&format!("unknown scenario {:?}", n)))).collect(),
        None if opts.quick => QUICK_SCENARIOS.to_vec(),
        None => SCENARIOS.iter().map(|(s, _)| *s).collect(),
    };
    let reps = opts.reps.unwrap_or(if opts.full { 5 } else { 3 });
    // Two records: every quick run (one per iteration) and the full checkpoints. Extra --define
    // variants may be recorded (a lever measured on its own); otherwise a record must have the
    // standard shape of its tier so that records stay comparable.
    let standard = opts.only.is_none() && opts.variants.is_none() && !opts.aa && opts.workloads.is_none() && opts.reps.is_none();
    if opts.record && !(standard && (opts.full || opts.quick)) {
        fail("--record needs a plain --quick or --full run (no --only, --variant, --workload, --reps or --aa; --define is allowed)");
    }
    let warmups = opts.warmups.unwrap_or(if opts.full { 1 } else { 0 });

    let toolchain = json!({ "pinned": ctx.sealed.toolchain, "cargo": ctx.sealed.version("cargo", &probe), "rustc": ctx.sealed.version("rustc", &probe) });
    eprintln!("haul-bench: {} · {} · {} rep(s) + {} warm-up · variants {} · scenarios {}", toolchain["rustc"].as_str().unwrap_or(""), work.display(), reps, warmups, variants.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(" "), selected.join(" "));

    // Provenance is taken before measuring: the binaries were built from this state, and later
    // edits to the checkout must not be attributed to them.
    let (haul_rev, haul_dirty) = provenance(repo);
    let started = Instant::now();
    let mut edit_n = 0u64;
    let mut problems: Vec<String> = Vec::new();
    let mut results: Vec<Value> = Vec::new();
    let mut indices: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for w in workloads {
        // Fresh checkouts for every variant: a = the edited working tree, b = a second clone.
        for v in &variants {
            let dir = ctx.variant_dir(v, w);
            let _ = fs::remove_dir_all(&dir);
            for co in ["a", "b"] {
                workload::checkout(&work, w, &dir.join(co)).unwrap_or_else(|e| fail(&e));
            }
            // Isolation pre-flight: Cargo must see exactly the target directory we assigned.
            let meta = ctx.command(v, w, &dir.join("a"), &["metadata", "--format-version", "1", "--no-deps"], false).output().unwrap_or_else(|e| fail(&e.to_string()));
            let td = serde_json::from_slice::<Value>(&meta.stdout).ok().and_then(|m| m["target_directory"].as_str().map(PathBuf::from));
            if td.as_deref() != Some(dir.join("a/target").as_path()) {
                fail(&format!("isolation: {} / {} would build in {:?}, not {}", w.name, v.name, td, dir.join("a/target").display()));
            }
        }
        let originals: Vec<(String, String)> = [&w.leaf, &w.core]
            .iter()
            .map(|e| (e.file.clone(), fs::read_to_string(ctx.variant_dir(&variants[0], w).join("a").join(&e.file)).unwrap_or_else(|err| fail(&err.to_string()))))
            .collect();
        // samples[variant][scenario] = measured samples; counts[(scenario, rep)][variant] = units run.
        let mut samples: BTreeMap<String, BTreeMap<String, Vec<Sample>>> = BTreeMap::new();
        let mut counts: BTreeMap<(String, usize), BTreeMap<String, u64>> = BTreeMap::new();
        let total = warmups + reps;
        for rep in 0..total {
            let rep_started = Instant::now();
            let measured = rep >= warmups;
            // Rotate the order so no variant always runs first (page cache, thermal and load drift).
            let n = variants.len();
            for k in 0..n {
                let v = &variants[(k + rep) % n];
                eprintln!("haul-bench: {} · {} {}/{} · {}", w.name, if measured { "repetition" } else { "warm-up" }, if measured { rep - warmups + 1 } else { rep + 1 }, if measured { reps } else { warmups }, v.name);
                let r = repetition(&ctx, v, w, &selected, &originals, &mut edit_n);
                if !measured {
                    continue;
                }
                for (scenario, s) in r {
                    counts.entry((scenario.clone(), rep)).or_default().insert(v.name.clone(), s.units_run);
                    samples.entry(v.name.clone()).or_default().entry(scenario).or_default().push(s);
                }
            }
            if rep == 0 {
                let per = rep_started.elapsed().as_secs_f64();
                eprintln!("haul-bench: {}: one repetition of all variants takes ~{:.0}s → ~{:.1} min for this workload", w.name, per, per * total as f64 / 60.0);
            }
        }

        // Every variant runs the same Cargo units; only how they are compiled may differ.
        for ((scenario, rep), by_variant) in &counts {
            let reference = by_variant.get("cargo").copied();
            for (vn, c) in by_variant {
                if scenario.starts_with("edit-") && *c == 0 {
                    problems.push(format!("{} / {} / {} rep {}: the edit compiled nothing", w.name, vn, scenario, rep));
                }
                if Some(*c) != reference && vn != "cargo" {
                    let msg = format!("{} / {} rep {}: {} ran {} units, cargo ran {}", w.name, scenario, rep, vn, c, reference.unwrap_or(0));
                    if vn == "haul-compat" || vn == "cargo-aa" {
                        problems.push(msg);
                    } else {
                        eprintln!("haul-bench: note: {}", msg);
                    }
                }
            }
        }

        // Medians, stability and the index for this workload.
        println!("\n{} @ {} — {}", w.name, &w.rev[..w.rev.len().min(10)], w.description);
        print!("{:<20}", "scenario");
        for v in &variants {
            print!("{:>13}", v.name);
        }
        for v in variants.iter().filter(|v| v.name != "cargo") {
            print!("{:>16}", format!("{} ×", v.name));
        }
        println!();
        let mut scen_rows = Vec::new();
        let mut speedups: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
        for scenario in &selected {
            let mut row = serde_json::Map::new();
            row.insert("name".into(), json!(scenario));
            let mut wall_med: BTreeMap<&str, u64> = BTreeMap::new();
            let mut cpu_med: BTreeMap<&str, u64> = BTreeMap::new();
            for v in &variants {
                let all = samples.get(&v.name).and_then(|m| m.get(*scenario)).cloned().unwrap_or_default();
                let valid: Vec<&Sample> = all.iter().filter(|s| s.cpu_pressure.map(|p| p <= opts.max_cpu_pressure).unwrap_or(true)).collect();
                if valid.len() * 2 < all.len().max(1) {
                    problems.push(format!("{} / {} / {}: {} of {} samples ran under CPU pressure above {}%", w.name, v.name, scenario, all.len() - valid.len(), all.len(), opts.max_cpu_pressure));
                }
                let walls: Vec<u64> = valid.iter().map(|s| s.wall_ns).collect();
                let cpus: Vec<u64> = valid.iter().map(|s| s.cpu_ns).collect();
                let (m, c) = (median(&walls), median(&cpus));
                if m >= 1_000_000_000 && walls.len() >= 2 {
                    let spread = (walls.iter().max().unwrap_or(&0) - walls.iter().min().unwrap_or(&0)) as f64 / m as f64;
                    if spread > 0.25 {
                        problems.push(format!("{} / {} / {}: samples {} … {} differ by {:.0}% (machine busy?)", w.name, v.name, scenario, secs(*walls.iter().min().unwrap_or(&0)), secs(*walls.iter().max().unwrap_or(&0)), spread * 100.0));
                    }
                }
                wall_med.insert(&v.name, m);
                cpu_med.insert(&v.name, c);
                row.insert(v.name.clone(), json!({
                    "wall_median_ns": m, "cpu_median_ns": c,
                    "samples": all.iter().map(|s| json!({ "wall_ns": s.wall_ns, "cpu_ns": s.cpu_ns, "max_rss_bytes": s.max_rss_bytes, "units_run": s.units_run, "target_bytes": s.target_bytes, "cpu_pressure": s.cpu_pressure, "io_pressure": s.io_pressure })).collect::<Vec<_>>(),
                }));
            }
            print!("{:<20}", scenario);
            for v in &variants {
                print!("{:>13}", secs(wall_med[v.name.as_str()]));
            }
            let base = (wall_med["cargo"] as f64, cpu_med["cargo"] as f64);
            for v in variants.iter().filter(|v| v.name != "cargo") {
                let (sw, sc) = (base.0 / wall_med[v.name.as_str()].max(1) as f64, base.1 / cpu_med[v.name.as_str()].max(1) as f64);
                let e = speedups.entry(v.name.clone()).or_default();
                e.0.push(sw);
                e.1.push(sc);
                print!("{:>16}", format!("{:.2}", sw));
            }
            println!();
            scen_rows.push(Value::Object(row));
        }
        let mut idx = serde_json::Map::new();
        for (vn, (sw, sc)) in &speedups {
            let (iw, ic) = (geomean(sw), geomean(sc));
            println!("  index {:<14} {:.3}   (cpu {:.3})", vn, iw, ic);
            idx.insert(vn.clone(), json!({ "wall": (iw * 1000.0).round() / 1000.0, "cpu": (ic * 1000.0).round() / 1000.0 }));
            let e = indices.entry(vn.clone()).or_default();
            e.0.push(iw);
            e.1.push(ic);
        }
        results.push(json!({ "name": w.name, "rev": w.rev, "args": w.args, "scenarios": scen_rows, "index": idx }));
    }

    println!("\nSpeed index (geometric mean of cargo-time / variant-time; 1.000 = parity, above = faster than Cargo)");
    let mut overall = serde_json::Map::new();
    for (vn, (w, c)) in &indices {
        let (iw, ic) = (geomean(w), geomean(c));
        println!("  {:<14} {:.3}   (cpu {:.3})", vn, iw, ic);
        overall.insert(vn.clone(), json!({ "wall": (iw * 1000.0).round() / 1000.0, "cpu": (ic * 1000.0).round() / 1000.0 }));
    }
    println!("  elapsed {:.1} min", started.elapsed().as_secs_f64() / 60.0);

    if provenance(repo) != (haul_rev.clone(), haul_dirty) {
        eprintln!("haul-bench: note: the Haul checkout changed during the run; the record describes it as it was at the start");
    }
    let entry = json!({
        "recorded_at_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "haul_rev": haul_rev, "haul_dirty": haul_dirty, "haul_settings": settings,
        "mode": if opts.full { "full" } else if opts.quick { "quick" } else { "custom" },
        "repetitions": reps, "warmups": warmups, "note": opts.note,
        "machine": machine(), "toolchain": toolchain,
        "variants": variants.iter().map(|v| json!({ "name": v.name, "base": format!("{:?}", v.base).to_lowercase(), "env": v.env.iter().map(|(k, val)| format!("{}={}", k, val)).collect::<Vec<_>>(), "config": v.config })).collect::<Vec<_>>(),
        "workloads": results, "index": overall, "problems": problems,
    });
    let results_dir = work.join("results");
    let _ = fs::create_dir_all(&results_dir);
    let file = results_dir.join(format!("{}.json", entry["recorded_at_unix"]));
    let _ = fs::write(&file, serde_json::to_vec_pretty(&entry).unwrap_or_default());
    println!("  full results: {}", file.display());

    if !problems.is_empty() {
        println!("\nNOT A VALID MEASUREMENT:");
        for p in &problems {
            println!("  {}", p);
        }
        if opts.record {
            std::process::exit(1);
        }
        return;
    }
    if opts.record {
        let path = repo.join(if opts.full { "benchmarks/speed-index.jsonl" } else { "benchmarks/speed-index-quick.jsonl" });
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        text.push_str(&entry.to_string());
        text.push('\n');
        fs::write(&path, text).unwrap_or_else(|e| fail(&format!("cannot write {}: {}", path.display(), e)));
        println!("  recorded in {}{}", path.display(), if haul_dirty { " (WARNING: the Haul checkout has uncommitted changes)" } else { "" });
    }
}
