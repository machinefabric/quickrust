//! Turns one recorded session into an immutable run report (DATA-001):
//! build units, the observed dependency DAG, the critical path, and the
//! waste analyses layered on top.
//!
//! Everything here is derived from evidence: rustc invocations seen by the
//! wrapper, Cargo's own metadata and messages, and files Cargo left behind.
//! What cannot be derived is reported as unavailable, never as zero (DATA-006).

use crate::config::Config;
use crate::discover;
use crate::inventory;
use crate::store::WsStore;
use crate::util::{self, path_str, Tokenizer};
use crate::waste;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// `-C` options that belong to the Cargo profile rather than to ad-hoc flags.
const PROFILE_KEYS: &[&str] = &[
    "opt-level", "debuginfo", "codegen-units", "lto", "panic", "debug-assertions", "overflow-checks", "strip", "split-debuginfo", "embed-bitcode", "linker-plugin-lto", "prefer-dynamic", "rpath",
];

#[derive(Debug, Clone, Default, Serialize)]
pub struct Fingerprint {
    pub compiler: String,
    pub target: String,
    pub profile: String,
    pub features: String,
    pub rustflags: String,
    pub environment: String,
    pub source: String,
    pub generated: String,
    pub dependencies: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Unit {
    pub id: String,
    pub stem: String,
    pub package: String,
    pub version: String,
    /// `path`, `registry`, `git` or `unknown`.
    pub source: String,
    pub manifest_dir: String,
    pub crate_name: String,
    /// `lib`, `bin`, `test`, `example`, `proc-macro`, `build-script`, `build-script-run`, `cdylib`, …
    pub kind: String,
    pub crate_types: Vec<String>,
    /// `check`, `build`, `test` or `run` (a build script execution).
    pub mode: String,
    pub target: Option<String>,
    /// `host`, `target` or `unknown`.
    pub role: String,
    pub primary: bool,
    pub features: Vec<String>,
    /// `compiled`, `restored` (from Haul's artifact cache), `failed`, `fresh` (referenced but not
    /// rebuilt) or `ran` (a build script execution).
    pub freshness: String,
    pub start_wall_ns: Option<u64>,
    pub duration_ns: Option<u64>,
    /// Offset from start at which metadata became available to pipelined dependents.
    pub rmeta_ready_ns: Option<u64>,
    pub cpu_ns: Option<u64>,
    pub max_rss_bytes: Option<u64>,
    pub output_bytes: Option<u64>,
    pub out_dir: String,
    pub build_out_dir: Option<String>,
    pub incremental: bool,
    /// Produces a linked artifact (binary, test, proc-macro, dynamic library, build script).
    pub links: bool,
    pub codegen: BTreeMap<String, String>,
    pub linker: Option<String>,
    /// sccache disposition class, by sccache's documented rules; `n/a` without sccache.
    pub cacheability: String,
    pub on_critical_path: bool,
    pub fingerprint: Fingerprint,
    pub exact_id: String,
    pub portable_id: String,
    pub logical_key: String,
    #[serde(skip)]
    pub externs: Vec<(String, String, bool)>,
    /// Local input files: path → (content digest or size marker, mtime).
    #[serde(skip)]
    pub inputs: BTreeMap<String, (String, u64)>,
    #[serde(skip)]
    pub generated_inputs: BTreeMap<String, (String, u64)>,
    #[serde(skip)]
    pub env_deps: Vec<String>,
}

impl Unit {
    /// Part of this build's output: compiled now or restored from the artifact cache.
    pub fn built(&self) -> bool {
        matches!(self.freshness.as_str(), "compiled" | "restored")
    }
}

#[derive(Debug, Clone)]
pub struct Dep {
    pub idx: usize,
    pub meta_only: bool,
    pub confidence: &'static str,
    pub evidence: &'static str,
}

pub struct Graph {
    pub units: Vec<Unit>,
    pub deps: Vec<Vec<Dep>>,
    pub unresolved_externs: u64,
}

fn source_kind(manifest_dir: &str) -> &'static str {
    if manifest_dir.is_empty() {
        "unknown"
    } else if manifest_dir.contains("/registry/src/") || manifest_dir.contains("\\registry\\src\\") {
        "registry"
    } else if manifest_dir.contains("/git/checkouts/") || manifest_dir.contains("\\git\\checkouts\\") {
        "git"
    } else {
        "path"
    }
}

fn input_map(v: &Value) -> BTreeMap<String, (String, u64)> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let digest = f["sha"].as_str().map(str::to_string).unwrap_or_else(|| format!("size:{}", f["bytes"].as_u64().unwrap_or(0)));
            Some((f["path"].as_str()?.to_string(), (digest, f["mtime_ns"].as_u64().unwrap_or(0))))
        })
        .collect()
}

/// Builds a unit from one wrapper `compile` event by reading rustc's own arguments.
///
/// Returns `None` for compiler invocations that are not Cargo build units: build scripts
/// run rustc themselves (through the same wrapper) to probe compiler features. Cargo passes
/// `-C metadata=…` for every unit it schedules; a probe never has it.
pub fn parse_unit(ev: &Value, sccache_active: bool, tok: &Tokenizer) -> Option<Unit> {
    let args: Vec<&str> = ev["args"].as_array()?.iter().filter_map(Value::as_str).collect();
    let mut u = Unit { role: "unknown".into(), ..Default::default() };
    let mut extra = String::new();
    let mut emit = String::new();
    let mut edition = String::new();
    let mut test = false;
    let mut cargo_unit = false;
    let mut profile_parts: Vec<String> = Vec::new();
    let mut flag_parts: Vec<String> = Vec::new();
    let mut feature_decl: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        i += 1;
        // Normalise `--name=value`, `--name value`, `-Xvalue` and `-X value`.
        let (name, inline): (&str, Option<&str>) = if let Some(rest) = a.strip_prefix("--") {
            match rest.split_once('=') {
                Some((n, v)) => (&a[..n.len() + 2], Some(v)),
                None => (a, None),
            }
        } else if a.len() > 2 && a.starts_with('-') && !a.starts_with("--") {
            (&a[..2], Some(&a[2..]))
        } else {
            (a, None)
        };
        let value = |i: &mut usize| -> String {
            match inline {
                Some(v) => v.to_string(),
                None => {
                    let v = args.get(*i).copied().unwrap_or("");
                    *i += 1;
                    v.to_string()
                }
            }
        };
        match name {
            "--crate-name" => u.crate_name = value(&mut i),
            "--crate-type" => u.crate_types.extend(value(&mut i).split(',').map(str::to_string)),
            "--out-dir" => u.out_dir = value(&mut i),
            "--target" => u.target = Some(value(&mut i)),
            "--edition" => edition = value(&mut i),
            "--emit" => emit = value(&mut i),
            "--test" => test = true,
            "--cfg" => {
                let c = value(&mut i);
                match c.strip_prefix("feature=") {
                    Some(f) => u.features.push(f.trim_matches('"').to_string()),
                    None => flag_parts.push(format!("--cfg {}", c)),
                }
            }
            "--check-cfg" => feature_decl.push(value(&mut i)),
            "--extern" => {
                let e = value(&mut i);
                if let Some((left, path)) = e.split_once('=') {
                    let name = left.rsplit(':').next().unwrap_or(left).to_string();
                    let file = Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    u.externs.push((name, inventory::artifact_stem(&file).to_string(), file.ends_with(".rmeta")));
                }
            }
            "-C" => {
                let c = value(&mut i);
                let (k, v) = c.split_once('=').map(|(k, v)| (k, v.to_string())).unwrap_or((c.as_str(), "true".to_string()));
                match k {
                    "extra-filename" => extra = v,
                    "metadata" => cargo_unit = true,
                    "incremental" => {
                        u.incremental = true;
                        profile_parts.push("incremental=on".into());
                    }
                    "linker" => {
                        u.linker = Some(Path::new(&v).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(v.clone()));
                        flag_parts.push(format!("-C linker={}", tok.apply(&v)));
                    }
                    "link-arg" | "link-args" => {
                        if let Some(l) = v.split_whitespace().find_map(|t| t.strip_prefix("-fuse-ld=")) {
                            u.linker = Some(l.to_string());
                        }
                        flag_parts.push(format!("-C {}={}", k, tok.apply(&v)));
                    }
                    _ if PROFILE_KEYS.contains(&k) => {
                        u.codegen.insert(k.to_string(), v.clone());
                        profile_parts.push(format!("{}={}", k, v));
                    }
                    _ => {
                        u.codegen.insert(k.to_string(), v.clone());
                        flag_parts.push(format!("-C {}={}", k, tok.apply(&v)));
                    }
                }
            }
            // Diagnostics plumbing and search paths never change what is compiled.
            "-L" | "--cap-lints" | "--error-format" | "--json" | "--diagnostic-width" | "--color" => {
                value(&mut i);
            }
            "-W" | "-A" | "-D" | "-F" | "-Z" | "--remap-path-prefix" | "--sysroot" | "--codegen" | "-l" => {
                let v = value(&mut i);
                // Fast mode's parallel frontend and its stable feature gate change how rustc
                // works, not what it builds.
                if !(name == "-Z" && (v.starts_with("threads") || v.starts_with("allow-features"))) {
                    flag_parts.push(format!("{} {}", name, tok.apply(&v)));
                }
            }
            "-O" | "-g" => profile_parts.push(a.to_string()),
            _ if a.starts_with('-') && a != "-" => flag_parts.push(tok.apply(a)),
            _ => {}
        }
    }
    if u.crate_name.is_empty() || !cargo_unit {
        return None;
    }
    let env = &ev["env"];
    u.package = util::vstr(env, "pkg_name");
    u.version = util::vstr(env, "pkg_version");
    u.manifest_dir = util::vstr(env, "manifest_dir");
    u.primary = env["primary"] == json!(true);
    u.build_out_dir = env["out_dir"].as_str().map(str::to_string);
    u.source = source_kind(&u.manifest_dir).to_string();
    u.stem = format!("{}{}", u.crate_name, extra);
    u.id = format!("unit:{}", u.stem);
    let cwd = util::vstr(ev, "cwd");
    u.out_dir = path_str(&Path::new(&cwd).join(&u.out_dir));
    let has = |t: &str| u.crate_types.iter().any(|c| c == t);
    let emits_link = emit.split(',').any(|e| e == "link");
    let in_build_dir = Path::new(&u.out_dir).parent().and_then(|p| p.file_name()).map(|n| n == "build").unwrap_or(false);
    u.kind = if u.crate_name.starts_with("build_script_") && in_build_dir {
        "build-script"
    } else if test {
        "test"
    } else if has("proc-macro") {
        "proc-macro"
    } else if has("bin") {
        if u.out_dir.ends_with("examples") {
            "example"
        } else {
            "bin"
        }
    } else if has("cdylib") {
        "cdylib"
    } else if has("dylib") {
        "dylib"
    } else if has("staticlib") {
        "staticlib"
    } else {
        "lib"
    }
    .to_string();
    u.mode = if !emits_link {
        "check"
    } else if test {
        "test"
    } else {
        "build"
    }
    .to_string();
    u.links = emits_link && matches!(u.kind.as_str(), "bin" | "example" | "test" | "proc-macro" | "cdylib" | "dylib" | "staticlib" | "build-script");
    if matches!(u.kind.as_str(), "proc-macro" | "build-script") {
        u.role = "host".into();
    }
    let ok = ev["exit_code"] == json!(0);
    u.freshness = match (ok, ev["cache"]["result"].as_str()) {
        (false, _) => "failed",
        (true, Some("hit")) => "restored",
        (true, _) => "compiled",
    }
    .into();
    u.start_wall_ns = ev["start_wall_ns"].as_u64();
    u.duration_ns = ev["dur_ns"].as_u64();
    u.rmeta_ready_ns = ev["rmeta_ready_ns"].as_u64();
    if ev["rusage"]["scope"] == json!("rustc") {
        u.cpu_ns = Some(ev["rusage"]["user_ns"].as_u64().unwrap_or(0) + ev["rusage"]["sys_ns"].as_u64().unwrap_or(0));
        u.max_rss_bytes = ev["rusage"]["max_rss_bytes"].as_u64();
    }
    u.output_bytes = ev["outputs"].as_array().map(|o| o.iter().filter(|f| !f["file"].as_str().unwrap_or("").ends_with(".d")).filter_map(|f| f["bytes"].as_u64()).sum());
    u.cacheability = if !sccache_active {
        "n/a".to_string()
    } else if u.links || has("bin") {
        "non-cacheable: crate type invokes the system linker".to_string()
    } else if u.incremental {
        "non-cacheable: incremental compilation is enabled".to_string()
    } else {
        "cacheable".to_string()
    };

    u.inputs = input_map(&ev["inputs"]["local"]);
    u.generated_inputs = input_map(&ev["inputs"]["generated"]);
    u.env_deps = ev["inputs"]["env_deps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| format!("{}={}", util::vstr(e, "name"), e["value_sha"].as_str().map(str::to_string).unwrap_or_else(|| if e["set"] == json!(true) { "<set>".into() } else { "<unset>".into() })))
        .collect();
    u.features.sort();
    profile_parts.sort();
    flag_parts.sort();
    feature_decl.sort();
    let mut dep_stems: Vec<&str> = u.externs.iter().map(|(_, s, _)| s.as_str()).collect();
    dep_stems.sort();
    let compiler = format!("{}|{}|{}|{}", util::vstr(ev, "rustc"), ev["rustc_id"]["bytes"], ev["rustc_id"]["mtime_ns"], util::vstr(env, "toolchain"));
    u.fingerprint = Fingerprint {
        compiler: util::digest_parts([compiler.as_str()]),
        target: u.target.clone().unwrap_or_else(|| "host".into()),
        profile: util::digest_parts(profile_parts.iter().map(String::as_str).chain([emit.as_str(), edition.as_str(), if test { "test" } else { "" }])),
        features: util::digest_parts(u.features.iter().chain(feature_decl.iter())),
        rustflags: util::digest_parts(&flag_parts),
        environment: util::digest_parts(&u.env_deps),
        source: util::digest_parts(u.inputs.iter().map(|(p, (d, _))| format!("{}={}", p, d))),
        generated: util::digest_parts(u.generated_inputs.iter().map(|(p, (d, _))| format!("{}={}", tok.apply(p), d))),
        dependencies: util::digest_parts(dep_stems),
    };
    let f = &u.fingerprint;
    let identity = [u.package.as_str(), u.version.as_str(), u.source.as_str(), u.crate_name.as_str(), u.kind.as_str(), u.mode.as_str()];
    // The logical key names "the same unit" across runs; local crates are tied to their directory.
    u.logical_key = util::digest_parts(identity.iter().copied().chain([if u.source == "path" { u.manifest_dir.as_str() } else { "" }, f.target.as_str()]));
    u.exact_id = util::digest_parts(identity.iter().copied().chain([
        u.manifest_dir.as_str(),
        f.compiler.as_str(),
        f.target.as_str(),
        f.profile.as_str(),
        f.features.as_str(),
        f.rustflags.as_str(),
        f.environment.as_str(),
        f.source.as_str(),
        f.generated.as_str(),
        f.dependencies.as_str(),
    ]));
    // Portable identity: equal across checkouts when Cargo assigned the same unit hash and the
    // inputs have the same content. Cargo hashes workspace members by their path relative to the
    // workspace root, so two worktrees share unit hashes even when their sources differ.
    u.portable_id = util::digest_parts(identity.iter().copied().chain([
        u.stem.as_str(),
        f.compiler.as_str(),
        f.target.as_str(),
        f.profile.as_str(),
        f.features.as_str(),
        f.rustflags.as_str(),
        f.source.as_str(),
        f.generated.as_str(),
    ]));
    Some(u)
}

/// Loads wrapper events for a session, oldest first. Returns `(compile events, probe count)`.
/// What a session's rustc-level events say, read one event at a time: a build of ten thousand
/// units has as many events, and only what is derived from them is kept.
#[derive(Default)]
pub struct Events {
    /// The compilations of units Cargo scheduled.
    pub units: Vec<Unit>,
    /// Compilers that build scripts ran to probe the toolchain: real cost, not units.
    pub script_probes: usize,
    pub script_probe_ns: u64,
    /// Toolchain queries the wrapper saw (`rustc -vV`, `--print`).
    pub queries: u64,
    /// The compiler of the first compilation.
    pub rustc: Option<String>,
    /// Artifact-cache results by kind (`hit`, `stored`, `miss`, `off`) and the time restores took.
    pub cache_results: HashMap<String, usize>,
    pub restore_ns: u64,
}

pub fn load_events(dir: &Path, sccache_active: bool, tok: &Tokenizer) -> Events {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
    names.sort();
    let mut out = Events::default();
    for p in names {
        if p.file_name().map(|n| n == "cargo-messages.json").unwrap_or(true) || p.extension().map(|e| e != "json").unwrap_or(true) {
            continue;
        }
        let Some(ev) = util::read_json(&p) else { continue };
        if ev["kind"] != json!("compile") {
            out.queries += 1;
            continue;
        }
        if out.rustc.is_none() {
            out.rustc = ev["rustc"].as_str().map(str::to_string);
        }
        if let Some(r) = ev["cache"]["result"].as_str() {
            *out.cache_results.entry(r.to_string()).or_default() += 1;
            if r == "hit" {
                out.restore_ns += ev["dur_ns"].as_u64().unwrap_or(0);
            }
        }
        match parse_unit(&ev, sccache_active, tok) {
            Some(u) => out.units.push(u),
            None => {
                out.script_probes += 1;
                out.script_probe_ns += ev["dur_ns"].as_u64().unwrap_or(0);
            }
        }
    }
    out
}

/// Facts about one build-script execution, read from what Cargo left on disk.
/// The script itself is never executed for analysis (SEC-022).
pub fn inspect_build_script_run(run_dir: &Path, session_start: u64, session_end: u64) -> Option<Value> {
    let invoked = std::fs::metadata(run_dir.join("invoked.timestamp")).ok()?;
    let started = util::mtime_ns(&invoked);
    // Cargo back-dates `output` to the invocation time, so the end of the run is taken
    // from the files it writes once the script has exited.
    let finished = ["stderr", "root-output"].iter().filter_map(|f| std::fs::metadata(run_dir.join(f)).ok()).map(|md| util::mtime_ns(&md)).max();
    // Only filesystems with whole-second timestamps need slack against our nanosecond clock.
    let margin = if started % 1_000_000_000 == 0 { 1_000_000_000 } else { 0 };
    let ran = started + margin >= session_start && started <= session_end + margin;
    let text = std::fs::read_to_string(run_dir.join("output")).unwrap_or_default();
    let (mut rerun_files, mut rerun_env, mut link, mut other) = (Vec::new(), Vec::new(), 0u64, 0u64);
    for line in text.lines() {
        let Some(directive) = line.strip_prefix("cargo::").or_else(|| line.strip_prefix("cargo:")) else { continue };
        let (key, value) = directive.split_once('=').unwrap_or((directive, ""));
        match key {
            "rerun-if-changed" => rerun_files.push(value.to_string()),
            "rerun-if-env-changed" => rerun_env.push(value.to_string()),
            k if k.starts_with("rustc-link") => link += 1,
            _ => other += 1,
        }
    }
    let out_bytes = inventory::scan_flat(&run_dir.join("out"), "build-script-output").and_then(|v| v["allocated_bytes"].as_u64());
    let duration = finished.filter(|f| *f > started).map(|f| f - started);
    Some(json!({
        "run_dir": path_str(run_dir),
        "ran_in_session": ran,
        "started_wall_ns": started,
        "duration_ns": if ran { json!(duration) } else { Value::Null },
        "duration_evidence": "mtime(stderr | root-output) − mtime(invoked.timestamp), as written by Cargo around the script's execution",
        "has_rerun_directives": !rerun_files.is_empty() || !rerun_env.is_empty(),
        "rerun_if_changed": rerun_files.len(),
        "rerun_if_changed_sample": rerun_files.iter().take(10).collect::<Vec<_>>(),
        "rerun_if_env_changed": rerun_env,
        "link_directives": link,
        "other_directives": other,
        "generated_output_bytes": out_bytes,
    }))
}

/// Connects units through the artifacts they consume (FR-030.1).
pub fn build_graph(mut units: Vec<Unit>, session_start: u64, session_end: u64, known_stems: &HashMap<String, Value>) -> (Graph, Vec<Value>) {
    let mut by_stem: HashMap<String, usize> = HashMap::new();
    for (i, u) in units.iter().enumerate() {
        // A unit compiled twice in one session (nested builds) keeps its last invocation.
        by_stem.insert(u.stem.clone(), i);
    }
    // Artifacts that were linked against but not rebuilt: fresh units, known only by reference.
    let observed = units.len();
    let mut unresolved = 0u64;
    for i in 0..observed {
        for (name, stem, _) in units[i].externs.clone() {
            if stem.is_empty() {
                unresolved += 1;
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = by_stem.entry(stem.clone()) {
                let prior = known_stems.get(&stem);
                let get = |k: &str| prior.map(|p| util::vstr(p, k)).unwrap_or_default();
                let fresh = Unit {
                    id: format!("unit:{}", stem),
                    stem: stem.clone(),
                    crate_name: name.clone(),
                    package: prior.map(|_| get("package")).unwrap_or_else(|| name.replace('_', "-")),
                    version: get("version"),
                    source: prior.map(|_| get("source")).unwrap_or_else(|| "unknown".into()),
                    kind: prior.map(|_| get("kind")).unwrap_or_else(|| "lib".into()),
                    mode: get("mode"),
                    role: "unknown".into(),
                    freshness: "fresh".into(),
                    cacheability: "n/a".into(),
                    ..Default::default()
                };
                slot.insert(units.len());
                units.push(fresh);
            }
        }
    }
    let mut deps: Vec<Vec<Dep>> = vec![Vec::new(); units.len()];
    for i in 0..observed {
        for (_, stem, meta_only) in &units[i].externs {
            if let Some(&d) = by_stem.get(stem) {
                if d != i && !deps[i].iter().any(|x| x.idx == d) {
                    deps[i].push(Dep { idx: d, meta_only: *meta_only, confidence: "observed", evidence: "rustc --extern argument" });
                }
            }
        }
    }

    // Build-script executions: Cargo runs them itself, so they are reconstructed from OUT_DIR.
    let mut scripts: Vec<Value> = Vec::new();
    let mut run_dirs: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, u) in units.iter().enumerate().take(observed) {
        if let Some(out) = &u.build_out_dir {
            if let Some(dir) = Path::new(out).parent() {
                run_dirs.entry(path_str(dir)).or_default().push(i);
            }
        }
    }
    for (dir, consumers) in run_dirs {
        let Some(mut info) = inspect_build_script_run(Path::new(&dir), session_start, session_end) else { continue };
        let owner = &units[consumers[0]];
        let (package, version, manifest_dir, source) = (owner.package.clone(), owner.version.clone(), owner.manifest_dir.clone(), owner.source.clone());
        let compile = (0..observed).find(|&i| units[i].kind == "build-script" && units[i].package == package && units[i].manifest_dir == manifest_dir);
        info["package"] = json!(package);
        info["version"] = json!(version);
        info["source"] = json!(source);
        info["compile_ns"] = json!(compile.and_then(|c| units[c].duration_ns));
        if info["ran_in_session"] == json!(true) {
            let dir_name = Path::new(&dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let run = Unit {
                id: format!("unit:run:{}", dir_name),
                stem: format!("run:{}", dir_name),
                package,
                version,
                source,
                manifest_dir,
                crate_name: "build_script_run".into(),
                kind: "build-script-run".into(),
                mode: "run".into(),
                role: "host".into(),
                freshness: "ran".into(),
                start_wall_ns: info["started_wall_ns"].as_u64(),
                duration_ns: info["duration_ns"].as_u64(),
                out_dir: dir.clone(),
                cacheability: "n/a".into(),
                ..Default::default()
            };
            let idx = units.len();
            info["unit_id"] = json!(run.id);
            units.push(run);
            deps.push(Vec::new());
            if let Some(c) = compile {
                deps[idx].push(Dep { idx: c, meta_only: false, confidence: "inferred", evidence: "build script of the same package" });
            }
            for c in consumers {
                deps[c].push(Dep { idx, meta_only: false, confidence: "inferred", evidence: "OUT_DIR of the consuming unit" });
            }
        }
        scripts.push(info);
    }

    // Host/target role: host if only host units consume it (FR-050.4).
    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); units.len()];
    for (i, ds) in deps.iter().enumerate() {
        for d in ds {
            consumers[d.idx].push(i);
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for i in 0..units.len() {
            if units[i].role != "unknown" {
                continue;
            }
            let role = if units[i].target.is_some() {
                "target"
            } else if consumers[i].is_empty() {
                if units[i].freshness == "fresh" {
                    continue;
                }
                "target"
            } else if consumers[i].iter().all(|&c| units[c].role == "host") {
                "host"
            } else if consumers[i].iter().any(|&c| units[c].role == "target") {
                "target"
            } else {
                continue;
            };
            units[i].role = role.into();
            changed = true;
        }
    }
    (Graph { units, deps, unresolved_externs: unresolved }, scripts)
}

pub struct CriticalPath {
    pub total_ns: u64,
    pub path: Vec<(usize, &'static str)>,
    pub finish: Vec<u64>,
}

/// Longest dependency chain through the observed DAG, honouring pipelining:
/// a dependent that only needs metadata may start when the `.rmeta` exists,
/// while a linking unit must wait for every transitive dependency to finish.
pub fn critical_path(g: &Graph) -> CriticalPath {
    let n = g.units.len();
    // Topological order by depth-first search; the graph is a DAG by construction of Cargo's units.
    let mut order = Vec::with_capacity(n);
    let mut state = vec![0u8; n];
    for root in 0..n {
        if state[root] != 0 {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        state[root] = 1;
        while let Some(&mut (u, ref mut next)) = stack.last_mut() {
            if *next < g.deps[u].len() {
                let d = g.deps[u][*next].idx;
                *next += 1;
                if state[d] == 0 {
                    state[d] = 1;
                    stack.push((d, 0));
                }
            } else {
                state[u] = 2;
                order.push(u);
                stack.pop();
            }
        }
    }
    let dur = |i: usize| g.units[i].duration_ns.unwrap_or(0);
    let mut start = vec![0u64; n];
    let mut finish = vec![0u64; n];
    let mut pred: Vec<Option<(usize, &'static str)>> = vec![None; n];
    // Latest finish within the transitive closure, and the unit that attains it.
    let mut closure: Vec<(u64, usize)> = (0..n).map(|i| (0, i)).collect();
    for &u in &order {
        let mut best: Option<(u64, usize, &'static str)> = None;
        for d in &g.deps[u] {
            let (ready, via, conf) = if g.units[u].links {
                let (t, who) = closure[d.idx];
                (t, who, if who == d.idx { d.confidence } else { "modeled" })
            } else if d.meta_only && g.units[d.idx].rmeta_ready_ns.is_some() {
                (start[d.idx] + g.units[d.idx].rmeta_ready_ns.unwrap_or(0), d.idx, d.confidence)
            } else {
                (finish[d.idx], d.idx, d.confidence)
            };
            if best.map(|(t, _, _)| ready > t).unwrap_or(true) {
                best = Some((ready, via, conf));
            }
        }
        if let Some((t, via, conf)) = best {
            start[u] = t;
            pred[u] = Some((via, conf));
        }
        finish[u] = start[u] + dur(u);
        let mut c = (finish[u], u);
        for d in &g.deps[u] {
            if closure[d.idx].0 > c.0 {
                c = closure[d.idx];
            }
        }
        closure[u] = c;
    }
    let Some(end) = (0..n).max_by_key(|&i| finish[i]) else {
        return CriticalPath { total_ns: 0, path: Vec::new(), finish };
    };
    let mut path = Vec::new();
    let mut cur = Some((end, "observed"));
    while let Some((u, conf)) = cur {
        path.push((u, conf));
        cur = pred[u];
        if path.len() > n {
            break;
        }
    }
    // `path` runs from the last unit back to the first; each entry's confidence is that of
    // the edge from it to its successor. Report instead the edge that placed a unit on the path.
    let mut out: Vec<(usize, &'static str)> = Vec::with_capacity(path.len());
    for (k, (u, _)) in path.iter().enumerate() {
        out.push((*u, path.get(k + 1).map(|p| p.1).unwrap_or("observed")));
    }
    out.reverse();
    CriticalPath { total_ns: finish[end], path: out, finish }
}

/// Peak number of compiler processes running at once.
fn max_concurrency(units: &[Unit]) -> u64 {
    let mut points: Vec<(u64, i64)> = Vec::new();
    for u in units {
        if let (Some(s), Some(d)) = (u.start_wall_ns, u.duration_ns) {
            points.push((s, 1));
            points.push((s + d, -1));
        }
    }
    points.sort();
    let (mut cur, mut max) = (0i64, 0i64);
    for (_, delta) in points {
        cur += delta;
        max = max.max(cur);
    }
    max as u64
}

fn unavailable(map: &mut serde_json::Map<String, Value>, field: &str, reason: impl Into<String>) {
    map.insert(field.to_string(), json!(reason.into()));
}

/// Adds what Haul's own artifact cache did in this run to the report's `cache` section.
fn artifact_cache_summary(mut cache: Value, session: &Value, events: &Events) -> Value {
    let count = |r: &str| events.cache_results.get(r).copied().unwrap_or(0);
    cache["artifacts"] = json!({
        "state": session["policy"]["cache"]["artifacts"],
        "restored": count("hit"),
        "stored": count("stored"),
        "missed": count("miss"),
        "not_cacheable": count("off"),
        "restore_time_ns": events.restore_ns,
    });
    cache
}

/// Finalizes a session into its report. Idempotent: an existing report is returned as is.
pub fn finalize(store: &WsStore, id: &str, cfg: &Config) -> Result<Value, String> {
    if let Some(existing) = store.load_run(id) {
        return Ok(existing);
    }
    if store.load_session(id).is_none() {
        return Err(format!("session {} has no record", id));
    }
    // Earlier sessions feed the unit index that rebuild analysis compares against: finalize in order.
    let ids = store.session_ids();
    for earlier in ids.iter().take_while(|e| e.as_str() < id) {
        if !store.run_path(earlier).exists() {
            finalize_one(store, earlier, cfg, false)?;
        }
    }
    finalize_one(store, id, cfg, ids.last().map(|l| l == id).unwrap_or(true))
}

fn finalize_one(store: &WsStore, id: &str, cfg: &Config, filesystem_current: bool) -> Result<Value, String> {
    let session = store.load_session(id).ok_or_else(|| format!("session {} has no record", id))?;
    let mut missing = serde_json::Map::new();
    let ws_root = session["workspace_root"].as_str().map(PathBuf::from);
    let cwd = PathBuf::from(session["cwd"].as_str().unwrap_or("."));
    let tok = Tokenizer::new(ws_root.as_deref(), false);
    let family = util::vstr(&session["command"], "family");
    let builds = crate::resolve::family_of(&family).builds();
    let start = session["started_wall_ns"].as_u64().unwrap_or(0);
    let wall = session["duration_ns"].as_u64().unwrap_or(0);
    let sccache_active = session["policy"]["cache"]["sccache_active"] == json!(true);
    let instrumented = session["instrumented"] == json!(true) && session["events_in_parent"] != json!(true);

    let mut index = waste::Index::load(&store.index_path())?;
    let mut events = match session["events_dir"].as_str() {
        Some(d) => load_events(Path::new(d), sccache_active, &tok),
        None => Events::default(),
    };
    let (probes, rustc_hint, script_probes, script_probe_ns) = (events.queries, events.rustc.clone(), events.script_probes, events.script_probe_ns);
    let (mut graph, mut scripts) = build_graph(std::mem::take(&mut events.units), start, start + wall, &index.by_stem());
    let cp = critical_path(&graph);
    for (i, _) in &cp.path {
        graph.units[*i].on_critical_path = true;
    }

    // Workspace and toolchain discovery (FR-010). Cargo metadata and VCS state describe the tree
    // as it is now, so they are only attached to a run the tree still reflects.
    let toolchain_override = session["command"]["toolchain_override"].as_str();
    let manifest_path = session["command"]["manifest_path"].as_str().map(PathBuf::from);
    const MOVED_ON: &str = "a later run was recorded before this one was analysed; the working tree no longer reflects it";
    let meta = match (&ws_root, cwd.is_dir(), filesystem_current) {
        (Some(_), true, true) => match discover::metadata(cfg, &cwd, toolchain_override, manifest_path.as_deref()) {
            Ok(m) => Some(m),
            Err(e) => {
                unavailable(&mut missing, "workspace", format!("cargo metadata failed: {}", e));
                None
            }
        },
        (Some(_), true, false) => {
            unavailable(&mut missing, "workspace", MOVED_ON);
            None
        }
        (Some(_), false, _) => {
            unavailable(&mut missing, "workspace", "the session's working directory no longer exists");
            None
        }
        (None, _, _) => {
            unavailable(&mut missing, "workspace", "the command did not run inside a Cargo workspace");
            None
        }
    };
    let snapshot = meta.as_ref().map(|m| discover::workspace_snapshot(&m.value));
    let target_dir = snapshot.as_ref().and_then(|s| s["target_directory"].as_str()).map(PathBuf::from);
    let build_dir = snapshot.as_ref().and_then(|s| s["build_directory"].as_str()).map(PathBuf::from);
    // The revision read during the build is the build-time truth; dirtiness can only be observed now.
    let source = match &ws_root {
        Some(root) if root.is_dir() && filesystem_current => {
            let mut s = discover::source_state(root, &target_dir.iter().cloned().collect::<Vec<_>>());
            if let Some(head) = session["git_head"].as_str() {
                s["revision_at_build"] = json!(head);
                if s["revision"].as_str().map(|r| r != head).unwrap_or(false) {
                    s["note"] = json!("HEAD moved between the build and its analysis; dirty state describes the tree at analysis time");
                }
            }
            s
        }
        _ => json!({ "vcs": if session["git_head"].is_string() { json!("git") } else { Value::Null }, "revision": session["git_head"], "revision_at_build": session["git_head"], "dirty": null, "unavailable": { "dirty": MOVED_ON } }),
    };
    let toolchain = discover::toolchain_fingerprint(cfg, if cwd.is_dir() { &cwd } else { Path::new(".") }, toolchain_override, rustc_hint.as_deref());
    let environment = discover::environment_fingerprint(build_dir.as_deref().or(target_dir.as_deref()));

    // Waste analyses.
    let cargo_messages = session["cargo_messages_file"].as_str().and_then(|p| util::read_json(Path::new(p)));
    let rebuilds = waste::rebuilds(&graph, &index, &scripts);
    waste::annotate_build_scripts(&mut scripts, &mut index);
    let duplication = waste::duplication(meta.as_ref(), &graph, &index);
    let proc_macros = waste::proc_macros(&graph, &cp);
    let codegen = waste::codegen(&graph, &session);
    let incremental = waste::incremental_policy(&graph, &session);

    let compiled: Vec<&Unit> = graph.units.iter().filter(|u| matches!(u.freshness.as_str(), "compiled" | "failed")).collect();
    let restored = graph.units.iter().filter(|u| u.freshness == "restored").count() as u64;
    let fresh_referenced = graph.units.iter().filter(|u| u.freshness == "fresh").count() as u64;
    let sum_ns: u64 = compiled.iter().filter_map(|u| u.duration_ns).sum();
    let script_ns: u64 = graph.units.iter().filter(|u| u.kind == "build-script-run").filter_map(|u| u.duration_ns).sum();
    let cpu: Vec<u64> = compiled.iter().filter_map(|u| u.cpu_ns).collect();
    let rss = compiled.iter().filter_map(|u| u.max_rss_bytes).max();

    // Cache (FR-100): server statistics plus per-unit cacheability by sccache's documented rules.
    let cache = match (session["policy"]["cache"].get("stats_before"), session["policy"]["cache"].get("stats_after")) {
        (Some(b), Some(a)) => {
            let mut c = crate::cache::delta(b, a);
            c["state"] = session["policy"]["cache"]["state"].clone();
            let class = |p: &str| compiled.iter().filter(|u| u.cacheability.starts_with(p)).count();
            c["units"] = json!({
                "cacheable": class("cacheable"),
                "non_cacheable_linking": class("non-cacheable: crate type"),
                "non_cacheable_incremental": class("non-cacheable: incremental"),
            });
            c["saved_compile_estimate"] = json!({ "kind": "unknown", "reason": "sccache does not report what a hit would have cost to compile" });
            c
        }
        _ => json!({
            "provider": if sccache_active { json!("sccache") } else { Value::Null },
            "state": session["policy"]["cache"]["state"],
            "detail": session["policy"]["cache"]["detail"],
            "requests": null, "hits": null, "misses": null, "non_cacheable": null, "read_bytes": null, "write_bytes": null, "lookup_time_ns": null,
            "unavailable": if sccache_active { json!("sccache statistics could not be read for this session") } else { json!("no compiler cache is active") },
        }),
    };

    // Artifact inventory (FR-110): only meaningful while the directory still reflects this run.
    let mut artifacts = Value::Null;
    if !cfg.bool("artifacts.inventory") {
        unavailable(&mut missing, "artifacts", "disabled by artifacts.inventory = false");
    } else if !builds {
        unavailable(&mut missing, "artifacts", "not a build command");
    } else if !filesystem_current {
        unavailable(&mut missing, "artifacts", "a later build ran before this session was analysed; the directory no longer reflects it");
    } else {
        let mut roots: Vec<PathBuf> = Vec::new();
        for d in [&build_dir, &target_dir].into_iter().flatten() {
            if d.is_dir() && !roots.contains(d) {
                roots.push(d.clone());
            }
        }
        if roots.is_empty() {
            unavailable(&mut missing, "artifacts", "no build directory was found for this workspace");
        } else {
            let opts = inventory::Options { stale_days: cfg.int("artifacts.stale-days"), now_ns: util::now_wall_ns(), size_budget: None };
            let known = index.knowledge(&graph);
            artifacts = json!(roots.iter().map(|r| inventory::scan(r, &known, &opts)).collect::<Vec<_>>());
        }
    }
    let sum_inv = |field: &str| artifacts.as_array().map(|a| a.iter().filter_map(|r| r[field].as_u64()).sum::<u64>());

    // Units Cargo considered fresh are only visible through its own messages.
    let (fresh_total, observed_total) = match cargo_messages.as_ref().and_then(Value::as_array) {
        Some(msgs) => {
            let arts: Vec<&Value> = msgs.iter().filter(|m| m["reason"] == json!("compiler-artifact")).collect();
            (json!(arts.iter().filter(|m| m["fresh"] == json!(true)).count()), json!(arts.len()))
        }
        None => {
            unavailable(&mut missing, "metrics.units_fresh", "Cargo reports fresh units only in its JSON messages; rerun with --message-format=json on a pipe to capture them");
            (Value::Null, json!(compiled.len() as u64 + fresh_referenced))
        }
    };
    if !instrumented {
        let why = if session["events_in_parent"] == json!(true) {
            "rustc invocations of this nested build are recorded in its parent session"
        } else if builds {
            "rustc-level observation was not active for this session"
        } else {
            "not a build command; session-level record only"
        };
        unavailable(&mut missing, "units", why);
    }
    if family == "doc" {
        unavailable(&mut missing, "units.rustdoc", "rustdoc is not run through RUSTC_WRAPPER; only the check units of `cargo doc` are observed");
    }
    if cpu.is_empty() && !compiled.is_empty() {
        unavailable(&mut missing, "metrics.cpu_time_ns", "compiler resource usage is attributed to the cache client when a wrapper such as sccache runs rustc");
    }

    let partial_reasons: Vec<String> = [
        (session["state"] != json!("completed")).then(|| format!("the command ended in state `{}`", util::vstr(&session, "state"))),
        (graph.unresolved_externs > 0).then(|| format!("{} extern argument(s) could not be mapped to an artifact", graph.unresolved_externs)),
        scripts.iter().any(|s| s["ran_in_session"] == json!(true) && s["duration_ns"].is_null()).then(|| "a build script ran without a measurable duration".to_string()),
        (!instrumented && builds).then(|| "no rustc-level events were recorded".to_string()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let critical: Vec<Value> = cp
        .path
        .iter()
        .map(|(i, conf)| {
            let u = &graph.units[*i];
            json!({
                "unit_id": u.id,
                "package": if u.version.is_empty() { u.package.clone() } else { format!("{}@{}", u.package, u.version) },
                "kind": u.kind,
                "freshness": u.freshness,
                "duration_ns": u.duration_ns,
                "edge_confidence": conf,
            })
        })
        .collect();
    let edges: Vec<Value> = graph
        .deps
        .iter()
        .enumerate()
        .flat_map(|(i, ds)| {
            let units = &graph.units;
            ds.iter().map(move |d| json!({ "from": units[d.idx].id, "to": units[i].id, "kind": if d.meta_only { "metadata" } else { "link" }, "confidence": d.confidence, "evidence": d.evidence }))
        })
        .collect();

    let mut report = json!({
        "schema_version": crate::SCHEMA_VERSION,
        "run": {
            "id": id,
            "parent_session_id": session["parent_session_id"],
            "state": session["state"],
            "started_at": session["started_at"],
            "started_wall_ns": start,
            "duration_ns": wall,
            "command": {
                "family": family,
                "entry": session["command"]["entry"],
                "argv_redacted": session["command"]["argv_redacted"],
                "cwd_token": tok.apply(&path_str(&cwd)),
                "profile": if session["command"]["release"] == json!(true) { "release" } else { "dev" },
                "targets": session["command"]["targets"],
            },
            "result": { "cargo_exit_code": session["result"]["exit_code"], "signal": session["result"]["signal"], "policy": "not-evaluated" },
            "scenario": session["scenario"],
            "upstream": session["upstream"],
            "wrappers": session["wrappers"],
            "overhead": session["overhead"],
            "notes": session["notes"],
        },
        "workspace": {
            "id": snapshot.as_ref().map(|s| s["id"].clone()).unwrap_or_else(|| json!(format!("sha256:{}", store.id))),
            "root": ws_root.as_ref().map(|r| path_str(r)),
            "members": snapshot.as_ref().map(|s| s["members"].clone()),
            "member_ids": snapshot.as_ref().map(|s| s["member_ids"].clone()),
            "virtual": snapshot.as_ref().map(|s| s["virtual"].clone()),
            "manifest_digest": snapshot.as_ref().map(|s| s["manifest_digest"].clone()),
            "manifest_digests": snapshot.as_ref().map(|s| s["manifest_digests"].clone()),
            "lockfile_digest": snapshot.as_ref().map(|s| s["lockfile_digest"].clone()),
            "target_directory": target_dir.as_ref().map(|d| path_str(d)),
            "build_directory": build_dir.as_ref().map(|d| path_str(d)),
            "source_revision": source["revision_at_build"].as_str().or(source["revision"].as_str()),
            "dirty": source["dirty"],
            "source": source,
        },
        "toolchain": toolchain,
        "environment": { "machine": environment, "variables": session["env"] },
        "policy": { "workload": session["policy"]["workload"], "fast": session["policy"]["fast"], "incremental": incremental, "cache": { "provider": session["policy"]["cache"]["provider"], "state": session["policy"]["cache"]["state"], "detail": session["policy"]["cache"]["detail"], "fail": session["policy"]["cache"]["fail"] } },
        "metrics": {
            "wall_time_ns": wall,
            "critical_path_ns": if compiled.is_empty() { Value::Null } else { json!(cp.total_ns) },
            "sum_unit_time_ns": if instrumented { json!(sum_ns) } else { Value::Null },
            "build_script_run_time_ns": if instrumented { json!(script_ns) } else { Value::Null },
            "average_parallelism": if wall > 0 && instrumented { json!(((sum_ns + script_ns) as f64 / wall as f64 * 100.0).round() / 100.0) } else { Value::Null },
            "max_concurrent_compilers": if instrumented { json!(max_concurrency(&graph.units)) } else { Value::Null },
            "cpu_time_ns": if cpu.is_empty() { Value::Null } else { json!(cpu.iter().sum::<u64>()) },
            "peak_rss_bytes": rss,
            "units_observed": if instrumented { observed_total } else { Value::Null },
            "units_compiled": if instrumented { json!(compiled.len()) } else { Value::Null },
            "units_restored": if instrumented { json!(restored) } else { Value::Null },
            "units_failed": if instrumented { json!(compiled.iter().filter(|u| u.freshness == "failed").count()) } else { Value::Null },
            "units_fresh": fresh_total,
            "units_fresh_referenced": if instrumented { json!(fresh_referenced) } else { Value::Null },
            "build_scripts_run": if instrumented { json!(scripts.iter().filter(|s| s["ran_in_session"] == json!(true)).count()) } else { Value::Null },
            "probe_invocations": probes,
            "build_script_compiler_invocations": if instrumented { json!(script_probes) } else { Value::Null },
            "build_script_compiler_time_ns": if instrumented { json!(script_probe_ns) } else { Value::Null },
            "output_bytes_written": if instrumented { json!(compiled.iter().filter_map(|u| u.output_bytes).sum::<u64>()) } else { Value::Null },
            "artifact_logical_bytes": sum_inv("logical_bytes"),
            "artifact_allocated_bytes": sum_inv("allocated_bytes"),
            "units": { "time": "ns", "size": "bytes" },
        },
        "cache": artifact_cache_summary(cache, &session, &events),
        "critical_path": critical,
        "critical_path_model": {
            "completeness": if partial_reasons.is_empty() { "observed" } else { "partial" },
            "partial_reasons": partial_reasons,
            "method": "longest chain over observed --extern edges; metadata-only dependents start at the dependency's .rmeta, linking units wait for their whole transitive closure",
            "note": "unit durations are wall-clock under the build's actual contention; the path bounds how fast this build could finish with unlimited parallelism",
        },
        "units": graph.units,
        "edges": edges,
        "rebuilds": rebuilds,
        "duplication": duplication,
        "build_scripts": scripts,
        "proc_macros": proc_macros,
        "codegen": codegen,
        "artifacts": artifacts,
        "recommendations": [],
        "budget": { "result": "not-evaluated", "evaluations": [] },
        "unavailable": {},
    });
    if let Some(m) = &meta {
        if let Some(why) = &m.deps_unavailable {
            unavailable(&mut missing, "duplication.versions", why.clone());
        }
    }
    report["unavailable"] = Value::Object(missing);
    let recs = crate::recommend::generate(&report, &index);
    report["recommendations"] = json!(recs);

    index.absorb(&graph, &report["rebuilds"], id, start, &family, session["result"]["exit_code"] == json!(0));
    index.save(&store.index_path()).map_err(|e| format!("could not update the unit index: {}", e))?;
    util::write_json(&store.run_path(id), &report).map_err(|e| format!("could not store the run report: {}", e))?;
    store.prune(cfg.int("measurement.keep-runs") as usize, cfg.int("measurement.keep-raw-events") as usize);
    Ok(report)
}

/// Portable copy of a report for export: machine-specific path prefixes become tokens (PRIV-004).
pub fn export(report: &Value, ws_root: Option<&Path>, cfg: &Config) -> Value {
    let tok = Tokenizer::new(ws_root, cfg.bool("security.redact-path-home"));
    fn walk(v: &Value, tok: &Tokenizer) -> Value {
        match v {
            Value::String(s) => Value::String(tok.apply(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, tok)).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (tok.apply(k), walk(x, tok))).collect()),
            other => other.clone(),
        }
    }
    walk(report, &tok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok() -> Tokenizer {
        Tokenizer::new(None, false)
    }

    fn event(name: &str, hash: &str, externs: &[(&str, &str)], extra: &[&str], dur: u64, rmeta: Option<u64>) -> Value {
        let mut args: Vec<String> = vec!["--crate-name".into(), name.into(), "--edition=2021".into(), "src/lib.rs".into(), "--emit=dep-info,metadata,link".into(), "-C".into(), format!("metadata={}", hash), "-C".into(), format!("extra-filename=-{}", hash), "--out-dir".into(), "/t/debug/deps".into()];
        for (n, file) in externs {
            args.push("--extern".into());
            args.push(format!("{}=/t/debug/deps/{}", n, file));
        }
        args.extend(extra.iter().map(|s| s.to_string()));
        if !extra.contains(&"--crate-type") {
            args.extend(["--crate-type".to_string(), "lib".to_string()]);
        }
        json!({ "kind": "compile", "args": args, "cwd": "/w/p", "exit_code": 0, "start_wall_ns": 1000, "dur_ns": dur, "rmeta_ready_ns": rmeta,
                "rustc": "/tc/bin/rustc", "rustc_id": { "bytes": 1, "mtime_ns": 2 },
                "env": { "pkg_name": name, "pkg_version": "1.0.0", "manifest_dir": format!("/w/{}", name), "primary": true, "toolchain": "stable" } })
    }

    fn graph(events: &[Value]) -> Graph {
        let units: Vec<Unit> = events.iter().map(|e| parse_unit(e, false, &tok()).unwrap()).collect();
        build_graph(units, 0, u64::MAX / 2, &HashMap::new()).0
    }

    #[test]
    fn extern_inputs_become_edges() {
        // FR-030 acceptance: B's --extern input maps to A's output ⇒ edge A→B.
        let g = graph(&[event("a", "1", &[], &[], 10, None), event("b", "2", &[("a", "liba-1.rlib")], &[], 5, None)]);
        assert_eq!(g.deps[1].len(), 1);
        assert_eq!(g.units[g.deps[1][0].idx].stem, "a-1");
        assert_eq!(g.deps[1][0].confidence, "observed");
        assert!(g.deps[0].is_empty());
    }

    #[test]
    fn unbuilt_dependencies_are_fresh_not_invented_timings() {
        let g = graph(&[event("b", "2", &[("serde", "libserde-9.rmeta")], &[], 5, None)]);
        assert_eq!(g.units.len(), 2);
        let fresh = &g.units[1];
        assert_eq!(fresh.freshness, "fresh");
        assert_eq!(fresh.duration_ns, None, "a unit that did not run has no duration, not a zero one");
    }

    #[test]
    fn critical_path_differs_from_total_cost() {
        // a(10) → c(4);  b(3) independent leaf feeding c as well. Total 17, critical 14.
        let g = graph(&[
            event("a", "1", &[], &[], 10, None),
            event("b", "2", &[], &[], 3, None),
            event("c", "3", &[("a", "liba-1.rlib"), ("b", "libb-2.rlib")], &[], 4, None),
        ]);
        let cp = critical_path(&g);
        assert_eq!(cp.total_ns, 14);
        let stems: Vec<&str> = cp.path.iter().map(|(i, _)| g.units[*i].stem.as_str()).collect();
        assert_eq!(stems, vec!["a-1", "c-3"]);
        let total: u64 = g.units.iter().filter_map(|u| u.duration_ns).sum();
        assert_eq!(total, 17);
    }

    #[test]
    fn pipelining_lets_metadata_dependents_start_early_but_linkers_wait() {
        // a takes 10 but its metadata is ready at 2. A lib `b` (metadata edge) can start at 2;
        // a binary linking a must wait for all 10.
        let lib = graph(&[event("a", "1", &[], &[], 10, Some(2)), event("b", "2", &[("a", "liba-1.rmeta")], &[], 5, None)]);
        assert_eq!(critical_path(&lib).total_ns, 10, "b finishes at 7, a at 10");
        let bin = graph(&[
            event("a", "1", &[], &[], 10, Some(2)),
            event("b", "2", &[("a", "liba-1.rmeta")], &[], 5, Some(1)),
            event("app", "3", &[("b", "libb-2.rlib")], &["--crate-type", "bin"], 1, None),
        ]);
        let cp = critical_path(&bin);
        assert_eq!(cp.total_ns, 11, "the binary waits for a (transitive) to finish at 10");
        assert_eq!(cp.path.first().map(|(i, _)| bin.units[*i].stem.as_str()), Some("a-1"));
        assert_eq!(cp.path.last().unwrap().1, "modeled", "the wait on a transitive dependency is modeled, not an observed edge");
    }

    #[test]
    fn features_and_profile_change_identity_separately() {
        let base = parse_unit(&event("a", "1", &[], &[], 1, None), false, &tok()).unwrap();
        let feat = parse_unit(&event("a", "2", &[], &["--cfg", "feature=\"std\""], 1, None), false, &tok()).unwrap();
        let opt = parse_unit(&event("a", "3", &[], &["-C", "opt-level=3"], 1, None), false, &tok()).unwrap();
        assert_eq!(feat.features, vec!["std"]);
        assert_eq!(base.logical_key, feat.logical_key);
        assert_ne!(base.fingerprint.features, feat.fingerprint.features);
        assert_eq!(base.fingerprint.profile, feat.fingerprint.profile);
        assert_ne!(base.fingerprint.profile, opt.fingerprint.profile);
        assert_eq!(base.fingerprint.features, opt.fingerprint.features);
        assert_ne!(base.exact_id, feat.exact_id);
    }

    #[test]
    fn kinds_and_cacheability() {
        let pm = parse_unit(&event("m", "1", &[], &["--crate-type", "proc-macro"], 1, None), true, &tok()).unwrap();
        assert_eq!((pm.kind.as_str(), pm.role.as_str()), ("proc-macro", "host"));
        assert!(pm.cacheability.starts_with("non-cacheable: crate type"));
        let inc = parse_unit(&event("l", "2", &[], &["-C", "incremental=/t/debug/incremental"], 1, None), true, &tok()).unwrap();
        assert!(inc.cacheability.contains("incremental"));
        let lib = parse_unit(&event("l", "3", &[], &[], 1, None), true, &tok()).unwrap();
        assert_eq!(lib.cacheability, "cacheable");
        assert_eq!(parse_unit(&event("l", "3", &[], &[], 1, None), false, &tok()).unwrap().cacheability, "n/a");
    }

    #[test]
    fn compilers_started_by_build_scripts_are_not_units() {
        // proc-macro2, serde and others compile a probe file with $RUSTC_WRAPPER $RUSTC; it inherits
        // the package environment but is not something Cargo scheduled.
        let mut probe = event("proc_macro2", "0", &[], &[], 5, None);
        let args: Vec<Value> = probe["args"].as_array().unwrap().iter().filter(|a| !a.as_str().unwrap().starts_with("metadata=")).cloned().collect();
        probe["args"] = json!(args);
        assert!(parse_unit(&probe, false, &tok()).is_none());
        assert!(parse_unit(&event("proc_macro2", "0", &[], &[], 5, None), false, &tok()).is_some());
    }

    #[test]
    fn export_tokenizes_paths() {
        let ws = std::env::temp_dir().join("haul-export-ws");
        let cfg = Config::defaults();
        let v = json!({ "p": format!("{}/src/lib.rs", ws.display()), "n": 3 });
        let out = export(&v, Some(&ws), &cfg);
        assert_eq!(out["p"], "$WORKSPACE/src/lib.rs");
        assert_eq!(out["n"], 3);
    }
}
