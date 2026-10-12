//! Haul's build output: calm, structured, nothing lost.
//!
//! When a build runs in a terminal, Haul asks Cargo for its machine-readable messages and
//! renders them itself: one live status line instead of a `Compiling` line per crate, errors
//! in full as they arrive (rustc's own rendering), warnings counted by lint and crate instead
//! of dumped, and a one-line summary. Every line Cargo and rustc produced is stored with the
//! run; `haul warnings`, `haul errors` and `haul log` show it again. Lines Haul does not
//! recognise (build script failures, `cargo:warning`, resolution errors) pass through as they
//! are. Anywhere else — pipes, IDEs, CI logs, `-v`, `-q`, `--message-format`, `output =
//! "cargo"` — Cargo's output is untouched.

use crate::config::Config;
use crate::proc::ExitInfo;
use crate::resolve::{Family, Invocation};
use crate::util::{self, human_dur};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// File in the session's event directory holding the build's complete output.
pub const OUTPUT_FILE: &str = "cargo-output.jsonl";
const MAX_STORED_LINES: usize = 200_000;

/// Whether Haul renders this invocation's output.
/// How Haul presents a command's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendering {
    /// Cargo's output reaches the terminal unchanged.
    Off,
    /// The command only builds: Haul renders all of it.
    Build,
    /// The command builds, then runs programs whose own output is the point (`cargo run`,
    /// `cargo test`): Haul first builds exactly what the command needs and renders that, then
    /// runs the command itself, which finds everything fresh, with the real terminal.
    BuildFirst(Prebuild),
    /// A command that does not build (`update`, `add`, `fetch`, `new`, `clean`, `package`, …):
    /// Haul renders Cargo's status lines from its stderr; stdout and stdin stay the terminal's.
    Lines,
}

/// How the build of a `BuildFirst` command is obtained from the command itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prebuild {
    /// `cargo test` and `cargo bench`: the same command with `--no-run`.
    NoRun,
    /// `cargo run`: the same command, with a runner that does not run the program.
    Runner,
}

pub fn rendering(inv: &Invocation, args: &[OsString], cfg: &Config) -> Rendering {
    // compat mode leaves the terminal to Cargo unless the user asked for Haul's output explicitly.
    let setting = if cfg.str("mode") == "compat" && cfg.origin("output") == "default" { "cargo" } else { cfg.str("output") };
    if setting != "haul" || !std::io::stderr().is_terminal() || inv.alias_of.is_some() {
        return Rendering::Off;
    }
    let own: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).take_while(|a| a != "--").collect();
    let has = |flag: &str| own.iter().any(|a| a == flag);
    let user_chose = own.iter().any(|a| a.starts_with("--message-format") || a == "-v" || a == "-vv" || a == "--verbose" || a == "-q" || a == "--quiet" || a.starts_with("--color"));
    if user_chose {
        return Rendering::Off;
    }
    let passthrough_rendered = matches!(inv.command.as_deref(), Some("update" | "fetch" | "add" | "remove" | "rm" | "new" | "init" | "generate-lockfile" | "package" | "publish" | "vendor"));
    match inv.family {
        Family::Build | Family::Check | Family::Clippy | Family::Doc => Rendering::Build,
        Family::Clean => Rendering::Lines,
        Family::Passthrough if passthrough_rendered => Rendering::Lines,
        // Queries that may download what they describe; their stdout is the terminal's.
        Family::Query if matches!(inv.command.as_deref(), Some("tree" | "metadata")) && !has("--help") && !has("-h") => Rendering::Lines,
        Family::Test | Family::Bench if has("--no-run") => Rendering::Build,
        // Doc tests are built while they run; there is nothing to build first.
        Family::Test | Family::Bench if has("--doc") => Rendering::Off,
        Family::Test | Family::Bench => Rendering::BuildFirst(Prebuild::NoRun),
        // One target, named as a triple: the do-nothing runner is set for exactly it.
        Family::Run if inv.targets.len() <= 1 && inv.targets.iter().all(|t| !t.ends_with(".json")) => Rendering::BuildFirst(Prebuild::Runner),
        _ => Rendering::Off,
    }
}

/// The internal argument under which the Haul binary is the do-nothing runner.
pub const NOOP_RUNNER: &str = "__haul-noop-runner";

/// What turns a `BuildFirst` command into the build it needs: arguments for the command, and
/// `--config` settings (as `--config`, `KEY=VALUE` pairs). `target` is the triple the program
/// is built for.
pub fn prebuild(how: Prebuild, haul: &Path, target: &str) -> (Vec<String>, Vec<String>) {
    match how {
        Prebuild::NoRun => (vec!["--no-run".into()], Vec::new()),
        Prebuild::Runner => {
            // For the triple itself: a runner set for the triple takes precedence over any
            // `cfg(...)` one the user configured, which must not run the program here.
            let runner = serde_json::to_string(&[util::path_str(haul), crate::resolve::AS_HAUL.to_string(), NOOP_RUNNER.to_string()]).unwrap_or_default();
            (Vec::new(), vec!["--config".into(), format!("target.{}.runner={}", target, runner)])
        }
    }
}

/// The closing line of a test run whose terminal Haul relayed (see `relay`): libtest's counts,
/// added up over every test program.
#[cfg(unix)]
pub fn tests_done(t: &crate::relay::Totals, exit: ExitInfo, ns: u64) {
    let r = Renderer::new();
    let dot = r.paint("2", " · ");
    let counts: Vec<String> = [(t.failed, "failed"), (t.passed, "passed"), (t.ignored, "ignored"), (t.measured, "measured"), (t.filtered, "filtered out")].iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{} {}", n, w)).collect();
    let line = if exit.signal.is_some() {
        format!("{} after {}", r.paint("1;33", "■ Interrupted"), human_dur(ns))
    } else if t.programs == 0 {
        // Programs that are not libtest's (a custom harness): their own output says it all.
        if exit.success() {
            return;
        }
        format!("{} after {}", r.paint("1;31", "✗ Tests failed"), human_dur(ns))
    } else if exit.success() {
        format!("{} {}{}{}", r.paint("1;32", "✓"), counts.join(&dot), dot, human_dur(ns))
    } else {
        let what = if t.failed > 0 { counts.join(&dot) } else { [vec!["a test program failed".to_string()], counts].concat().join(&dot) };
        format!("{} {}{}{}", r.paint("1;31", "✗"), what, dot, human_dur(ns))
    };
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{}", line);
    let _ = err.flush();
}

/// A size as Cargo prints it (`645.0KiB`, `242B`) as Haul prints sizes (`645.0 KiB`, `242 B`).
fn cargo_size(s: &str) -> String {
    match s.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) if i > 0 && s[..i].chars().all(|c| c.is_ascii_digit() || c == '.') && ["B", "KiB", "MiB", "GiB", "TiB"].contains(&&s[i..]) => format!("{} {}", &s[..i], &s[i..]),
        _ => s.to_string(),
    }
}

/// `text` with every size in it as Haul prints sizes (`cargo_size`): `943B (694B compressed)`.
fn cargo_sizes(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        out.push_str(&cargo_size(word));
        word.clear();
    };
    for c in text.chars() {
        if c == ' ' || c == '(' || c == ')' || c == ',' {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

pub fn color() -> bool {
    if std::env::var_os("NO_COLOR").map(|v| !v.is_empty()).unwrap_or(false) {
        return false;
    }
    !matches!(std::env::var("CARGO_TERM_COLOR").as_deref(), Ok("never"))
}

/// The argument added to the delegated command: Cargo's JSON, whose diagnostics carry rustc's
/// rendering (`rendered`), with colour when the terminal gets colour.
pub fn message_format_arg() -> &'static str {
    if color() {
        "--message-format=json-diagnostic-rendered-ansi"
    } else {
        "--message-format=json"
    }
}

/// Inserts `arg` among the subcommand's own arguments (before a `--`, if any).
pub fn with_arg(args: Vec<OsString>, arg: &str) -> Vec<OsString> {
    let mut args = args;
    let at = args.iter().position(|a| a == "--").unwrap_or(args.len());
    args.insert(at, arg.into());
    args
}

fn terminal_width() -> usize {
    #[cfg(unix)]
    {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(100)
}

enum Ev {
    Out(String),
    Err(String),
    End { exit: ExitInfo, wall_ns: u64, cache_hits: u64 },
}

/// What a rendered build leaves behind for the session record.
pub struct Finished {
    /// Cargo's JSON messages in the compact form the analysis reads.
    pub messages: Vec<Value>,
    /// Every line, in order, as `{"s": "out"|"err", "l": line}`.
    pub log: Vec<Value>,
    /// Units the build compiled or found fresh.
    pub units: u64,
    /// The warnings line of the summary, as printed.
    pub warnings_line: Option<String>,
}

pub struct Live {
    tx: Sender<Ev>,
    readers: Vec<JoinHandle<()>>,
    render: JoinHandle<Finished>,
}

/// Reads a stream line by line; on stderr also at each `\r`, where Cargo redraws its progress.
fn reader(from: impl Read + Send + 'static, tx: Sender<Ev>, out: bool) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(from).split(b'\n').map_while(Result::ok) {
            let line = String::from_utf8_lossy(&line).into_owned();
            let segments: Vec<&str> = if out { vec![line.trim_end_matches('\r')] } else { line.split('\r').filter(|s| !s.trim().is_empty()).collect() };
            for seg in segments.into_iter().filter(|s| !out || !s.is_empty() || line.is_empty()) {
                if tx.send(if out { Ev::Out(seg.to_string()) } else { Ev::Err(seg.to_string()) }).is_err() {
                    return;
                }
            }
        }
    })
}

/// Something Haul does itself (installing a toolchain or a crate): a live status line while it
/// runs, when stderr is a terminal, and one result line at the end, everywhere.
pub struct Phase {
    color: bool,
    width: usize,
    live: bool,
    shown: bool,
    started: Instant,
    last_draw: Option<Instant>,
}

impl Default for Phase {
    fn default() -> Self {
        Phase::start()
    }
}

impl Phase {
    pub fn start() -> Phase {
        let live = std::io::stderr().is_terminal();
        let color = color() && (live || matches!(std::env::var("CARGO_TERM_COLOR").as_deref(), Ok("always")));
        Phase { color, width: terminal_width(), live, shown: false, started: Instant::now(), last_draw: None }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{}m{}\x1b[0m", code, s)
        } else {
            s.to_string()
        }
    }

    /// Replaces the status line (at most every 80 ms unless `force`).
    pub fn status(&mut self, text: &str, force: bool) {
        if !self.live || (!force && self.last_draw.is_some_and(|t| t.elapsed() < Duration::from_millis(80))) {
            return;
        }
        self.last_draw = Some(Instant::now());
        let max = self.width.saturating_sub(1).max(20);
        let mut text = format!("  {}", text);
        if text.chars().count() > max {
            text = text.chars().take(max.saturating_sub(1)).collect::<String>() + "…";
        }
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r\x1b[2K{}", self.paint("2", &text));
        let _ = err.flush();
        self.shown = true;
    }

    fn clear(&mut self) {
        if self.shown {
            let _ = write!(std::io::stderr(), "\r\x1b[2K");
            self.shown = false;
        }
    }

    pub fn elapsed_ns(&self) -> u64 {
        self.started.elapsed().as_nanos() as u64
    }

    /// The result line: `✓ text · 1.24s` (or `✗` when `ok` is false).
    pub fn finish(mut self, ok: bool, text: &str) {
        self.clear();
        let mark = if ok { self.paint("1;32", "✓") } else { self.paint("1;31", "✗") };
        let _ = writeln!(std::io::stderr(), "{} {}{}{}", mark, text, self.paint("2", " · "), human_dur(self.elapsed_ns()));
    }

    /// The result line of something done at once: `✓ text`, no time.
    pub fn done(mut self, text: &str) {
        self.clear();
        let _ = writeln!(std::io::stderr(), "{} {}", self.paint("1;32", "✓"), text);
    }

    /// Takes the status line off the screen (output of something else follows).
    pub fn clear_line(&mut self) {
        self.clear();
    }

    /// A line printed above the status line.
    pub fn line(&mut self, text: &str) {
        self.clear();
        let _ = writeln!(std::io::stderr(), "{}", text);
    }
}

/// What a build-family command did, for its summary: `cargo check` and Clippy compile no code
/// to run, `cargo doc` documents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Done {
    #[default]
    Built,
    Checked,
    Documented,
}

impl Done {
    fn of(command: &str) -> Done {
        match command {
            "check" | "c" | "clippy" => Done::Checked,
            "doc" | "d" | "rustdoc" => Done::Documented,
            _ => Done::Built,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Done::Built => "Built",
            Done::Checked => "Checked",
            Done::Documented => "Documented",
        }
    }
}

/// `text` cut to fit a terminal `width` wide (one column spare), with `…` where it was cut.
fn truncate(text: &str, width: usize) -> String {
    let max = width.saturating_sub(1).max(20);
    if text.chars().count() > max {
        text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    } else {
        text.to_string()
    }
}

/// Why a build failed on a full disk, and what Haul's cache holds there.
fn disk_full_note() -> String {
    let home = util::haul_home();
    let free = crate::space::fs_space(&home).map(|f| util::human_bytes(f.available)).unwrap_or_else(|_| "?".into());
    let cache = crate::space::usage(&util::cache_dir()).and_then(|u| u["total_bytes"].as_u64()).map(util::human_bytes).unwrap_or_else(|| "an uncounted amount".into());
    format!("  the disk is full: {} free where Haul keeps its cache, which holds {} · `haul gc` frees what is not in use, `haul cache status` shows it", free, cache)
}

/// A 20-cell progress bar: `━━━━━━━━╺───────────`.
pub fn bar(done: u64, total: u64) -> String {
    const CELLS: u64 = 20;
    let filled = (done.min(total) * CELLS).checked_div(total).unwrap_or(0) as usize;
    let mut s = "━".repeat(filled);
    if filled < CELLS as usize {
        s.push('╺');
        s.push_str(&"─".repeat(CELLS as usize - filled - 1));
    }
    s
}

/// The progress of a toolchain install, as a `Phase`.
pub struct ToolchainProgress {
    phase: Option<Phase>,
    what: String,
    /// What is being added to an installed release (`rust-analyzer`, `target wasm32-…`) and
    /// that release, when the installer says so: then that is what it shows.
    adding: Option<(String, String)>,
}

impl ToolchainProgress {
    /// `what`: how the toolchain was asked for (`stable`, `1.98`), shown while installing.
    pub fn new(what: &str) -> ToolchainProgress {
        ToolchainProgress { phase: None, what: what.to_string(), adding: None }
    }


    fn phase(&mut self) -> &mut Phase {
        self.phase.get_or_insert_with(Phase::start)
    }

    /// Something was installed (or waited for).
    pub fn worked(&self) -> bool {
        self.phase.is_some()
    }

    /// The result line, if anything was installed.
    pub fn finish(self, tc: &crate::toolchain::Toolchain) {
        if let Some(p) = self.phase {
            let release = tc.record().map(|r| r.version).unwrap_or_else(|| tc.id.clone());
            match &self.adding {
                Some((what, _)) => p.finish(true, &format!("{} added to Rust {}", what, release)),
                None => p.finish(true, &format!("Rust {} ready ({})", release, tc.id)),
            }
        }
    }

    /// Takes its status line off the screen, for output that says what was done instead.
    pub fn clear(self) {
        if let Some(mut p) = self.phase {
            p.clear_line();
        }
    }

    pub fn fail(self, why: &str) {
        if let Some(p) = self.phase {
            match &self.adding {
                Some((what, release)) => p.finish(false, &format!("{} could not be added to Rust {}: {}", what, release, why)),
                None => p.finish(false, &format!("Rust {} could not be installed: {}", self.what, why)),
            }
        }
    }
}

impl crate::toolchain::install::Report for ToolchainProgress {
    fn waiting(&mut self, id: &str) {
        let what = format!("Waiting for another Haul installing {}", id);
        self.phase().status(&what, true);
    }

    fn progress(&mut self, done: usize, total: usize, bytes: u64, current: &str) {
        let doing = match &self.adding {
            Some((what, release)) => format!("Adding {} to Rust {}", what, release),
            None => format!("Installing Rust {}", self.what),
        };
        let text = format!("{} {}  {}/{} · {}{}", doing, bar(done as u64, total as u64), done, total, util::human_bytes(bytes), if current.is_empty() { String::new() } else { format!(" · {}", current) });
        self.phase().status(&text, done == total);
    }

    fn adding(&mut self, release: &str, what: &[String]) {
        self.adding = Some((what.join(", "), release.to_string()));
    }
}

/// Once per Haul home: restored crates whose build-script code was compiled under another
/// target directory name that directory in panic locations and debug information.
pub fn notice_foreign_out_dir(units: u64) {
    let marker = util::haul_home().join("notices").join("foreign-out-dir");
    if marker.exists() {
        return;
    }
    let r = Renderer::new();
    let _ = writeln!(
        std::io::stderr(),
        "{} {} crate{} restored from Haul's cache include code their build script generated in another target directory: panic messages and debug information for that code name that directory. Program behaviour is the same. (Shown once; `mode = \"compat\"` builds everything here.)",
        r.paint("2", "note:"),
        units,
        if units == 1 { "" } else { "s" }
    );
    let _ = util::create_private_dir(marker.parent().unwrap_or(&marker)).and_then(|_| util::write_atomic_fast(&marker, b""));
}

/// Haul started building its compiler driver for this toolchain (see `driver.rs`).
pub fn notice_driver_build(release: &str) {
    let r = Renderer::new();
    let _ = writeln!(
        std::io::stderr(),
        "{} building Haul's compiler driver for Rust {} in the background, once per toolchain (it installs the toolchain's rustc-dev component if needed). Later builds then skip rebuilding crates an edit cannot affect. (`fast.early-cutoff = \"off\"` turns this off.)",
        r.paint("2", "note:"),
        release
    );
}

/// A note shown at most once an hour (per Haul home and `key`).
pub fn notice_hourly(key: &str, text: &str) {
    let marker = util::haul_home().join("notices").join(key);
    let recent = std::fs::metadata(&marker).map(|m| util::mtime_ns(&m) + 3_600 * 1_000_000_000 > util::now_wall_ns()).unwrap_or(false);
    if recent {
        return;
    }
    note(text);
    let _ = util::create_private_dir(marker.parent().unwrap_or(&marker)).and_then(|_| util::write_atomic_fast(&marker, b""));
}

/// A note of Haul's own on stderr.
pub fn note(text: &str) {
    let r = Renderer::new();
    let _ = writeln!(std::io::stderr(), "{} {}", r.paint("2", "note:"), text);
}

/// Haul could not start building its compiler driver.
pub fn notice_driver_unavailable(why: &str) {
    let r = Renderer::new();
    let _ = writeln!(std::io::stderr(), "{} Haul's compiler driver cannot be built: {}", r.paint("2", "note:"), why);
}

/// The summary of a build Haul knows to be fresh without asking Cargo: the build phase of a
/// `BuildFirst` command whose inputs have not changed since it last succeeded.
pub fn up_to_date(units: u64, warnings_line: Option<&str>) {
    let r = Renderer::new();
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{}{}{} fresh", r.paint("1;32", "✓ Up to date"), r.paint("2", " · "), units);
    if let Some(w) = warnings_line {
        let _ = writeln!(err, "{}", w);
    }
    let _ = err.flush();
}

/// Starts rendering the output of a spawned Cargo. `prebuild`: the build of a `BuildFirst`
/// command, whose "Running" and "Executable" lines name programs it does not run.
/// `command`: the Cargo command (or what an alias expands to), which names the result.
pub fn start(stdout: impl Read + Send + 'static, stderr: impl Read + Send + 'static, prebuild: bool, command: &str, step: bool) -> Live {
    let (tx, rx) = channel();
    let readers = vec![reader(stdout, tx.clone(), true), reader(stderr, tx.clone(), false)];
    let done = Done::of(command);
    let render = std::thread::spawn(move || Renderer { prebuild, done, step, ..Renderer::new() }.run(rx));
    Live { tx, readers, render }
}

/// Starts rendering the stderr of a command that does not build (`Rendering::Lines`), named
/// `command` (`update`, `add`, …).
pub fn start_lines(stderr: impl Read + Send + 'static, command: &str) -> Live {
    let (tx, rx) = channel();
    let readers = vec![reader(stderr, tx.clone(), false)];
    let command = command.to_string();
    let render = std::thread::spawn(move || Renderer { lines: Some(Lines { command, ..Default::default() }), ..Renderer::new() }.run(rx));
    Live { tx, readers, render }
}

/// What a command that does not build changed, for its summary.
#[derive(Default)]
struct Lines {
    command: String,
    updated: u64,
    downgraded: u64,
    added: u64,
    removed: u64,
    /// A line with the command's result was printed (`Created …`, `Removed …`, `Packaged …`).
    result: bool,
    /// `cargo add`'s feature list, collected until it ends.
    features: Vec<String>,
    /// Inside a `Features:` block.
    in_features: bool,
    /// What Cargo is busy with (`Packaging demo v0.1.0`).
    doing: Option<String>,
    /// For a manifest edit (`add`, `remove`): the lockfile's changes, said in one line at the end.
    locked: Vec<String>,
}

impl Lines {
    fn edits_manifest(&self) -> bool {
        matches!(self.command.as_str(), "add" | "remove" | "rm")
    }
}

impl Live {
    /// Prints the summary once Cargo has exited, and returns what was collected.
    pub fn finish(self, exit: ExitInfo, wall_ns: u64, cache_hits: u64) -> Finished {
        // A grandchild that keeps a pipe open must not hang the build: wait briefly, then summarise.
        let deadline = Instant::now() + Duration::from_secs(2);
        for r in self.readers {
            while !r.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let _ = self.tx.send(Ev::End { exit, wall_ns, cache_hits });
        drop(self.tx);
        match self.render.join() {
            Ok(f) => f,
            Err(_) => {
                // Cargo's result stands; what was lost is said, not hidden.
                eprintln!("haul: internal error: the output renderer failed; Cargo's result stands, but this run's output was not stored");
                Finished { messages: Vec::new(), log: Vec::new(), units: 0, warnings_line: None }
            }
        }
    }
}

#[derive(Default)]
struct Renderer {
    prebuild: bool,
    /// What the summary says the command did.
    done: Done,
    /// A step of a command that reports the result (`session::Spec::step`).
    step: bool,
    color: bool,
    width: usize,
    status_shown: bool,
    last_draw: Option<Instant>,
    compiled: u64,
    fresh: u64,
    running: Vec<String>,
    errors: u64,
    error_crates: Vec<String>,
    first_error: Option<String>,
    last_error_crate: Option<String>,
    warnings: BTreeMap<String, u64>,
    warning_crates: Vec<String>,
    warning_total: u64,
    messages: Vec<Value>,
    log: Vec<Value>,
    activity: Activity,
    /// Rendering a command that does not build.
    lines: Option<Lines>,
}

/// What Cargo is doing, as its status lines and progress say.
#[derive(Default)]
struct Activity {
    /// `Building [..] n/N`: units done of all.
    progress: Option<(u64, u64)>,
    /// Downloading: the most crates seen remaining (the total), those remaining now, the bytes left.
    downloading: Option<(u64, u64, String)>,
    /// `Blocking waiting for file lock on …`: on what, and whether Haul's cache collection is
    /// what holds it (otherwise another Cargo).
    waiting: Option<(String, bool)>,
    /// `Updating crates.io index`: which.
    updating: Option<String>,
    /// `Fetch n complete; m pending`: index entries looked up so far.
    looked_up: Option<u64>,
    /// `Generated …/index.html` (documentation).
    generated: Vec<String>,
    /// Cargo or rustc reported a full disk.
    disk_full: bool,
}

impl Activity {
    /// Notes a status line or progress update of Cargo's; what it prints instead, if anything.
    fn observe(&mut self, line: &crate::cargo_lines::Line) -> Option<String> {
        use crate::cargo_lines::Line;
        let done_waiting = !matches!(line, Line::Status { verb: "Blocking", .. });
        if done_waiting {
            self.waiting = None;
        }
        match *line {
            Line::Progress { verb: "Building", done: Some(d), total: Some(t), .. } => {
                self.progress = Some((d, t));
                self.updating = None;
                self.looked_up = None;
            }
            Line::Progress { verb: "Downloading", total, detail, .. } => {
                let remaining = total.unwrap_or(0);
                let most = self.downloading.as_ref().map(|d| d.0).unwrap_or(0).max(remaining);
                self.downloading = Some((most, remaining, detail.to_string()));
                self.updating = None;
                self.looked_up = None;
            }
            Line::Progress { verb: "Fetch", done, .. } => self.looked_up = done,
            Line::Status { verb: "Blocking", rest } => self.waiting = Some((rest.trim_start_matches("waiting for file lock on ").to_string(), crate::space::collecting())),
            Line::Status { verb: "Updating", rest } if rest.ends_with(" index") => {
                self.updating = Some(rest.to_string());
                self.looked_up = None;
            }
            Line::Status { verb: "Downloaded", rest } => {
                // `Downloaded 7 crates (645.0KiB) in 0.16s`: all done.
                if let Some((n, more)) = rest.split_once(' ') {
                    if n.parse::<u64>().is_ok() && (more.starts_with("crates") || more.starts_with("crate ")) {
                        self.downloading = None;
                        let size = more.split_once('(').and_then(|(_, r)| r.split_once(')')).map(|(b, _)| cargo_size(b)).unwrap_or_default();
                        // Cargo 1.99 adds `(largest was `encoding_rs` at 1.3MiB)` after the duration.
                        let took = more.split_once(" in ").map(|(_, t)| t.split(' ').next().unwrap_or("")).unwrap_or("");
                        return Some(format!("{} {}{}{}", n, if n == "1" { "crate" } else { "crates" }, if size.is_empty() { String::new() } else { format!(" · {}", size) }, if took.is_empty() { String::new() } else { format!(" · {}", took) }));
                    }
                }
            }
            Line::Status { verb: "Generated", rest } => self.generated.push(rest.to_string()),
            _ => {}
        }
        None
    }

    /// The status line for what Cargo is doing before (or apart from) compiling.
    fn status(&self) -> Option<String> {
        if let Some((w, by_haul)) = &self.waiting {
            return Some(format!("Waiting for the lock on the {} ({} holds it)", w, if *by_haul { "Haul's cache collection" } else { "another Cargo" }));
        }
        if let Some((most, remaining, bytes)) = &self.downloading {
            if *most == 0 {
                return Some("Downloading crates".to_string());
            }
            let done = most.saturating_sub(*remaining);
            return Some(format!("Downloading {}  {}/{} crates{}", bar(done, *most), done, most, if bytes.is_empty() { String::new() } else { format!(" · {} left", bytes) }));
        }
        self.updating.as_ref().map(|u| match self.looked_up {
            Some(n) => format!("Updating {} · {} {} looked up", u, n, if n == 1 { "package" } else { "packages" }),
            None => format!("Updating {}", u),
        })
    }
}

fn crate_label(msg: &Value) -> String {
    let t = &msg["target"];
    let name = t["name"].as_str().unwrap_or("?");
    match t["kind"].as_array().and_then(|k| k.first()).and_then(Value::as_str) {
        Some("lib") | Some("rlib") | None => name.to_string(),
        Some(kind) => format!("{} ({})", name, kind),
    }
}

impl Renderer {
    fn new() -> Renderer {
        Renderer { color: color(), width: terminal_width(), ..Default::default() }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{}m{}\x1b[0m", code, s)
        } else {
            s.to_string()
        }
    }

    fn clear_status(&mut self, err: &mut impl Write) {
        if self.status_shown {
            let _ = write!(err, "\r\x1b[2K");
            self.status_shown = false;
        }
    }

    fn draw_status(&mut self, err: &mut impl Write, force: bool) {
        if !force && self.last_draw.map(|t| t.elapsed() < Duration::from_millis(80)).unwrap_or(false) {
            return;
        }
        self.last_draw = Some(Instant::now());
        let lines_doing = self.lines.as_ref().and_then(|l| l.doing.clone());
        let status = self.activity.status().or(lines_doing);
        if let Some(text) = &status {
            let _ = write!(err, "\r\x1b[2K{}", self.paint("2", &truncate(&format!("  {}", text), self.width)));
            let _ = err.flush();
            self.status_shown = true;
            return;
        }
        if self.lines.is_some() {
            // Nothing under way: no status line.
            self.clear_status(err);
            return;
        }
        let mut text = match self.activity.progress {
            Some((d, t)) => format!("  Building {}  {}/{} · {} compiled · {} fresh", bar(d, t), d, t, self.compiled, self.fresh),
            None => format!("  Building · {} compiled · {} fresh", self.compiled, self.fresh),
        };
        if self.errors > 0 {
            text.push_str(&format!(" · {} error{}", self.errors, if self.errors == 1 { "" } else { "s" }));
        }
        if !self.running.is_empty() {
            text.push_str(&format!(" · {}", self.running.join(", ")));
        }
        let max = self.width.saturating_sub(1).max(20);
        if text.chars().count() > max {
            text = text.chars().take(max.saturating_sub(1)).collect::<String>() + "…";
        }
        let _ = write!(err, "\r\x1b[2K{}", self.paint("2", &text));
        let _ = err.flush();
        self.status_shown = true;
    }

    /// Prints a block above the status line.
    fn print_above(&mut self, err: &mut impl Write, block: &str) {
        self.clear_status(err);
        let _ = write!(err, "{}", block);
        if !block.ends_with('\n') {
            let _ = writeln!(err);
        }
    }

    fn on_stdout(&mut self, err: &mut impl Write, line: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            // Not JSON on Cargo's stdout (a build script printing there): show it.
            self.print_above(err, line);
            return;
        };
        match msg["reason"].as_str() {
            Some("compiler-artifact") => {
                if msg["fresh"] == json!(true) {
                    self.fresh += 1;
                } else {
                    self.compiled += 1;
                }
                let name = msg["target"]["name"].as_str().unwrap_or("").replace('-', "_");
                self.running.retain(|r| r.replace('-', "_") != name);
            }
            Some("compiler-message") => {
                let m = &msg["message"];
                let level = m["level"].as_str().unwrap_or("");
                let text = m["message"].as_str().unwrap_or("");
                // rustc's own tallies ("3 warnings emitted") are recomputed by Haul.
                let tally = m["code"].is_null() && (text.ends_with("warnings emitted") || text.ends_with("warning emitted") || text.starts_with("aborting due to"));
                if tally {
                } else if level == "error" || level == "error: internal compiler error" {
                    self.errors += 1;
                    let label = crate_label(&msg);
                    if !self.error_crates.contains(&label) {
                        self.error_crates.push(label.clone());
                    }
                    if self.first_error.is_none() {
                        let span = m["spans"].as_array().and_then(|s| s.iter().find(|x| x["is_primary"] == json!(true)).or_else(|| s.first()));
                        self.first_error = span.map(|s| format!("{}:{}:{}", util::vstr(s, "file_name"), s["line_start"], s["column_start"]));
                    }
                    let mut block = String::new();
                    if self.last_error_crate.as_deref() != Some(label.as_str()) {
                        block.push_str(&self.paint("1;31", &format!("── {} ", label)));
                        block.push('\n');
                        self.last_error_crate = Some(label);
                    }
                    block.push_str(m["rendered"].as_str().unwrap_or(text));
                    self.print_above(err, &block);
                } else if level == "warning" {
                    self.warning_total += 1;
                    let code = m["code"]["code"].as_str().unwrap_or("other").to_string();
                    *self.warnings.entry(code).or_default() += 1;
                    let label = crate_label(&msg);
                    if !self.warning_crates.contains(&label) {
                        self.warning_crates.push(label);
                    }
                }
            }
            _ => {}
        }
        if let Some(c) = crate::session::summarize_message(line.as_bytes()) {
            if self.messages.len() < MAX_STORED_LINES {
                self.messages.push(c);
            }
        }
    }

    /// Prints `cargo add`'s collected feature list, if any.
    fn flush_features(&mut self, err: &mut impl Write) {
        let Some(l) = self.lines.as_mut() else { return };
        if l.features.is_empty() {
            return;
        }
        let list = std::mem::take(&mut l.features).join(" ");
        let text = self.paint("2", &format!("    features {}", list));
        self.print_above(err, &text);
    }

    /// A line of a command that does not build (see `Rendering::Lines`).
    fn on_lines(&mut self, err: &mut impl Write, line: &str) {
        use crate::cargo_lines::{change, classify, package, Line};
        let plain = strip_ansi(line);
        let classified = classify(&plain);
        // `cargo add`'s features: `+ derive` / `- alloc` lines under `Features:`.
        if let (Line::Other(t), Some(l)) = (&classified, self.lines.as_mut()) {
            let t = t.trim();
            if t == "Features:" {
                l.in_features = true;
                return;
            }
            if l.in_features {
                let item = t.strip_prefix("+ ").map(|f| format!("+{}", f)).or_else(|| t.strip_prefix("- ").map(|f| format!("−{}", f)));
                if let Some(item) = item.filter(|i| i.len() > 1 && !i.contains(' ')) {
                    l.features.push(item);
                    return;
                }
            }
        }
        if let Some(l) = self.lines.as_mut() {
            l.in_features = false;
        }
        self.flush_features(err);
        if let Some(done) = self.activity.observe(&classified) {
            let mark = self.paint("1;36", "↓");
            self.print_above(err, &format!("{} Downloaded {}", mark, done));
            return;
        }
        let paint_mark = |s: &Self, code: &str, m: &str| s.paint(code, m);
        let out: Option<String> = match classified {
            Line::Progress { verb: v @ ("Building" | "Downloading" | "Fetch"), .. } => {
                let _ = v;
                None
            }
            Line::Progress { verb, detail, .. } => {
                if let Some(l) = self.lines.as_mut() {
                    l.doing = Some(format!("{} {}", verb, detail));
                }
                None
            }
            Line::Status { verb: "Updating", rest } if rest.ends_with(" index") => None,
            Line::Status { verb: "Locking", .. } => None,
            // A manifest edit: `Adding serde v1.0.229 to dependencies`, `Adding greet =1.0.0 to
            // dev-dependencies`, `Removing serde from dependencies`.
            Line::Status { verb: "Adding", rest } if rest.contains(" to ") => {
                let (what, table) = rest.split_once(" to ").unwrap_or((rest, ""));
                let (name, req) = what.split_once(' ').unwrap_or((what, ""));
                let req = req.strip_prefix('v').filter(|r| r.bytes().next().is_some_and(|c| c.is_ascii_digit())).unwrap_or(req);
                Some(format!("{} {:<24} {} → {}", paint_mark(self, "1;32", "+"), name, req, table))
            }
            Line::Status { verb: "Removing", rest } if rest.contains(" from ") => {
                let (name, table) = rest.split_once(" from ").unwrap_or((rest, ""));
                Some(format!("{} {} ← {}", paint_mark(self, "1;31", "−"), name, table))
            }
            // The lockfile: listed for `update`, one line at the end of a manifest edit.
            Line::Status { verb: v @ ("Updating" | "Downgrading" | "Adding" | "Removing"), rest } => {
                let edit = self.lines.as_ref().is_some_and(|l| l.edits_manifest());
                let shown = match v {
                    "Updating" | "Downgrading" => change(rest).map(|(n, f, t)| (n.to_string(), format!("{} → {}", f, t))),
                    _ => package(rest).map(|(n, ver)| (n.to_string(), ver.to_string())),
                };
                let Some((name, what)) = shown else { return self.print_above(err, line) };
                let (mark, code) = match v {
                    "Updating" => ("↑", "1;32"),
                    "Downgrading" => ("↓", "1;33"),
                    "Adding" => ("+", "1;32"),
                    _ => ("−", "1;31"),
                };
                if let Some(l) = self.lines.as_mut() {
                    match v {
                        "Updating" => l.updated += 1,
                        "Downgrading" => l.downgraded += 1,
                        "Adding" => l.added += 1,
                        _ => l.removed += 1,
                    }
                    if edit {
                        l.locked.push(format!("{}{} {}", mark, name, what));
                        return;
                    }
                }
                Some(format!("{} {:<24} {}", paint_mark(self, code, mark), name, what))
            }
            Line::Status { verb: "Unchanged", rest } => Some(self.paint("2", &format!("  = {}", rest))),
            Line::Status { verb: "Creating" | "Removed" | "Packaged" | "Uploaded" | "Published" | "Vendored", rest } => {
                if let Some(l) = self.lines.as_mut() {
                    l.result = true;
                }
                let verb = match classified {
                    Line::Status { verb: "Creating", .. } => "Created",
                    Line::Status { verb, .. } => verb,
                    _ => "",
                };
                // `Removed 125 files, 75.8MiB total`, `Packaged 4 files, 943B (694B compressed)`
                let rest = cargo_sizes(rest.strip_suffix(" total").filter(|_| verb == "Removed").unwrap_or(rest));
                let rest = match rest.split_once(" files, ") {
                    Some((n, more)) if n.parse::<u64>().is_ok() => format!("{} files{}{}", n, self.paint("2", " · "), more),
                    _ => rest,
                };
                Some(format!("{} {} {}", paint_mark(self, "1;32", "✓"), verb, rest))
            }
            Line::Status { verb: v @ ("Packaging" | "Verifying" | "Uploading" | "Waiting" | "Vendoring" | "Archiving" | "Compiling" | "Checking" | "Documenting" | "Finished" | "Fresh" | "Blocking" | "Downloaded" | "Generated"), rest } => {
                if let Some(l) = self.lines.as_mut() {
                    l.doing = Some(format!("{} {}", v, rest));
                }
                None
            }
            // Cargo's notes (`note: see more `Cargo.toml` keys …`): asides, dimly.
            Line::Other(t) if t.trim_start().starts_with("note: ") => Some(self.paint("2", t.trim_end())),
            _ => Some(line.to_string()),
        };
        if let Some(text) = out {
            self.print_above(err, &text);
        }
    }

    fn on_stderr(&mut self, err: &mut impl Write, line: &str) {
        if self.lines.is_some() {
            return self.on_lines(err, line);
        }
        let plain = strip_ansi(line);
        if plain.contains("No space left on device") {
            self.activity.disk_full = true;
        }
        let classified = crate::cargo_lines::classify(&plain);
        if let Some(done) = self.activity.observe(&classified) {
            let mark = self.paint("1;36", "↓");
            self.print_above(err, &format!("{} Downloaded {}", mark, done));
            return;
        }
        match classified {
            crate::cargo_lines::Line::Progress { .. } => return,
            // The lockfile changed: said once, dimly (each package is in `haul log`).
            crate::cargo_lines::Line::Status { verb: "Locking", rest } if !rest.starts_with("0 ") => {
                let text = self.paint("2", &format!("  Locked {}", rest));
                self.print_above(err, &text);
                return;
            }
            crate::cargo_lines::Line::Status { verb: "Updating" | "Locking" | "Adding" | "Removing" | "Downgrading" | "Unchanged" | "Downloaded" | "Downloading" | "Generated", .. } => return,
            _ => {}
        }
        let t = line.trim_start();
        let word = t.split_whitespace().next().unwrap_or("");
        match word {
            // Cargo's progress: summarised in the status line.
            "Compiling" | "Checking" | "Documenting" => {
                if let Some(name) = t.split_whitespace().nth(1) {
                    self.running.push(name.to_string());
                }
            }
            "Fresh" | "Finished" | "Building" | "Blocking" => {}
            "Running" | "Executable" if self.prebuild => {}
            _ if t.starts_with("warning: `") && t.contains(" generated ") => {}
            _ if t.starts_with("error: could not compile `") => {}
            _ if t.is_empty() => {}
            // Everything else is shown as Cargo printed it.
            _ => self.print_above(err, line),
        }
    }

    /// The summary of a command that does not build.
    fn lines_summary(&mut self, err: &mut impl Write, exit: ExitInfo, wall_ns: u64) {
        self.flush_features(err);
        self.clear_status(err);
        let Some(l) = self.lines.take() else { return };
        let dot = self.paint("2", " · ");
        if !l.locked.is_empty() {
            let _ = writeln!(err, "{}", self.paint("2", &format!("    locked {}", l.locked.join(" "))));
        }
        let s = if exit.signal.is_some() {
            format!("{} after {}", self.paint("1;33", "■ Interrupted"), human_dur(wall_ns))
        } else if !exit.success() {
            format!("{} after {}", self.paint("1;31", &format!("✗ cargo {} failed", l.command)), human_dur(wall_ns))
        } else if l.result || matches!(l.command.as_str(), "tree" | "metadata") {
            // The command's result says it (`Removed …`), or is what it printed.
            return;
        } else {
            let changes: Vec<String> = [(l.updated, "updated"), (l.downgraded, "downgraded"), (l.added, "added"), (l.removed, "removed")].iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{} {}", n, w)).collect();
            let what = match l.command.as_str() {
                "update" | "generate-lockfile" if changes.is_empty() => "Lockfile up to date".to_string(),
                "update" | "generate-lockfile" => format!("Lockfile updated{}{}", dot, changes.join(&dot)),
                "add" | "remove" | "rm" => "Manifest updated".to_string(),
                "fetch" => "Fetched".to_string(),
                other => format!("cargo {}", other),
            };
            format!("{} {}{}{}", self.paint("1;32", "✓"), what, dot, human_dur(wall_ns))
        };
        let _ = writeln!(err, "{}", s);
        let _ = err.flush();
    }

    fn summary(&mut self, err: &mut impl Write, exit: ExitInfo, wall_ns: u64, cache_hits: u64) {
        if self.lines.is_some() {
            return self.lines_summary(err, exit, wall_ns);
        }
        self.clear_status(err);
        let dot = self.paint("2", " · ");
        if exit.success() && self.step {
            // Part of a command that reports the result.
        } else if exit.success() {
            let mut s = format!("{} {}", self.paint("1;32", &format!("✓ {}", self.done.word())), human_dur(wall_ns));
            for (n, what) in [(self.compiled, "compiled"), (self.fresh, "fresh"), (cache_hits, "restored from Haul's cache")] {
                if n > 0 {
                    s.push_str(&format!("{}{} {}", dot, n, what));
                }
            }
            let _ = writeln!(err, "{}", s);
            for g in &self.activity.generated {
                let _ = writeln!(err, "  {} {}", self.paint("2", "docs"), g);
            }
        } else if exit.signal.is_some() {
            let _ = writeln!(err, "{} after {}", self.paint("1;33", "■ Interrupted"), human_dur(wall_ns));
        } else {
            let mut s = format!("{} after {}", self.paint("1;31", "✗ Build failed"), human_dur(wall_ns));
            if self.errors > 0 {
                s.push_str(&format!("{}{} error{} in {}", dot, self.errors, if self.errors == 1 { "" } else { "s" }, self.error_crates.join(", ")));
            }
            if let Some(first) = &self.first_error {
                s.push_str(&format!("{}first at {}", dot, first));
            }
            let _ = writeln!(err, "{}", s);
            if self.activity.disk_full {
                let _ = writeln!(err, "{}", self.paint("1;31", &disk_full_note()));
            }
        }
        if let Some(line) = self.warnings_line() {
            let _ = writeln!(err, "{}", line);
        }
        let _ = err.flush();
    }

    fn warnings_line(&self) -> Option<String> {
        if self.warning_total > 0 {
            let dot = self.paint("2", " · ");
            let mut lints: Vec<(&String, &u64)> = self.warnings.iter().collect();
            lints.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let shown: Vec<String> = lints.iter().take(3).map(|(k, n)| format!("{} ×{}", k, n)).collect();
            let more = if lints.len() > 3 { format!(", +{} more", lints.len() - 3) } else { String::new() };
            return Some(format!(
                "{} {} warning{} in {}: {}{}{}`haul warnings` to show them",
                self.paint("1;33", "⚠"),
                self.warning_total,
                if self.warning_total == 1 { "" } else { "s" },
                self.warning_crates.join(", "),
                shown.join(", "),
                more,
                dot
            ));
        }
        None
    }

    fn run(mut self, rx: Receiver<Ev>) -> Finished {
        let mut err = std::io::stderr();
        while let Ok(ev) = rx.recv() {
            match ev {
                Ev::Out(line) => {
                    self.on_stdout(&mut err, &line);
                    self.store("out", line);
                }
                Ev::Err(line) => {
                    self.on_stderr(&mut err, &line);
                    // Progress redraws are not output to keep.
                    if !matches!(crate::cargo_lines::classify(&strip_ansi(&line)), crate::cargo_lines::Line::Progress { .. }) {
                        self.store("err", line);
                    }
                }
                Ev::End { exit, wall_ns, cache_hits } => {
                    self.summary(&mut err, exit, wall_ns, cache_hits);
                    break;
                }
            }
            self.draw_status(&mut err, false);
        }
        let warnings_line = self.warnings_line();
        Finished { units: self.compiled + self.fresh, warnings_line, messages: self.messages, log: self.log }
    }

    fn store(&mut self, stream: &str, line: String) {
        if self.log.len() < MAX_STORED_LINES {
            self.log.push(json!({ "s": stream, "l": line }));
        }
    }
}

/// Diagnostics of one stored run, for `haul warnings` / `haul errors` / `haul log`.
pub fn stored_output(events_dir: &std::path::Path) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(events_dir.join(OUTPUT_FILE)).map_err(|_| "this run's output was not stored: it was not rendered by Haul (not a terminal, or output = \"cargo\"), or its raw events were pruned".to_string())?;
    Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
}

/// Prints stored diagnostics of `level` ("warning" or "error"), optionally filtered.
pub fn show(lines: &[Value], level: &str, crate_filter: Option<&str>, lint_filter: Option<&str>) -> usize {
    let mut shown = 0;
    let mut out = std::io::stdout().lock();
    for l in lines.iter().filter(|l| l["s"] == json!("out")) {
        let Ok(msg) = serde_json::from_str::<Value>(l["l"].as_str().unwrap_or("")) else { continue };
        let m = &msg["message"];
        if msg["reason"] != json!("compiler-message") || m["level"].as_str() != Some(level) || m["code"].is_null() && m["message"].as_str().map(|t| t.contains("emitted") || t.starts_with("aborting")).unwrap_or(false) {
            continue;
        }
        if crate_filter.map(|c| msg["target"]["name"].as_str() != Some(c) && !util::vstr(&msg, "package_id").contains(c)).unwrap_or(false) {
            continue;
        }
        if lint_filter.map(|f| m["code"]["code"].as_str() != Some(f)).unwrap_or(false) {
            continue;
        }
        let rendered = m["rendered"].as_str().unwrap_or("");
        let rendered = if std::io::stdout().is_terminal() && color() { rendered.to_string() } else { strip_ansi(rendered) };
        let _ = writeln!(out, "{}", rendered.trim_end());
        shown += 1;
    }
    shown
}

/// Prints a stored run's output in the order it was produced.
pub fn show_log(lines: &[Value], raw: bool) {
    let mut out = std::io::stdout().lock();
    for l in lines {
        let line = l["l"].as_str().unwrap_or("");
        if raw || l["s"] == json!("err") {
            let _ = writeln!(out, "{}", if raw { line.to_string() } else { strip_ansi(line) });
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<Value>(line) {
            if msg["reason"] == json!("compiler-message") {
                let _ = writeln!(out, "{}", strip_ansi(msg["message"]["rendered"].as_str().unwrap_or("")).trim_end());
            }
        }
    }
}

/// `s` without terminal control sequences: colours and other CSI sequences, and character-set
/// choices (`ESC ( B`, which terminfo's colour reset sends).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if c == '\x1b' && matches!(chars.peek(), Some('(' | ')')) {
            chars.next();
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> Renderer {
        Renderer { width: 80, ..Default::default() }
    }

    /// What a command that does not build shows for Cargo's `lines` and exit status `code`.
    fn lines(command: &str, lines: &[&str], code: i32) -> String {
        let mut ren = Renderer { width: 80, lines: Some(Lines { command: command.into(), ..Default::default() }), ..Default::default() };
        let mut sink: Vec<u8> = Vec::new();
        for l in lines {
            ren.on_stderr(&mut sink, l);
        }
        ren.summary(&mut sink, ExitInfo { code: Some(code), signal: None }, 920_000_000, 0);
        String::from_utf8(sink).unwrap()
    }

    #[test]
    fn lockfile_changes_are_listed_and_summed_up() {
        // Cargo 1.99's lines.
        let out = lines("update", &["    Updating crates.io index", "     Locking 3 packages to latest Rust 1.99.0 compatible versions", "    Updating serde v1.0.200 -> v1.0.229", "      Adding itoa v1.0.18", "    Removing autocfg v1.3.0"], 0);
        assert_eq!(
            out,
            "↑ serde                    1.0.200 → 1.0.229\n+ itoa                     1.0.18\n− autocfg                  1.3.0\n✓ Lockfile updated · 1 updated · 1 added · 1 removed · 920ms\n"
        );
        assert_eq!(lines("update", &["    Updating crates.io index", "     Locking 0 packages to latest Rust 1.99.0 compatible versions"], 0), "✓ Lockfile up to date · 920ms\n");
    }

    #[test]
    fn manifest_edits_and_their_features_read_as_one() {
        let out = lines("add", &["    Updating crates.io index", "      Adding serde v1.0.229 to dependencies", "             Features:", "             + derive", "             + std", "             - alloc", "    Updating crates.io index", "     Locking 7 packages to latest compatible versions"], 0);
        assert_eq!(out, "+ serde                    1.0.229 → dependencies\n    features +derive +std −alloc\n✓ Manifest updated · 920ms\n");
        // What the edit locked is one line, at the end.
        let out = lines("add", &["      Adding greet =1.0.0 to dev-dependencies", "     Locking 2 packages to latest compatible versions", "      Adding greet v1.0.0", "      Adding itoa v1.0.18"], 0);
        assert_eq!(out, "+ greet                    =1.0.0 → dev-dependencies\n    locked +greet 1.0.0 +itoa 1.0.18\n✓ Manifest updated · 920ms\n");
        assert_eq!(lines("remove", &["    Removing serde from dependencies"], 0), "− serde ← dependencies\n✓ Manifest updated · 920ms\n");
    }

    #[test]
    fn a_result_cargo_states_is_the_summary_and_failures_show_cargos_words() {
        assert_eq!(lines("new", &["    Creating binary (application) `demo` package", "note: see more `Cargo.toml` keys"], 0), "✓ Created binary (application) `demo` package\nnote: see more `Cargo.toml` keys\n");
        assert_eq!(lines("clean", &["    Cleaning [=====>       ]  21.43%", "     Removed 125 files, 75.8MiB total"], 0), "✓ Removed 125 files · 75.8 MiB\n");
        assert_eq!(lines("package", &["   Packaging p v0.1.0 (/w/p)", "    Packaged 4 files, 943B (694B compressed)"], 0), "✓ Packaged 4 files · 943 B (694 B compressed)\n");
        assert_eq!(cargo_sizes("1.5KiB, 3B (x) 10MiBs 2.0Gi"), "1.5 KiB, 3 B (x) 10MiBs 2.0Gi");
        let failed = lines("update", &["    Updating crates.io index", "error: failed to select a version for the requirement `serde = \"^9\"`", "location searched: crates.io index"], 101);
        assert_eq!(failed, "error: failed to select a version for the requirement `serde = \"^9\"`\nlocation searched: crates.io index\n✗ cargo update failed after 920ms\n");
    }

    #[test]
    fn index_lookups_count_in_the_status_line_and_are_never_printed() {
        let mut ren = r();
        let mut sink: Vec<u8> = Vec::new();
        ren.on_stderr(&mut sink, "    Updating crates.io index");
        ren.on_stderr(&mut sink, "       Fetch 26 complete; 4 pending");
        assert_eq!(ren.activity.status().as_deref(), Some("Updating crates.io index · 26 packages looked up"));
        ren.on_stderr(&mut sink, "       Fetch 50 complete; 1 pending");
        assert_eq!(ren.activity.status().as_deref(), Some("Updating crates.io index · 50 packages looked up"));
        // Cargo 1.99's summary names the largest crate; Haul's line ends at the duration.
        ren.on_stderr(&mut sink, "  Downloaded 25 crates (4.4MiB) in 0.32s (largest was `encoding_rs` at 1.3MiB)");
        ren.on_stderr(&mut sink, "    Building [>         ] 0/54: memchr");
        assert_eq!(ren.activity.status(), None);
        assert_eq!(String::from_utf8(sink).unwrap(), "↓ Downloaded 25 crates · 4.4 MiB · 0.32s\n");
        // Nor by a command that does not build.
        assert_eq!(lines("update", &["    Updating crates.io index", "       Fetch 3 complete; 1 pending", "     Locking 0 packages to latest compatible versions"], 0), "✓ Lockfile up to date · 920ms\n");
    }

    #[test]
    fn downloads_show_progress_then_one_line() {
        let mut ren = r();
        let mut sink: Vec<u8> = Vec::new();
        ren.on_stderr(&mut sink, " Downloading crates ...");
        assert_eq!(ren.activity.status().as_deref(), Some("Downloading crates"));
        ren.on_stderr(&mut sink, " Downloading 4 crates");
        ren.on_stderr(&mut sink, "  Downloaded quote v1.0.47");
        ren.on_stderr(&mut sink, " Downloading 3 crates, remaining bytes: 281.0KiB");
        assert_eq!(ren.activity.status().as_deref(), Some("Downloading ━━━━━╺──────────────  1/4 crates · 281.0KiB left"));
        ren.on_stderr(&mut sink, "  Downloaded 4 crates (645.0KiB) in 0.16s");
        assert_eq!(ren.activity.status(), None);
        ren.on_stderr(&mut sink, "    Building [==>      ] 2/16: syn, quote");
        assert_eq!(ren.activity.progress, Some((2, 16)));
        let text = String::from_utf8(sink).unwrap();
        assert_eq!(text, "↓ Downloaded 4 crates · 645.0 KiB · 0.16s\n");
    }

    #[test]
    fn errors_print_in_full_warnings_are_counted() {
        let mut ren = r();
        let mut sink: Vec<u8> = Vec::new();
        ren.on_stderr(&mut sink, "   Compiling app v0.1.0 (/w/app)");
        assert_eq!(ren.running, vec!["app"]);
        let warn = json!({ "reason": "compiler-message", "package_id": "path+file:///w/app#0.1.0", "target": { "name": "app", "kind": ["bin"] },
            "message": { "level": "warning", "message": "unused variable: `x`", "code": { "code": "unused_variables" }, "rendered": "warning: unused variable: `x`\n" } });
        let tally = json!({ "reason": "compiler-message", "target": { "name": "app", "kind": ["bin"] }, "message": { "level": "warning", "message": "1 warning emitted", "code": null, "rendered": "warning: 1 warning emitted\n" } });
        let error = json!({ "reason": "compiler-message", "target": { "name": "app", "kind": ["bin"] },
            "message": { "level": "error", "message": "mismatched types", "code": { "code": "E0308" }, "rendered": "error[E0308]: mismatched types\n --> src/main.rs:3:5\n", "spans": [{ "is_primary": true, "file_name": "src/main.rs", "line_start": 3, "column_start": 5 }] } });
        for m in [&warn, &tally, &error] {
            ren.on_stdout(&mut sink, &m.to_string());
        }
        ren.on_stderr(&mut sink, "warning: `app` (bin \"app\") generated 1 warning");
        ren.on_stderr(&mut sink, "error: could not compile `app` (bin \"app\") due to 1 previous error; 1 warning emitted");
        ren.on_stderr(&mut sink, "error: failed to run custom build command for `gen v0.1.0`");
        ren.summary(&mut sink, ExitInfo { code: Some(101), signal: None }, 1_200_000_000, 0);
        let text = String::from_utf8(sink).unwrap();
        assert!(text.contains("error[E0308]: mismatched types"), "errors are shown in full: {}", text);
        assert!(!text.contains("unused variable"), "warnings are not dumped: {}", text);
        assert!(text.contains("failed to run custom build command"), "unrecognised lines pass through: {}", text);
        assert!(!text.contains("could not compile"), "Cargo's tallies are replaced by the summary: {}", text);
        assert!(text.contains("✗ Build failed after 1.20s · 1 error in app (bin) · first at src/main.rs:3:5"), "{}", text);
        assert!(text.contains("⚠ 1 warning in app (bin): unused_variables ×1"), "{}", text);
    }

    #[test]
    fn success_summary_and_ansi_stripping() {
        let mut ren = r();
        let mut sink: Vec<u8> = Vec::new();
        ren.on_stdout(&mut sink, &json!({ "reason": "compiler-artifact", "fresh": false, "target": { "name": "app", "kind": ["bin"] } }).to_string());
        ren.on_stdout(&mut sink, &json!({ "reason": "compiler-artifact", "fresh": true, "target": { "name": "dep", "kind": ["lib"] } }).to_string());
        ren.summary(&mut sink, ExitInfo { code: Some(0), signal: None }, 4_210_000_000, 3);
        let text = String::from_utf8(sink).unwrap();
        assert!(text.contains("✓ Built 4.21s · 1 compiled · 1 fresh · 3 restored from Haul's cache"), "{}", text);
        // What the command did: `cargo check` and Clippy build nothing to run.
        for (command, says) in [("check", "✓ Checked "), ("clippy", "✓ Checked "), ("doc", "✓ Documented "), ("test", "✓ Built "), ("run", "✓ Built ")] {
            let mut ren = Renderer { done: Done::of(command), ..r() };
            let mut sink: Vec<u8> = Vec::new();
            ren.summary(&mut sink, ExitInfo { code: Some(0), signal: None }, 4_210_000_000, 0);
            assert!(String::from_utf8(sink).unwrap().starts_with(says), "{}", command);
        }
        assert_eq!(strip_ansi("\x1b[1m\x1b[31merror\x1b[0m: x"), "error: x");
    }

    #[test]
    fn the_message_format_goes_before_a_double_dash() {
        let a = with_arg(vec!["build".into(), "--release".into()], "--message-format=json");
        assert_eq!(a, vec![OsString::from("build"), "--release".into(), "--message-format=json".into()]);
        let a = with_arg(vec!["test".into(), "--no-run".into(), "--".into(), "x".into()], "--m");
        assert_eq!(a[2], "--m");
    }
}
