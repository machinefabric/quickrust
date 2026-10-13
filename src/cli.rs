//! The `qrust` management CLI and the multi-call entry point.
//!
//! Invoked as `cargo`, the binary is the compatibility shim and forwards its
//! arguments untouched. Enhanced commands exist only under `qrust`, so they can
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
qrust: Cargo, faster. Every Cargo command, with Rust toolchains, a shared build cache and output
that QuickRust manages

USAGE
    cargo <args...>                  any Cargo command, through QuickRust (`qrust setup` makes `cargo` QuickRust)
    qrust <cargo-command> [args...]  the same
    qrust upstream <args...>         the toolchain's own Cargo as it is: no QuickRust settings, nothing recorded

PROGRAMS (built through QuickRust's cache, installed into ~/.qrust/bin)
    qrust install <CRATE>[@VERSION]... [--git URL [--branch B | --tag T | --rev R] | --path DIR]
                  [--locked] [--force] [--features F] [--bin NAME]... [--root DIR] [--profile P] [--target T]
    qrust install --list
    qrust uninstall <CRATE>... [--bin NAME]... [--root DIR]

OUTPUT OF A BUILD (stored for every build QuickRust rendered in a terminal)
    qrust warnings [--run REF] [--crate NAME] [--lint LINT]
    qrust errors [--run REF] [--crate NAME]
    qrust log [--run REF] [--raw]

ANALYSIS
    qrust analyze [SECTION] [--run REF] [--format text|json]
                  [--filter FIELD=VALUE]... [--sort FIELD[:asc|desc]] [--limit N]
                                     SECTION: timing rebuilds duplication scripts proc-macros
                                              codegen cache storage recommendations worktrees
                                     filter/sort/limit work on a section's records, e.g.
                                     `qrust analyze timing --filter source=registry --sort duration_ns`
    qrust report [--run REF] [--format text|json|html] [-o FILE]
    qrust runs [--limit N] [--format text|json]
    qrust inspect [--format text|json]       workspace, toolchain and environment fingerprint
    qrust driver [status|build] [--rustc PATH] QuickRust's compiler driver for this toolchain
    qrust compare <BASE> <CANDIDATE> [--format text|json]

MEASUREMENT
    qrust benchmark --scenario NAME [--reps N] [--warmups N] [--edit FILE] [--allow-mutation]
                    [--timeout DURATION] [--format text|json] [-- <cargo args>]
    qrust benchmark --list
    qrust experiment [codegen|linker] [--name NAME] [--set KEY=VALUE]... [--env KEY=VALUE]...
                    [--codegen-units N] [--debug VALUE] [--opt-level VALUE] [--lto VALUE] [--linker NAME]
                    [--scenario NAME] [--reps N] [--warmups N] [--verify '<cargo args>'] [-- <cargo args>]

BUDGETS
    qrust baseline set NAME [--run REF | --benchmark ID]
    qrust baseline list | show NAME
    qrust ci evaluate [--run REF | --benchmark ID] [--baseline NAME] [--budgets FILE] [--format text|json]

STORAGE
    qrust inventory [--caches] [--older-than DAYS] [--max-size SIZE] [--format text|json]
                                     what this workspace's build directory holds, and what in it is stale
    qrust gc [--dry-run] [--to SIZE] [--format text|json]
                                     collect QuickRust's cache now (it does so by itself when it must: see
                                     cache.max-size and cache.min-free)

POLICY AND REMEDIATION
    qrust policy explain [--format text|json]
    qrust cache status [--format text|json]
    qrust fix --dry-run [REC_ID] [--run REF] [--option ALTERNATIVE] [--patch] [--format text|json]
    qrust fix --apply REMEDIATION_ID | --rollback REMEDIATION_ID

BUILD OUTPUT LOCATION (QuickRust keeps each workspace's build directory in its cache, not in `target/`)
    qrust path                       this workspace's target directory
    qrust path bin <NAME> [--release | --profile P] [--target T]
                                     where `cargo build` puts binary NAME

TOOLCHAINS (installed on first use; `+<toolchain>`, RUSTUP_TOOLCHAIN or rust-toolchain.toml choose)
    qrust toolchain list
    qrust toolchain install <TOOLCHAIN>... [--profile minimal|default|complete] [--component C]... [--target T]...
    qrust toolchain update [--channel C]  bring stable, beta, nightly and 1.xx to their latest release
                                          (QuickRust checks once a day in the background; `toolchain.auto-update`)
    qrust toolchain remove <TOOLCHAIN>...
    qrust toolchain default [<TOOLCHAIN>]
    qrust toolchain link <NAME> <DIR> | unlink <NAME>
    qrust toolchain which [<TOOL>] [--toolchain T]
    qrust toolchain component add|remove <NAME>... [--toolchain T]
    qrust toolchain target add|remove <TRIPLE>... [--toolchain T]

INSTALLATION
    qrust doctor
    qrust setup [--no-modify-path] [--toolchain T|none]
                                     install QuickRust for this user: ~/.qrust/bin first on PATH (cargo, rustc,
                                     rustfmt, ... are QuickRust), credentials and registries imported once from
                                     ~/.cargo, the stable toolchain installed
    qrust self update                install QuickRust's latest release in place of this one
    qrust self uninstall --yes       remove ~/.qrust and the PATH lines

RUN REFERENCES
    latest, latest~N, a run id or a unique prefix/suffix of one; in `compare`
    also baseline:NAME and benchmark:ID.

QuickRust's own configuration lives in .qrust/config.toml and QRUST_* variables;
`qrust policy explain` shows every effective setting and where it came from.
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
            Some(a) => Err(format!("unexpected argument {:?}; see `qrust help`", a)),
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
        let cfg = Config::load(&cwd).map_err(|e| format!("invalid QuickRust configuration: {}", e))?;
        let root = crate::engine::for_command(&resolve::classify(&[]), &cwd, &cfg)?.workspace_root;
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
    let mut args: Vec<OsString> = argv.collect();
    let itself = args.first().is_some_and(|a| a == resolve::AS_QRUST);
    if itself {
        args.remove(0);
        // The runner QuickRust sets while it builds what `cargo run` needs: the program is run
        // afterwards, by `cargo run` itself.
        if args.first().is_some_and(|a| a == crate::ui::NOOP_RUNNER) {
            std::process::exit(0);
        }
    } else {
        let invoked_as = Path::new(&argv0).file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if invoked_as == "cargo" {
            session::run_and_exit(args, Mode::Shim);
        }
        if crate::engine::PROXIES.contains(&invoked_as.as_str()) {
            crate::engine::run_proxy(&invoked_as, args);
        }
    }
    let command = args.first().map(|a| util::lossy(a)).unwrap_or_default();
    let rest = if args.is_empty() { &args[..] } else { &args[1..] };
    let result: CmdResult = match command.as_str() {
        "" | "help" | "--help" | "-h" => {
            print!("{}", HELP);
            Ok(0)
        }
        "version" | "--version" | "-V" => {
            println!("qrust {}", env!("CARGO_PKG_VERSION"));
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
        "toolchain" => cmd_toolchain(rest),
        "path" => cmd_path(rest),
        "compare" => cmd_compare(rest),
        "benchmark" => cmd_benchmark(rest),
        "experiment" => cmd_experiment(rest),
        "baseline" => cmd_baseline(rest),
        "ci" => cmd_ci(rest),
        "inventory" => cmd_inventory(rest),
        "gc" => cmd_gc(rest),
        "policy" => cmd_policy(rest),
        "cache" => cmd_cache(rest),
        "fix" => cmd_fix(rest),
        "doctor" => cmd_doctor(rest),
        "setup" => cmd_setup(rest),
        "self" => cmd_self(rest),
        // Anything else is Cargo syntax (including `+toolchain` and global flags).
        _ => session::run_and_exit(args, Mode::Explicit),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => session::fail(&e),
    }
}

/// `qrust warnings` / `qrust errors` / `qrust log`: the stored output of a rendered build.
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
        return Err("--filter, --sort and --limit apply to the records of one section: `qrust analyze <section> ...`".into());
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
            crate::ui::say(crate::ui::Tag::Ok, &format!("Wrote {}", path));
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
        println!("No recorded runs for this workspace. Run a build through `qrust` or QuickRust's `cargo` first.");
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

/// The compiler of the toolchain a build here uses.
fn toolchain_rustc() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let cfg = crate::config::Config::load(&cwd)?;
    Ok(crate::engine::for_command(&crate::resolve::classify(&[OsString::from("build")]), &cwd, &cfg)?.rustc())
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
            let compiler = crate::driver::Compiler::of(&util::path_str(&rustc))?;
            let vv = &compiler.version_verbose;
            match (crate::driver::find(&compiler), crate::driver::build_failure(&compiler)) {
                (Some(exe), _) => println!("built: {}", exe.display()),
                (None, Some(why)) => println!("could not be built for {}: {}\n`qrust driver build` tries again", vv.lines().next().unwrap_or("this compiler"), why.trim()),
                (None, None) => println!("not built for {} (expected in {}); `qrust driver build` builds it", vv.lines().next().unwrap_or("this compiler"), crate::driver::dir(&compiler)?.display()),
            }
            Ok(0)
        }
        Some(other) => Err(format!("unknown `qrust driver` action `{}` (status, build)", other)),
    }
}

/// `qrust path`: where this workspace's builds go.
fn cmd_path(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let profile = match (a.flag("--release"), a.opt("--profile")?) {
        (true, Some(_)) => return Err("--release and --profile exclude each other".into()),
        (true, None) => "release".to_string(),
        (false, p) => p.unwrap_or_else(|| "dev".into()),
    };
    let target = a.opt("--target")?;
    let what = a.positional();
    let name = a.positional();
    a.finish()?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let cfg = Config::load(&cwd).map_err(|e| format!("invalid QuickRust configuration: {}", e))?;
    let engine = crate::engine::for_command(&resolve::classify(&[]), &cwd, &cfg)?;
    let dir = engine.target.ok_or("not inside a Cargo workspace: there is no target directory here")?.dir;
    match (what.as_deref(), name) {
        (None, None) => println!("{}", dir.display()),
        (Some("bin"), Some(name)) => {
            let profile_dir = match profile.as_str() {
                "dev" | "test" => "debug",
                "release" | "bench" => "release",
                other => other,
            };
            let mut p = dir;
            if let Some(t) = &target {
                p = p.join(t);
            }
            let exe = if target.as_deref().unwrap_or(&crate::toolchain::host()).contains("windows") { format!("{}.exe", name) } else { name };
            println!("{}", p.join(profile_dir).join(exe).display());
        }
        (Some("bin"), None) => return Err("`qrust path bin` needs the binary's name".into()),
        (Some(other), _) => return Err(format!("unknown `qrust path` item `{}` (bin)", other)),
        (None, Some(_)) => unreachable!("a second positional argument needs a first"),
    }
    Ok(0)
}

/// `qrust toolchain …`: QuickRust's Rust toolchains.
fn cmd_toolchain(raw: &[OsString]) -> CmdResult {
    use crate::toolchain::{self, file::Profile, spec::Spec, state, Layout, Needs, Selection, Source};
    let mut a = Args::new(raw);
    let action = match a.items.first() {
        Some(x) if !x.starts_with('-') => a.items.remove(0),
        _ => "list".to_string(),
    };
    let cwd = std::env::current_dir().map_err(|e| format!("cannot determine the current directory: {}", e))?;
    let cfg = Config::load(&cwd).map_err(|e| format!("invalid QuickRust configuration: {}", e))?;
    let server = cfg.str("toolchain.dist-server").to_string();
    let layout = Layout::current();
    let positionals = |a: &mut Args| -> Vec<String> { std::iter::from_fn(|| a.positional()).collect() };
    // The toolchain `--toolchain T` names, or the one this directory uses, installed.
    let chosen = |a: &mut Args, needs: &Needs| -> Result<toolchain::Toolchain, String> {
        let sel = match a.opt("--toolchain")? {
            Some(t) => Selection { spec: Spec::parse(&t)?, source: Source::Plus, file: None },
            None => toolchain::select(&layout, &cwd, None)?,
        };
        let mut progress = crate::ui::ToolchainProgress::new(&sel.spec.to_string());
        match toolchain::ensure(&layout, &sel, needs, &server, &mut progress) {
            Ok(tc) => {
                progress.finish(&tc);
                Ok(tc)
            }
            Err(e) => {
                progress.fail(&e);
                Err(e)
            }
        }
    };
    match action.as_str() {
        "list" => {
            a.finish()?;
            let st = state::load(&layout.toolchains())?;
            let active = toolchain::select(&layout, &cwd, None);
            let host = toolchain::host();
            let installed = toolchain::installed(&layout);
            if installed.is_empty() && st.linked.is_empty() {
                println!("No toolchains installed yet: the first build installs {}.", st.default);
            }
            for tc in &installed {
                let rec = tc.record();
                let channels: Vec<String> = st.channels.iter().filter(|(_, c)| c.toolchain == tc.id).map(|(k, _)| k.strip_suffix(&format!("-{}", host)).unwrap_or(k).to_string()).collect();
                let targets: Vec<String> = rec.iter().flat_map(|r| r.components.iter().filter(|(p, t, _)| p == "rust-std" && *t != r.host).map(|(_, t, _)| t.clone())).collect();
                let comps = rec.as_ref().map(|r| r.components.len()).unwrap_or(0);
                println!("{:<44} Rust {:<18} {}{} components{}", tc.id, rec.as_ref().map(|r| r.version.as_str()).unwrap_or("?"), if channels.is_empty() { String::new() } else { format!("{}; ", channels.join(", ")) }, comps, if targets.is_empty() { String::new() } else { format!("; targets {}", targets.join(", ")) });
            }
            for (name, dir) in &st.linked {
                println!("{:<44} linked -> {}", name, dir.display());
            }
            println!();
            println!("default: {}", st.default);
            match active {
                Ok(sel) => println!("here:    {} (from {})", sel.spec, sel.source),
                Err(e) => println!("here:    none: {}", e),
            }
            Ok(0)
        }
        "install" => {
            let profile = a.opt("--profile")?.map(|p| Profile::parse(&p)).transpose()?;
            let components = a.multi("--component")?;
            let targets = a.multi("--target")?;
            let specs = positionals(&mut a);
            a.finish()?;
            if specs.is_empty() {
                return Err("`qrust toolchain install` needs a toolchain, such as stable, 1.98 or nightly".into());
            }
            let needs = Needs { components, targets, profile };
            for spec in specs {
                let sel = Selection { spec: Spec::parse(&spec)?, source: Source::Plus, file: None };
                let mut progress = crate::ui::ToolchainProgress::new(&spec);
                match toolchain::ensure(&layout, &sel, &needs, &server, &mut progress) {
                    Ok(tc) if !progress.worked() => crate::ui::say_out(crate::ui::Tag::Ok, &format!("{} is installed: {}", spec, tc.id)),
                    Ok(tc) => progress.finish(&tc),
                    Err(e) => {
                        progress.fail(&e);
                        return Err(e);
                    }
                }
            }
            Ok(0)
        }
        "update" => {
            let only = a.opt("--channel")?;
            // Started by a build (see `toolchain::start_update`): one at a time, quietly.
            let background = a.flag("--background");
            a.finish()?;
            let _lock = match util::ExclusiveLock::try_acquire(&layout.toolchains().join(".locks").join("update.lock")).map_err(|e| e.to_string())? {
                Some(l) => l,
                None if background => return Ok(0),
                None => util::ExclusiveLock::acquire(&layout.toolchains().join(".locks").join("update.lock")).map_err(|e| e.to_string())?,
            };
            let mut progress = crate::ui::ToolchainProgress::new("update");
            // A channel's new release gets QuickRust's compiler driver before builds use it.
            let wanted = crate::driver::wanted(&Config::load(&std::env::current_dir().map_err(|e| e.to_string())?)?);
            let mut prepare = |tc: &toolchain::Toolchain| {
                if wanted {
                    crate::driver::prepare(tc);
                }
            };
            let moved = toolchain::update(&layout, &server, only.as_deref(), &mut prepare, &mut progress).and_then(|m| toolchain::retire(&layout, &mut progress).map(|r| (m, r)));
            let (moved, retired) = match moved {
                Ok(m) => m,
                Err(e) => {
                    progress.fail(&e);
                    return Err(e);
                }
            };
            progress.clear();
            if moved.is_empty() {
                crate::ui::say_out(crate::ui::Tag::Ok, "Every channel is at its latest release");
            }
            for m in moved {
                crate::ui::say_out(crate::ui::Tag::Ok, &format!("Updated {}: {} -> {} (the old one is removed after a day without use)", m.channel, m.from, m.to));
            }
            for r in retired {
                crate::ui::say_out(crate::ui::Tag::Ok, &format!("Removed {}, which no channel names and which was not used for a day", r));
            }
            Ok(0)
        }
        "remove" => {
            let ids = positionals(&mut a);
            a.finish()?;
            if ids.is_empty() {
                return Err("`qrust toolchain remove` needs a toolchain (`qrust toolchain list` shows them)".into());
            }
            let st = state::load(&layout.toolchains())?;
            for given in ids {
                // A channel name removes the toolchain it names.
                let id = st.channels.get(&format!("{}-{}", given, toolchain::host())).map(|c| c.toolchain.clone()).unwrap_or(given);
                toolchain::remove(&layout, &id, &mut crate::ui::ToolchainProgress::new(&id))?;
                crate::ui::say_out(crate::ui::Tag::Ok, &format!("Removed {}", id));
            }
            Ok(0)
        }
        "default" => {
            let spec = a.positional();
            a.finish()?;
            match spec {
                None => println!("{}", state::load(&layout.toolchains())?.default),
                Some(s) => {
                    let spec = Spec::parse(&s)?;
                    if matches!(spec, Spec::Path(_)) {
                        return Err("the default toolchain is a release, a channel or a linked toolchain's name".into());
                    }
                    state::update(&layout.toolchains(), |st| {
                        if let Spec::Named(n) = &spec {
                            if !st.linked.contains_key(n) {
                                return Err(format!("no toolchain is named `{}` (`qrust toolchain link {} <dir>` names one)", n, n));
                            }
                        }
                        st.default = s.clone();
                        Ok(())
                    })?;
                    crate::ui::say_out(crate::ui::Tag::Ok, &format!("Default toolchain: {}", s));
                }
            }
            Ok(0)
        }
        "link" => {
            let name = a.positional().ok_or("`qrust toolchain link` needs a name and a directory")?;
            let dir = a.positional().ok_or("`qrust toolchain link` needs a name and a directory")?;
            a.finish()?;
            if !crate::toolchain::spec::is_link_name(&name) || !matches!(Spec::parse(&name), Ok(Spec::Named(_))) {
                return Err(format!("`{}` cannot name a linked toolchain: use letters, digits, `-`, `_` and `.`, and not a channel or release name", name));
            }
            let dir = std::fs::canonicalize(&dir).map_err(|e| format!("{}: {}", dir, e))?;
            let tc = toolchain::Toolchain { id: name.clone(), dir: dir.clone(), linked: true };
            for tool in ["rustc", "cargo"] {
                let bin = tc.bin(tool);
                if !util::is_executable(&bin) {
                    return Err(format!("{} is not a toolchain: it has no {}", dir.display(), bin.display()));
                }
                if std::env::current_exe().ok().and_then(|e| util::file_identity(&e)).is_some_and(|me| util::file_identity(&bin) == Some(me)) {
                    return Err(format!("{} is QuickRust itself, not a toolchain's {}", bin.display(), tool));
                }
            }
            state::update(&layout.toolchains(), |st| {
                st.linked.insert(name.clone(), dir.clone());
                Ok(())
            })?;
            crate::ui::say_out(crate::ui::Tag::Ok, &format!("Linked {} -> {}", name, dir.display()));
            Ok(0)
        }
        "unlink" => {
            let name = a.positional().ok_or("`qrust toolchain unlink` needs a name")?;
            a.finish()?;
            state::update(&layout.toolchains(), |st| st.linked.remove(&name).map(|_| ()).ok_or_else(|| format!("no toolchain is linked as `{}`", name)))?;
            crate::ui::say_out(crate::ui::Tag::Ok, &format!("Unlinked {}", name));
            Ok(0)
        }
        "which" => {
            let tc = chosen(&mut a, &Needs::default())?;
            let tool = a.positional();
            a.finish()?;
            match tool {
                Some(t) => {
                    let bin = tc.bin(&t);
                    if !bin.is_file() {
                        return Err(format!("toolchain {} has no {}", tc.id, t));
                    }
                    println!("{}", bin.display());
                }
                None => println!("{}", tc.dir.display()),
            }
            Ok(0)
        }
        "component" | "target" => {
            let verb = a.positional().ok_or_else(|| format!("`qrust toolchain {}` needs add or remove", action))?;
            let toolchain_opt = a.opt("--toolchain")?;
            let names = positionals(&mut a);
            a.finish()?;
            if names.is_empty() {
                return Err(format!("`qrust toolchain {} {}` needs at least one {}", action, verb, if action == "target" { "target triple" } else { "component" }));
            }
            let mut b = Args { items: toolchain_opt.iter().flat_map(|t| ["--toolchain".to_string(), t.clone()]).collect(), passthrough: vec![] };
            match verb.as_str() {
                "add" => {
                    let needs = if action == "target" { Needs { targets: names, ..Default::default() } } else { Needs { components: names, ..Default::default() } };
                    let tc = chosen(&mut b, &needs)?;
                    crate::ui::say_out(crate::ui::Tag::Ok, &format!("{} has {}", tc.id, needs.components.iter().chain(&needs.targets).cloned().collect::<Vec<_>>().join(", ")));
                    Ok(0)
                }
                "remove" => {
                    let tc = chosen(&mut b, &Needs::default())?;
                    let rec = tc.record().ok_or_else(|| format!("toolchain {} is linked, and QuickRust does not change a linked toolchain", tc.id))?;
                    let which: Vec<(String, String)> = names
                        .iter()
                        .map(|n| {
                            if action == "target" {
                                return Ok(("rust-std".to_string(), n.clone()));
                            }
                            let found = rec.components.iter().find(|(p, t, _)| (p == n || *p == format!("{}-preview", n)) && (*t == rec.host || t == "*"));
                            found.map(|(p, t, _)| (p.clone(), t.clone())).ok_or_else(|| format!("{} has no component {}", tc.id, n))
                        })
                        .collect::<Result<_, String>>()?;
                    let locked = toolchain::install::lock(&layout, &tc.id, &mut crate::ui::ToolchainProgress::new(&tc.id))?;
                    toolchain::install::remove_components(&locked, &tc.dir, &which)?;
                    crate::ui::say_out(crate::ui::Tag::Ok, &format!("Removed {} from {}", names.join(", "), tc.id));
                    Ok(0)
                }
                other => Err(format!("unknown `qrust toolchain {}` action `{}` (add, remove)", action, other)),
            }
        }
        other => Err(format!("unknown `qrust toolchain` action `{}` (list, install, update, remove, default, link, unlink, which, component, target)", other)),
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
    println!("  machine         {} {}, {}, {} logical CPUs, {}", vstr(m, "os"), vstr(m, "arch"), m["cpu_model"].as_str().unwrap_or("cpu unavailable"), m["logical_cpus"], m["memory_bytes"].as_u64().map(human_bytes).unwrap_or_else(|| "memory unavailable".into()));
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
        _ => return Err("usage: qrust compare <BASE> <CANDIDATE>".into()),
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
            let name = a.positional().ok_or("usage: qrust baseline set NAME [--run REF | --benchmark ID]")?;
            a.finish()?;
            let subject = subject.resolve(&ctx)?;
            if subject["succeeded"] != json!(true) {
                return Err("refusing to promote a failed run to a baseline".into());
            }
            let path = compare::save_baseline(&root, &name, &ctx.export(&subject))?;
            crate::ui::say_out(crate::ui::Tag::Ok, &format!("Baseline `{}` set from {} -> {}", name, vstr(&subject, "label"), path.display()));
            println!("{}Commit this file so CI evaluates budgets against the same numbers.", crate::ui::INDENT);
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
            let name = a.positional().ok_or("usage: qrust baseline show NAME")?;
            a.finish()?;
            println!("{}", serde_json::to_string_pretty(&compare::load_baseline(&root, &name)?).unwrap_or_default());
            Ok(0)
        }
        _ => Err("usage: qrust baseline set NAME | list | show NAME".into()),
    }
}

fn cmd_ci(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    let budgets_file = a.opt("--budgets")?;
    let baseline_flag = a.opt("--baseline")?;
    let subject = SubjectFlags::read(&mut a)?;
    if a.positional().as_deref() != Some("evaluate") {
        return Err("usage: qrust ci evaluate [--run REF | --benchmark ID] [--baseline NAME] [--budgets FILE]".into());
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

fn cmd_inventory(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    let caches = a.flag("--caches");
    let ctx = Ctx::load()?;
    let mut opts = inventory::Options { stale_days: ctx.cfg.int("artifacts.stale-days"), now_ns: util::now_wall_ns(), size_budget: None };
    if let Some(d) = a.number::<u64>("--older-than")? {
        opts.stale_days = d;
    }
    if let Some(s) = a.opt("--max-size")? {
        opts.size_budget = Some(util::parse_bytes(&s).ok_or_else(|| format!("--max-size {:?} is not a size (examples: 20GiB, 500MB)", s))?);
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
        out.push_str("Build directory\n");
        render::inventory(s, &mut out);
        render::gc_plan(s, &mut out);
    }
    print!("{}", out);
    for g in global {
        println!("Shared cache [{}]  {}: {} allocated, {} files", vstr(&g, "category"), vstr(&g, "root"), g["allocated_bytes"].as_u64().map(human_bytes).unwrap_or_default(), g["files"]);
    }
    Ok(0)
}

/// `qrust gc`: QuickRust's cache collected now (see `space`).
fn cmd_gc(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    let dry_run = a.flag("--dry-run");
    let background = a.flag("--background");
    let to = a.opt("--to")?.map(|s| util::parse_bytes(&s).ok_or_else(|| format!("--to {:?} is not a size (examples: 20GiB, 500MB)", s))).transpose()?;
    a.finish()?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let cfg = Config::load(&cwd).map_err(|e| format!("invalid QuickRust configuration: {}", e))?;
    let cache = util::cache_dir();
    let lock_path = cache.join("gc.lock");
    let _lock = match util::ExclusiveLock::try_acquire(&lock_path).map_err(|e| e.to_string())? {
        Some(l) => l,
        // Started by a build while another collection runs: that one does the work.
        None if background => return Ok(0),
        None => util::ExclusiveLock::acquire(&lock_path).map_err(|e| e.to_string())?,
    };
    let fs = crate::space::fs_space(util::qrust_home().as_path()).map_err(|e| format!("cannot measure the free space of {}: {}", util::qrust_home().display(), e))?;
    let policy = crate::space::policy(&cfg, &fs);
    let phase = crate::ui::Phase::start();
    let done = crate::space::collect(&cache, crate::space::downloads().as_ref(), &policy, to, dry_run, None)?;
    if background {
        return Ok(0);
    }
    if format == "json" {
        let removed: Vec<Value> = done.removed.iter().map(|(k, p, b, why)| json!({ "kind": k.name(), "path": path_str(p), "bytes": b, "why": why })).collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "dry_run": dry_run, "before_bytes": done.before, "after_bytes": done.after, "removed": removed, "skipped_in_use": done.skipped_in_use, "max_bytes": policy.max_bytes, "min_free": policy.min_free, "free": fs.available })).unwrap_or_default());
        return Ok(0);
    }
    for (kind, path, bytes, why) in &done.removed {
        println!("{}{} {:<10} {}  {} ({})", crate::ui::INDENT, if dry_run { "would remove" } else { "removed" }, human_bytes(*bytes), kind.name(), path.display(), why);
    }
    let verb = if dry_run { "Would free" } else { "Freed" };
    phase.finish(true, &format!("{} {}: QuickRust's cache {} -> {} (at most {}; keeps {} of the disk free, {} free now){}", verb, human_bytes(done.before.saturating_sub(done.after)), human_bytes(done.before), human_bytes(done.after), human_bytes(policy.max_bytes), human_bytes(policy.min_free), human_bytes(fs.available), if done.skipped_in_use > 0 { format!(", {} in use left alone", done.skipped_in_use) } else { String::new() }));
    Ok(0)
}

fn cmd_policy(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let format = a.format(&["text", "json"])?;
    if a.positional().as_deref() != Some("explain") {
        return Err("usage: qrust policy explain [--format text|json]".into());
    }
    a.finish()?;
    let ctx = Ctx::load()?;
    let settings = ctx.cfg.explain();
    let workload = if util::is_ci() { "ci" } else { "local-dev" };
    if format == "json" {
        // Configuration values hold no credentials; anything secret-looking would still print as <redacted> (CFG-003).
        let list: Vec<Value> = settings.iter().map(|s| json!({ "key": s.key, "value": if util::is_secret_name(s.key) { "<redacted>".to_string() } else { s.value.clone() }, "origin": s.origin, "env": s.env, "description": s.help })).collect();
        ctx.print_json(&json!({ "workload": workload, "repository_config": ctx.cfg.repo_config.as_ref().map(|p| path_str(p)), "user_config": path_str(&util::qrust_home().join("config.toml")), "settings": list }));
        return Ok(0);
    }
    println!("Effective QuickRust configuration (precedence: default < user < repository < environment < flag)");
    println!("  user config        {}", util::qrust_home().join("config.toml").display());
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
        return Err("usage: qrust cache status [--format text|json]".into());
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
                crate::ui::say(crate::ui::Tag::Warn, &e);
                ("degraded", None)
            }
        },
        None if provider == "sccache" => ("degraded", None),
        None => ("disabled", None),
    };
    let last = ctx.store.resolve_run_ref("latest", true).ok().and_then(|id| ctx.store.load_run(&id)).map(|r| json!({ "run": r["run"]["id"], "cache": r["cache"] }));
    let qrust_cache = util::cache_dir();
    let fs = crate::space::fs_space(util::qrust_home().as_path()).ok();
    let bounds = fs.map(|f| crate::space::policy(&ctx.cfg, &f));
    let v = json!({
        "provider_policy": provider, "policy_origin": ctx.cfg.origin("cache.provider"), "fail": ctx.cfg.str("cache.fail"),
        "existing_rustc_wrapper": existing.as_ref().map(|(w, s)| json!({ "wrapper": w, "source": s })),
        "sccache": sccache, "state": state, "server_statistics": stats, "last_analysed_run": last,
        "qrust_cache": {
            "dir": path_str(&qrust_cache), "usage": crate::space::usage(&qrust_cache), "recent_collections": crate::space::recent(&qrust_cache, 5),
            "max_bytes": bounds.map(|b| b.max_bytes), "min_free": bounds.map(|b| b.min_free), "free": fs.map(|f| f.available),
        },
    });
    if format == "json" {
        ctx.print_json(&v);
        return Ok(0);
    }
    let hc = &v["qrust_cache"];
    let now = util::now_wall_ns();
    match hc["usage"].as_object().filter(|u| u["counted"] != json!(false)) {
        Some(u) => {
            let total = u["total_bytes"].as_u64().unwrap_or(0);
            println!("QuickRust's cache: {}, counted {}, {}", human_bytes(total), u["scanned_ns"].as_u64().map(|t| util::human_ago(t, now)).unwrap_or_default(), qrust_cache.display());
            let mut kinds_sum = 0;
            for (kind, k) in u["by_kind"].as_object().into_iter().flatten() {
                kinds_sum += k["bytes"].as_u64().unwrap_or(0);
                println!("  {:<24} {:>10}  {}", kind, k["bytes"].as_u64().map(human_bytes).unwrap_or_default(), k["count"]);
            }
            // Build directories hold hard links into the artifact cache: one file, two kinds.
            if kinds_sum > total {
                println!("  {:<24} {:>10}  in files two kinds share (hard links), counted once in the total", "shared", human_bytes(kinds_sum - total));
            }
            if u["unmet"] == json!(true) {
                println!("  {:<24} the last collection could not get within the bounds: what is left was used in the last 15 minutes or is in use", "short of its bounds");
            }
        }
        None => println!("QuickRust's cache: not counted yet, {}; `qrust gc --dry-run` counts it", qrust_cache.display()),
    }
    if let (Some(m), Some(f), Some(free)) = (hc["max_bytes"].as_u64(), hc["min_free"].as_u64(), hc["free"].as_u64()) {
        // A size setting reads back in bytes; `auto` and a share of the disk as written.
        let setting = |key: &str| {
            let v = ctx.cfg.str(key);
            v.parse::<u64>().map(human_bytes).unwrap_or_else(|_| v.to_string())
        };
        println!("  {:<24} at most {} (cache.max-size = {}), keeps {} of the disk free (cache.min-free = {}), {} free now", "bounds", human_bytes(m), setting("cache.max-size"), human_bytes(f), setting("cache.min-free"), human_bytes(free));
    }
    for c in hc["recent_collections"].as_array().into_iter().flatten() {
        let at = util::parse_iso8601(&vstr(c, "at")).map(|t| util::human_ago(t, now)).unwrap_or_else(|| vstr(c, "at"));
        println!("  {:<24} {}, {} -> {}, {} removed", "collected", at, c["before"].as_u64().map(human_bytes).unwrap_or_default(), c["after"].as_u64().map(human_bytes).unwrap_or_default(), c["removed"].as_array().map(|r| r.len()).unwrap_or(0));
    }
    println!();
    // A compiler cache (sccache) besides QuickRust's own: what it is and what it did.
    if state == "disabled" && existing.is_none() {
        println!("Compiler cache: none (cache.provider = {}, {}); QuickRust's artifact cache keeps what QuickRust builds", provider, ctx.cfg.origin("cache.provider"));
        return Ok(0);
    }
    println!("Compiler cache: {}", state);
    println!("  policy            cache.provider = {} ({}), cache.fail = {}", provider, ctx.cfg.origin("cache.provider"), ctx.cfg.str("cache.fail"));
    match &existing {
        Some((w, s)) => println!("  existing wrapper  {} ({}), preserved; the QuickRust wrapper runs outside it", w, s),
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
        crate::ui::say_out(crate::ui::Tag::Ok, &format!("Applied {} to {}; undo with: {}", id, vstr(&r, "file"), vstr(&r["rollback"], "command")));
        return Ok(0);
    }
    if let Some(id) = a.opt("--rollback")? {
        a.finish()?;
        let r = remedy::rollback(&ctx.store, &id)?;
        crate::ui::say_out(crate::ui::Tag::Ok, &format!("Rolled back {} ({} restored)", id, vstr(&r, "file")));
        return Ok(0);
    }
    // Remediation is preview-only unless a specific previewed change is applied by id (FR-170.1).
    if !a.flag("--dry-run") {
        return Err("usage: qrust fix --dry-run [REC_ID] | --apply REMEDIATION_ID | --rollback REMEDIATION_ID   (for `cargo fix`, use `cargo fix` or `qrust upstream fix`)".into());
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
    let out = crate::engine::toolchain_program(program).args(args).env(crate::ENV_EXEC_GUARD, format!("{}:doctor", std::process::id())).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string())
}

fn cmd_doctor(raw: &[OsString]) -> CmdResult {
    Args::new(raw).finish()?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut problems = 0;
    let mut problem = |msg: String| {
        problems += 1;
        crate::ui::say_out(crate::ui::Tag::Fail, &msg);
    };
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "(unset)".into());
    let (exe, _) = resolve::self_identity();
    println!("QuickRust {}", env!("CARGO_PKG_VERSION"));
    println!("  qrust binary       {}", exe.display());
    let (setup, first) = crate::setup::status();
    match &setup {
        Some(r) => println!("  set up             {} (QuickRust {}); its bin is {} on PATH", r["bin"].as_str().unwrap_or("?"), r["version"].as_str().unwrap_or("?"), if first { "first" } else { "NOT first" }),
        None => println!("  set up             no (`qrust setup` installs QuickRust for this user); `qrust <command>` works without it"),
    }
    if setup.is_some() && !first {
        problem(format!("`cargo` on PATH is not {}: open a new shell, or check the PATH set up by `qrust setup`", crate::engine::bin_dir().join("cargo").display()));
    }
    let wrapper = exe.parent().map(|d| d.join(if cfg!(windows) { "qrust-rustc-wrapper.exe" } else { "qrust-rustc-wrapper" }));
    match wrapper.filter(|w| util::is_executable(w)) {
        Some(w) => println!("  rustc wrapper      {}", w.display()),
        None => problem("qrust-rustc-wrapper is not next to the qrust binary; builds are delegated but rustc-level observation is off".into()),
    }
    let cfg = match Config::load(&cwd) {
        Ok(c) => Some(c),
        Err(e) => {
            problem(format!("invalid QuickRust configuration: {}", e));
            None
        }
    };
    println!("Toolchain");
    match crate::engine::for_command(&resolve::classify(&[]), &cwd, cfg.as_ref().unwrap_or(&Config::defaults())) {
        Ok(engine) => {
            println!("  active toolchain   {}  (selected by: {})", engine.toolchain.id, engine.selected_by);
            match version_of(&engine.program, &["-V"]) {
                Some(v) => println!("  Cargo              {}", v),
                None => problem(format!("{} -V failed", engine.program.display())),
            }
            println!("  runs as            {}", engine.program.display());
            match version_of(&engine.rustc(), &["-V"]) {
                Some(v) => println!("  rustc              {}", v),
                None => problem(format!("{} -V failed", engine.rustc().display())),
            }
        }
        Err(e) => problem(e),
    }
    println!("  cargo home         {}", util::cargo_home().display());
    if let Some(theirs) = std::env::var_os("CARGO_HOME").filter(|v| !v.is_empty() && std::path::Path::new(v) != util::cargo_home()) {
        println!("  note               CARGO_HOME={} is set; builds run through QuickRust use {}", theirs.to_string_lossy(), util::cargo_home().display());
    }
    println!("Environment");
    for k in ["CARGO", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTUP_TOOLCHAIN", "CARGO_TARGET_DIR", "CARGO_INCREMENTAL", crate::ENV_HOME] {
        println!("  {:<24} {}", k, env(k));
    }
    let subset = resolve::cargo_config_subset(&cwd, &[]);
    match resolve::existing_rustc_wrapper(&subset) {
        Some((w, src)) => println!("  existing rustc wrapper   {} ({}), preserved inside the QuickRust wrapper", w, src),
        None => println!("  existing rustc wrapper   none"),
    }
    println!("Workspace");
    let root = cfg.as_ref().and_then(|c| crate::engine::for_command(&resolve::classify(&[]), &cwd, c).ok()).and_then(|e| e.workspace_root);
    match (root, &cfg) {
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
                    problem(format!("an interrupted benchmark left a source edit journal at {}; run `qrust benchmark` again to restore, or inspect it", e.path().display()));
                }
            }
        }
        (None, _) => println!("  not inside a Cargo workspace"),
        (Some(root), None) => println!("  root               {}", root.display()),
    }
    println!("State");
    let home = util::qrust_home();
    println!("  qrust home         {}", home.display());
    if let Err(e) = util::create_private_dir(&home).and_then(|_| util::write_atomic_fast(&home.join(".doctor"), b"ok")).and_then(|_| std::fs::remove_file(home.join(".doctor"))) {
        problem(format!("{} is not writable ({}); builds still run but are not recorded", home.display(), e));
    }
    println!("  active sessions    {}", store::active_sessions().len());
    if let Some(cfg) = &cfg {
        println!("  cache policy       provider {}, fail {}", cfg.str("cache.provider"), cfg.str("cache.fail"));
        println!("  nested cargo       {}", cfg.str("compatibility.nested-cargo"));
    }
    match util::which("sccache") {
        Some(s) => println!("  sccache            {} ({})", s.display(), version_of(&s, &["--version"]).unwrap_or_default()),
        None => println!("  sccache            not installed"),
    }
    println!();
    if problems == 0 {
        crate::ui::say_out(crate::ui::Tag::Ok, "");
        Ok(0)
    } else {
        crate::ui::say_out(crate::ui::Tag::Fail, &format!("{} problem{}", problems, if problems == 1 { "" } else { "s" }));
        Ok(1)
    }
}

/// `qrust setup`: QuickRust installed for this user (see `setup.rs`).
fn cmd_setup(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let toolchain = a.opt("--toolchain")?.unwrap_or_else(|| "stable".into());
    let modify_path = !a.flag("--no-modify-path");
    a.finish()?;
    let opts = crate::setup::Options { modify_path, toolchain: (toolchain != "none").then_some(toolchain) };
    let phase = crate::ui::Phase::start();
    let done = crate::setup::setup(&opts)?;
    phase.finish(true, &format!("QuickRust {} installed in {}: cargo, rustc, rustfmt, cargo-clippy and {} more", done.version, util::tilde(&done.bin), done.proxies.saturating_sub(4)));
    let i = crate::ui::INDENT;
    match (&done.path_edits[..], modify_path) {
        (_, false) => eprintln!("{}PATH left alone (--no-modify-path): put {} first on PATH yourself", i, util::tilde(&done.bin)),
        ([], true) => eprintln!("{}PATH: already set up", i),
        (files, true) => eprintln!("{}PATH: {} first, from {}", i, util::tilde(&done.bin), files.iter().map(|f| util::tilde(f)).collect::<Vec<_>>().join(", ")),
    }
    if let Some((from, imported, skipped)) = &done.import {
        if !imported.is_empty() {
            eprintln!("{}imported from {}: {}", i, util::tilde(from), imported.join(", "));
        }
        if !skipped.is_empty() {
            eprintln!("{}not imported: {} (builds of projects under your home still read {}/config.toml)", i, skipped.join(", "), util::tilde(from));
        }
    }
    if let Some(spec) = &opts.toolchain {
        let layout = crate::toolchain::Layout::current();
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let cfg = Config::load(&cwd)?;
        let sel = crate::toolchain::Selection { spec: crate::toolchain::spec::Spec::parse(spec)?, source: crate::toolchain::Source::Plus, file: None };
        let mut progress = crate::ui::ToolchainProgress::new(spec);
        let tc = match crate::toolchain::ensure(&layout, &sel, &crate::toolchain::Needs::default(), cfg.str("toolchain.dist-server"), &mut progress) {
            Ok(tc) if progress.worked() => {
                progress.finish(&tc);
                tc
            }
            Ok(tc) => {
                eprintln!("{}Rust: {} is installed ({})", crate::ui::INDENT, spec, tc.id);
                tc
            }
            Err(e) => {
                progress.fail(&e);
                return Err(e);
            }
        };
        if crate::driver::wanted(&cfg) {
            crate::driver::prepare(&tc);
        }
    }
    if modify_path && !done.path_edits.is_empty() && !cfg!(windows) {
        // As a shell takes it: `~` is not expanded inside quotes, `$HOME` is.
        let home = util::qrust_home();
        crate::ui::say(crate::ui::Tag::Note, &format!("open a new shell, or run:  . \"{}\"  (fish: source \"{}\")", crate::setup::shell_path(&home.join("env")), crate::setup::shell_path(&home.join("env.fish"))));
    }
    Ok(0)
}

/// `qrust self uninstall`: QuickRust removed from this machine.
fn cmd_self(raw: &[OsString]) -> CmdResult {
    let mut a = Args::new(raw);
    let action = a.positional();
    let yes = a.flag("--yes");
    a.finish()?;
    match action.as_deref() {
        Some("update") => {
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let cfg = Config::load(&cwd).map_err(|e| format!("invalid QuickRust configuration: {}", e))?;
            let mut phase = crate::ui::Phase::start();
            phase.status("Looking for a newer QuickRust", true);
            let u = crate::setup::self_update(cfg.str("self.update-url")).inspect_err(|_| phase.clear_line())?;
            if u.from == u.to {
                phase.finish(true, &format!("QuickRust {} is the latest", u.to));
            } else {
                phase.finish(true, &format!("QuickRust {} installed (was {})", u.to, u.from));
            }
            Ok(0)
        }
        Some("uninstall") if !yes => Err(format!("`qrust self uninstall --yes` removes {} with everything in it (toolchains, installed programs, caches, records) and the PATH lines `qrust setup` added", util::qrust_home().display())),
        Some("uninstall") => {
            for r in crate::setup::uninstall()? {
                crate::ui::say_out(crate::ui::Tag::Ok, &format!("Removed {}", r));
            }
            Ok(0)
        }
        Some(other) => Err(format!("unknown `qrust self` action `{}` (update, uninstall)", other)),
        None => Err("`qrust self` needs an action: update, uninstall".into()),
    }
}
