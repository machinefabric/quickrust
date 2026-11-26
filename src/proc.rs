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
/// and current directory, read from its process parameters. Each is named by its real path, as
/// Linux and macOS name them: a process names them as it was given them, which can be through a
/// short (8.3) name, a junction or a `subst` drive.
#[cfg(windows)]
fn list_running() -> io::Result<Vec<Running>> {
    win::processes()
}

/// FreeBSD: the kernel's table of processes (`kern.proc.proc`), with each one's name (`ki_comm`),
/// program (`kern.proc.pathname`) and current directory (`kern.proc.cwd`).
#[cfg(target_os = "freebsd")]
fn list_running() -> io::Result<Vec<Running>> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    /// What the sysctl `mib` answers: asked for its size, then read; asked again when it grew in
    /// between (a process started).
    fn sysctl(mib: &[libc::c_int]) -> io::Result<Vec<u8>> {
        loop {
            let mut len: libc::size_t = 0;
            if unsafe { libc::sysctl(mib.as_ptr(), mib.len() as libc::c_uint, std::ptr::null_mut(), &mut len, std::ptr::null(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buf = vec![0u8; len];
            if unsafe { libc::sysctl(mib.as_ptr(), mib.len() as libc::c_uint, buf.as_mut_ptr().cast(), &mut len, std::ptr::null(), 0) } != 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::ENOMEM) {
                    continue;
                }
                return Err(e);
            }
            buf.truncate(len);
            return Ok(buf);
        }
    }
    let until_nul = |b: &[u8]| -> Option<std::path::PathBuf> {
        let b = &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())];
        (!b.is_empty()).then(|| std::path::PathBuf::from(OsStr::from_bytes(b)))
    };
    let table = sysctl(&[libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PROC])?;
    let record = std::mem::size_of::<libc::kinfo_proc>();
    // The directory is one file record, whose path runs from `kf_path` to the record's end.
    let at_path = std::mem::offset_of!(libc::kinfo_file, kf_path);
    let mut out = vec![];
    for at in (0..table.len()).step_by(record) {
        let bytes = &table[at..];
        // Each record begins with its size: a kernel whose records are of another size lays them
        // out otherwise, and none of it is read as this layout.
        let size = bytes.get(..4).map(|b| i32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as usize);
        if size != Some(record) || bytes.len() < record {
            let size = size.map_or_else(|| format!("{} bytes long", bytes.len()), |s| format!("{s} bytes"));
            return Err(io::Error::other(format!("the kernel's process records are {size}, and QuickRust reads records of {record} bytes")));
        }
        let info: libc::kinfo_proc = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast()) };
        let pid = info.ki_pid;
        let name: Vec<u8> = info.ki_comm.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        // Another user's process, or one that ended since the table was read, answers neither.
        let exe = sysctl(&[libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PATHNAME, pid]).ok().and_then(|b| until_nul(&b));
        let cwd = sysctl(&[libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_CWD, pid]).ok().filter(|b| b.len() > at_path).and_then(|b| until_nul(&b[at_path..]));
        out.push(Running { pid: pid as u32, name: String::from_utf8_lossy(&name).into_owned(), exe, cwd });
    }
    Ok(out)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd", windows)))]
fn list_running() -> io::Result<Vec<Running>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "QuickRust cannot list the running processes on this platform"))
}

/// Signal number if a termination signal was forwarded during a `run`.
pub fn interrupted() -> Option<i32> {
    imp::interrupted()
}

/// Starts `cmd` in the background, detached: it outlives this process, and Ctrl-C at this terminal
/// does not reach it; it runs at the lowest scheduling priority, with stdin from the null device
/// and stdout and stderr to `log`; and it is given nothing else of this process's open. A pipe a
/// script reads this process's output from, or a file the script redirected it to, stays the
/// script's: the script is not kept waiting for the background work, and can append to the file
/// or remove it while that work runs. Unix: a session of its own, which init reaps; std opens
/// everything else close-on-exec. Windows: see `win::spawn_detached`.
// The process outlives this one on purpose.
#[allow(clippy::zombie_processes)]
pub fn spawn_detached(cmd: &mut Command, log: std::fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
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
        win::spawn_detached(cmd, &log)
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
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
                out.push(super::Running { pid: entry.pid, name: String::from_utf16_lossy(name), exe: image(entry.pid).map(real), cwd: current_directory(entry.pid).map(real) });
            }
            more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }
        let end = std::io::Error::last_os_error();
        if end.raw_os_error() != Some(ERROR_NO_MORE_FILES) {
            return Err(end);
        }
        Ok(out)
    }

    /// The real path of what a process names `path`; the path as it was named when it is gone (a
    /// directory removed while the process works in it).
    fn real(path: std::path::PathBuf) -> std::path::PathBuf {
        std::fs::canonicalize(&path).unwrap_or(path)
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

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn DuplicateHandle(source_process: *mut c_void, source: *mut c_void, target_process: *mut c_void, target: *mut *mut c_void, access: u32, inherit: i32, options: u32) -> i32;
        fn CreateFileW(name: *const u16, access: u32, share: u32, security: *const SecurityAttributes, disposition: u32, flags: u32, template: *mut c_void) -> *mut c_void;
        fn InitializeProcThreadAttributeList(list: *mut c_void, count: u32, flags: u32, size: *mut usize) -> i32;
        fn UpdateProcThreadAttribute(list: *mut c_void, flags: u32, attribute: usize, value: *const c_void, size: usize, previous: *mut c_void, returned: *mut usize) -> i32;
        fn DeleteProcThreadAttributeList(list: *mut c_void);
        fn CreateProcessW(application: *const u16, command_line: *mut u16, process_security: *const c_void, thread_security: *const c_void, inherit: i32, flags: u32, environment: *const c_void, directory: *const u16, startup: *const StartupInfo, info: *mut ProcessInformation) -> i32;
    }

    /// SECURITY_ATTRIBUTES.
    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit: i32,
    }

    /// STARTUPINFOW.
    #[repr(C)]
    struct StartupInfo {
        size: u32,
        reserved: *mut u16,
        desktop: *mut u16,
        title: *mut u16,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        columns: u32,
        rows: u32,
        fill_attribute: u32,
        flags: u32,
        show_window: u16,
        reserved2_size: u16,
        reserved2: *mut u8,
        input: *mut c_void,
        output: *mut c_void,
        error: *mut c_void,
    }

    /// STARTUPINFOEXW.
    #[repr(C)]
    struct StartupInfoEx {
        info: StartupInfo,
        attributes: *mut c_void,
    }

    /// PROCESS_INFORMATION.
    #[repr(C)]
    struct ProcessInformation {
        process: *mut c_void,
        thread: *mut c_void,
        pid: u32,
        tid: u32,
    }

    /// A process-and-thread attribute list, deleted when dropped.
    struct Attributes(Vec<usize>);

    impl Attributes {
        /// A list with room for `count` attributes.
        fn new(count: u32) -> std::io::Result<Attributes> {
            let mut size = 0usize;
            // Asked with no list, it says how large one is (and fails, for want of the room).
            unsafe {
                InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size);
            }
            let mut words = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
            if unsafe { InitializeProcThreadAttributeList(words.as_mut_ptr().cast(), count, 0, &mut size) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Attributes(words))
        }

        fn as_ptr(&mut self) -> *mut c_void {
            self.0.as_mut_ptr().cast()
        }
    }

    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe {
                DeleteProcThreadAttributeList(self.as_ptr());
            }
        }
    }

    /// `super::spawn_detached` on Windows: no console, a process group of its own (Ctrl-C at this
    /// terminal does not reach it), idle priority, stdin from the null device, and stdout and
    /// stderr to `log`. Windows gives a new process every inheritable handle of its parent unless
    /// it is told which, and this process holds inheritable handles it never asked for: every one
    /// its starter had inheritable when it started it, passed down from process to process, which
    /// need not be the handles that became this process's standard streams. The `cargo check`
    /// Clippy runs through QuickRust holds so the file a script's `cmd /c "cargo clippy >> log
    /// 2>&1"` opened, which only the outer QuickRust had as its standard streams. Each would stay
    /// open for as long as the background process runs. So the process
    /// is made with exactly the handles of its own standard streams, the null device and `log`,
    /// listed as all it inherits (PROC_THREAD_ATTRIBUTE_HANDLE_LIST), which std's spawn cannot ask
    /// for. `cmd` gives the program, by its full path (no search of PATH finds it), its
    /// arguments, its changes to this process's environment and its directory; its standard
    /// streams are these.
    pub fn spawn_detached(cmd: &std::process::Command, log: &std::fs::File) -> std::io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::AsRawHandle;
        const GENERIC_READ: u32 = 0x8000_0000;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        const OPEN_EXISTING: u32 = 3;
        const DUPLICATE_SAME_ACCESS: u32 = 0x2;
        const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;
        const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
        const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
        const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
        let program = std::path::Path::new(cmd.get_program());
        if !program.is_absolute() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{} is not a full path, and a detached process is started by its full path", program.display())));
        }
        let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let application = wide(program.as_os_str());
        let mut line = command_line(program.as_os_str(), cmd.get_args());
        let environment = environment_block(cmd);
        let directory = cmd.get_current_dir().map(|d| wide(d.as_os_str()));
        // What it inherits: inheritable handles of this process's own, closed here once it has its
        // copies. Another thread starting a process meanwhile could take them too; nothing else
        // of this process's is ever made inheritable for this one.
        let inheritable = SecurityAttributes { length: std::mem::size_of::<SecurityAttributes>() as u32, descriptor: std::ptr::null_mut(), inherit: 1 };
        let null_device = wide(std::ffi::OsStr::new("NUL"));
        let input = unsafe { CreateFileW(null_device.as_ptr(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &inheritable, OPEN_EXISTING, 0, std::ptr::null_mut()) };
        if input as isize == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let input = Handle(input);
        let mut output = std::ptr::null_mut();
        if unsafe { DuplicateHandle(GetCurrentProcess(), log.as_raw_handle(), GetCurrentProcess(), &mut output, 0, 1, DUPLICATE_SAME_ACCESS) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let output = Handle(output);
        let inherited = [input.0, output.0];
        let mut attributes = Attributes::new(1)?;
        if unsafe { UpdateProcThreadAttribute(attributes.as_ptr(), 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, inherited.as_ptr().cast(), std::mem::size_of_val(&inherited), std::ptr::null_mut(), std::ptr::null_mut()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut startup: StartupInfoEx = unsafe { std::mem::zeroed() };
        startup.info.size = std::mem::size_of::<StartupInfoEx>() as u32;
        startup.info.flags = STARTF_USESTDHANDLES;
        startup.info.input = input.0;
        startup.info.output = output.0;
        startup.info.error = output.0;
        startup.attributes = attributes.as_ptr();
        let mut info: ProcessInformation = unsafe { std::mem::zeroed() };
        let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | IDLE_PRIORITY_CLASS | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT;
        let directory = directory.as_ref().map_or(std::ptr::null(), |d| d.as_ptr());
        if unsafe { CreateProcessW(application.as_ptr(), line.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 1, flags, environment.as_ptr().cast(), directory, &startup.info, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        drop((Handle(info.process), Handle(info.thread)));
        Ok(())
    }

    /// The command line that gives a program built with Microsoft's C runtime (Rust's among them)
    /// `args` as its arguments, after `program`, the program it runs (null-terminated UTF-16). A
    /// program's path holds no quote, and is quoted whole; an argument is quoted when it is empty
    /// or holds a space, a tab or a quote, its quotes escaped with a backslash and the backslashes
    /// before a quote, or before the closing quote, doubled.
    fn command_line<'a>(program: &std::ffi::OsStr, args: impl Iterator<Item = &'a std::ffi::OsStr>) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        const QUOTE: u16 = b'"' as u16;
        const BACKSLASH: u16 = b'\\' as u16;
        let mut line = vec![QUOTE];
        line.extend(program.encode_wide());
        line.push(QUOTE);
        for arg in args {
            line.push(b' ' as u16);
            let arg: Vec<u16> = arg.encode_wide().collect();
            if !arg.is_empty() && !arg.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == QUOTE) {
                line.extend(arg);
                continue;
            }
            line.push(QUOTE);
            let mut backslashes = 0;
            for c in arg {
                if c == BACKSLASH {
                    backslashes += 1;
                } else {
                    if c == QUOTE {
                        line.extend(std::iter::repeat(BACKSLASH).take(backslashes + 1));
                    }
                    backslashes = 0;
                }
                line.push(c);
            }
            line.extend(std::iter::repeat(BACKSLASH).take(backslashes));
            line.push(QUOTE);
        }
        line.push(0);
        line
    }

    /// The environment `cmd` gives the program it runs, as CreateProcessW takes it: this process's,
    /// with the variables `cmd` sets and removes, sorted by name without regard to case as Windows
    /// keeps them, each `NAME=value` null-terminated, and a null after the last.
    fn environment_block(cmd: &std::process::Command) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        let mut vars: std::collections::BTreeMap<String, (std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os().map(|(k, v)| (k.to_string_lossy().to_uppercase(), (k, v))).collect();
        for (k, v) in cmd.get_envs() {
            let name = k.to_string_lossy().to_uppercase();
            match v {
                Some(v) => vars.insert(name, (k.to_os_string(), v.to_os_string())),
                None => vars.remove(&name),
            };
        }
        let mut block = Vec::new();
        for (k, v) in vars.values() {
            block.extend(k.encode_wide());
            block.push(b'=' as u16);
            block.extend(v.encode_wide());
            block.push(0);
        }
        // An empty environment is two nulls.
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        block
    }

    #[cfg(test)]
    mod tests {
        #[link(name = "shell32")]
        extern "system" {
            fn CommandLineToArgvW(line: *const u16, count: *mut i32) -> *mut *mut u16;
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }

        /// The arguments Windows' own parser reads from `line`, the program first.
        fn parsed(line: &[u16]) -> Vec<String> {
            let mut count = 0;
            let argv = unsafe { CommandLineToArgvW(line.as_ptr(), &mut count) };
            assert!(!argv.is_null(), "{}", std::io::Error::last_os_error());
            let args = (0..count as usize)
                .map(|i| {
                    let arg = unsafe { *argv.add(i) };
                    let len = (0..).take_while(|&n| unsafe { *arg.add(n) } != 0).count();
                    String::from_utf16(unsafe { std::slice::from_raw_parts(arg, len) }).unwrap()
                })
                .collect();
            unsafe {
                LocalFree(argv.cast());
            }
            args
        }

        // TEST0366: A detached process's command line gives the program every argument as it was
        #[test]
        fn test0366_a_detached_processs_command_line_gives_the_program_every_argument_as_it_was() {
            let program = std::ffi::OsStr::new(r"C:\Program Files\q\cargo.exe");
            let args = ["__qrust", "gc", "--background", "", "two words", "tab\there", r#"a"quote"#, r#"back\"slash"#, r"trailing\", r"two trailing\\", r#"\\""#, r"C:\dir with space\"];
            let line = super::command_line(program, args.iter().map(std::ffi::OsStr::new));
            let mut expected = vec![program.to_str().unwrap().to_string()];
            expected.extend(args.iter().map(|a| a.to_string()));
            assert_eq!(parsed(&line), expected);
        }

        // TEST0367: A detached process's environment is this process's with what its command sets and removes
        #[test]
        fn test0367_a_detached_processs_environment_is_this_processs_with_what_its_command_sets_and_removes() {
            let mut c = std::process::Command::new(r"C:\x.exe");
            let (kept, _) = std::env::vars_os().next().expect("this process has an environment");
            c.env("QRUSTTEST_SET", "a=b").env_remove(&kept);
            let block = super::environment_block(&c);
            assert_eq!(block[block.len() - 2..], [0, 0]);
            let vars: Vec<String> = String::from_utf16(&block[..block.len() - 2]).unwrap().split('\0').map(str::to_string).collect();
            assert!(vars.contains(&"QRUSTTEST_SET=a=b".to_string()), "{:?}", vars);
            assert!(!vars.iter().any(|v| v.to_uppercase().starts_with(&format!("{}=", kept.to_string_lossy().to_uppercase()))), "{:?} kept {:?}", vars, kept);
            let names: Vec<String> = vars.iter().map(|v| v.split('=').next().unwrap().to_uppercase()).collect();
            assert!(names.windows(2).all(|w| w[0] < w[1]), "not sorted by name: {:?}", names);
        }
    }
}

/// Holds termination signals for a longer operation (a benchmark) so that an
/// interruption is observed through [`interrupted`] and cleanup can run, instead
/// of the process dying between child runs.
pub fn hold_signals() -> imp::Guard {
    imp::Guard::install()
}

/// Asks the currently running child to terminate (used for timeouts).
/// Returns false when there is no child or the platform cannot signal it.
pub fn terminate_child() -> bool {
    imp::terminate_child()
}

#[cfg(test)]
mod running_tests {
    use super::*;

    // TEST0365: The running processes name a child with its program, its directory and its name, and leave this one out
    #[test]
    fn test0365_the_running_processes_name_a_child_with_its_program_its_directory_and_its_name_and_leave_this_one_out() {
        let dir = std::env::temp_dir().join(format!("quickrust-running-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The system names a process's directory and program by their real paths.
        let real = |p: &std::path::Path| std::fs::canonicalize(p).unwrap();
        let (real_dir, program) = (real(&dir), std::env::current_exe().unwrap());
        // This test binary, sleeping in `dir` (`space::tests::sleeper`).
        let mut child = Command::new(&program)
            .args(["--exact", "space::tests::sleeper", "--ignored", "--quiet"])
            .env("QRUSTTEST_SLEEPER", "1")
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let listed = running();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let listed = listed.expect("the running processes are listed");
        assert!(listed.iter().all(|p| p.pid != std::process::id()), "this process is left out");
        let found = listed.iter().find(|p| p.pid == child.id()).unwrap_or_else(|| panic!("the child {} is not listed among {} processes", child.id(), listed.len()));
        assert_eq!(found.exe.as_deref(), Some(real(&program).as_path()), "{found:?}");
        assert_eq!(found.cwd.as_deref(), Some(real_dir.as_path()), "{found:?}");
        // The system's name for it: its program's file name, cut short where the system cuts it.
        let file = program.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!found.name.is_empty() && file.starts_with(&found.name), "{found:?} is not named after {file}");
    }
}
