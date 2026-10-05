//! Scenario runner (FR-020) and A/B experiments (FR-080, FR-160).
//!
//! Benchmarks build in a target directory Haul owns, so "clean" never deletes
//! the user's artifacts. Synthetic source edits are journaled before they are
//! made and restored on every exit path, including interruption.

use crate::compare;
use crate::config::Config;
use crate::discover;
use crate::session::{self, Mode, Spec};
use crate::store::WsStore;
use crate::util::{self, human_dur, path_str};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const SCENARIOS: &[(&str, &str, &[&str], &str)] = &[
    ("clean", "cold", &["build"], "full build from an empty build directory"),
    ("no-change", "warm", &["build"], "rebuild with nothing changed (build-system overhead)"),
    ("small-edit", "warm", &["build"], "rebuild after a one-line edit to a workspace source file"),
    ("test-build", "cold", &["test", "--no-run"], "build all test targets from an empty build directory"),
    ("release", "cold", &["build", "--release"], "optimized build from an empty build directory"),
];

const MARKER: &str = ".haul-owned";
const EDIT_TAG: &str = "// haul-benchmark-edit ";

#[derive(Debug, Clone)]
pub struct Options {
    pub scenario: String,
    pub reps: u32,
    pub warmups: u32,
    pub edit: Option<PathBuf>,
    pub allow_mutation: bool,
    pub timeout: Option<Duration>,
    /// Cargo arguments; empty means the scenario's default command.
    pub cargo_args: Vec<OsString>,
    /// Ephemeral `--config KEY=VALUE` overlays (experiments).
    pub overlays: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Distinguishes build directories of baseline and candidate runs.
    pub label: String,
}

impl Options {
    pub fn new(scenario: &str) -> Options {
        Options { scenario: scenario.to_string(), reps: 3, warmups: 1, edit: None, allow_mutation: false, timeout: None, cargo_args: Vec::new(), overlays: Vec::new(), env: Vec::new(), label: "default".into() }
    }
}

/// A synthetic edit that restores the original bytes when dropped (FR-020.2).
struct Mutation {
    file: PathBuf,
    original: Vec<u8>,
    journal: PathBuf,
    backup: PathBuf,
    restored: bool,
}

impl Mutation {
    fn begin(file: &Path, journal_dir: &Path, id: &str) -> Result<Mutation, String> {
        let original = fs::read(file).map_err(|e| format!("cannot read {}: {}", file.display(), e))?;
        util::create_private_dir(journal_dir).map_err(|e| format!("cannot create {}: {}", journal_dir.display(), e))?;
        let backup = journal_dir.join(format!("{}.orig", id));
        let journal = journal_dir.join(format!("{}.json", id));
        // The journal is durable before the first byte of the source file changes.
        util::write_atomic(&backup, &original).map_err(|e| format!("cannot write backup: {}", e))?;
        util::write_json(&journal, &json!({ "file": path_str(file), "backup": path_str(&backup), "sha256": util::sha256_hex(&original) })).map_err(|e| format!("cannot write restore journal: {}", e))?;
        Ok(Mutation { file: file.to_path_buf(), original, journal, backup, restored: false })
    }

    fn apply(&self, n: u32) -> Result<(), String> {
        let mut bytes = self.original.clone();
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(format!("{}{}\n", EDIT_TAG, n).as_bytes());
        fs::write(&self.file, bytes).map_err(|e| format!("cannot edit {}: {}", self.file.display(), e))
    }

    fn restore(&mut self) -> Result<(), String> {
        if self.restored {
            return Ok(());
        }
        fs::write(&self.file, &self.original).map_err(|e| format!("could not write {}: {}", self.file.display(), e))?;
        let back = fs::read(&self.file).map_err(|e| e.to_string())?;
        if back != self.original {
            return Err(format!("{} does not match its original content after restore", self.file.display()));
        }
        self.restored = true;
        let _ = fs::remove_file(&self.journal);
        let _ = fs::remove_file(&self.backup);
        Ok(())
    }
}

impl Drop for Mutation {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            eprintln!("haul: ERROR: SOURCE FILE NOT RESTORED: {}\n  the original content is saved at {}", e, self.backup.display());
        }
    }
}

/// Restores edits left behind by a benchmark that was killed before it could clean up.
/// A file is only rewritten when it is exactly the original plus Haul's marker lines.
pub fn recover_interrupted(store: &WsStore) -> Result<Vec<String>, String> {
    let dir = store.dir.join("restore");
    let mut restored = Vec::new();
    let Ok(rd) = fs::read_dir(&dir) else { return Ok(restored) };
    for e in rd.filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false)) {
        let journal = util::read_json(&e.path()).ok_or_else(|| format!("restore journal {} is unreadable; inspect it manually", e.path().display()))?;
        let (file, backup) = (PathBuf::from(util::vstr(&journal, "file")), PathBuf::from(util::vstr(&journal, "backup")));
        let original = fs::read(&backup).map_err(|err| format!("backup {} for {} is missing: {}", backup.display(), file.display(), err))?;
        let current = fs::read(&file).map_err(|err| format!("cannot read {}: {}", file.display(), err))?;
        if current != original {
            let mut base = original.clone();
            if !base.ends_with(b"\n") {
                base.push(b'\n');
            }
            let only_markers = current.starts_with(&base) && String::from_utf8_lossy(&current[base.len()..]).lines().all(|l| l.starts_with(EDIT_TAG));
            if !only_markers {
                return Err(format!(
                    "{} was edited by an interrupted benchmark and has changed since; it was NOT restored automatically.\n  original content: {}\n  compare the two, fix the file, then delete {}",
                    file.display(),
                    backup.display(),
                    e.path().display()
                ));
            }
            fs::write(&file, &original).map_err(|err| format!("cannot restore {}: {}", file.display(), err))?;
            restored.push(path_str(&file));
        }
        let _ = fs::remove_file(e.path());
        let _ = fs::remove_file(&backup);
    }
    Ok(restored)
}

fn owned_dir(target_dir: &Path, scenario: &str, label: &str) -> PathBuf {
    target_dir.join("haul-bench").join(format!("{}-{}", scenario, label))
}

/// Empties a benchmark build directory, refusing anything Haul did not create.
fn reset_owned_dir(dir: &Path) -> Result<(), String> {
    match fs::symlink_metadata(dir) {
        Ok(md) => {
            if md.file_type().is_symlink() || !md.is_dir() {
                return Err(format!("{} is not a directory Haul created; refusing to delete it", dir.display()));
            }
            if !dir.join(MARKER).is_file() {
                return Err(format!("{} lacks the {} marker; refusing to delete a directory Haul does not own", dir.display(), MARKER));
            }
            fs::remove_dir_all(dir).map_err(|e| format!("cannot clear {}: {}", dir.display(), e))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot inspect {}: {}", dir.display(), e)),
    }
    fs::create_dir_all(dir).and_then(|_| fs::write(dir.join(MARKER), "benchmark build directory created by haul; safe to delete\n")).map_err(|e| format!("cannot create {}: {}", dir.display(), e))
}

fn confirm_mutation(file: &Path, opts: &Options, cfg: &Config) -> Result<(), String> {
    if opts.allow_mutation || cfg.bool("benchmark.allow-mutation") {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!("the small-edit scenario temporarily edits {}; pass --allow-mutation or set benchmark.allow-mutation = true to permit it non-interactively", file.display()));
    }
    eprint!("haul: the small-edit scenario will temporarily append a comment to {} and restore it afterwards. Proceed? [y/N] ", file.display());
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
    if matches!(answer.trim(), "y" | "Y" | "yes") {
        Ok(())
    } else {
        Err("source mutation declined".into())
    }
}

/// The file to edit: the requested one, or the first workspace member's library or binary root.
fn edit_target(opts: &Options, meta: &Value, root: &Path) -> Result<PathBuf, String> {
    let file = match &opts.edit {
        Some(f) => std::env::current_dir().map_err(|e| e.to_string())?.join(f),
        None => {
            let members: Vec<&str> = meta["workspace_default_members"].as_array().or(meta["workspace_members"].as_array()).into_iter().flatten().filter_map(Value::as_str).collect();
            let pkgs = meta["packages"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let pick = |kind: &str| {
                members.iter().find_map(|m| {
                    pkgs.iter().find(|p| p["id"].as_str() == Some(m)).and_then(|p| p["targets"].as_array()?.iter().find(|t| t["kind"].as_array().map(|k| k.iter().any(|x| x == kind)).unwrap_or(false))?["src_path"].as_str().map(PathBuf::from))
                })
            };
            pick("lib").or_else(|| pick("bin")).ok_or("no library or binary target found to edit; pass --edit <file>")?
        }
    };
    let canon = fs::canonicalize(&file).map_err(|e| format!("cannot edit {}: {}", file.display(), e))?;
    // Mutations are confined to the workspace (FR-020 validation).
    if !canon.starts_with(util::canon(root)) {
        return Err(format!("{} is outside the workspace {}; refusing to edit it", canon.display(), root.display()));
    }
    if canon.extension().map(|e| e != "rs").unwrap_or(true) {
        return Err(format!("{} is not a Rust source file; the synthetic edit is a `//` comment", canon.display()));
    }
    Ok(canon)
}

struct RunResult {
    record: Value,
    summary: Option<Value>,
}

fn one_run(opts: &Options, args: &[OsString], env: &[(String, String)], scenario: &Value, root: &Path, skip: &[PathBuf]) -> Result<RunResult, String> {
    let before = discover::source_snapshot(root, skip);
    let mut spec = Spec::new(args.to_vec(), Mode::Explicit);
    spec.env = env.to_vec();
    spec.scenario = Some(scenario.clone());
    spec.force_finalize = true;
    let done = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let watchdog = opts.timeout.map(|limit| {
        let (done, timed_out) = (done.clone(), timed_out.clone());
        std::thread::spawn(move || {
            let deadline = Instant::now() + limit;
            while !done.load(Ordering::SeqCst) {
                if Instant::now() >= deadline {
                    timed_out.store(true, Ordering::SeqCst);
                    crate::proc::terminate_child();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    });
    let started = Instant::now();
    let outcome = session::execute(&spec);
    done.store(true, Ordering::SeqCst);
    if let Some(w) = watchdog {
        let _ = w.join();
    }
    let outcome = outcome?;
    let elapsed = started.elapsed().as_nanos() as u64;
    let after = discover::source_snapshot(root, skip);
    let mut invalid: Option<String> = None;
    if timed_out.load(Ordering::SeqCst) {
        invalid = Some(format!("timed out after {}", human_dur(opts.timeout.map(|t| t.as_nanos() as u64).unwrap_or(0))));
    } else if crate::proc::interrupted().is_some() {
        invalid = Some("interrupted".into());
    } else if !outcome.exit.success() {
        invalid = Some(match outcome.exit.code {
            Some(c) => format!("cargo exited with status {}", c),
            None => format!("cargo was terminated by signal {}", outcome.exit.signal.unwrap_or(0)),
        });
    } else if before != after {
        // Someone else changed the tree while we measured (FR-020.3).
        invalid = Some("source files changed during the measurement".into());
    }
    let summary = outcome.report.as_ref().map(compare::summarize_run);
    if summary.is_none() && invalid.is_none() {
        invalid = Some("the run could not be analysed".into());
    }
    Ok(RunResult {
        record: json!({
            "run_id": outcome.session_id, "phase": scenario["phase"], "iteration": scenario["iteration"], "wall_time_ns": elapsed,
            "exit_code": outcome.exit.code, "signal": outcome.exit.signal, "valid": invalid.is_none(), "invalid_reason": invalid,
        }),
        summary,
    })
}

/// Runs one scenario and stores the result. `Err` means the benchmark could not run at all.
pub fn run(opts: &Options, store: &WsStore, cfg: &Config) -> Result<Value, String> {
    let (_, cache_state, default_args, _) = SCENARIOS.iter().find(|(n, ..)| *n == opts.scenario).ok_or_else(|| format!("unknown scenario `{}` (available: {})", opts.scenario, SCENARIOS.iter().map(|s| s.0).collect::<Vec<_>>().join(", ")))?;
    if opts.reps == 0 {
        return Err("--reps must be at least 1".into());
    }
    let root = store.root.clone().ok_or("benchmarks need a Cargo workspace; run this inside one")?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    for r in recover_interrupted(store)? {
        eprintln!("haul: warning: restored {} (left edited by an interrupted benchmark)", r);
    }
    let mut args: Vec<OsString> = if opts.cargo_args.is_empty() { default_args.iter().map(OsString::from).collect() } else { opts.cargo_args.clone() };
    let inv = crate::resolve::classify(&args);
    if inv.target_dir.is_some() {
        return Err("benchmarks manage their own build directory; remove --target-dir".into());
    }
    if !inv.family.builds() {
        return Err(format!("`{}` is not a build command; benchmarks measure build, check, test, bench, run, doc, rustc or clippy", inv.command.unwrap_or_default()));
    }
    for o in opts.overlays.iter().rev() {
        args.insert(0, o.into());
        args.insert(0, "--config".into());
    }
    let meta = discover::metadata(cfg, &cwd, inv.toolchain.as_deref(), inv.manifest_path.as_deref())?;
    let target_dir = PathBuf::from(meta.value["target_directory"].as_str().ok_or("cargo metadata did not report a target directory")?);
    let bench_dir = owned_dir(&target_dir, &opts.scenario, &opts.label);
    reset_owned_dir(&bench_dir)?;
    let mut env = opts.env.clone();
    env.push(("CARGO_TARGET_DIR".into(), path_str(&bench_dir)));
    env.push(("CARGO_BUILD_BUILD_DIR".into(), path_str(&bench_dir)));
    let skip = vec![target_dir.clone()];

    let id = util::new_id();
    let _signals = crate::proc::hold_signals();
    let mut mutation = if opts.scenario == "small-edit" {
        let file = edit_target(opts, &meta.value, &root)?;
        confirm_mutation(&file, opts, cfg)?;
        Some(Mutation::begin(&file, &store.dir.join("restore"), &id)?)
    } else {
        None
    };
    let label = |phase: &str, i: u32| json!({ "name": opts.scenario, "cache_state": cache_state, "phase": phase, "iteration": i, "benchmark": id, "label": opts.label, "overlays": opts.overlays });
    let mut records: Vec<Value> = Vec::new();
    let mut measured: Vec<Value> = Vec::new();
    let mut state = "complete";
    let mut failure: Option<String> = None;
    let overall = Instant::now();
    let cold = *cache_state == "cold";

    let mut phases: Vec<(&str, u32)> = Vec::new();
    if !cold {
        // Warm scenarios start from a fully built tree; the priming build is never a sample.
        phases.push(("prime", 1));
    }
    phases.extend((1..=opts.warmups).map(|i| ("warmup", i)));
    phases.extend((1..=opts.reps).map(|i| ("measured", i)));
    let mut edit_counter = 0u32;
    for (phase, i) in phases {
        if crate::proc::interrupted().is_some() {
            state = "interrupted";
            break;
        }
        let total = if phase == "measured" { opts.reps } else { opts.warmups.max(1) };
        eprintln!("haul: benchmark {} · {} {}/{} · elapsed {}", opts.scenario, phase, i, total, human_dur(overall.elapsed().as_nanos() as u64));
        if cold {
            reset_owned_dir(&bench_dir)?;
        }
        if let (Some(m), true) = (&mutation, phase != "prime") {
            edit_counter += 1;
            m.apply(edit_counter)?;
        }
        let r = one_run(opts, &args, &env, &label(phase, i), &root, &skip)?;
        let valid = r.record["valid"] == json!(true);
        if !valid && phase != "measured" {
            failure = Some(format!("{} run failed: {}", phase, util::vstr(&r.record, "invalid_reason")));
        }
        if let (true, "measured", Some(s)) = (valid, phase, r.summary) {
            measured.push(s);
        }
        records.push(r.record);
        if failure.is_some() {
            state = "invalid";
            break;
        }
    }
    if crate::proc::interrupted().is_some() {
        state = "interrupted";
    }
    // Restore before anything else can fail; a restore problem is the most important thing to report.
    let restore_error = mutation.as_mut().and_then(|m| m.restore().err());
    if let Some(e) = &restore_error {
        state = "invalid";
        eprintln!("haul: ERROR: SOURCE FILE NOT RESTORED: {}", e);
    }
    let (group, excluded) = compare::homogeneous_group(measured);
    for s in &excluded {
        eprintln!("haul: warning: run {} used a different toolchain or machine class and is excluded from the sample group", util::vstr(s, "id"));
    }
    let invalid_measured = records.iter().filter(|r| r["phase"] == json!("measured") && r["valid"] != json!(true)).count();
    if state == "complete" && group.is_empty() {
        state = "invalid";
        failure = Some("no valid measured run".into());
    }
    let summary = compare::summarize_group(&id, &json!({ "name": opts.scenario, "cache_state": cache_state }), &group);
    let result = json!({
        "schema_version": crate::SCHEMA_VERSION,
        "id": id,
        "kind": "benchmark",
        "state": state,
        "failure": failure,
        "scenario": { "name": opts.scenario, "cache_state": cache_state, "label": opts.label, "overlays": opts.overlays, "env": opts.env.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>() },
        "command": std::iter::once("cargo".to_string()).chain(util::redact_args(&args.iter().map(|a| util::lossy(a)).collect::<Vec<_>>())).collect::<Vec<_>>(),
        "build_directory": path_str(&bench_dir),
        "requested": { "repetitions": opts.reps, "warmups": opts.warmups, "timeout_ns": opts.timeout.map(|t| t.as_nanos() as u64) },
        "runs": records,
        "samples_valid": group.len(),
        "samples_discarded": invalid_measured + excluded.len(),
        "excluded_heterogeneous": excluded.iter().map(|s| json!({ "run_id": s["id"], "rustc_version": s["environment"]["rustc_version"], "machine_class": s["environment"]["machine_class"] })).collect::<Vec<_>>(),
        "mutation": mutation.as_ref().map(|m| json!({ "file": path_str(&m.file), "restored": m.restored, "restore_error": restore_error })),
        "summary": summary,
    });
    util::write_json(&store.benchmarks_dir().join(format!("{}.json", id)), &result).map_err(|e| format!("could not store the benchmark result: {}", e))?;
    Ok(result)
}

#[derive(Debug, Clone, Default)]
pub struct Experiment {
    pub name: String,
    pub overlays: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Cargo arguments of a verification command run with the candidate settings.
    pub verify: Option<Vec<OsString>>,
}

/// What a setting trades away at run time or for debugging, stated next to its build effect (FR-080).
fn runtime_risk(overlay: &str) -> &'static str {
    let key = overlay.split('=').next().unwrap_or("");
    if key.contains("codegen-units") {
        "more codegen units parallelise code generation but can reduce generated-code performance; fewer do the opposite"
    } else if key.contains("opt-level") {
        "lower optimization shortens builds and slows the produced code"
    } else if key.contains("lto") {
        "less link-time optimization shortens linking and can reduce runtime performance and increase binary size"
    } else if key.ends_with("debug") || key.contains("debuginfo") || key.contains("split-debuginfo") || key.contains("strip") {
        "no effect on generated code; reduces what a debugger can show (variables, types, or symbols depending on the setting)"
    } else if key.contains("linker") || key.contains("rustflags") {
        "a different linker or flags must produce a working artifact; only functional verification establishes that"
    } else if key.contains("incremental") {
        "incremental compilation can inhibit optimizations; it is not recommended for release builds"
    } else {
        "runtime effect of this setting is not characterised by Haul; verify with the project's own tests and benchmarks"
    }
}

/// Baseline versus candidate under the same scenario, with optional functional verification.
pub fn experiment(exp: &Experiment, base_opts: &Options, store: &WsStore, cfg: &Config) -> Result<Value, String> {
    if exp.overlays.is_empty() && exp.env.is_empty() {
        return Err("an experiment needs at least one candidate setting (--set KEY=VALUE, --env KEY=VALUE, --linker, --codegen-units, --debug)".into());
    }
    for o in &exp.overlays {
        if o.parse::<toml::Table>().is_err() {
            return Err(format!("--set {:?} is not a valid Cargo `--config` assignment (TOML `key = value`)", o));
        }
    }
    let mut base = base_opts.clone();
    base.label = "baseline".into();
    eprintln!("haul: experiment `{}`: measuring baseline", exp.name);
    let baseline = run(&base, store, cfg)?;
    if baseline["state"] != json!("complete") {
        return Err(format!("the baseline benchmark did not complete ({}); nothing to compare against", baseline["failure"].as_str().unwrap_or(baseline["state"].as_str().unwrap_or("unknown"))));
    }
    let mut cand = base_opts.clone();
    cand.label = format!("candidate-{}", exp.name);
    cand.overlays = exp.overlays.clone();
    cand.env.extend(exp.env.iter().cloned());
    eprintln!("haul: experiment `{}`: measuring candidate", exp.name);
    let candidate = run(&cand, store, cfg)?;

    // Functional verification with the candidate settings (FR-080.2).
    let verification = match (&exp.verify, candidate["state"] == json!("complete")) {
        (Some(verify_args), true) => {
            eprintln!("haul: experiment `{}`: verifying candidate", exp.name);
            let mut args: Vec<OsString> = Vec::new();
            for o in &exp.overlays {
                args.push("--config".into());
                args.push(o.into());
            }
            args.extend(verify_args.iter().cloned());
            let mut spec = Spec::new(args, Mode::Explicit);
            spec.env = cand.env.clone();
            for var in ["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"] {
                spec.env.push((var.into(), util::vstr(&candidate, "build_directory")));
            }
            spec.scenario = Some(json!({ "name": "verification", "experiment": exp.name }));
            let out = session::execute(&spec)?;
            json!({ "performed": true, "passed": out.exit.success(), "exit_code": out.exit.code, "command": verify_args.iter().map(|a| util::lossy(a)).collect::<Vec<_>>() })
        }
        (Some(_), false) => json!({ "performed": false, "reason": "the candidate did not build" }),
        (None, _) => json!({ "performed": false, "reason": "no --verify command was given" }),
    };

    let comparison = compare::compare(&baseline["summary"], &candidate["summary"]);
    let wall = comparison["metrics"].as_array().and_then(|m| m.iter().find(|r| r["metric"] == json!("wall_time_ns"))).cloned().unwrap_or(Value::Null);
    let delta = wall["delta_percent"].as_f64();
    let noise = [&baseline, &candidate].iter().filter_map(|b| b["summary"]["distributions"]["wall_time_ns"]["relative_stddev_percent"].as_f64()).fold(0.0f64, f64::max);
    // A verdict needs a built candidate, a comparable pair, and — to be "verified" — a passing check.
    let candidate_built = candidate["state"] == json!("complete");
    let verification_failed = verification["performed"] == json!(true) && verification["passed"] != json!(true);
    let incomparable = comparison["comparability"]["state"] == json!("incomparable");
    let within_noise = delta.map(|d| d.abs() <= noise.max(1.0)).unwrap_or(true);
    let status = if !candidate_built || verification_failed {
        "invalid"
    } else if incomparable || within_noise {
        "inconclusive"
    } else if verification["passed"] == json!(true) {
        "verified"
    } else {
        "measured-unverified"
    };
    let reason = match status {
        "invalid" if !candidate_built => "the candidate settings did not build".to_string(),
        "invalid" => "the verification command failed with the candidate settings; the candidate is rejected regardless of build time".to_string(),
        "inconclusive" if incomparable => "baseline and candidate ran in incomparable environments".to_string(),
        "inconclusive" => format!("the difference ({:+.1}%) is within run-to-run noise (±{:.1}%); collect more samples with --reps", delta.unwrap_or(0.0), noise.max(1.0)),
        "verified" => "measured and functionally verified".to_string(),
        _ => "measured, but not functionally verified; rerun with --verify before adopting it".to_string(),
    };
    let result = json!({
        "schema_version": crate::SCHEMA_VERSION,
        "id": util::new_id(),
        "kind": "experiment",
        "name": exp.name,
        "status": status,
        "reason": reason,
        "candidate_settings": {
            "cargo_config": exp.overlays,
            "environment": exp.env.iter().map(|(k, v)| format!("{}={}", k, if util::is_secret_name(k) { "<redacted>" } else { v })).collect::<Vec<_>>(),
            "isolation": "applied through `cargo --config` and the environment for these runs only; no file in the repository was changed",
        },
        "dimensions": {
            "build": { "wall_time_delta_percent": delta, "noise_percent": noise, "faster": delta.map(|d| d < 0.0) },
            "runtime_and_debugging": exp.overlays.iter().map(|o| json!({ "setting": o, "risk": runtime_risk(o) })).chain(exp.env.iter().map(|(k, _)| json!({ "setting": k, "risk": runtime_risk(&k.to_ascii_lowercase().replace('_', "-")) }))).collect::<Vec<_>>(),
            "output_size_delta_bytes": comparison["metrics"].as_array().and_then(|m| m.iter().find(|r| r["metric"] == json!("output_bytes_written"))).map(|r| r["delta"].clone()),
        },
        "verification": verification,
        "comparison": comparison,
        "baseline_benchmark": baseline["id"],
        "candidate_benchmark": candidate["id"],
        "proposed_config": if status == "verified" || status == "measured-unverified" {
            json!({ "file": ".cargo/config.toml or Cargo.toml (review required)", "settings": exp.overlays, "note": "profile, linker and flag changes are never applied automatically (FR-170)" })
        } else {
            Value::Null
        },
    });
    util::write_json(&store.benchmarks_dir().join(format!("{}.json", util::vstr(&result, "id"))), &result).map_err(|e| format!("could not store the experiment result: {}", e))?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("haul-bench-{}-{}", name, util::new_id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn mutation_restores_exact_bytes_on_drop() {
        let dir = tmp("mut");
        let file = dir.join("lib.rs");
        // No trailing newline: restoring must not "tidy" the file.
        fs::write(&file, b"pub fn f() {}").unwrap();
        {
            let m = Mutation::begin(&file, &dir.join("restore"), "b1").unwrap();
            m.apply(1).unwrap();
            let edited = fs::read_to_string(&file).unwrap();
            assert!(edited.starts_with("pub fn f() {}\n") && edited.contains(EDIT_TAG));
            assert!(dir.join("restore/b1.json").exists(), "the journal exists while the file is edited");
        }
        assert_eq!(fs::read(&file).unwrap(), b"pub fn f() {}");
        assert!(!dir.join("restore/b1.json").exists());
        fs::remove_dir_all(&dir).ok();
    }

    fn store_at(dir: &Path) -> WsStore {
        WsStore { id: "t".into(), dir: dir.to_path_buf(), root: None }
    }

    #[test]
    fn recovery_restores_after_a_kill_but_refuses_when_the_file_diverged() {
        let dir = tmp("recover");
        let file = dir.join("lib.rs");
        fs::write(&file, "fn a() {}\n").unwrap();
        // Simulate a benchmark killed mid-run: journal written, file edited, no cleanup.
        let m = Mutation::begin(&file, &dir.join("restore"), "b1").unwrap();
        m.apply(3).unwrap();
        std::mem::forget(m);
        assert_eq!(recover_interrupted(&store_at(&dir)).unwrap(), vec![path_str(&file)]);
        assert_eq!(fs::read_to_string(&file).unwrap(), "fn a() {}\n");

        let m = Mutation::begin(&file, &dir.join("restore"), "b2").unwrap();
        m.apply(1).unwrap();
        std::mem::forget(m);
        fs::write(&file, "fn a() {}\nfn user_work() {}\n").unwrap();
        let err = recover_interrupted(&store_at(&dir)).unwrap_err();
        assert!(err.contains("NOT restored"), "{}", err);
        assert_eq!(fs::read_to_string(&file).unwrap(), "fn a() {}\nfn user_work() {}\n", "user edits are never overwritten");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_owned_directories_are_cleared() {
        let dir = tmp("own");
        let foreign = dir.join("foreign");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("keep"), "x").unwrap();
        assert!(reset_owned_dir(&foreign).unwrap_err().contains("refusing"));
        assert!(foreign.join("keep").exists());
        let owned = dir.join("owned");
        reset_owned_dir(&owned).unwrap();
        fs::write(owned.join("junk"), "x").unwrap();
        reset_owned_dir(&owned).unwrap();
        assert!(!owned.join("junk").exists() && owned.join(MARKER).exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_target_is_confined_to_the_workspace() {
        let dir = tmp("confine");
        let ws = dir.join("ws");
        fs::create_dir_all(ws.join("src")).unwrap();
        fs::write(ws.join("src/lib.rs"), "").unwrap();
        fs::write(dir.join("outside.rs"), "").unwrap();
        let mut o = Options::new("small-edit");
        o.edit = Some(dir.join("outside.rs"));
        assert!(edit_target(&o, &json!({}), &ws).unwrap_err().contains("outside the workspace"));
        o.edit = Some(ws.join("src/lib.rs"));
        assert!(edit_target(&o, &json!({}), &ws).is_ok());
        fs::remove_dir_all(&dir).ok();
    }
}
