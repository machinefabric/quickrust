//! A test run's terminal, relayed. After QuickRust has built what `cargo test` or `cargo bench` runs
//! (see `session`), Cargo runs the test programs with a terminal of their own: a pseudo-terminal
//! the size of the real one, as their stderr (and stdout, when that is the terminal too), so they
//! behave exactly as in the terminal (colour, `isatty`, the order of their output). QuickRust passes
//! everything through as it comes, byte for byte (a test's `test x ... ` shows before its `ok`),
//! except Cargo's own lines: `Finished` goes (QuickRust's summary said it), `Running` and `Doc-tests`
//! show dimly without the program's path. The counts of libtest's `test result:` lines are added
//! up for QuickRust's closing line.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// What libtest's `test result:` lines added up to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Totals {
    /// Test programs that reported a result.
    pub programs: u64,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub measured: u64,
    pub filtered: u64,
}

pub struct Relay {
    master: File,
    slave: OwnedFd,
    stdout_too: bool,
}

/// A relay at work (`Relay::start`).
pub struct Relaying {
    done: Arc<AtomicBool>,
    thread: JoinHandle<Totals>,
}

/// After the command has exited, what its programs still write is relayed while it keeps coming
/// at least this often.
const QUIET_MS: i32 = 50;

impl Relay {
    /// A pseudo-terminal the size of the terminal on stderr; `stdout_too` when stdout is that
    /// terminal as well (one stream keeps the order of what the programs write to both).
    pub fn open(stdout_too: bool) -> io::Result<Relay> {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        let sized = unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut size) } == 0;
        let (mut m, mut s) = (-1, -1);
        // Pointers to constants on Linux, to mutables on macOS: `*mut` serves both.
        let modes: *mut libc::termios = std::ptr::null_mut();
        let size: *mut libc::winsize = if sized { &mut size } else { std::ptr::null_mut() };
        let r = unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), modes, size) };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        let (master, slave) = unsafe { (File::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        // Bytes as the programs write them: no `\r` before each `\n` (the real terminal adds its own).
        let mut modes: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(s, &mut modes) } != 0 {
            return Err(io::Error::last_os_error());
        }
        modes.c_oflag &= !libc::OPOST;
        if unsafe { libc::tcsetattr(s, libc::TCSANOW, &modes) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // The programs get the terminal's other side only.
        unsafe {
            libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        Ok(Relay { master, slave, stdout_too })
    }

    /// `cmd` writes to the relayed terminal.
    pub fn attach(&self, cmd: &mut Command) -> io::Result<()> {
        cmd.stderr(Stdio::from(self.slave.try_clone()?));
        if self.stdout_too {
            cmd.stdout(Stdio::from(self.slave.try_clone()?));
        }
        Ok(())
    }

    /// Starts relaying to the real terminal; `color` when QuickRust's own lines may be coloured.
    pub fn start(self, color: bool) -> Relaying {
        let Relay { master, slave, stdout_too } = self;
        drop(slave);
        let done = Arc::new(AtomicBool::new(false));
        let seen = done.clone();
        let thread = std::thread::spawn(move || {
            let out: Box<dyn Write> = if stdout_too { Box::new(io::stdout()) } else { Box::new(io::stderr()) };
            relay(master, out, Filter::new(color), &seen)
        });
        Relaying { done, thread }
    }
}

impl Relaying {
    /// The command has exited: relays what its programs still write, then the totals.
    pub fn finish(self) -> Totals {
        self.done.store(true, Ordering::SeqCst);
        self.thread.join().unwrap_or_default()
    }
}

fn relay(mut master: File, mut out: Box<dyn Write>, mut filter: Filter, done: &AtomicBool) -> Totals {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let finished = done.load(Ordering::SeqCst);
        let mut ready = libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        match unsafe { libc::poll(&mut ready, 1, if finished { QUIET_MS } else { 200 }) } {
            0 if finished => break,
            0 => continue,
            n if n < 0 => {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            _ => {}
        }
        match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let shown = filter.feed(&buf[..n]);
                let _ = out.write_all(&shown).and_then(|_| out.flush());
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // Every program holding the terminal has closed it (Linux reports that as EIO).
            Err(_) => break,
        }
    }
    let shown = filter.finish();
    let _ = out.write_all(&shown).and_then(|_| out.flush());
    filter.totals
}

/// Cargo's own lines in a test run, right-aligned to twelve columns as Cargo prints its verbs.
const CARGO_VERBS: &[&str] = &["Finished ", "Running ", "Doc-tests "];

struct Filter {
    color: bool,
    /// The line so far.
    line: Vec<u8>,
    /// The line could still be one of Cargo's: held until it is known.
    holding: bool,
    totals: Totals,
}

impl Filter {
    fn new(color: bool) -> Filter {
        Filter { color, line: Vec::new(), holding: true, totals: Totals::default() }
    }

    /// What to show of `bytes`.
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            self.line.push(b);
            if self.holding {
                if b == b'\n' {
                    match self.cargo_line() {
                        Some(shown) => out.extend_from_slice(shown.as_bytes()),
                        None => out.extend_from_slice(&self.line),
                    }
                    self.end_line();
                } else if !could_be_cargo(&self.line) {
                    out.extend_from_slice(&self.line);
                    self.holding = false;
                }
            } else {
                out.push(b);
                if b == b'\n' {
                    self.end_line();
                }
            }
        }
        out
    }

    /// What is held when the programs are done.
    fn finish(&mut self) -> Vec<u8> {
        let held = if self.holding { std::mem::take(&mut self.line) } else { Vec::new() };
        self.end_line();
        held
    }

    fn end_line(&mut self) {
        if let Some(counts) = test_result(&crate::ui::strip_ansi(&String::from_utf8_lossy(&self.line))) {
            self.totals.programs += 1;
            self.totals.passed += counts.passed;
            self.totals.failed += counts.failed;
            self.totals.ignored += counts.ignored;
            self.totals.measured += counts.measured;
            self.totals.filtered += counts.filtered;
        }
        self.line.clear();
        self.holding = true;
    }

    /// The held line, complete, as QuickRust shows it if it is one of Cargo's.
    fn cargo_line(&self) -> Option<String> {
        let text = crate::ui::strip_ansi(&String::from_utf8_lossy(&self.line));
        let text = text.trim_end();
        let verb = text.trim_start();
        if text.len() - verb.len() > 12 {
            return None;
        }
        let dim = |s: String| if self.color { format!("\x1b[2m{}\x1b[0m\n", s) } else { format!("{}\n", s) };
        if verb.starts_with("Finished ") {
            Some(String::new())
        } else if let Some(what) = verb.strip_prefix("Running ") {
            // `Running unittests src/lib.rs (<target>/debug/deps/app-…)`
            let what = what.rsplit_once(" (").map(|(w, _)| w).unwrap_or(what);
            Some(dim(format!("{}{}", crate::ui::INDENT, what)))
        } else {
            verb.strip_prefix("Doc-tests ").map(|crate_name| dim(format!("{}doc-tests {}", crate::ui::INDENT, crate_name)))
        }
    }
}

/// Whether the line begun in `held` may still turn out to be one of Cargo's.
fn could_be_cargo(held: &[u8]) -> bool {
    let text = String::from_utf8_lossy(held);
    // A control sequence not complete yet (Cargo colours its verbs) decides nothing.
    let text = match text.rfind('\x1b') {
        Some(i) if !escape_complete(&text[i..]) => &text[..i],
        _ => &text[..],
    };
    let text = crate::ui::strip_ansi(text);
    let verb = text.trim_start_matches(' ');
    text.len() - verb.len() <= 12 && CARGO_VERBS.iter().any(|v| v.starts_with(verb) || verb.starts_with(v))
}

/// Whether `seq`, an escape and what follows it, is a whole control sequence (CSI up to its final
/// letter, or a character-set choice).
fn escape_complete(seq: &str) -> bool {
    let mut chars = seq.chars().skip(1);
    match chars.next() {
        Some('[') => chars.any(|c| c.is_ascii_alphabetic()),
        Some('(' | ')') => chars.next().is_some(),
        Some(_) => true,
        None => false,
    }
}

/// The counts of a libtest summary line:
/// `test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 2 filtered out; finished in 0.00s`.
fn test_result(line: &str) -> Option<Totals> {
    let rest = line.trim().strip_prefix("test result: ")?;
    let (_, counts) = rest.split_once(". ")?;
    let mut t = Totals::default();
    for part in counts.split(';') {
        let mut words = part.split_whitespace();
        let Some(Ok(n)) = words.next().map(str::parse::<u64>) else { continue };
        match words.next() {
            Some("passed") => t.passed = n,
            Some("failed") => t.failed = n,
            Some("ignored") => t.ignored = n,
            Some("measured") => t.measured = n,
            Some("filtered") => t.filtered = n,
            _ => {}
        }
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(chunks: &[&str]) -> (String, Totals) {
        let mut f = Filter::new(false);
        let mut out = Vec::new();
        for c in chunks {
            out.extend(f.feed(c.as_bytes()));
        }
        out.extend(f.finish());
        (String::from_utf8(out).unwrap(), f.totals)
    }

    #[test]
    fn cargo_lines_are_restyled_and_everything_else_passes_as_it_comes() {
        let (text, totals) = shown(&[
            "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.04s\n",
            "     Running unittests src/lib.rs (/h/cache/builds/app-0123/debug/deps/app-4567)\n",
            "\nrunning 2 tests\n",
            "test a ... ",
            "ok\ntest b ... ok\n\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.00s\n\n",
            "   Doc-tests app\n",
            "\x1b[0m\x1b[0m\x1b[1m\x1b[92m     Running\x1b[0m tests/it.rs (/h/x)\n",
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s\n",
        ]);
        assert_eq!(text, "     unittests src/lib.rs\n\nrunning 2 tests\ntest a ... ok\ntest b ... ok\n\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.00s\n\n     doc-tests app\n     tests/it.rs\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s\n");
        assert_eq!(totals, Totals { programs: 2, passed: 3, failed: 1, ignored: 1, measured: 0, filtered: 3 });
    }

    #[test]
    fn a_line_is_held_only_while_it_may_be_cargos() {
        let mut f = Filter::new(false);
        // A test's `test x ... ` shows before its result.
        assert_eq!(f.feed(b"test slow ... "), b"test slow ... ");
        assert_eq!(f.feed(b"ok\n"), b"ok\n");
        // Cargo's lines arrive whole, but until the verb is known nothing shows.
        assert_eq!(f.feed(b"     Run"), b"");
        assert_eq!(f.feed(b"ning tests/a.rs (/p)\n"), "     tests/a.rs\n".as_bytes());
        // Text that merely starts like a verb shows as soon as it differs.
        assert_eq!(f.feed(b"  Runner says hi"), b"  Runner says hi");
        // Indented further than any of Cargo's verbs: not Cargo's.
        assert_eq!(f.feed(b"\n              Finished"), b"\n              Finished");
        // Held text is shown when the programs are done.
        assert_eq!(f.feed(b"\n  Fin"), b"\n");
        assert_eq!(f.finish(), b"  Fin");
        // Cargo's colours, arriving a byte at a time, are held with the verb they colour.
        let mut f = Filter::new(false);
        let mut out = Vec::new();
        for b in b"\x1b[1m\x1b[92m     Running\x1b[0m tests/b.rs (/p)\n\x1b[32mtest\x1b[0m" {
            out.extend(f.feed(&[*b]));
        }
        assert_eq!(String::from_utf8(out).unwrap(), "     tests/b.rs\n\x1b[32mtest\x1b[0m");
    }
}
