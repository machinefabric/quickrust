//! Timing one command: wall clock, CPU of the whole process tree, and what Cargo compiled.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub wall_ns: u64,
    /// User + system CPU of the command and every descendant it waited for (rustc included).
    pub cpu_ns: u64,
    /// Largest resident set of any single process in the tree (not the tree's sum).
    pub max_rss_bytes: u64,
    /// `Compiling` / `Checking` lines Cargo printed: units it ran rustc for.
    pub units_run: u64,
    pub cpu_pressure: Option<f64>,
    pub io_pressure: Option<f64>,
    pub target_bytes: u64,
}

/// `some avg10` of a Linux pressure-stall file: the share of the last 10 s in which some task waited.
pub fn pressure(resource: &str) -> Option<f64> {
    let text = fs::read_to_string(format!("/proc/pressure/{}", resource)).ok()?;
    let line = text.lines().find(|l| l.starts_with("some"))?;
    line.split_whitespace().find_map(|f| f.strip_prefix("avg10=")).and_then(|v| v.parse().ok())
}

/// Runs `cmd` to completion. `Err` carries the tail of stderr when the command failed.
pub fn run(cmd: &mut Command) -> Result<Sample, String> {
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("cannot run {:?}: {}", cmd.get_program(), e))?;
    let stderr = child.stderr.take().ok_or("no stderr pipe")?;
    let reader = std::thread::spawn(move || {
        let (mut units, mut tail) = (0u64, std::collections::VecDeque::new());
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let t = line.trim_start();
            if t.starts_with("Compiling ") || t.starts_with("Checking ") {
                units += 1;
            }
            tail.push_back(line);
            if tail.len() > 40 {
                tail.pop_front();
            }
        }
        (units, tail.into_iter().collect::<Vec<_>>().join("\n"))
    });
    let pid = child.id() as libc::pid_t;
    let mut status: libc::c_int = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        let r = unsafe { libc::wait4(pid, &mut status, 0, &mut ru) };
        if r == pid {
            break;
        }
        if r == -1 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("wait4: {}", std::io::Error::last_os_error()));
        }
    }
    let wall_ns = started.elapsed().as_nanos() as u64;
    // Reaped by wait4 already.
    std::mem::forget(child);
    let (units_run, tail) = reader.join().unwrap_or_default();
    if !(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0) {
        return Err(format!("{:?} failed:\n{}", cmd.get_program(), tail));
    }
    let tv = |t: libc::timeval| t.tv_sec as u64 * 1_000_000_000 + t.tv_usec as u64 * 1_000;
    let rss_unit = if cfg!(target_vendor = "apple") { 1 } else { 1024 };
    Ok(Sample { wall_ns, cpu_ns: tv(ru.ru_utime) + tv(ru.ru_stime), max_rss_bytes: ru.ru_maxrss as u64 * rss_unit, units_run, cpu_pressure: pressure("cpu"), io_pressure: pressure("io"), target_bytes: 0 })
}

/// Allocated bytes below `dir`, each hard-linked inode counted once.
pub fn allocated_bytes(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut seen = std::collections::HashSet::new();
    let (mut total, mut stack) = (0u64, vec![dir.to_path_buf()]);
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(md) = fs::symlink_metadata(e.path()) else { continue };
            if md.is_dir() {
                stack.push(e.path());
            } else if md.nlink() <= 1 || seen.insert((md.dev(), md.ino())) {
                total += md.blocks() * 512;
            }
        }
    }
    total
}
