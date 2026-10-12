//! Timing one command: wall clock, CPU of the whole process tree, and what Cargo compiled.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub wall_ns: u64,
    /// User + system CPU of the command and every descendant: those it waited for (rustc
    /// included) and those that outlived it (QuickRust's driver finishing rustc's incremental save
    /// after its outputs were written), which the bench adopts (`adopt_orphans`).
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

/// Makes this process the one that orphaned descendants of the commands it runs are handed to
/// (Linux), so that `run` can wait for them and count their CPU: work a command leaves running
/// costs the machine as much as work it waits for. Elsewhere such work goes uncounted.
pub fn adopt_orphans() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
}

/// Runs `cmd` to completion: its wall time ends when it exits; then the descendants it left
/// running (see `adopt_orphans`) are waited for, outside the wall time, and their CPU counted.
/// `Err` carries the tail of stderr when the command failed.
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
    let tv = |t: libc::timeval| t.tv_sec as u64 * 1_000_000_000 + t.tv_usec as u64 * 1_000;
    let (mut orphans_cpu_ns, mut orphans_rss) = (0u64, 0i64);
    let ended = Instant::now();
    loop {
        let mut st: libc::c_int = 0;
        let mut oru: libc::rusage = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::wait4(-1, &mut st, libc::WNOHANG, &mut oru) };
        if r > 0 {
            orphans_cpu_ns += tv(oru.ru_utime) + tv(oru.ru_stime);
            orphans_rss = orphans_rss.max(oru.ru_maxrss);
            continue;
        }
        if r == 0 {
            // Descendants still running; one that never ends (a daemon) is the command's fault.
            if ended.elapsed() > std::time::Duration::from_secs(60) {
                return Err(format!("{:?}: descendants still running 60 s after it ended", cmd.get_program()));
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        break;
    }
    let (units_run, tail) = reader.join().unwrap_or_default();
    if !(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0) {
        return Err(format!("{:?} failed:\n{}", cmd.get_program(), tail));
    }
    let rss_unit = if cfg!(target_vendor = "apple") { 1 } else { 1024 };
    Ok(Sample { wall_ns, cpu_ns: tv(ru.ru_utime) + tv(ru.ru_stime) + orphans_cpu_ns, max_rss_bytes: ru.ru_maxrss.max(orphans_rss) as u64 * rss_unit, units_run, cpu_pressure: pressure("cpu"), io_pressure: pressure("io"), target_bytes: 0 })
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
