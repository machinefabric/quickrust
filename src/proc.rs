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

    /// Whether process `pid` is running: it can be opened and has not exited, or it exists but
    /// belongs to someone this process may not ask.
    pub fn pid_alive(pid: u32) -> bool {
        pid != 0 && super::win::process_running(pid)
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

/// A running process: its id, its name, the program it runs and its working directory, each as
/// the system says it (`None` where it cannot be read: another user's process, or one that ended
/// while it was asked).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    pub pid: u32,
    pub name: String,
    pub exe: Option<std::path::PathBuf>,
    pub cwd: Option<std::path::PathBuf>,
}

/// The processes running now, this one left out. `Err` when the system's list of processes
/// cannot be read at all.
pub fn running() -> io::Result<Vec<Running>> {
    let me = std::process::id();
    Ok(list_running()?.into_iter().filter(|p| p.pid != me).collect())
}

/// Linux: `/proc/<pid>/exe`, `cwd` and `comm`. A program or directory deleted since reads
/// `<path> (deleted)`; the path is what it was.
#[cfg(target_os = "linux")]
fn list_running() -> io::Result<Vec<Running>> {
    use std::os::unix::ffi::OsStrExt;
    let link = |p: std::path::PathBuf| {
        std::fs::read_link(p).ok().map(|l| match l.as_os_str().as_bytes().strip_suffix(b" (deleted)") {
            Some(path) => std::path::PathBuf::from(std::ffi::OsStr::from_bytes(path)),
            None => l,
        })
    };
    let mut out = vec![];
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let p = entry.path();
        let name = std::fs::read_to_string(p.join("comm")).map(|s| s.trim_end().to_string()).unwrap_or_default();
        out.push(Running { pid, name, exe: link(p.join("exe")), cwd: link(p.join("cwd")) });
    }
    Ok(out)
}

/// macOS: libproc's list of every process, with each one's path (`proc_pidpath`), current
/// directory (`proc_pidinfo`, `PROC_PIDVNODEPATHINFO`) and name.
#[cfg(target_os = "macos")]
fn list_running() -> io::Result<Vec<Running>> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    // Room for the processes started since they were counted.
    let mut pids: Vec<libc::c_int> = vec![0; count as usize + 256];
    let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int) };
    if listed < 0 {
        return Err(io::Error::last_os_error());
    }
    pids.truncate(listed as usize);
    let until_nul = |b: &[u8]| -> Option<std::path::PathBuf> {
        let b = &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())];
        (!b.is_empty()).then(|| std::path::PathBuf::from(OsStr::from_bytes(b)))
    };
    let mut out = vec![];
    for pid in pids.into_iter().filter(|&p| p > 0) {
        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        let exe = if n > 0 { until_nul(&path[..n as usize]) } else { None };
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&mut info as *mut libc::proc_vnodepathinfo).cast(), size) };
        // The directory's path: MAXPATHLEN bytes, NUL-terminated (libc declares them in rows).
        let cwd = if got == size { until_nul(unsafe { std::slice::from_raw_parts(info.pvi_cdir.vip_path.as_ptr().cast::<u8>(), std::mem::size_of_val(&info.pvi_cdir.vip_path)) }) } else { None };
        let mut name = [0u8; 256];
        let n = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
        let name = if n > 0 { String::from_utf8_lossy(&name[..n as usize]).into_owned() } else { String::new() };
        out.push(Running { pid: pid as u32, name, exe, cwd });
    }
    Ok(out)
}

/// Windows: a snapshot of the processes, with each one's program (`QueryFullProcessImageNameW`)
/// and current directory, read from its process parameters.
#[cfg(windows)]
fn list_running() -> io::Result<Vec<Running>> {
    win::processes()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn list_running() -> io::Result<Vec<Running>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "QuickRust cannot list the running processes on this platform"))
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
/// stdin from /dev/null and stdout and stderr to `log`, and nothing else of this process's open
/// (a pipe that a script reads this process's output from stays this process's: the script is not
/// kept waiting for the background work). Init reaps it.
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
        cmd.spawn().map(|_| ())
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | IDLE_PRIORITY_CLASS);
        // Windows hands a new process every inheritable handle of this one, its standard handles
        // included (those it got from whoever started it). The child's own are the log's (std
        // gives every child inheritable copies of what it is to use), so this process's are kept
        // from it while it is created.
        let _kept = win::StandardHandlesKept::new();
        cmd.spawn().map(|_| ())
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetHandleInformation(handle: *mut c_void, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetExitCodeProcess(process: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    /// Whether process `pid` exists and has not exited.
    pub fn process_running(pid: u32) -> bool {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;
        const ERROR_ACCESS_DENIED: i32 = 5;
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            // Another user's process exists; no process with that id does not.
            return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED);
        }
        let mut code = 0u32;
        let asked = unsafe { GetExitCodeProcess(h, &mut code) } != 0;
        unsafe {
            CloseHandle(h);
        }
        !asked || code == STILL_ACTIVE
    }

    // A process's parameters are read at the offsets of a 64-bit process's environment block.
    #[cfg(not(target_pointer_width = "64"))]
    compile_error!("QuickRust reads other processes' current directories as a 64-bit program only");

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry) -> i32;
        fn QueryFullProcessImageNameW(process: *mut c_void, flags: u32, name: *mut u16, size: *mut u32) -> i32;
        fn ReadProcessMemory(process: *mut c_void, address: *const c_void, buffer: *mut c_void, size: usize, read: *mut usize) -> i32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(process: *mut c_void, class: u32, info: *mut c_void, length: u32, returned: *mut u32) -> i32;
    }

    /// PROCESSENTRY32W.
    #[repr(C)]
    struct ProcessEntry {
        size: u32,
        usage: u32,
        pid: u32,
        default_heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    /// A handle, closed when dropped.
    struct Handle(*mut c_void);

    impl Handle {
        fn process(access: u32, pid: u32) -> Option<Handle> {
            let h = unsafe { OpenProcess(access, 0, pid) };
            (!h.is_null()).then_some(Handle(h))
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Every process in a snapshot of them, with its program and current directory where this
    /// process may read them.
    pub fn processes() -> std::io::Result<Vec<super::Running>> {
        const TH32CS_SNAPPROCESS: u32 = 0x2;
        const ERROR_NO_MORE_FILES: i32 = 18;
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot as isize == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let snapshot = Handle(snapshot);
        let mut entry: ProcessEntry = unsafe { std::mem::zeroed() };
        entry.size = std::mem::size_of::<ProcessEntry>() as u32;
        let mut out = vec![];
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
        while more {
            let name = &entry.exe_file[..entry.exe_file.iter().position(|&c| c == 0).unwrap_or(entry.exe_file.len())];
            // Process 0 is the system's idle process.
            if entry.pid != 0 {
                out.push(super::Running { pid: entry.pid, name: String::from_utf16_lossy(name), exe: image(entry.pid), cwd: current_directory(entry.pid) });
            }
            more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }
        let end = std::io::Error::last_os_error();
        if end.raw_os_error() != Some(ERROR_NO_MORE_FILES) {
            return Err(end);
        }
        Ok(out)
    }

    /// The program process `pid` runs.
    fn image(pid: u32) -> Option<std::path::PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        let h = Handle::process(PROCESS_QUERY_LIMITED_INFORMATION, pid)?;
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        (unsafe { QueryFullProcessImageNameW(h.0, 0, buf.as_mut_ptr(), &mut len) } != 0).then(|| std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize])))
    }

    /// The current directory of process `pid`: the `CurrentDirectory` of its process parameters
    /// (a 32-bit process under WOW64 keeps its own, in its 32-bit parameters).
    fn current_directory(pid: u32) -> Option<std::path::PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
        const PROCESS_VM_READ: u32 = 0x0010;
        const PROCESS_BASIC_INFORMATION: u32 = 0;
        const PROCESS_WOW64_INFORMATION: u32 = 26;
        let h = Handle::process(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, pid)?;
        let read = |address: u64, buf: &mut [u8]| -> Option<()> {
            let mut n = 0usize;
            (unsafe { ReadProcessMemory(h.0, address as usize as *const c_void, buf.as_mut_ptr().cast(), buf.len(), &mut n) } != 0 && n == buf.len()).then_some(())
        };
        let query = |class: u32, info: &mut [usize]| unsafe { NtQueryInformationProcess(h.0, class, info.as_mut_ptr().cast(), std::mem::size_of_val(info) as u32, std::ptr::null_mut()) } >= 0;
        let mut wow64 = [0usize; 1];
        if !query(PROCESS_WOW64_INFORMATION, &mut wow64) {
            return None;
        }
        // Where the path's UTF-16 is, and its length in bytes (a UNICODE_STRING).
        let (at, length) = if wow64[0] != 0 {
            // PEB32.ProcessParameters at 0x10; their CurrentDirectory.DosPath at 0x24.
            let mut p = [0u8; 4];
            read(wow64[0] as u64 + 0x10, &mut p)?;
            let mut s = [0u8; 8];
            read(u32::from_le_bytes(p) as u64 + 0x24, &mut s)?;
            (u32::from_le_bytes([s[4], s[5], s[6], s[7]]) as u64, u16::from_le_bytes([s[0], s[1]]))
        } else {
            // PROCESS_BASIC_INFORMATION: the environment block's address is its second word.
            let mut basic = [0usize; 6];
            if !query(PROCESS_BASIC_INFORMATION, &mut basic) {
                return None;
            }
            // PEB.ProcessParameters at 0x20; their CurrentDirectory.DosPath at 0x38.
            let mut p = [0u8; 8];
            read(basic[1] as u64 + 0x20, &mut p)?;
            let mut s = [0u8; 16];
            read(u64::from_le_bytes(p) + 0x38, &mut s)?;
            (u64::from_le_bytes([s[8], s[9], s[10], s[11], s[12], s[13], s[14], s[15]]), u16::from_le_bytes([s[0], s[1]]))
        };
        let mut bytes = vec![0u8; length as usize];
        read(at, &mut bytes)?;
        let wide: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        (!wide.is_empty()).then(|| std::path::PathBuf::from(std::ffi::OsString::from_wide(&wide)))
    }

    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE: (DWORD)-10, -11, -12.
    const STANDARD: [u32; 3] = [-10i32 as u32, -11i32 as u32, -12i32 as u32];

    /// While it lives, this process's standard handles are not inheritable; then as they were.
    pub struct StandardHandlesKept(Vec<*mut c_void>);

    impl StandardHandlesKept {
        pub fn new() -> StandardHandlesKept {
            let mut cleared = Vec::new();
            for which in STANDARD {
                let h = unsafe { GetStdHandle(which) };
                if h.is_null() || h as isize == -1 || cleared.contains(&h) {
                    continue;
                }
                let mut flags = 0u32;
                if unsafe { GetHandleInformation(h, &mut flags) } != 0 && flags & HANDLE_FLAG_INHERIT != 0 && unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) } != 0 {
                    cleared.push(h);
                }
            }
            StandardHandlesKept(cleared)
        }
    }

    impl Drop for StandardHandlesKept {
        fn drop(&mut self) {
            for h in &self.0 {
                unsafe {
                    SetHandleInformation(*h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
                }
            }
        }
    }
}

pub fn hold_signals() -> imp::Guard {
    imp::Guard::install()
}

/// Asks the currently running child to terminate (used for timeouts).
/// Returns false when there is no child or the platform cannot signal it.
pub fn terminate_child() -> bool {
    imp::terminate_child()
}
