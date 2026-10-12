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
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// File in the session's event directory holding the build's complete output.
pub const OUTPUT_FILE: &str = "cargo-output.jsonl";
const MAX_STORED_LINES: usize = 200_000;

/// Whether Haul renders this invocation's output.
pub fn wanted(inv: &Invocation, args: &[OsString], cfg: &Config) -> bool {
    // compat mode leaves the terminal to Cargo unless the user asked for Haul's output explicitly.
    let setting = if cfg.str("mode") == "compat" && cfg.origin("output") == "default" { "cargo" } else { cfg.str("output") };
    if setting != "haul" || !std::io::stderr().is_terminal() {
        return false;
    }
    // Commands whose own output is the point (programs run by `cargo run` / `cargo test`) keep a
    // real terminal: they are left to Cargo. `test --no-run` only builds.
    let builds_only = matches!(inv.family, Family::Build | Family::Check | Family::Clippy) || (inv.family == Family::Test && args.iter().any(|a| a == "--no-run"));
    let user_chose = args.iter().map(|a| a.to_string_lossy()).take_while(|a| a != "--").any(|a| {
        a.starts_with("--message-format") || a == "-v" || a == "-vv" || a == "--verbose" || a == "-q" || a == "--quiet" || a.starts_with("--color")
    });
    builds_only && !user_chose && inv.alias_of.is_none()
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
}

pub struct Live {
    tx: Sender<Ev>,
    readers: Vec<JoinHandle<()>>,
    render: JoinHandle<Finished>,
}

fn reader(from: impl Read + Send + 'static, tx: Sender<Ev>, out: bool) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(from).split(b'\n').map_while(Result::ok) {
            let line = String::from_utf8_lossy(&line).trim_end_matches('\r').to_string();
            if tx.send(if out { Ev::Out(line) } else { Ev::Err(line) }).is_err() {
                break;
            }
        }
    })
}

/// Starts rendering the output of a spawned Cargo.
pub fn start(stdout: impl Read + Send + 'static, stderr: impl Read + Send + 'static) -> Live {
    let (tx, rx) = channel();
    let readers = vec![reader(stdout, tx.clone(), true), reader(stderr, tx.clone(), false)];
    let render = std::thread::spawn(move || Renderer::new().run(rx));
    Live { tx, readers, render }
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
                Finished { messages: Vec::new(), log: Vec::new() }
            }
        }
    }
}

#[derive(Default)]
struct Renderer {
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
        let mut text = format!("  Building · {} compiled · {} fresh", self.compiled, self.fresh);
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

    fn on_stderr(&mut self, err: &mut impl Write, line: &str) {
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
            _ if t.starts_with("warning: `") && t.contains(" generated ") => {}
            _ if t.starts_with("error: could not compile `") => {}
            _ if t.is_empty() => {}
            // Everything else is shown as Cargo printed it.
            _ => self.print_above(err, line),
        }
    }

    fn summary(&mut self, err: &mut impl Write, exit: ExitInfo, wall_ns: u64, cache_hits: u64) {
        self.clear_status(err);
        let dot = self.paint("2", " · ");
        if exit.success() {
            let mut s = format!("{} {}", self.paint("1;32", "✓ Built"), human_dur(wall_ns));
            s.push_str(&format!("{}{} compiled{}{} fresh", dot, self.compiled, dot, self.fresh));
            if cache_hits > 0 {
                s.push_str(&format!("{}{} restored from Haul's cache", dot, cache_hits));
            }
            let _ = writeln!(err, "{}", s);
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
        }
        if self.warning_total > 0 {
            let mut lints: Vec<(&String, &u64)> = self.warnings.iter().collect();
            lints.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let shown: Vec<String> = lints.iter().take(3).map(|(k, n)| format!("{} ×{}", k, n)).collect();
            let more = if lints.len() > 3 { format!(", +{} more", lints.len() - 3) } else { String::new() };
            let _ = writeln!(
                err,
                "{} {} warning{} in {}: {}{}{}`haul warnings` to show them",
                self.paint("1;33", "⚠"),
                self.warning_total,
                if self.warning_total == 1 { "" } else { "s" },
                self.warning_crates.join(", "),
                shown.join(", "),
                more,
                dot
            );
        }
        let _ = err.flush();
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
                    self.store("err", line);
                }
                Ev::End { exit, wall_ns, cache_hits } => {
                    self.summary(&mut err, exit, wall_ns, cache_hits);
                    break;
                }
            }
            self.draw_status(&mut err, false);
        }
        Finished { messages: self.messages, log: self.log }
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
