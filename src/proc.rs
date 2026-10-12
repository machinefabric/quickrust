//! Cross-platform process abstraction: spawn with signal forwarding, exact
//! exit/signal propagation (COMP-006, FR-001.3), and process replacement.

use std::io;
use std::process::{Child, Command};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitInfo {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Resource usage of a waited child, where the platform reports it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub user_ns: u64,
    pub sys_ns: u64,
    pub max_rss_bytes: u64,
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};

    static CHILD: AtomicI32 = AtomicI32::new(0);
    /// Last forwarded signal; lets long-running callers (benchmarks) notice interruption.
    pub static INTERRUPTED: AtomicI32 = AtomicI32::new(0);

    const FORWARDED: [libc::c_int; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

    extern "C" fn forward(sig: libc::c_int) {
        INTERRUPTED.store(sig, Ordering::SeqCst);
        let pid = CHILD.load(Ordering::SeqCst);
        if pid > 0 {
            // Only the direct child: a terminal-generated signal already reached the
            // whole foreground group, so its descendants must not see it twice.
            unsafe {
                libc::kill(pid, sig);
            }
        }
    }

    /// Keeps the forwarding handlers installed while alive; restores the previous ones on drop.
    pub struct Guard(Vec<(libc::c_int, libc::sigaction)>, i32);

    impl Guard {
        pub fn install() -> Guard {
            let mut saved = Vec::new();
            for &sig in &FORWARDED {
                unsafe {
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(sig, std::ptr::null(), &mut old) != 0 {
                        continue;
                    }
                    // An inherited "ignore" (nohup, background jobs) must stay ignored.
                    if old.sa_sigaction == libc::SIG_IGN {
                        continue;
                    }
                    let mut new: libc::sigaction = std::mem::zeroed();
                    new.sa_sigaction = forward as extern "C" fn(libc::c_int) as usize;
                    new.sa_flags = libc::SA_RESTART;
                    libc::sigemptyset(&mut new.sa_mask);
                    if libc::sigaction(sig, &new, std::ptr::null_mut()) == 0 {
                        saved.push((sig, old));
                    }
                }
            }
            Guard(saved, CHILD.load(Ordering::SeqCst))
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            CHILD.store(self.1, Ordering::SeqCst);
            for (sig, old) in &self.0 {
                unsafe {
                    libc::sigaction(*sig, old, std::ptr::null_mut());
                }
            }
        }
    }

    pub fn run(cmd: &mut Command, forward_signals: bool, on_spawn: &mut dyn FnMut(&mut Child)) -> io::Result<(ExitInfo, Option<Usage>)> {
        let _guard = if forward_signals { Some(Guard::install()) } else { None };
        let mut child = cmd.spawn()?;
        on_spawn(&mut child);
        let pid = child.id() as libc::pid_t;
        CHILD.store(pid, Ordering::SeqCst);
        let mut status: libc::c_int = 0;
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        loop {
            let r = unsafe { libc::wait4(pid, &mut status, 0, &mut ru) };
            if r == pid {
                break;
            }
            if r == -1 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
        }
        CHILD.store(0, Ordering::SeqCst);
        // `child` is intentionally not waited again; wait4 already reaped it.
        std::mem::forget(child);
        let info = if libc::WIFEXITED(status) {
            ExitInfo { code: Some(libc::WEXITSTATUS(status)), signal: None }
        } else if libc::WIFSIGNALED(status) {
            ExitInfo { code: None, signal: Some(libc::WTERMSIG(status)) }
        } else {
            ExitInfo { code: Some(1), signal: None }
        };
        let tv = |t: libc::timeval| (t.tv_sec as u64) * 1_000_000_000 + (t.tv_usec as u64) * 1_000;
        // ru_maxrss is bytes on Apple platforms and kilobytes elsewhere.
        let rss = ru.ru_maxrss as u64 * if cfg!(target_vendor = "apple") { 1 } else { 1024 };
        Ok((info, Some(Usage { user_ns: tv(ru.ru_utime), sys_ns: tv(ru.ru_stime), max_rss_bytes: rss })))
    }

    pub fn propagate(info: ExitInfo) -> ! {
        if let Some(sig) = info.signal {
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
            std::process::exit(128 + sig);
        }
        std::process::exit(info.code.unwrap_or(1))
    }

    pub fn exec(cmd: &mut Command) -> io::Error {
        use std::os::unix::process::CommandExt;
        cmd.exec()
    }

    pub fn pid_alive(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    pub fn parent_pid() -> u32 {
        unsafe { libc::getppid() as u32 }
    }

    pub fn interrupted() -> Option<i32> {
        match INTERRUPTED.load(Ordering::SeqCst) {
            0 => None,
            s => Some(s),
        }
    }

    pub fn terminate_child() -> bool {
        let pid = CHILD.load(Ordering::SeqCst);
        pid > 0 && unsafe { libc::kill(pid, libc::SIGTERM) } == 0
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    pub fn run(cmd: &mut Command, _forward_signals: bool, on_spawn: &mut dyn FnMut(&mut Child)) -> io::Result<(ExitInfo, Option<Usage>)> {
        // Console control events reach the whole process group on Windows, so the
        // child receives Ctrl-C directly; we only wait and mirror its status.
        let mut child = cmd.spawn()?;
        on_spawn(&mut child);
        let status = child.wait()?;
        Ok((ExitInfo { code: Some(status.code().unwrap_or(1)), signal: None }, None))
    }

    pub fn propagate(info: ExitInfo) -> ! {
        std::process::exit(info.code.unwrap_or(1))
    }

    pub fn exec(cmd: &mut Command) -> io::Error {
        match cmd.status() {
            Ok(s) => std::process::exit(s.code().unwrap_or(1)),
            Err(e) => e,
        }
    }

    pub fn pid_alive(_pid: u32) -> bool {
        // Liveness cannot be established without extra platform APIs; callers treat
        // "unknown" as "possibly alive" and stay conservative.
        true
    }

    pub fn parent_pid() -> u32 {
        0
    }

    pub fn interrupted() -> Option<i32> {
        None
    }

    pub struct Guard;

    impl Guard {
        pub fn install() -> Guard {
            Guard
        }
    }

    pub fn terminate_child() -> bool {
        false
    }
}

/// Spawns `cmd`, waits for it, and forwards termination signals to it meanwhile.
pub fn run(cmd: &mut Command, forward_signals: bool) -> io::Result<(ExitInfo, Option<Usage>)> {
    imp::run(cmd, forward_signals, &mut |_| {})
}

/// Like [`run`], with access to the spawned child (e.g. to take a piped stdout) before waiting.
pub fn run_with(cmd: &mut Command, forward_signals: bool, on_spawn: &mut dyn FnMut(&mut Child)) -> io::Result<(ExitInfo, Option<Usage>)> {
    imp::run(cmd, forward_signals, on_spawn)
}

/// Terminates the current process the same way the child terminated.
pub fn propagate(info: ExitInfo) -> ! {
    imp::propagate(info)
}

/// Replaces the current process (Unix) or runs-and-exits (elsewhere). Returns only on failure.
pub fn exec(cmd: &mut Command) -> io::Error {
    imp::exec(cmd)
}

pub fn pid_alive(pid: u32) -> bool {
    imp::pid_alive(pid)
}

pub fn parent_pid() -> u32 {
    imp::parent_pid()
}

/// Signal number if a termination signal was forwarded during a `run`.
pub fn interrupted() -> Option<i32> {
    imp::interrupted()
}

/// Holds termination signals for a longer operation (a benchmark) so that an
/// interruption is observed through [`interrupted`] and cleanup can run, instead
/// of the process dying between child runs.
/// Starts `cmd` in the background, detached: a session of its own (it outlives this process,
/// and Ctrl-C at this terminal does not reach it), at the lowest scheduling priority, with
/// stdin from /dev/null and stdout and stderr to `log`. Init reaps it.
// The process outlives this one on purpose.
#[allow(clippy::zombie_processes)]
pub fn spawn_detached(cmd: &mut Command, log: std::fs::File) -> io::Result<()> {
    cmd.stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                libc::setpriority(libc::PRIO_PROCESS, 0, 19);
                Ok(())
            });
        }
    }
    cmd.spawn().map(|_| ())
}

pub fn hold_signals() -> imp::Guard {
    imp::Guard::install()
}

/// Asks the currently running child to terminate (used for timeouts).
/// Returns false when there is no child or the platform cannot signal it.
pub fn terminate_child() -> bool {
    imp::terminate_child()
}
