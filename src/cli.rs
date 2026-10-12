//! The `haul` management CLI and the multi-call entry point.
//!
//! Invoked as `cargo`, the binary is the compatibility shim and forwards its
//! arguments untouched. Enhanced commands exist only under `haul`, so they can
//! never collide with a command a future Cargo introduces (OBJ-006).

use crate::analyze::{self, Graph};
use crate::bench;
use crate::compare;
use crate::config::Config;
use crate::discover;
use crate::inventory;
use crate::remedy;
use crate::render;
use crate::resolve;
use crate::session::{self, Mode};
use crate::store::{self, WsStore};
use crate::util::{self, human_bytes, human_dur, path_str, vstr};
use crate::waste;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const HELP: &str = "\
haul — drop-in Cargo replacement: orchestration, observation and policy around upstream Cargo

USAGE
    haul <cargo-command> [args…]     run a Cargo command through Haul (same syntax as cargo)
    haul upstream <args…>            run upstream Cargo directly: no policy, no instrumentation
    cargo <args…>                    the same as the first form, when the shim is on PATH

OUTPUT OF A BUILD (stored for every build Haul rendered in a terminal)
    haul warnings [--run REF] [--crate NAME] [--lint LINT]
    haul errors [--run REF] [--crate NAME]
    haul log [--run REF] [--raw]

ANALYSIS
    haul analyze [SECTION] [--run REF] [--format text|json]
                 [--filter FIELD=VALUE]… [--sort FIELD[:asc|desc]] [--limit N]
                                     SECTION: timing rebuilds duplication scripts proc-macros
                                              codegen cache storage recommendations worktrees
                                     filter/sort/limit work on a section's records, e.g.
                                     `haul analyze timing --filter source=registry --sort duration_ns`
    haul report [--run REF] [--format text|json|html] [-o FILE]
    haul runs [--limit N] [--format text|json]
    haul inspect [--format text|json]        workspace, toolchain and environment fingerprint
    haul driver [status|build] [--rustc PATH] Haul's compiler driver for this toolchain
    haul compare <BASE> <CANDIDATE> [--format text|json]

MEASUREMENT
    haul benchmark --scenario NAME [--reps N] [--warmups N] [--edit FILE] [--allow-mutation]
                   [--timeout DURATION] [--format text|json] [-- <cargo args>]
    haul benchmark --list
    haul experiment [codegen|linker] [--name NAME] [--set KEY=VALUE]… [--env KEY=VALUE]…
                   [--codegen-units N] [--debug VALUE] [--opt-level VALUE] [--lto VALUE] [--linker NAME]
                   [--scenario NAME] [--reps N] [--warmups N] [--verify '<cargo args>'] [-- <cargo args>]

BUDGETS
    haul baseline set NAME [--run REF | --benchmark ID]
    haul baseline list | show NAME
    haul ci evaluate [--run REF | --benchmark ID] [--baseline NAME] [--budgets FILE] [--format text|json]

STORAGE
    haul inventory [--caches] [--format text|json]
    haul gc --dry-run [--older-than DAYS] [--max-size SIZE] [--format text|json]

POLICY AND REMEDIATION
    haul policy explain [--format text|json]
    haul cache status [--format text|json]
    haul fix --dry-run [REC_ID] [--run REF] [--option ALTERNATIVE] [--patch] [--format text|json]
    haul fix --apply REMEDIATION_ID | --rollback REMEDIATION_ID

INSTALLATION
    haul doctor
    haul install-shim [--dir DIR]
    haul uninstall-shim

RUN REFERENCES
    latest, latest~N, a run id or a unique prefix/suffix of one; in `compare`
    also baseline:NAME and benchmark:ID.

Haul's own configuration lives in .haul/config.toml and HAUL_* variables;
`haul policy explain` shows every effective setting and where it came from.
";

type CmdResult = Result<i32, String>;

/// Minimal strict argument reader for management commands: unknown options are errors.
struct Args {
    items: Vec<String>,
    /// Everything after a literal `--`, forwarded to Cargo.
    passthrough: Vec<OsString>,
}

impl Args {
    fn new(raw: &[OsString]) -> Args {
        let split = raw.iter().position(|a| a == "--");
        let (own, pass) = match split {
            Some(i) => (&raw[..i], raw[i + 1..].to_vec()),
            None => (raw, Vec::new()),
        };
        Args { items: own.iter().map(|a| util::lossy(a)).collect(), passthrough: pass }
    }

    fn flag(&mut self, name: &str) -> bool {
        match self.items.iter().position(|a| a == name) {
            Some(i) => {
                self.items.remove(i);
                true
            }
            None => false,
        }
    }

    fn opt(&mut self, name: &str) -> Result<Option<String>, String> {
        let prefix = format!("{}=", name);
        for i in 0..self.items.len() {
            if self.items[i] == name {
                if i + 1 >= self.items.len() {
                    return Err(format!("{} needs a value", name));
                }
                self.items.remove(i);
                return Ok(Some(self.items.remove(i)));
            }
            if let Some(v) = self.items[i].strip_prefix(&prefix) {
                let v = v.to_string();
                self.items.remove(i);
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    fn multi(&mut self, name: &str) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        while let Some(v) = self.opt(name)? {
            out.push(v);
        }
        Ok(out)
    }

    /// Next non-option argument. Call only after every value-taking option has been read,
    /// otherwise an option's value could be mistaken for it.
    fn positional(&mut self) -> Option<String> {
        let i = self.items.iter().position(|a| !a.starts_with('-'))?;
        Some(self.items.remove(i))
    }

    fn number<T: std::str::FromStr>(&mut self, name: &str) -> Result<Option<T>, String> {
        match self.opt(name)? {
            Some(v) => v.parse().map(Some).map_err(|_| format!("{} expects a number, got {:?}", name, v)),
            None => Ok(None),
        }
    }

    fn format(&mut self, allowed: &[&str]) -> Result<String, String> {
        let f = self.opt("--format")?.unwrap_or_else(|| "text".into());
        if !allowed.contains(&f.as_str()) {
            return Err(format!("--format must be one of {}", allowed.join(", ")));
        }
        Ok(f)
    }

    fn finish(self) -> Result<(), String> {
        match self.items.first() {
            Some(a) => Err(format!("unexpected argument {:?}; see `haul help`", a)),
            None => Ok(()),
        }
    }
}

struct Ctx {
    cwd: PathBuf,
    cfg: Config,
    store: WsStore,
}

impl Ctx {
    fn load() -> Result<Ctx, String> {
        let cwd = std::env::current_dir().map_err(|e| format!("cannot determine the current directory: {}", e))?;
        let cfg = Config::load(&cwd).map_err(|e| format!("invalid Haul configuration: {}", e))?;
        let root = resolve::guess_workspace_root(&cwd);
        Ok(Ctx { store: WsStore::open(root.as_deref()), cwd, cfg })
    }

    fn root(&self) -> Result<&Path, String> {
        self.store.root.as_deref().ok_or_else(|| "this command needs a Cargo workspace; no Cargo.toml was found in this directory or its parents".to_string())
    }

    fn report(&self, reference: Option<&str>) -> Result<Value, String> {
        let id = self.store.resolve_run_ref(reference.unwrap_or("latest"), reference.is_none())?;
        analyze::finalize(&self.store, &id, &self.cfg)
    }

    /// Analyses any recorded run that has not been analysed yet, so the unit index is current.
    fn finalize_pending(&self) -> Result<(), String> {
        match self.store.session_ids().last() {
            Some(latest) => analyze::finalize(&self.store, latest, &self.cfg).map(|_| ()),
            None => Ok(()),
        }
    }

    fn export(&self, v: &Value) -> Value {
        analyze::export(v, self.store.root.as_deref(), &self.cfg)
    }

    fn print_json(&self, v: &Value) {
        println!("{}", serde_json::to_string_pretty(&self.export(v)).unwrap_or_default());
    }
}

fn empty_graph() -> Graph {
    Graph { units: Vec::new(), deps: Vec::new(), unresolved_externs: 0 }
}

pub fn main() {
    let mut argv = std::env::args_os();
    let argv0 = argv.next().unwrap_or_default();
    let args: Vec<OsString> = argv.collect();
    let invoked_as = Path::new(&argv0).file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if invoked_as == "cargo" {
        session::run_and_exit(args, Mode::Shim);
    }
    if invoked_as == crate::toolchain_cache::PROBE_NAME {
        crate::toolchain_cache::report_environment();
    }
    let command = args.first().map(|a| util::lossy(a)).unwrap_or_default();
    // The runner Haul sets while it builds what `cargo run` needs: the program is run afterwards,
    // by `cargo run` itself.
    if command == crate::ui::NOOP_RUNNER {
        std::process::exit(0);
    }
    let rest = if args.is_empty() { &args[..] } else { &args[1..] };
    let result: CmdResult = match command.as_str() {
        "" | "help" | "--help" | "-h" => {
            print!("{}", HELP);
            Ok(0)
        }
        "version" | "--version" | "-V" => {
            println!("haul {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "upstream" => session::exec_upstream(rest.to_vec()),
        "warnings" => cmd_output(rest, Some("warning")),
        "errors" => cmd_output(rest, Some("error")),
        "log" => cmd_output(rest, None),
        "analyze" => cmd_analyze(rest),
        "report" => cmd_report(rest),
        "runs" => cmd_runs(rest),
        "inspect" => cmd_inspect(rest),
        "driver" => cmd_driver(rest),
        "compare" => cmd_compare(rest),
        "benchmark" => cmd_benchmark(rest),
        "experiment" => cmd_experiment(rest),
        "baseline" => cmd_baseline(rest),
        "ci" => cmd_ci(rest),
        "inventory" => cmd_inventory(rest, false),
        "gc" => cmd_inventory(rest, true),
        "policy" => cmd_policy(rest),
        "cache" => cmd_cache(rest),
        "fix" => cmd_fix(rest),
        "doctor" => cmd_doctor(rest),
        "install-shim" => cmd_install(rest),
        "uninstall-shim" => cmd_uninstall(rest),
        // Anything else is Cargo syntax (including `+toolchain` and global flags).
        _ => session::run_and_exit(args, Mode::Explicit),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => session::fail(&e),
    }
}

/// `haul warnings` / `haul errors` / `haul log`: the stored output of a rendered build.
fn cmd_output(raw: &[OsString], level: Option<&str>) -> CmdResult {
    let mut a = Args::new(raw);
    let run = a.opt("--run")?;
    let krate = a.opt("--crate")?;
    let lint = if level == Some("warning") { a.opt("--lint")? } else { None };
    let raw_log = level.is_none() && a.flag("--raw");
    a.finish()?;
    let ctx = Ctx::load()?;
    let id = ctx.store.resolve_run_ref(run.as_deref().unwrap_or("latest"), true)?;
    let session = ctx.store.load_session(&id).ok_or_else(|| format!("run {} has no record", id))?;
    let dir = PathBuf::from(session["events_dir"].as_str().ok_or("this run has no stored output")?);
    let lines = crate::ui::stored_output(&dir)?;
    match level {
        Some(level) => {
            let n = crate::ui::show(&lines, level, krate.as_deref(), lint.as_deref());
            if n == 0 {
                println!("No {}s in run {}{}.", level, id, if krate.is_some() || lint.is_some() { " matching the filter" } else { "" });
            }
        }
        None => crate::ui::show_log(&lines, raw_log),
    }
    Ok(0)
}

fn cmd_analyze(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    let run = a.opt("--run")?;
    let view = crate::view::View::parse(a.multi("--filter")?, a.opt("--sort")?, a.number("--limit")?)?;
    let section = a.positional();
    a.finish()?;
    if !view.is_empty() && matches!(section.as_deref(), None | Some("worktrees")) {
        return Err("--filter, --sort and --limit apply to the records of one section: `haul analyze <section> …`".into());
    }
    let ctx = Ctx::load()?;
    if section.as_deref() == Some("worktrees") {
        ctx.finalize_pending()?;
        let w = waste::worktrees(&ctx.store, &ctx.cfg)?;
        if format == "json" {
            ctx.print_json(&w);
        } else {
            print!("{}", render::worktrees(&w));
        }
        return Ok(0);
    }
    if let Some(s) = &section {
        if !render::SECTIONS.contains(&s.as_str()) || s == "summary" {
            return Err(format!("unknown analysis section `{}` (available: {}, worktrees)", s, render::SECTIONS[1..].join(", ")));
        }
    }
    let mut report = ctx.report(run.as_deref())?;
    if let Some(s) = section.as_deref().filter(|_| !view.is_empty()) {
        view.apply(&mut report, s)?;
    }
    if format == "json" {
        let v = match section.as_deref() {
            None => report,
            Some("timing") => json!({ "metrics": report["metrics"], "critical_path": report["critical_path"], "critical_path_model": report["critical_path_model"], "units": report["units"], "edges": report["edges"] }),
            Some("scripts") => report["build_scripts"].clone(),
            Some("proc-macros") => report["proc_macros"].clone(),
            Some("storage") => report["artifacts"].clone(),
            Some("codegen") => json!({ "codegen": report["codegen"], "incremental": report["policy"]["incremental"] }),
            Some(other) => report[other].clone(),
        };
        ctx.print_json(&v);
    } else {
        let sections: Vec<&str> = match section.as_deref() {
            Some(s) => vec![s],
            None => render::SECTIONS.to_vec(),
        };
        print!("{}", render::run_text(&report, &sections, render::Presentation { as_given: !view.is_empty(), limit: view.limit() }));
    }
    Ok(0)
}

fn cmd_report(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json", "html"])?;
    let run = a.opt("--run")?;
    let output = match a.opt("-o")? {
        Some(o) => Some(o),
        None => a.opt("--output")?,
    };
    a.finish()?;
    let ctx = Ctx::load()?;
    let report = ctx.report(run.as_deref())?;
    let exported = ctx.export(&report);
    let body = match format.as_str() {
        "json" => format!("{}\n", serde_json::to_string_pretty(&exported).unwrap_or_default()),
        "html" => render::html(&exported),
        _ => render::run_text(&report, render::SECTIONS, render::Presentation::default()),
    };
    match output {
        Some(path) => {
            util::write_atomic(&ctx.cwd.join(&path), body.as_bytes()).map_err(|e| format!("could not write {}: {}", path, e))?;
            eprintln!("haul: wrote {}", path);
        }
        None => print!("{}", body),
    }
    Ok(0)
}

fn cmd_runs(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    let limit: usize = a.number("--limit")?.unwrap_or(20);
    a.finish()?;
    let ctx = Ctx::load()?;
    let sessions: Vec<Value> = ctx.store.session_ids().iter().rev().take(limit).filter_map(|id| ctx.store.load_session(id)).collect();
    if format == "json" {
        ctx.print_json(&json!(sessions));
        return Ok(0);
    }
    if sessions.is_empty() {
        println!("No recorded runs for this workspace. Run a build through `haul` or the `cargo` shim first.");
        return Ok(0);
    }
    println!("{:<26}  {:<24}  {:<11}  {:<9}  {:>9}  {:<8}  COMMAND", "RUN", "STARTED", "FAMILY", "STATE", "WALL", "OBSERVED");
    for s in sessions {
        let observed = match (s["instrumented"] == json!(true), s["events_in_parent"] == json!(true)) {
            (true, false) => "rustc",
            (true, true) => "parent",
            _ => "session",
        };
        let argv = s["command"]["argv_redacted"].as_array().map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        println!("{:<26}  {:<24}  {:<11}  {:<9}  {:>9}  {:<8}  {}", vstr(&s, "id"), vstr(&s, "started_at"), vstr(&s["command"], "family"), vstr(&s, "state"), human_dur(s["duration_ns"].as_u64().unwrap_or(0)), observed, argv);
    }
    Ok(0)
}

fn live_snapshot(ctx: &Ctx) -> Result<Value, String> {
    ctx.root()?;
    let meta = discover::metadata(&ctx.cfg, &ctx.cwd, None, None)?;
    let snapshot = discover::workspace_snapshot(&meta.value);
    let target = snapshot["target_directory"].as_str().map(PathBuf::from);
    let source = discover::source_state(ctx.root()?, &target.iter().cloned().collect::<Vec<_>>());
    let tok = util::Tokenizer::new(ctx.store.root.as_deref(), false);
    Ok(json!({
        "schema_version": crate::SCHEMA_VERSION,
        "workspace": snapshot,
        "source": source,
        "toolchain": discover::toolchain_fingerprint(&ctx.cfg, &ctx.cwd, None, None),
        "environment": { "machine": discover::environment_fingerprint(target.as_deref()), "variables": session::env_fingerprint(&tok, ctx.cfg.str("security.persist-environment")) },
        "unavailable": meta.deps_unavailable.map(|d| json!({ "dependency_graph": d })).unwrap_or(json!({})),
    }))
}

/// The compiler of the toolchain a build here would use, as Haul resolves it.
fn toolchain_rustc() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let cfg = crate::config::Config::load(&cwd)?;
    let up = crate::resolve::resolve_upstream(&crate::resolve::classify(&[OsString::from("build")]), &cfg)?;
    if !up.toolchain_cargo {
        return Err(format!("Haul resolved {} ({}), not a toolchain's own Cargo; pass --rustc PATH", up.program.display(), up.rule));
    }
    Ok(up.program.with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX)))
}

fn cmd_driver(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let rustc = a.opt("--rustc")?;
    let action = a.positional();
    a.finish()?;
    let rustc = match rustc {
        Some(r) => PathBuf::from(r),
        None => toolchain_rustc()?,
    };
    match action.as_deref() {
        Some("build") => {
            let exe = crate::driver::build(&rustc)?;
            println!("{}", exe.display());
            Ok(0)
        }
        None | Some("status") => {
            let vv = crate::rustc_info::version_verbose(&util::haul_home().join("rustc"), &util::path_str(&rustc))?;
            match (crate::driver::find(&vv), crate::driver::build_failure(&vv)) {
                (Some(exe), _) => println!("built: {}", exe.display()),
                (None, Some(why)) => println!("could not be built for {}: {}\n`haul driver build` tries again", vv.lines().next().unwrap_or("this compiler"), why.trim()),
                (None, None) => println!("not built for {} (expected in {}); `haul driver build` builds it", vv.lines().next().unwrap_or("this compiler"), crate::driver::dir(&vv)?.display()),
            }
            Ok(0)
        }
        Some(other) => Err(format!("unknown `haul driver` action `{}` (status, build)", other)),
    }
}

fn cmd_inspect(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    a.finish()?;
    let ctx = Ctx::load()?;
    let snap = live_snapshot(&ctx)?;
    if format == "json" {
        ctx.print_json(&snap);
        return Ok(0);
    }
    let (w, t, m) = (&snap["workspace"], &snap["toolchain"], &snap["environment"]["machine"]);
    println!("Workspace");
    println!("  root            {}", vstr(w, "root"));
    println!("  id              {}", vstr(w, "id"));
    println!("  members         {}{}", w["members"], if w["virtual"] == json!(true) { " (virtual workspace)" } else { "" });
    for id in w["member_ids"].as_array().into_iter().flatten() {
        println!("    {}", id.as_str().unwrap_or(""));
    }
    println!("  manifests       {}", vstr(w, "manifest_digest"));
    println!("  lockfile        {}", w["lockfile_digest"].as_str().unwrap_or("none"));
    println!("  target dir      {}", vstr(w, "target_directory"));
    println!("  build dir       {}", vstr(w, "build_directory"));
    let s = &snap["source"];
    match s["revision"].as_str() {
        Some(r) => println!("  source          git {}{}", r, match s["dirty"].as_bool() { Some(true) => format!(" (dirty: {} path(s))", s["dirty_files"]), Some(false) => " (clean)".into(), None => String::new() }),
        None => println!("  source          no VCS; snapshot {} over {} file(s)", vstr(s, "snapshot_digest"), s["snapshot_files"]),
    }
    println!("Toolchain");
    println!("  cargo           {}", t["cargo_version"].as_str().unwrap_or("unavailable"));
    println!("  rustc           {}", t["rustc_version"].as_str().unwrap_or("unavailable"));
    println!("  host            {}", t["host"].as_str().unwrap_or("unavailable"));
    println!("  capabilities    {}", t["capabilities"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default());
    println!("Environment");
    println!("  machine         {} {} · {} · {} logical CPUs · {}", vstr(m, "os"), vstr(m, "arch"), m["cpu_model"].as_str().unwrap_or("cpu unavailable"), m["logical_cpus"], m["memory_bytes"].as_u64().map(human_bytes).unwrap_or_else(|| "memory unavailable".into()));
    println!("  class           {}   filesystem {}", vstr(m, "class"), m["filesystem"].as_str().unwrap_or("unavailable"));
    for (k, v) in snap["environment"]["variables"]["vars"].as_object().into_iter().flatten() {
        println!("  {:<15} {}", k, v["value"].as_str().unwrap_or(""));
    }
    for (k, v) in snap["unavailable"].as_object().into_iter().flatten() {
        println!("  unavailable: {}: {}", k, v.as_str().unwrap_or(""));
    }
    Ok(0)
}

fn cmd_compare(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    let (base, cand) = match (a.positional(), a.positional()) {
        (Some(b), Some(c)) => (b, c),
        _ => return Err("usage: haul compare <BASE> <CANDIDATE>".into()),
    };
    a.finish()?;
    let ctx = Ctx::load()?;
    let c = compare::compare(&compare::resolve_subject(&ctx.store, &ctx.cfg, &base)?, &compare::resolve_subject(&ctx.store, &ctx.cfg, &cand)?);
    if format == "json" {
        ctx.print_json(&c);
    } else {
        print!("{}", render::comparison(&c));
    }
    Ok(0)
}

fn bench_options(a: &mut Args, default_scenario: &str) -> Result<bench::Options, String> {
    let mut o = bench::Options::new(&a.opt("--scenario")?.unwrap_or_else(|| default_scenario.into()));
    if let Some(n) = a.number("--reps")? {
        o.reps = n;
    }
    if let Some(n) = a.number("--warmups")? {
        o.warmups = n;
    }
    o.edit = a.opt("--edit")?.map(PathBuf::from);
    o.allow_mutation = a.flag("--allow-mutation");
    if let Some(t) = a.opt("--timeout")? {
        o.timeout = Some(Duration::from_nanos(util::parse_duration_ns(&t).filter(|n| *n > 0).ok_or_else(|| format!("--timeout {:?} is not a duration (examples: 90s, 5m)", t))?));
        if cfg!(not(unix)) {
            return Err("--timeout is not supported on this platform".into());
        }
    }
    o.cargo_args = a.passthrough.clone();
    Ok(o)
}

fn cmd_benchmark(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    if a.flag("--list") {
        a.finish()?;
        for (name, cache, cmd, what) in bench::SCENARIOS {
            println!("{:<11} {:<5} cargo {:<16} {}", name, cache, cmd.join(" "), what);
        }
        return Ok(0);
    }
    let format = a.format(&["text", "json"])?;
    let opts = bench_options(&mut a, "no-change")?;
    a.finish()?;
    let ctx = Ctx::load()?;
    let result = bench::run(&opts, &ctx.store, &ctx.cfg)?;
    if format == "json" {
        ctx.print_json(&result);
    } else {
        print!("{}", render::benchmark(&result));
    }
    Ok(if result["state"] == json!("complete") { 0 } else { 1 })
}

fn cmd_experiment(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    let mut exp = bench::Experiment { overlays: a.multi("--set")?, ..Default::default() };
    for e in a.multi("--env")? {
        let (k, v) = e.split_once('=').ok_or_else(|| format!("--env expects KEY=VALUE, got {:?}", e))?;
        exp.env.push((k.to_string(), v.to_string()));
    }
    let profile = if a.passthrough.iter().any(|p| p == "--release" || p == "-r") { "release" } else { "dev" };
    for (flag, key, quoted) in [("--codegen-units", "codegen-units", false), ("--debug", "debug", true), ("--opt-level", "opt-level", true), ("--lto", "lto", true)] {
        if let Some(v) = a.opt(flag)? {
            // Booleans and integers stay bare; names such as "thin" or "line-tables-only" are TOML strings.
            let bare = !quoted || v == "true" || v == "false" || v.parse::<i64>().is_ok();
            exp.overlays.push(if bare { format!("profile.{}.{}={}", profile, key, v) } else { format!("profile.{}.{}=\"{}\"", profile, key, v) });
        }
    }
    let ctx = Ctx::load()?;
    if let Some(linker) = a.opt("--linker")? {
        let host = discover::toolchain_fingerprint(&ctx.cfg, &ctx.cwd, None, None)["host"].as_str().map(str::to_string).ok_or("the host triple could not be determined; pass the linker setting with --set instead")?;
        exp.overlays.push(format!("target.{}.rustflags=[\"-C\",\"link-arg=-fuse-ld={}\"]", host, linker));
    }
    if let Some(v) = a.opt("--verify")? {
        let parts: Vec<OsString> = v.split_whitespace().map(OsString::from).collect();
        if parts.is_empty() {
            return Err("--verify needs Cargo arguments, for example --verify 'test'".into());
        }
        exp.verify = Some(parts);
    }
    let name = a.opt("--name")?;
    let opts = bench_options(&mut a, "clean")?;
    let preset = a.positional();
    a.finish()?;
    if let Some(p) = &preset {
        if p != "codegen" && p != "linker" {
            return Err(format!("unknown experiment kind `{}` (codegen, linker, or none)", p));
        }
    }
    exp.name = name.unwrap_or_else(|| preset.clone().unwrap_or_else(|| "candidate".into()));
    if !exp.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || exp.name.is_empty() {
        return Err(format!("experiment name {:?} may only contain letters, digits, `-` and `_`", exp.name));
    }
    let result = bench::experiment(&exp, &opts, &ctx.store, &ctx.cfg)?;
    if format == "json" {
        ctx.print_json(&result);
    } else {
        print!("{}", render::experiment(&result));
    }
    Ok(if result["status"] == json!("invalid") { 1 } else { 0 })
}

/// `--run` / `--benchmark` selection, read before positionals.
struct SubjectFlags {
    run: Option<String>,
    benchmark: Option<String>,
}

impl SubjectFlags {
    fn read(a: &mut Args) -> Result<SubjectFlags, String> {
        let s = SubjectFlags { run: a.opt("--run")?, benchmark: a.opt("--benchmark")? };
        if s.run.is_some() && s.benchmark.is_some() {
            return Err("--run and --benchmark are mutually exclusive".into());
        }
        Ok(s)
    }

    fn resolve(&self, ctx: &Ctx) -> Result<Value, String> {
        match &self.benchmark {
            Some(b) => compare::resolve_subject(&ctx.store, &ctx.cfg, &format!("benchmark:{}", b)),
            None => Ok(compare::summarize_run(&ctx.report(self.run.as_deref())?)),
        }
    }
}

fn cmd_baseline(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let subject = SubjectFlags::read(&mut a)?;
    let ctx = Ctx::load()?;
    let root = ctx.root()?.to_path_buf();
    match a.positional().as_deref() {
        Some("set") => {
            let name = a.positional().ok_or("usage: haul baseline set NAME [--run REF | --benchmark ID]")?;
            a.finish()?;
            let subject = subject.resolve(&ctx)?;
            if subject["succeeded"] != json!(true) {
                return Err("refusing to promote a failed run to a baseline".into());
            }
            let path = compare::save_baseline(&root, &name, &ctx.export(&subject))?;
            println!("Baseline `{}` set from {} → {}", name, vstr(&subject, "label"), path.display());
            println!("Commit this file so CI evaluates budgets against the same numbers.");
            Ok(0)
        }
        Some("list") => {
            a.finish()?;
            let names = compare::list_baselines(&root);
            if names.is_empty() {
                println!("No baselines in {}.", compare::baseline_dir(&root).display());
            }
            for n in names {
                let b = compare::load_baseline(&root, &n)?;
                println!("{:<20} {}  wall {}  ({})", n, vstr(&b["scenario"], "name"), b["metrics"]["wall_time_ns"].as_u64().map(human_dur).unwrap_or_else(|| "unavailable".into()), vstr(&b["baseline"], "promoted_at"));
            }
            Ok(0)
        }
        Some("show") => {
            let name = a.positional().ok_or("usage: haul baseline show NAME")?;
            a.finish()?;
            println!("{}", serde_json::to_string_pretty(&compare::load_baseline(&root, &name)?).unwrap_or_default());
            Ok(0)
        }
        _ => Err("usage: haul baseline set NAME | list | show NAME".into()),
    }
}

fn cmd_ci(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    let budgets_file = a.opt("--budgets")?;
    let baseline_flag = a.opt("--baseline")?;
    let subject = SubjectFlags::read(&mut a)?;
    if a.positional().as_deref() != Some("evaluate") {
        return Err("usage: haul ci evaluate [--run REF | --benchmark ID] [--baseline NAME] [--budgets FILE]".into());
    }
    a.finish()?;
    let ctx = Ctx::load()?;
    let root = ctx.root()?.to_path_buf();
    let budgets = match budgets_file {
        Some(file) => {
            let path = ctx.cwd.join(&file);
            compare::parse_budgets(&std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?, &file)?
        }
        None => compare::load_budgets(&root, &ctx.cfg)?,
    };
    let baseline_name = baseline_flag.unwrap_or_else(|| ctx.cfg.str("ci.baseline").to_string());
    let candidate = subject.resolve(&ctx)?;
    let relative = budgets.budgets.iter().any(|b| b.max_increase_percent.is_some() || b.max_decrease_percent.is_some());
    // A missing baseline is an error only when a budget actually needs one.
    let baseline = match compare::load_baseline(&root, &baseline_name) {
        Ok(b) => Some(b),
        Err(e) if relative => return Err(e),
        Err(_) => None,
    };
    let result = compare::evaluate(&budgets, baseline.as_ref(), &ctx.export(&candidate));
    // The verdict goes to stdout of this management command only; Cargo's streams are never involved (AC-016).
    if format == "json" {
        ctx.print_json(&result);
    } else {
        print!("{}", render::budget(&result));
    }
    Ok(compare::exit_code(&result, &budgets).unwrap_or(0))
}

fn build_roots(ctx: &Ctx) -> Result<Vec<PathBuf>, String> {
    ctx.root()?;
    let meta = discover::metadata(&ctx.cfg, &ctx.cwd, None, None)?;
    let mut roots: Vec<PathBuf> = Vec::new();
    for key in ["build_directory", "target_directory"] {
        if let Some(d) = meta.value[key].as_str().map(PathBuf::from) {
            if d.is_dir() && !roots.contains(&d) {
                roots.push(d);
            }
        }
    }
    Ok(roots)
}

fn cmd_inventory(raw: &[OsString], gc: bool) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    let caches = !gc && a.flag("--caches");
    let ctx = Ctx::load()?;
    let mut opts = inventory::Options { stale_days: ctx.cfg.int("artifacts.stale-days"), now_ns: util::now_wall_ns(), size_budget: None };
    if gc {
        if a.flag("--apply") {
            return Err("this release plans garbage collection but does not delete: only `haul gc --dry-run` is available. Use `cargo clean` for Cargo's own removal semantics.".into());
        }
        // Destructive behaviour is never implicit: the plan must be asked for by name.
        if !a.flag("--dry-run") {
            return Err("`haul gc` requires --dry-run: it computes a plan and deletes nothing".into());
        }
        if let Some(d) = a.number::<u64>("--older-than")? {
            opts.stale_days = d;
        }
        if let Some(s) = a.opt("--max-size")? {
            opts.size_budget = Some(util::parse_bytes(&s).ok_or_else(|| format!("--max-size {:?} is not a size (examples: 20GiB, 500MB)", s))?);
        }
    }
    a.finish()?;
    ctx.finalize_pending()?;
    let roots = build_roots(&ctx)?;
    let known = waste::Index::load(&ctx.store.index_path())?.knowledge(&empty_graph());
    let scans: Vec<Value> = roots.iter().map(|r| inventory::scan(r, &known, &opts)).collect();
    let global = if caches { inventory::scan_global_caches() } else { Vec::new() };
    if format == "json" {
        ctx.print_json(&json!({ "schema_version": crate::SCHEMA_VERSION, "build_directories": scans, "shared_caches": global }));
        return Ok(0);
    }
    if scans.is_empty() {
        println!("No build directory exists yet for this workspace; nothing to inventory.");
    }
    let mut out = String::new();
    for s in &scans {
        if gc {
            render::gc_plan(s, &mut out);
        } else {
            out.push_str("Build directory\n");
            render::inventory(s, &mut out);
        }
    }
    print!("{}", out);
    for g in global {
        println!("Shared cache [{}]  {}  — {} allocated, {} files", vstr(&g, "category"), vstr(&g, "root"), g["allocated_bytes"].as_u64().map(human_bytes).unwrap_or_default(), g["files"]);
    }
    Ok(0)
}

fn cmd_policy(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    if a.positional().as_deref() != Some("explain") {
        return Err("usage: haul policy explain [--format text|json]".into());
    }
    a.finish()?;
    let ctx = Ctx::load()?;
    let settings = ctx.cfg.explain();
    let workload = if util::is_ci() { "ci" } else { "local-dev" };
    if format == "json" {
        // Configuration values hold no credentials; anything secret-looking would still print as <redacted> (CFG-003).
        let list: Vec<Value> = settings.iter().map(|s| json!({ "key": s.key, "value": if util::is_secret_name(s.key) { "<redacted>".to_string() } else { s.value.clone() }, "origin": s.origin, "env": s.env, "description": s.help })).collect();
        ctx.print_json(&json!({ "workload": workload, "repository_config": ctx.cfg.repo_config.as_ref().map(|p| path_str(p)), "user_config": path_str(&util::haul_home().join("config.toml")), "settings": list }));
        return Ok(0);
    }
    println!("Effective Haul configuration (precedence: default < user < repository < environment < flag)");
    println!("  user config        {}", util::haul_home().join("config.toml").display());
    println!("  repository config  {}", ctx.cfg.repo_config.as_ref().map(|p| path_str(p)).unwrap_or_else(|| "none found".into()));
    println!("  workload class     {}  (decides which incremental.* policy applies)", workload);
    println!();
    println!("  {:<32} {:<22} {:<34} ORIGIN", "KEY", "VALUE", "ENVIRONMENT VARIABLE");
    for s in settings {
        println!("  {:<32} {:<22} {:<34} {}", s.key, s.value, s.env, s.origin);
        println!("      {}", s.help);
    }
    Ok(0)
}

fn cmd_cache(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    if a.positional().as_deref() != Some("status") {
        return Err("usage: haul cache status [--format text|json]".into());
    }
    a.finish()?;
    let ctx = Ctx::load()?;
    let subset = resolve::cargo_config_subset(&ctx.cwd, &[]);
    let existing = resolve::existing_rustc_wrapper(&subset);
    let provider = ctx.cfg.str("cache.provider");
    let sccache = existing.as_ref().filter(|(w, _)| crate::cache::is_sccache(w)).map(|(w, _)| w.clone()).or_else(|| (provider == "sccache").then(|| util::which("sccache").map(|p| path_str(&p))).flatten());
    let (state, stats) = match &sccache {
        Some(bin) => match crate::cache::stats(bin) {
            Ok(s) => ("local", Some(s)),
            Err(e) => {
                eprintln!("haul: warning: {}", e);
                ("degraded", None)
            }
        },
        None if provider == "sccache" => ("degraded", None),
        None => ("disabled", None),
    };
    let last = ctx.store.resolve_run_ref("latest", true).ok().and_then(|id| ctx.store.load_run(&id)).map(|r| json!({ "run": r["run"]["id"], "cache": r["cache"] }));
    let v = json!({
        "provider_policy": provider, "policy_origin": ctx.cfg.origin("cache.provider"), "fail": ctx.cfg.str("cache.fail"),
        "existing_rustc_wrapper": existing.as_ref().map(|(w, s)| json!({ "wrapper": w, "source": s })),
        "sccache": sccache, "state": state, "server_statistics": stats, "last_analysed_run": last,
    });
    if format == "json" {
        ctx.print_json(&v);
        return Ok(0);
    }
    println!("Compiler cache: {}", state);
    println!("  policy            cache.provider = {} ({}), cache.fail = {}", provider, ctx.cfg.origin("cache.provider"), ctx.cfg.str("cache.fail"));
    match &existing {
        Some((w, s)) => println!("  existing wrapper  {} ({}) — preserved; the Haul wrapper runs outside it", w, s),
        None => println!("  existing wrapper  none"),
    }
    if let Some(s) = &v["server_statistics"].as_object() {
        let st = &s["stats"];
        let count = |v: &Value| v["counts"].as_object().map(|o| o.values().filter_map(Value::as_u64).sum::<u64>()).unwrap_or(0);
        let (h, m) = (count(&st["cache_hits"]), count(&st["cache_misses"]));
        println!("  sccache           {} at {}", s["version"].as_str().unwrap_or(""), s["cache_location"].as_str().unwrap_or(""));
        println!("  since server start {}/{} cacheable requests hit, {} non-cacheable, {} compile requests", h, h + m, st["requests_not_cacheable"].as_u64().unwrap_or(0) + st["non_cacheable_compilations"].as_u64().unwrap_or(0), st["compile_requests"]);
        println!("  cache size        {} of {}", s["cache_size"].as_u64().map(human_bytes).unwrap_or_else(|| "unavailable".into()), s["max_cache_size"].as_u64().map(human_bytes).unwrap_or_else(|| "unavailable".into()));
    }
    match &v["last_analysed_run"] {
        Value::Null => println!("  last run          no analysed build run yet"),
        l => {
            println!("  last run          {}", vstr(l, "run"));
            let mut out = String::new();
            render::cache(l, &mut out);
            print!("{}", out);
        }
    }
    Ok(0)
}

fn cmd_fix(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let ctx = Ctx::load()?;
    if let Some(id) = a.opt("--apply")? {
        a.finish()?;
        let r = remedy::apply(&ctx.store, &id)?;
        println!("Applied {} to {}. Undo with: {}", id, vstr(&r, "file"), vstr(&r["rollback"], "command"));
        return Ok(0);
    }
    if let Some(id) = a.opt("--rollback")? {
        a.finish()?;
        let r = remedy::rollback(&ctx.store, &id)?;
        println!("Rolled back {} ({} restored).", id, vstr(&r, "file"));
        return Ok(0);
    }
    // Remediation is preview-only unless a specific previewed change is applied by id (FR-170.1).
    if !a.flag("--dry-run") {
        return Err("usage: haul fix --dry-run [REC_ID] | --apply REMEDIATION_ID | --rollback REMEDIATION_ID   (for `cargo fix`, use `haul upstream fix` or the cargo shim)".into());
    }
    let format = a.format(&["text", "json"])?;
    let patch = a.flag("--patch");
    let run = a.opt("--run")?;
    let option = a.opt("--option")?;
    let only = a.positional();
    a.finish()?;
    let root = ctx.root()?.to_path_buf();
    let report = ctx.report(run.as_deref())?;
    let previews = remedy::preview(&report, &root, only.as_deref(), option.as_deref())?;
    remedy::save_previews(&ctx.store, &root, &previews)?;
    if format == "json" {
        ctx.print_json(&json!(previews));
    } else if previews.is_empty() {
        println!("No recommendations in run {}; nothing to remediate.", vstr(&report["run"], "id"));
    } else {
        print!("{}", render::remediations(&previews, patch));
    }
    Ok(0)
}

fn version_of(program: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program).args(args).env(crate::ENV_EXEC_GUARD, format!("{}:doctor", std::process::id())).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string())
}

fn cmd_doctor(raw: &[OsString]) -> CmdResult {
    Args::new(raw).finish()?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut problems = 0;
    let mut problem = |msg: String| {
        problems += 1;
        println!("  PROBLEM: {}", msg);
    };
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "(unset)".into());
    let (exe, _) = resolve::self_identity();
    println!("Haul {}", env!("CARGO_PKG_VERSION"));
    println!("  haul binary        {}", exe.display());
    let shim = session::shim_path();
    println!("  shim path          {}", shim.as_ref().map(|p| path_str(p)).unwrap_or_else(|| "not installed on PATH (`haul install-shim`); `haul <command>` works without it".into()));
    let wrapper = exe.parent().map(|d| d.join(if cfg!(windows) { "haul-rustc-wrapper.exe" } else { "haul-rustc-wrapper" }));
    match wrapper.filter(|w| util::is_executable(w)) {
        Some(w) => println!("  rustc wrapper      {}", w.display()),
        None => problem("haul-rustc-wrapper is not next to the haul binary; builds are delegated but rustc-level observation is off".into()),
    }
    let cfg = match Config::load(&cwd) {
        Ok(c) => Some(c),
        Err(e) => {
            problem(format!("invalid Haul configuration: {}", e));
            None
        }
    };
    println!("Upstream");
    match resolve::resolve_upstream(&resolve::classify(&[]), cfg.as_ref().unwrap_or(&Config::defaults())) {
        Ok(up) => {
            println!("  real Cargo path    {}  (selected by: {})", up.program.display(), up.rule);
            match version_of(&up.program, &["-V"]) {
                Some(v) => println!("  Cargo version      {}", v),
                None => problem(format!("{} -V failed", up.program.display())),
            }
            if let Some(s) = &shim {
                // The shim only takes effect if it precedes the real Cargo on PATH.
                if util::which("cargo").map(|first| util::canon(&first) != util::canon(s)).unwrap_or(false) {
                    problem(format!("{} is not the first `cargo` on PATH; another cargo shadows the shim", s.display()));
                }
            }
        }
        Err(e) => problem(e),
    }
    match util::which("rustup").or_else(|| util::cargo_home().map(|h| h.join("bin").join("rustup")).filter(|p| util::is_executable(p))) {
        Some(r) => {
            println!("  rustup             {}  ({})", r.display(), version_of(&r, &["--version"]).unwrap_or_else(|| "version unavailable".into()));
            println!("  active toolchain   {}", version_of(&r, &["show", "active-toolchain"]).unwrap_or_else(|| "unavailable".into()));
        }
        None => println!("  rustup             not found (supported, not required)"),
    }
    println!("Environment");
    for k in ["CARGO", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTUP_TOOLCHAIN", "CARGO_TARGET_DIR", "CARGO_INCREMENTAL", crate::ENV_REAL_CARGO, crate::ENV_HOME] {
        println!("  {:<24} {}", k, env(k));
    }
    let subset = resolve::cargo_config_subset(&cwd, &[]);
    match resolve::existing_rustc_wrapper(&subset) {
        Some((w, src)) => println!("  existing rustc wrapper   {} ({}) — preserved inside the Haul wrapper", w, src),
        None => println!("  existing rustc wrapper   none"),
    }
    println!("Workspace");
    match (resolve::guess_workspace_root(&cwd), &cfg) {
        (Some(root), Some(cfg)) => {
            println!("  root               {}", root.display());
            match discover::metadata(cfg, &cwd, None, None) {
                Ok(m) => {
                    println!("  target directory   {}", m.value["target_directory"].as_str().unwrap_or("unavailable"));
                    println!("  build directory    {}", m.value["build_directory"].as_str().or(m.value["target_directory"].as_str()).unwrap_or("unavailable"));
                }
                Err(e) => problem(format!("cargo metadata failed: {}", e)),
            }
            let t = discover::toolchain_fingerprint(cfg, &cwd, None, None);
            println!("  capabilities       {}", t["capabilities"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default());
            let ws = WsStore::open(Some(&root));
            println!("  recorded runs      {}", ws.session_ids().len());
            if let Ok(rd) = std::fs::read_dir(ws.dir.join("restore")) {
                for e in rd.filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false)) {
                    problem(format!("an interrupted benchmark left a source edit journal at {}; run `haul benchmark` again to restore, or inspect it", e.path().display()));
                }
            }
        }
        (None, _) => println!("  not inside a Cargo workspace"),
        (Some(root), None) => println!("  root               {}", root.display()),
    }
    println!("State");
    let home = util::haul_home();
    println!("  haul home          {}", home.display());
    if let Err(e) = util::create_private_dir(&home).and_then(|_| util::write_atomic_fast(&home.join(".doctor"), b"ok")).and_then(|_| std::fs::remove_file(home.join(".doctor"))) {
        problem(format!("{} is not writable ({}); builds still run but are not recorded", home.display(), e));
    }
    println!("  active sessions    {}", store::active_sessions().len());
    if let Some(cfg) = &cfg {
        println!("  cache policy       provider {} · fail {}", cfg.str("cache.provider"), cfg.str("cache.fail"));
        println!("  nested cargo       {}", cfg.str("compatibility.nested-cargo"));
    }
    match util::which("sccache") {
        Some(s) => println!("  sccache            {} ({})", s.display(), version_of(&s, &["--version"]).unwrap_or_default()),
        None => println!("  sccache            not installed"),
    }
    if problems == 0 {
        println!("\nNo problems found.");
        Ok(0)
    } else {
        println!("\n{} problem(s) found.", problems);
        Ok(1)
    }
}

fn install_record() -> PathBuf {
    util::haul_home().join("install.json")
}

fn cmd_install(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let dir = a.opt("--dir")?.map(PathBuf::from).unwrap_or_else(|| util::haul_home().join("shim"));
    a.finish()?;
    if install_record().exists() {
        return Err(format!("a shim installation is already recorded in {}; run `haul uninstall-shim` first", install_record().display()));
    }
    let (exe, _) = resolve::self_identity();
    let dir = std::env::current_dir().map_err(|e| e.to_string())?.join(dir);
    let shim = dir.join(if cfg!(windows) { "cargo.exe" } else { "cargo" });
    // Never replace an existing cargo (rustup's proxy in particular): the original setup must survive (MIG-001).
    if std::fs::symlink_metadata(&shim).is_ok() {
        return Err(format!("{} already exists; choose an empty directory with --dir. Haul never overwrites an existing cargo.", shim.display()));
    }
    let dir_created = !dir.exists();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&exe, &shim).map_err(|e| format!("cannot create {}: {}", shim.display(), e))?;
    #[cfg(not(unix))]
    std::fs::hard_link(&exe, &shim).or_else(|_| std::fs::copy(&exe, &shim).map(|_| ())).map_err(|e| format!("cannot create {}: {}", shim.display(), e))?;
    let upstream = resolve::resolve_upstream(&resolve::classify(&[]), &Config::defaults()).ok().map(|u| path_str(&u.program));
    util::write_json(&install_record(), &json!({
        "shim": path_str(&shim), "dir": path_str(&dir), "dir_created": dir_created, "target": path_str(&exe),
        "upstream_cargo_at_install": upstream, "installed_at": util::iso8601(util::now_wall_ns()),
    }))
    .map_err(|e| format!("the shim was created but the install record could not be written: {}", e))?;
    println!("Installed the Haul cargo shim at {}", shim.display());
    println!("Put its directory first on PATH to make `cargo` resolve to Haul:\n");
    println!("    export PATH=\"{}:$PATH\"\n", dir.display());
    println!("Nothing else was changed: rustup, the real Cargo ({}) and your Cargo configuration are untouched.", upstream.unwrap_or_else(|| "not found".into()));
    println!("Escape hatch: `haul upstream <args>`. Remove with `haul uninstall-shim`. Check with `haul doctor`.");
    Ok(0)
}

fn cmd_uninstall(raw: &[OsString]) -> CmdResult {
    Args::new(raw).finish()?;
    let rec = util::read_json(&install_record()).ok_or_else(|| format!("no shim installation is recorded in {}", install_record().display()))?;
    let shim = PathBuf::from(vstr(&rec, "shim"));
    let (exe, id) = resolve::self_identity();
    match std::fs::symlink_metadata(&shim) {
        // Only remove what is verifiably ours.
        Ok(_) if util::canon(&shim) == exe || (id.is_some() && util::file_identity(&shim) == id) || util::canon(&shim) == Path::new(&vstr(&rec, "target")) => {
            std::fs::remove_file(&shim).map_err(|e| format!("cannot remove {}: {}", shim.display(), e))?;
            println!("Removed {}", shim.display());
        }
        Ok(_) => return Err(format!("{} is no longer the Haul shim; leaving it in place. Remove {} manually once you have checked it.", shim.display(), install_record().display())),
        Err(_) => println!("{} was already gone", shim.display()),
    }
    if rec["dir_created"] == json!(true) {
        // remove_dir only succeeds on an empty directory, so nothing else can be lost.
        if std::fs::remove_dir(vstr(&rec, "dir")).is_ok() {
            println!("Removed {}", vstr(&rec, "dir"));
        }
    }
    std::fs::remove_file(install_record()).map_err(|e| format!("cannot remove {}: {}", install_record().display(), e))?;
    println!("`cargo` now resolves to {} again. Recorded runs remain in {}; delete that directory to remove them.", rec["upstream_cargo_at_install"].as_str().unwrap_or("your original Cargo"), util::haul_home().display());
    Ok(0)
}
