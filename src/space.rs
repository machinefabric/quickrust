//! QuickRust's cache keeps itself within its space.
//!
//! Everything under `~/.qrust/cache` can be rebuilt or downloaded again: workspaces' build
//! directories, the artifact cache, compiler drivers, sources of installed programs, toolchain
//! downloads, remembered answers.
//! QuickRust bounds it two ways (`cache.max-size`, `cache.min-free`): it uses at most so much, and
//! leaves at least so much of the disk free. A collection removes, in order:
//!
//! 1. what nothing can use any more, whatever the space: build directories of workspaces that are
//!    gone, drivers of toolchains that are gone, stale downloads and memos, leftovers;
//! 2. while the cache is above its size or the disk below its free space, build directories,
//!    artifact entries and sources of installed programs, least recently used first;
//! 3. crates and git checkouts downloaded into QuickRust's Cargo home: Cargo removes them itself
//!    (`cargo -Zgc clean gc`, which knows when each was last used and waits for builds reading
//!    them), unused for its usual ages at every collection, and, while the bounds are still
//!    exceeded, those unused for a month, a week, a day, an hour, 15 minutes in turn. They come
//!    last: they need the network to come back. Cargo's clean runs only while no Cargo that
//!    QuickRust started is running (see `CargoRuns`): it deletes the tracker rows of registries
//!    it does not find in the Cargo home, such as a packaging run's temporary one, and a running
//!    Cargo that holds such a row's id panics at its next save.
//!
//! A collection goes by its own count of what it frees, never by measuring the disk again while
//! it runs: other processes write to the disk meanwhile, and a snapshot keeps the blocks of every
//! file that changed before it was taken, so the disk would show the removals as no progress, and
//! the collection would remove everything it may.
//!
//! What a snapshot keeps is known, not guessed from what the disk failed to gain: a removed file
//! is kept when it last changed (its status change time, which nothing sets back) before the
//! volume's newest snapshot was taken. On macOS that is the APFS volume's snapshots, Time
//! Machine's hourly local ones among them, listed without privilege (`fs_snapshot_list`). Linux
//! has no unprivileged way to list a btrfs or ZFS file system's snapshots, and a Windows shadow
//! copy keeps what it holds in storage of its own, so neither sees one: a cache under a snapshot
//! there is collected by the disk's own figure. What a collection removed that a snapshot keeps
//! is recorded with the count, by snapshot, and counted as free (`free`) while that snapshot
//! exists, for at most a day, the life of a Time Machine local snapshot. Anything else the disk
//! does not show was taken by another writer, and the bounds go by the disk as it is.
//!
//! Nothing used in the last 15 minutes is removed, nor a build directory a command holds (its
//! lock) or Cargo is building in (Cargo's locks), nor a source an install holds (its lock), nor
//! an artifact entry whose files are mostly linked by something that stays (a build directory
//! in use: removing the entry would gain little space and lose it). A removal renames the thing away first, so
//! a build never sees it half deleted; a build that finds it gone builds it again. Whether a build
//! directory is held or was used is asked again under its lock just before it is removed: a
//! collection runs for minutes, and a run that started since its scan holds directories the scan
//! found idle.
//!
//! Sizes are blocks allocated, and a file with several links (the artifact cache links what it
//! restores into build directories) counts once, and is freed only with its last link.
//!
//! After a build QuickRust checks whether a collection is due (one `statvfs`, on macOS the
//! volume's snapshot list, and a small file read):
//! below the free space (as `free` counts it), above the size by the free space it has lost since
//! the last count, or a day after that count; but not within 15 minutes of one that could not get
//! within the bounds (a disk others fill), when nothing more has become removable. It then runs
//! `qrust gc --background` detached, at the lowest priority.
//! When the disk is nearly full (a quarter of the free space QuickRust keeps) a command collects
//! first, for at most ten seconds. `qrust gc` collects now.

use crate::config::Config;
use crate::util;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What was used this recently is never removed.
pub const PROTECTED_NS: u64 = 15 * 60 * 1_000_000_000;
const DAY_NS: u64 = 24 * 3_600 * 1_000_000_000;
const USAGE: &str = "usage.json";
/// The format of `USAGE`; a count of another is no count, and the next collection writes one.
const USAGE_FORMAT: u64 = 3;
const LOG: &str = "gc-log.jsonl";
const LOG_KEEP: usize = 200;
/// The format of a line of `LOG`; lines of another are not read, and are dropped at the next write.
const LOG_FORMAT: u64 = 3;

/// A filesystem's size and what is available to unprivileged users.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FsSpace {
    pub total: u64,
    pub available: u64,
}

/// The volume a path is on: its space, and when each of its snapshots was taken (wall-clock
/// nanoseconds; none where QuickRust cannot see them, see the module notes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Volume {
    pub space: FsSpace,
    pub snapshots: Vec<u64>,
}

impl Volume {
    /// When its newest snapshot was taken.
    fn newest_snapshot(&self) -> Option<u64> {
        self.snapshots.iter().max().copied()
    }
}

/// The volume `path` is on, measured now.
pub fn volume(path: &Path) -> Result<Volume, String> {
    let space = fs_space(path).map_err(|e| format!("cannot measure the free space of {}: {}", path.display(), e))?;
    let snapshots = snapshot_times(path).map_err(|e| format!("cannot list the snapshots of the volume {} is on: {}", path.display(), e))?;
    Ok(Volume { space, snapshots })
}

/// When each snapshot of the APFS volume `path` is on was taken (`fs_snapshot_list` on the
/// volume's root, which needs no privilege); none on a volume of another kind.
#[cfg(target_os = "macos")]
fn snapshot_times(path: &Path) -> io::Result<Vec<u64>> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }
    extern "C" {
        fn fs_snapshot_list(dirfd: libc::c_int, alist: *mut AttrList, attrbuf: *mut libc::c_void, bufsize: libc::size_t, flags: u32) -> libc::c_int;
    }
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    // The name is not used, but the call refuses a list without it (EINVAL).
    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_CRTIME: u32 = 0x0000_0200;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    let c = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { CStr::from_ptr(st.f_fstypename.as_ptr()) }.to_bytes() != b"apfs" {
        return Ok(vec![]);
    }
    let root = std::fs::File::open(std::path::Path::new(std::ffi::OsStr::from_bytes(unsafe { CStr::from_ptr(st.f_mntonname.as_ptr()) }.to_bytes())))?;
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&root);
    let mut list = AttrList { bitmapcount: ATTR_BIT_MAP_COUNT, reserved: 0, commonattr: ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_CRTIME, volattr: 0, dirattr: 0, fileattr: 0, forkattr: 0 };
    let mut buf = vec![0u8; 16 * 1024];
    let mut times = vec![];
    loop {
        let n = unsafe { fs_snapshot_list(fd, &mut list, buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            return Ok(times);
        }
        // Each entry: its length, the attributes returned (five words), a reference to the name
        // (two words), the creation time (seconds, nanoseconds), the name.
        let mut at = 0usize;
        for _ in 0..n {
            let word = |o: usize| buf.get(o..o + 4).map(|b| u32::from_ne_bytes(b.try_into().expect("four bytes")));
            let long = |o: usize| buf.get(o..o + 8).map(|b| i64::from_ne_bytes(b.try_into().expect("eight bytes")));
            let (Some(len), Some(returned)) = (word(at), word(at + 4)) else { return Err(io::Error::other("fs_snapshot_list returned a truncated entry")) };
            if len < 48 {
                return Err(io::Error::other(format!("fs_snapshot_list returned an entry of {} bytes", len)));
            }
            if returned & ATTR_CMN_CRTIME == 0 {
                return Err(io::Error::other("fs_snapshot_list returned a snapshot without its creation time"));
            }
            let (Some(sec), Some(nsec)) = (long(at + 32), long(at + 40)) else { return Err(io::Error::other("fs_snapshot_list returned a truncated entry")) };
            times.push((sec.max(0) as u64) * 1_000_000_000 + nsec.max(0) as u64);
            at += len as usize;
        }
    }
}

/// None seen: Linux has no unprivileged way to list a btrfs or ZFS file system's snapshots, and a
/// Windows shadow copy keeps what it holds in storage of its own (see the module notes).
#[cfg(not(target_os = "macos"))]
fn snapshot_times(_path: &Path) -> io::Result<Vec<u64>> {
    Ok(vec![])
}

#[cfg(unix)]
pub fn fs_space(path: &Path) -> io::Result<FsSpace> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let unit = st.f_frsize as u64;
    Ok(FsSpace { total: st.f_blocks as u64 * unit, available: st.f_bavail as u64 * unit })
}

#[cfg(windows)]
pub fn fs_space(path: &Path) -> io::Result<FsSpace> {
    use std::os::windows::ffi::OsStrExt;
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FsSpace { total, available })
}

/// The bounds in force: the cache's most, and the disk's least free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub max_bytes: u64,
    pub min_free: u64,
}

/// `auto`, a share (`15%`) or bytes, as `Kind::Space` validated it, for a filesystem of `total`.
fn resolve_space(raw: &str, total: u64, auto: (f64, u64, u64)) -> u64 {
    const GIB: u64 = 1 << 30;
    let share = |p: f64| (total as f64 * p / 100.0) as u64;
    match raw {
        "auto" => share(auto.0).clamp(auto.1 * GIB, auto.2 * GIB),
        r if r.ends_with('%') => share(r.trim_end_matches('%').trim().parse().unwrap_or(100.0)),
        r => r.parse().unwrap_or(u64::MAX),
    }
}

pub fn policy(cfg: &Config, fs: &FsSpace) -> Policy {
    Policy { max_bytes: resolve_space(cfg.str("cache.max-size"), fs.total, (10.0, 10, 100)), min_free: resolve_space(cfg.str("cache.min-free"), fs.total, (5.0, 2, 20)) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Build,
    Artifact,
    Driver,
    Download,
    Memo,
    Leftover,
    /// QuickRust's Cargo home's registry and git downloads (removed by Cargo, see `cargo_gc`).
    Registry,
    /// A copy of an installed program's source (`qrust install`).
    Source,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Build => "build directories",
            Kind::Artifact => "artifact cache",
            Kind::Driver => "compiler drivers",
            Kind::Download => "downloads",
            Kind::Memo => "remembered answers",
            Kind::Leftover => "leftovers",
            Kind::Registry => "crate and git downloads",
            Kind::Source => "program sources",
        }
    }
}

/// One thing the cache can remove as a whole.
#[derive(Clone, Debug)]
pub struct Unit {
    pub kind: Kind,
    pub path: PathBuf,
    /// Files that go with it (a build directory's record and lock).
    pub companions: Vec<PathBuf>,
    pub last_used_ns: u64,
    /// Why nothing can use it any more, if so.
    pub orphan: Option<String>,
    /// Bytes of its files with one link.
    own_bytes: u64,
    /// Of those, the bytes of files the volume's newest snapshot keeps.
    own_kept: u64,
    /// Its files with more than one link: identity, bytes, links in this unit.
    shared: Vec<((u64, u64), u64, u64)>,
}

/// What removing a unit frees: its bytes, and of those, what a snapshot keeps (the disk shows it
/// free only once the snapshot is deleted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Freed {
    bytes: u64,
    kept: u64,
}

impl Unit {
    /// The bytes of its files, those it shares with other units included.
    fn bytes(&self) -> u64 {
        self.own_bytes + self.shared.iter().map(|(_, b, _)| b).sum::<u64>()
    }
}

/// A file with several links, counted across the cache.
#[derive(Clone, Copy, Debug)]
struct LinkCount {
    bytes: u64,
    /// Its links in what the cache still holds.
    left: u64,
    /// Its links in the cache when counted.
    in_cache: u64,
    /// All its links (the filesystem's count).
    nlink: u64,
    /// The volume's newest snapshot keeps it.
    kept: bool,
}

/// Every file with several links, by identity: freed when its last link goes, and only if it
/// has none outside the cache.
#[derive(Clone, Default)]
struct Links {
    map: HashMap<(u64, u64), LinkCount>,
}

impl Links {
    /// What removing `unit` frees; the counts are updated as if it were removed.
    fn release(&mut self, unit: &Unit) -> Freed {
        let mut freed = Freed { bytes: unit.own_bytes, kept: unit.own_kept };
        for (id, _, here) in &unit.shared {
            if let Some(c) = self.map.get_mut(id) {
                c.left = c.left.saturating_sub(*here);
                if c.left == 0 && c.in_cache >= c.nlink {
                    freed.bytes += c.bytes;
                    if c.kept {
                        freed.kept += c.bytes;
                    }
                }
            }
        }
        freed
    }
}

/// When `md`'s file last changed, its data or its links (the status change time, which nothing
/// sets back): a snapshot taken after it keeps its blocks.
#[cfg(unix)]
fn changed_ns(md: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    (md.ctime().max(0) as u64) * 1_000_000_000 + md.ctime_nsec().max(0) as u64
}

/// No snapshot is seen off Unix (see `snapshot_times`), so nothing is ever kept.
#[cfg(not(unix))]
fn changed_ns(_md: &fs::Metadata) -> u64 {
    u64::MAX
}

/// Whether the snapshot taken at `newest_snapshot` keeps the file of `md`: it changed before.
fn kept_by(md: &fs::Metadata, newest_snapshot: Option<u64>) -> bool {
    newest_snapshot.is_some_and(|t| changed_ns(md) < t)
}

fn allocated(md: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        md.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        md.len()
    }
}

#[cfg(unix)]
fn identity(md: &fs::Metadata) -> Option<((u64, u64), u64)> {
    use std::os::unix::fs::MetadataExt;
    (md.nlink() > 1).then(|| ((md.dev(), md.ino()), md.nlink()))
}

#[cfg(not(unix))]
fn identity(_md: &fs::Metadata) -> Option<((u64, u64), u64)> {
    None
}

/// Adds the file or directory of `md` to `unit`'s own bytes.
fn add_own(unit: &mut Unit, md: &fs::Metadata, newest_snapshot: Option<u64>) {
    unit.own_bytes += allocated(md);
    if kept_by(md, newest_snapshot) {
        unit.own_kept += allocated(md);
    }
}

/// Measures `path` into `unit` (links not followed), with what the snapshot taken at
/// `newest_snapshot` keeps.
fn measure(path: &Path, unit: &mut Unit, all_links: &mut HashMap<(u64, u64), LinkCount>, newest_snapshot: Option<u64>) {
    let mut stack = vec![path.to_path_buf()];
    let mut here: HashMap<(u64, u64), (u64, u64)> = HashMap::new();
    while let Some(p) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&p) else { continue };
        if md.is_dir() {
            add_own(unit, &md, newest_snapshot);
            stack.extend(fs::read_dir(&p).into_iter().flatten().flatten().map(|e| e.path()));
            continue;
        }
        match identity(&md) {
            Some((id, nlink)) => {
                let e = here.entry(id).or_insert((allocated(&md), 0));
                e.1 += 1;
                let g = all_links.entry(id).or_insert(LinkCount { bytes: allocated(&md), left: 0, in_cache: 0, nlink, kept: kept_by(&md, newest_snapshot) });
                g.left += 1;
                g.in_cache += 1;
            }
            None => add_own(unit, &md, newest_snapshot),
        }
    }
    unit.shared = here.into_iter().map(|(id, (bytes, n))| (id, bytes, n)).collect();
}

/// Everything in the cache, measured.
pub struct Scan {
    pub units: Vec<Unit>,
    links: Links,
}

impl Scan {
    /// The cache's size: each file once.
    pub fn total(&self) -> u64 {
        self.units.iter().map(|u| u.own_bytes).sum::<u64>() + self.links.map.values().map(|c| c.bytes).sum::<u64>()
    }

    pub fn by_kind(&self) -> Vec<(Kind, usize, u64)> {
        let mut m: std::collections::BTreeMap<Kind, (usize, u64)> = Default::default();
        for u in &self.units {
            let e = m.entry(u.kind).or_default();
            e.0 += 1;
            e.1 += u.bytes();
        }
        m.into_iter().map(|(k, (n, b))| (k, n, b)).collect()
    }
}

fn mtime(p: &Path) -> u64 {
    fs::symlink_metadata(p).map(|m| util::mtime_ns(&m)).unwrap_or(0)
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    v.sort();
    v
}

/// QuickRust's Cargo home, a Cargo to collect its downloads with, and the processes that run
/// Cargo with that home.
#[derive(Clone, Debug)]
pub struct Downloads {
    pub cargo_home: PathBuf,
    pub cargo: PathBuf,
    pub runs: CargoRuns,
}

/// QuickRust's Cargo home with the newest installed toolchain's Cargo; `None` without a toolchain.
pub fn downloads() -> Option<Downloads> {
    let layout = crate::toolchain::Layout::current();
    let newest = crate::toolchain::installed(&layout).into_iter().filter_map(|tc| Some((semver::Version::parse(&tc.record()?.version).ok()?, tc))).max_by(|a, b| a.0.cmp(&b.0))?;
    Some(Downloads { cargo_home: util::cargo_home(), cargo: newest.1.cargo(), runs: CargoRuns::current() })
}

/// The QuickRust processes that may be running a Cargo with QuickRust's Cargo home: one empty file
/// per process, named by its id, in `$QRUST_HOME/cargo-runs`, written and read under the lock
/// beside them. A process registers before it starts the first (`engine::for_command`); an exec'd
/// Cargo keeps its process id, and so its record. A toolchain program run through a proxy does not
/// register (see `engine::for_tool`). A record whose process is gone holds nothing, and
/// whoever next registers or reads removes it.
///
/// Cargo's own clean (`cargo -Zgc clean gc`) runs only while it holds the lock and no process
/// is registered (`idle`); a command that starts meanwhile waits for it, as it waits for Cargo's
/// package lock.
#[derive(Clone, Debug)]
pub struct CargoRuns {
    dir: PathBuf,
}

/// This process's record, while it has one.
static REGISTERED: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

impl CargoRuns {
    pub fn current() -> CargoRuns {
        CargoRuns { dir: util::qrust_home().join("cargo-runs") }
    }

    fn lock(&self) -> PathBuf {
        self.dir.join(".lock")
    }

    /// Records process `pid`; `waiting` is called first when a collection holds the lock.
    fn register(&self, pid: u32, waiting: impl FnOnce()) -> io::Result<PathBuf> {
        let _lock = match util::ExclusiveLock::try_acquire(&self.lock())? {
            Some(l) => l,
            None => {
                waiting();
                util::ExclusiveLock::acquire(&self.lock())?
            }
        };
        self.running();
        let record = self.dir.join(pid.to_string());
        fs::write(&record, b"")?;
        Ok(record)
    }

    /// The processes registered and running; the records of those that are gone are removed.
    /// Read under the lock.
    fn running(&self) -> Vec<u32> {
        let mut alive = vec![];
        for p in entries(&self.dir) {
            let Some(pid) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<u32>().ok()) else { continue };
            if crate::proc::pid_alive(pid) {
                alive.push(pid);
            } else {
                let _ = fs::remove_file(&p);
            }
        }
        alive
    }

    /// The lock, held, when no registered process is running: Cargo's clean may run while it is
    /// held. Otherwise the processes that are.
    fn idle(&self) -> io::Result<Result<util::ExclusiveLock, Vec<u32>>> {
        let lock = util::ExclusiveLock::acquire(&self.lock())?;
        let running = self.running();
        Ok(if running.is_empty() { Ok(lock) } else { Err(running) })
    }
}

/// Records that this process runs Cargo with QuickRust's Cargo home, once (see `CargoRuns`).
pub fn running_cargo() -> Result<(), String> {
    let mut registered = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    if registered.is_some() {
        return Ok(());
    }
    let runs = CargoRuns::current();
    let record = runs.register(std::process::id(), || crate::ui::note("waiting for QuickRust to finish collecting Cargo's downloads")).map_err(|e| format!("cannot record in {} that this command runs Cargo, and a collection could remove Cargo's records under it: {}", runs.dir.display(), e))?;
    *registered = Some(record);
    Ok(())
}

/// This process's Cargo has ended: its record goes, so that the collection it starts next may run
/// Cargo's clean.
pub fn done_running_cargo() {
    if let Some(record) = REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = fs::remove_file(record);
    }
}

/// When the bounds need room from Cargo's downloads: what was used longer ago than each of these
/// is removed in turn, until there is enough. The last is the protection window: like the rest
/// of the cache, nothing used in it is removed (a size limit would let Cargo remove the crates a
/// build has just extracted, and the next build would extract them again).
const CARGO_AGES: &[(&str, u64)] = &[("30 days", 30 * DAY_NS), ("7 days", 7 * DAY_NS), ("1 day", DAY_NS), ("1 hour", DAY_NS / 24), ("15 minutes", PROTECTED_NS)];
/// The shortest of Cargo's own ages (`gc.auto`: sources and checkouts unused for a month).
const CARGO_OWN_AGE_NS: u64 = 30 * DAY_NS;

/// When the oldest of Cargo's downloads came: a `.crate`, an extracted source, a git clone or
/// checkout, by the time its file was made. Nothing was last used before it was downloaded, so
/// a collection by an age younger than this can remove nothing, and is not run (it would hold
/// Cargo's package lock, which builds wait for, for nothing).
fn oldest_download_ns(d: &Downloads) -> Option<u64> {
    let mut oldest: Option<u64> = None;
    for dir in ["registry/cache", "registry/src", "git/checkouts"] {
        for group in entries(&d.cargo_home.join(dir)) {
            for e in entries(&group) {
                let t = mtime(&e);
                oldest = Some(oldest.map_or(t, |o| o.min(t)));
            }
        }
    }
    for e in entries(&d.cargo_home.join("git/db")) {
        let t = mtime(&e);
        oldest = Some(oldest.map_or(t, |o| o.min(t)));
    }
    oldest
}

/// Cargo's collection of the downloads in `d.cargo_home`: what was not used for `max_age`, or by
/// Cargo's own ages. Cargo's summary line, and the bytes it says it removed (or would).
fn cargo_gc(d: &Downloads, max_age: Option<&str>, dry_run: bool) -> Result<(String, u64), String> {
    let mut c = crate::engine::toolchain_program(&d.cargo);
    c.args(["-Zgc", "clean", "gc"]).env("CARGO_HOME", &d.cargo_home).env("RUSTC_BOOTSTRAP", "1").env("CARGO_TERM_COLOR", "never").current_dir(&d.cargo_home).stdin(std::process::Stdio::null());
    if dry_run {
        c.arg("--dry-run");
    }
    if let Some(age) = max_age {
        c.arg(format!("--max-download-age={}", age));
    }
    let out = c.output().map_err(|e| format!("cannot run {}: {}", d.cargo.display(), e))?;
    let err = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!("Cargo could not collect its downloads: {}", err.lines().find(|l| l.contains("error")).unwrap_or(err.trim())));
    }
    // `Removed 308 files, 4.8MiB total`, or with --dry-run `Summary 67 files, 3.8MiB total`.
    let line = err.lines().map(str::trim).find(|l| l.starts_with("Removed ") || l.starts_with("Summary ")).unwrap_or("").to_string();
    let bytes = line.rsplit_once(", ").and_then(|(_, t)| t.strip_suffix(" total")).and_then(util::parse_bytes).unwrap_or(0);
    Ok((line, bytes))
}

/// The bytes QuickRust's Cargo home's downloads take: registry, git.
fn download_bytes(d: &Downloads) -> (u64, u64) {
    let mut all = HashMap::new();
    let mut size = |sub: &str| {
        let mut u = Unit { kind: Kind::Registry, path: d.cargo_home.join(sub), companions: vec![], last_used_ns: 0, orphan: None, own_bytes: 0, own_kept: 0, shared: vec![] };
        measure(&d.cargo_home.join(sub), &mut u, &mut all, None);
        u.own_bytes + u.shared.iter().map(|(_, b, _)| b).sum::<u64>()
    };
    (size("registry"), size("git"))
}

/// Everything in the cache at `cache`, measured `now`, with what the volume's snapshot taken at
/// `newest_snapshot` keeps.
pub fn scan(cache: &Path, now: u64, newest_snapshot: Option<u64>) -> Scan {
    let mut units: Vec<Unit> = vec![];
    let mut all: HashMap<(u64, u64), LinkCount> = HashMap::new();
    let mut add = |kind: Kind, path: PathBuf, companions: Vec<PathBuf>, last_used_ns: u64, orphan: Option<String>| {
        let mut u = Unit { kind, path, companions, last_used_ns, orphan, own_bytes: 0, own_kept: 0, shared: vec![] };
        measure(&u.path.clone(), &mut u, &mut all, newest_snapshot);
        for c in u.companions.clone() {
            if let Ok(md) = fs::symlink_metadata(&c) {
                add_own(&mut u, &md, newest_snapshot);
            }
        }
        units.push(u);
    };
    // Workspaces' build directories, with their records and locks.
    let builds = cache.join("builds");
    let in_builds = entries(&builds);
    // The directories a running process holds (`builddir::hold`), by name.
    let held: std::collections::HashSet<String> = in_builds
        .iter()
        .filter(|p| !p.is_dir())
        .filter_map(|p| crate::builddir::hold_of(&util::lossy(p.file_name().unwrap_or_default())).filter(|(_, pid)| crate::proc::pid_alive(*pid)).map(|(dir, _)| dir.to_string()))
        .collect();
    for p in in_builds {
        let name = util::lossy(p.file_name().unwrap_or_default());
        if p.is_dir() {
            let companions = vec![p.with_file_name(format!("{}.json", name)), p.with_file_name(format!("{}.lock", name))];
            let (last_used, orphan) = match crate::builddir::meta(&p) {
                Some((root, _, used)) if root.join("Cargo.toml").is_file() => (used, None),
                Some((root, _, used)) => (used, Some(format!("its workspace {} is gone", root.display()))),
                None => (mtime(&p), Some("it has no record of its workspace".to_string())),
            };
            // Held is in use, now: whatever its record says of when it was last built in.
            let last_used = if held.contains(&name) { now } else { last_used };
            add(Kind::Build, p, companions, last_used, orphan);
        } else if let Some((_, pid)) = crate::builddir::hold_of(&name) {
            if !crate::proc::pid_alive(pid) {
                add(Kind::Leftover, p.clone(), vec![], mtime(&p), Some(format!("the process that held the build directory ({}) is gone", pid)));
            }
        } else if let Some(stem) = name.strip_suffix(".json").or_else(|| name.strip_suffix(".lock")) {
            if !builds.join(stem).exists() {
                add(Kind::Leftover, p.clone(), vec![], mtime(&p), Some("the build directory it belonged to is gone".into()));
            }
        }
    }
    // The artifact cache: entries, and the small records next to them.
    let artifacts = cache.join("artifacts");
    for p in entries(&artifacts.join("e")) {
        add(Kind::Artifact, p.clone(), vec![], mtime(&p.join("entry.json")), None);
    }
    for (sub, days) in [("h", 14), ("q", 14), ("m", 30)] {
        for p in entries(&artifacts.join(sub)) {
            let t = mtime(&p);
            let stale = t + days * DAY_NS < now;
            add(Kind::Memo, p, vec![], t, stale.then(|| format!("unused for {} days", days)));
        }
    }
    for p in entries(&artifacts.join("l")) {
        let live = crate::artifact_cache::link_record_live(&artifacts, &p);
        add(Kind::Memo, p.clone(), vec![], mtime(&p), (!live).then(|| "the library it describes is gone".to_string()));
    }
    // Compiler drivers: for a toolchain that is gone, or builds that did not finish.
    for p in entries(&cache.join("drivers")) {
        let name = util::lossy(p.file_name().unwrap_or_default());
        let orphan = if name.starts_with("build-") {
            (mtime(&p) + DAY_NS < now).then(|| "an unfinished driver build".to_string())
        } else {
            match util::read_json(&p.join(crate::driver::COMPILER)).and_then(|v| v["sysroot"].as_str().map(PathBuf::from)) {
                Some(sysroot) if !sysroot.exists() => Some(format!("its toolchain {} is gone", sysroot.display())),
                _ => None,
            }
        };
        add(Kind::Driver, p.clone(), vec![], mtime(&p), orphan);
    }
    // Sources of installed programs, with their locks; copies that never finished.
    let sources = cache.join("src");
    for p in entries(&sources) {
        let name = util::lossy(p.file_name().unwrap_or_default());
        if name.starts_with('.') && name.ends_with(".part") {
            let t = mtime(&p);
            add(Kind::Leftover, p, vec![], t, (t + 3_600 * 1_000_000_000 < now).then(|| "a copy that never finished".to_string()));
        } else if p.is_dir() {
            // When an install last used it (an unfinished clone: when it was made).
            let complete = p.join(crate::install::COMPLETE);
            let used = if complete.is_file() { mtime(&complete) } else { mtime(&p) };
            add(Kind::Source, p.clone(), vec![crate::install::copy_lock(&p)], used, None);
        } else if let Some(stem) = name.strip_suffix(".lock") {
            if !sources.join(stem).exists() {
                add(Kind::Leftover, p.clone(), vec![], mtime(&p), Some("the source it belonged to is gone".into()));
            }
        }
    }
    // Downloads that never finished, remembered answers, leftovers of removals.
    for p in entries(&cache.join("downloads")) {
        let t = mtime(&p);
        add(Kind::Download, p, vec![], t, (t + 7 * DAY_NS < now).then(|| "an unfinished download, a week old".to_string()));
    }
    for kind in entries(&cache.join("memo")) {
        for p in entries(&kind) {
            let t = mtime(&p);
            add(Kind::Memo, p, vec![], t, (t + 30 * DAY_NS < now).then(|| "unused for 30 days".to_string()));
        }
    }
    for dir in [cache.join("tmp"), artifacts.join("tmp")] {
        for p in entries(&dir) {
            let t = mtime(&p);
            add(Kind::Leftover, p, vec![], t, (t + 3_600 * 1_000_000_000 < now).then(|| "left by a removal that stopped".to_string()));
        }
    }
    Scan { units, links: Links { map: all } }
}

/// Why a collection removed something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// Nothing can use it any more; the reason.
    Unusable(String),
    /// The bounds needed the room, and it was used longest ago.
    LeastRecentlyUsed,
    /// Cargo removed it from its downloads; its summary.
    Cargo(String),
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Why::Unusable(reason) => f.write_str(reason),
            Why::LeastRecentlyUsed => f.write_str("used least recently"),
            Why::Cargo(summary) => write!(f, "Cargo: {}", summary),
        }
    }
}

/// One thing a collection removed (or, in a dry run, would).
#[derive(Clone, Debug)]
pub struct Removal {
    pub kind: Kind,
    pub path: PathBuf,
    /// The bytes its removal frees (shared files with their last link only).
    pub bytes: u64,
    pub why: Why,
}

/// A collection's outcome.
#[derive(Debug, Default)]
pub struct Collected {
    pub before: u64,
    pub after: u64,
    /// The free space it started from, as the bounds count it (`free`).
    pub free_before: u64,
    pub removed: Vec<Removal>,
    /// What would have been removed but is in use.
    pub skipped_in_use: usize,
    pub stopped_early: bool,
    /// The bounds were still exceeded at the end: everything else was in use or too recent.
    pub unmet: bool,
    /// What it and the collections before it removed that the volume's snapshots keep: counted
    /// as free while they do (see `Kept`).
    pub kept: u64,
    /// Why Cargo's downloads were left, when they were: a Cargo QuickRust started was running.
    pub cargo_left: Option<String>,
    /// Why the collection stopped short (a removal or Cargo failed), when it did; what it did
    /// until then is recorded all the same.
    pub failed: Option<String>,
}

pub struct Goal {
    /// Remove until the cache is at most this big.
    pub max_total: u64,
    /// Remove until the disk has this much free.
    pub min_free: u64,
}

/// The goal for `policy` now, with `total` in the cache and `free` on the disk (`free`): back under
/// 90% of the size, and a quarter above the free space.
pub fn goal(policy: &Policy, total: u64, free: u64) -> Goal {
    Goal { max_total: if total > policy.max_bytes { policy.max_bytes / 10 * 9 } else { u64::MAX }, min_free: if free < policy.min_free { policy.min_free / 4 * 5 } else { 0 } }
}

/// Bytes collections removed that a snapshot of the volume keeps: every file of them changed
/// before the snapshot taken at `snapshot_ns`, the volume's newest when they were removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kept {
    snapshot_ns: u64,
    /// When the first of them was removed.
    removed_ns: u64,
    bytes: u64,
}

impl Kept {
    fn of(v: &Value) -> Option<Kept> {
        Some(Kept { snapshot_ns: v["snapshot_ns"].as_u64()?, removed_ns: v["removed_ns"].as_u64()?, bytes: v["bytes"].as_u64()? })
    }

    fn json(&self) -> Value {
        json!({ "snapshot_ns": self.snapshot_ns, "removed_ns": self.removed_ns, "bytes": self.bytes })
    }

    /// Whether it still counts as free on a volume whose snapshots were taken at `snapshots`,
    /// `now`: while its snapshot exists, and for a day at most (a Time Machine local snapshot's
    /// life; past it the disk's own figure is taken, see the module notes).
    fn counts(&self, snapshots: &[u64], now: u64) -> bool {
        self.removed_ns + DAY_NS > now && snapshots.contains(&self.snapshot_ns)
    }
}

/// `kept`, one entry per snapshot: their bytes summed, from the first removal.
fn merged(kept: impl IntoIterator<Item = Kept>) -> Vec<Kept> {
    let mut by_snapshot: std::collections::BTreeMap<u64, Kept> = Default::default();
    for k in kept {
        by_snapshot
            .entry(k.snapshot_ns)
            .and_modify(|e| {
                e.bytes += k.bytes;
                e.removed_ns = e.removed_ns.min(k.removed_ns);
            })
            .or_insert(k);
    }
    by_snapshot.into_values().collect()
}

/// The cache's last count, as `USAGE` records it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Count {
    scanned_ns: u64,
    total_bytes: u64,
    /// The disk's free space when it was taken.
    free_at_scan: u64,
    /// What collections removed that the volume's snapshots kept then.
    kept: Vec<Kept>,
    /// The collection could not get within the bounds.
    unmet: bool,
}

impl Count {
    /// The count `v` records, if it is one of `USAGE_FORMAT` with every field.
    fn of(v: &Value) -> Option<Count> {
        if v["format"] != json!(USAGE_FORMAT) {
            return None;
        }
        let kept = v["kept_by_snapshots"].as_array()?.iter().map(Kept::of).collect::<Option<Vec<Kept>>>()?;
        Some(Count { scanned_ns: v["scanned_ns"].as_u64()?, total_bytes: v["total_bytes"].as_u64()?, free_at_scan: v["free_at_scan"].as_u64()?, kept, unmet: v["unmet"].as_bool()? })
    }

    /// What it records as kept that still counts on `volume`, `now`.
    fn kept_on(&self, volume: &Volume, now: u64) -> Vec<Kept> {
        self.kept.iter().copied().filter(|k| k.counts(&volume.snapshots, now)).collect()
    }
}

/// The last count of the cache at `cache`.
fn count(cache: &Path) -> Option<Count> {
    util::read_json(&cache.join(USAGE)).as_ref().and_then(Count::of)
}

/// What the cache at `cache` removed that `volume`'s snapshots keep, as counted `now`.
fn kept(cache: &Path, volume: &Volume, now: u64) -> u64 {
    count(cache).map(|c| c.kept_on(volume, now).iter().map(|k| k.bytes).sum()).unwrap_or(0)
}

/// The disk's free space as the bounds count it: what `volume` measures `now`, and what the cache
/// at `cache` removed that its snapshots keep (see the module notes).
pub fn free(cache: &Path, volume: &Volume, now: u64) -> u64 {
    volume.space.available.saturating_add(kept(cache, volume, now))
}

/// Removes a build directory unless a command or Cargo uses it, or a program's source unless an
/// install uses it.
fn remove_locked_dir(cache: &Path, dir: &Path, companions: &[PathBuf], build: bool) -> Result<bool, String> {
    let lock_path = companions.iter().find(|c| c.extension().is_some_and(|e| e == "lock"));
    let ours = match lock_path {
        Some(l) => match util::ExclusiveLock::try_acquire(l).map_err(|e| e.to_string())? {
            Some(g) => Some(g),
            None => return Ok(false),
        },
        None => None,
    };
    // What the scan said of a build directory is as old as the collection: one that removes
    // gigabytes runs for minutes, and a run that started meanwhile holds or builds in directories
    // the scan found idle. Under its lock nothing new starts using it, so it is asked again here.
    if build && (cargo_building_in(dir) || used_since_scan(dir)) {
        return Ok(false);
    }
    let doomed = cache.join("tmp").join(format!("{}.{}", util::lossy(dir.file_name().unwrap_or_default()), std::process::id()));
    util::create_private_dir(&cache.join("tmp")).map_err(|e| e.to_string())?;
    fs::rename(dir, &doomed).map_err(|e| format!("cannot remove {}: {}", dir.display(), e))?;
    for c in companions {
        let _ = fs::remove_file(c);
    }
    // Gone from where anything looks for it: what waits for the lock (a build, a hold) need not
    // wait for gigabytes to be deleted.
    drop(ours);
    let _ = fs::remove_dir_all(&doomed);
    Ok(true)
}

/// Whether a build directory is in use now, whatever the scan found: held by a running process,
/// or used within the protection by its record.
fn used_since_scan(dir: &Path) -> bool {
    crate::builddir::held(dir) || crate::builddir::meta(dir).is_some_and(|(_, _, used)| used + PROTECTED_NS > util::now_wall_ns())
}

/// Whether a Cargo holds one of its locks in this target directory (`<dir>/<profile>/.cargo-lock`
/// and the like, two levels down).
fn cargo_building_in(dir: &Path) -> bool {
    let mut candidates = vec![];
    for a in entries(dir) {
        if a.is_dir() {
            candidates.extend(entries(&a).into_iter().filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".cargo-") && n.to_string_lossy().ends_with("lock"))));
        } else if a.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".cargo-") && n.to_string_lossy().ends_with("lock")) {
            candidates.push(a);
        }
    }
    candidates.iter().any(|lock| matches!(util::ExclusiveLock::try_acquire(lock), Ok(None)))
}

fn remove(cache: &Path, unit: &Unit) -> Result<bool, String> {
    match unit.kind {
        Kind::Build => remove_locked_dir(cache, &unit.path, &unit.companions, true),
        Kind::Source => remove_locked_dir(cache, &unit.path, &unit.companions, false),
        Kind::Artifact => Ok(crate::artifact_cache::discard(&cache.join("artifacts"), &unit.path)),
        _ => {
            let md = fs::symlink_metadata(&unit.path).map_err(|e| e.to_string());
            let r = match md {
                Ok(m) if m.is_dir() => fs::remove_dir_all(&unit.path),
                Ok(_) => fs::remove_file(&unit.path),
                Err(_) => return Ok(true),
            };
            r.map(|_| true).map_err(|e| format!("cannot remove {}: {}", unit.path.display(), e))
        }
    }
}

/// Collects the cache at `cache`, and the downloads of `downloads` (see the module notes).
/// `dry_run`: says what would go. `max_total`: removes down to this size whatever the policy
/// says (`qrust gc --to`). With a `deadline`, Cargo's downloads are left alone (Cargo waits for
/// builds reading them). A removal or Cargo that fails stops it (`Collected::failed`), and what it
/// did until then is counted and logged like any collection; `Err` only when the volume cannot be
/// measured.
pub fn collect(cache: &Path, downloads: Option<&Downloads>, policy: &Policy, max_total: Option<u64>, dry_run: bool, deadline: Option<Instant>) -> Result<Collected, String> {
    collect_on(cache, downloads, policy, max_total, dry_run, deadline, &|| volume(cache))
}

/// `collect`, with the volume measured by `measure`.
fn collect_on(cache: &Path, downloads: Option<&Downloads>, policy: &Policy, max_total: Option<u64>, dry_run: bool, deadline: Option<Instant>, measure: &dyn Fn() -> Result<Volume, String>) -> Result<Collected, String> {
    let now = util::now_wall_ns();
    let start = measure()?;
    // What earlier collections removed that the volume's snapshots still keep counts as free, as
    // it does for the bounds outside a collection.
    let carried = count(cache).map(|c| c.kept_on(&start, now)).unwrap_or_default();
    let newest_snapshot = start.newest_snapshot();
    let mut scan = scan(cache, now, newest_snapshot);
    let (registry_before, git_before) = downloads.map(download_bytes).unwrap_or((0, 0));
    let mut out = Collected { before: scan.total() + registry_before + git_before, free_before: start.space.available.saturating_add(carried.iter().map(|k| k.bytes).sum()), ..Default::default() };
    // Of what it removes, the bytes the newest snapshot keeps.
    let mut kept_now = 0u64;
    let mut total = out.before;
    // The free space, by this collection's count of what it has freed: the disk is not measured
    // again until the end (see the module notes).
    let mut available = out.free_before;
    let mut units = std::mem::take(&mut scan.units);
    let late = |u: &Unit| u.last_used_ns + PROTECTED_NS > now;
    // 1: what nothing can use.
    let (orphans, rest): (Vec<Unit>, Vec<Unit>) = units.drain(..).partition(|u| u.orphan.is_some());
    // What stays whatever the bounds: the files it links are not freed by removing another unit.
    let mut staying: Vec<Unit> = vec![];
    for u in orphans {
        // Even what nothing can use waits out the protection: a command writes a build
        // directory's record before Cargo makes the directory.
        if late(&u) {
            staying.push(u);
            continue;
        }
        let freed = scan.links.release(&u);
        match if dry_run { Ok(true) } else { remove(cache, &u) } {
            Ok(true) => {
                total = total.saturating_sub(freed.bytes);
                available = available.saturating_add(freed.bytes);
                kept_now += freed.kept;
                let why = Why::Unusable(u.orphan.clone().expect("what nothing can use says why"));
                out.removed.push(Removal { kind: u.kind, path: u.path, bytes: freed.bytes, why });
            }
            Ok(false) => out.skipped_in_use += 1,
            Err(e) => {
                out.failed = Some(e);
                break;
            }
        }
    }
    // 2: least recently used, while the bounds are exceeded.
    let mut goal = goal(policy, total, available);
    if let Some(m) = max_total {
        goal.max_total = goal.max_total.min(m);
    }
    let (mut candidates, others): (Vec<Unit>, Vec<Unit>) = rest.into_iter().partition(|u| matches!(u.kind, Kind::Build | Kind::Artifact | Kind::Source));
    candidates.sort_by_key(|u| (u.last_used_ns, u.path.clone()));
    staying.extend(others);
    staying.extend(candidates.iter().filter(|u| late(u)).cloned());
    // An artifact entry most of whose bytes are files something staying links (a build
    // directory in use, or a link from outside the cache) is kept: removing it would lose it and
    // gain little space. (A build directory is not: what it has of its own, incremental state
    // above all, is freed, and its outputs come back from the artifact cache.)
    let held: std::collections::HashSet<(u64, u64)> = staying.iter().flat_map(|u| u.shared.iter().map(|(id, _, _)| *id)).chain(scan.links.map.iter().filter(|(_, c)| c.in_cache < c.nlink).map(|(id, _)| *id)).collect();
    let mostly_held = |u: &Unit| {
        let freeable = u.own_bytes + u.shared.iter().filter(|(id, _, _)| !held.contains(id)).map(|(_, b, _)| b).sum::<u64>();
        freeable * 2 < u.bytes()
    };
    for u in candidates {
        if out.failed.is_some() || (total <= goal.max_total && available >= goal.min_free) {
            break;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            out.stopped_early = true;
            break;
        }
        if late(&u) {
            out.skipped_in_use += 1;
            continue;
        }
        if u.kind == Kind::Artifact && mostly_held(&u) {
            continue;
        }
        let freed = scan.links.release(&u);
        match if dry_run { Ok(true) } else { remove(cache, &u) } {
            Ok(true) => {
                total = total.saturating_sub(freed.bytes);
                available = available.saturating_add(freed.bytes);
                kept_now += freed.kept;
                out.removed.push(Removal { kind: u.kind, path: u.path, bytes: freed.bytes, why: Why::LeastRecentlyUsed });
            }
            Ok(false) => out.skipped_in_use += 1,
            Err(e) => out.failed = Some(e),
        }
    }
    // 3: Cargo's downloads, by its ages, and down to what the bounds leave if still needed; only
    // while no Cargo QuickRust started runs (see `CargoRuns`).
    if let (Some(d), None, None) = (downloads, deadline, &out.failed) {
        match d.runs.idle() {
            Err(e) => out.failed = Some(format!("Cargo's downloads were not collected: cannot read which commands run Cargo ({}): {}", d.runs.dir.display(), e)),
            Ok(Err(running)) => out.cargo_left = Some(format!("Cargo's downloads were left: {} QuickRust command{} running Cargo (process {})", running.len(), if running.len() == 1 { " was" } else { "s were" }, running.iter().map(u32::to_string).collect::<Vec<_>>().join(", "))),
            Ok(Ok(_idle)) => {
                let (freed, summary, failed) = collect_downloads(d, registry_before + git_before, &goal, total, available, now, dry_run);
                total = total.saturating_sub(freed);
                available = available.saturating_add(freed);
                if freed > 0 {
                    out.removed.push(Removal { kind: Kind::Registry, path: d.cargo_home.clone(), bytes: freed, why: Why::Cargo(summary) });
                }
                out.failed = failed;
            }
        }
    }
    out.after = total;
    out.unmet = total > goal.max_total || available < goal.min_free;
    // What the newest snapshot keeps of what it removed, with what earlier collections removed
    // that their snapshots still keep. (Cargo's downloads are not counted: Cargo removes them,
    // and says only how much.)
    let kept = merged(carried.into_iter().chain(newest_snapshot.filter(|_| kept_now > 0).map(|snapshot_ns| Kept { snapshot_ns, removed_ns: now, bytes: kept_now })));
    out.kept = kept.iter().map(|k| k.bytes).sum();
    if !dry_run {
        let end = measure()?;
        let mut by_kind: serde_json::Map<String, Value> = scan_summary(cache, now);
        if let Some(d) = downloads {
            let (r, g) = download_bytes(d);
            by_kind.insert(Kind::Registry.name().to_string(), json!({ "count": 2, "bytes": r + g }));
        }
        let usage = json!({ "format": USAGE_FORMAT, "scanned_ns": now, "total_bytes": total, "free_at_scan": end.space.available, "kept_by_snapshots": kept.iter().map(Kept::json).collect::<Vec<Value>>(), "fs_total": end.space.total, "max_bytes": policy.max_bytes, "min_free": policy.min_free, "by_kind": by_kind, "counted": true, "unmet": out.unmet });
        // A count that is not written would forget what the snapshots keep, and the next
        // collection would remove as much again: the collection fails with it.
        if let Err(e) = util::write_json_fast(&cache.join(USAGE), &usage) {
            failed(&mut out, format!("cannot record the count in {}: {}", cache.join(USAGE).display(), e));
        }
        if !out.removed.is_empty() || out.cargo_left.is_some() || out.failed.is_some() {
            if let Err(e) = log(cache, &out, &goal, now) {
                failed(&mut out, format!("cannot log the collection in {}: {}", cache.join(LOG).display(), e));
            }
        }
    }
    Ok(out)
}

/// Cargo's collection of the downloads of `d`, which held `before` bytes: by Cargo's own ages,
/// then by each of CARGO_AGES while the bounds need room, with the cache at `total` and the disk's
/// free space at `available` (a dry run goes by what Cargo says it would remove); none that cannot
/// remove anything. What it freed, Cargo's last summary, and why it stopped if Cargo failed.
fn collect_downloads(d: &Downloads, before: u64, goal: &Goal, total: u64, available: u64, now: u64, dry_run: bool) -> (u64, String, Option<String>) {
    let measured = |d: &Downloads| {
        let (r, g) = download_bytes(d);
        r + g
    };
    let oldest = oldest_download_ns(d);
    let may_remove = |age_ns: u64| oldest.is_some_and(|o| o + age_ns < now);
    let (mut summary, mut after) = (String::new(), before);
    let ages = std::iter::once((None, CARGO_OWN_AGE_NS)).chain(CARGO_AGES.iter().map(|&(age, age_ns)| (Some(age), age_ns)));
    for (age, age_ns) in ages {
        let freed = before.saturating_sub(after);
        let needed = total.saturating_sub(freed).saturating_sub(goal.max_total).max(goal.min_free.saturating_sub(available.saturating_add(freed)));
        // Cargo's own ages apply at every collection; the shorter ones only while needed.
        if age.is_some() && needed == 0 {
            break;
        }
        if !may_remove(age_ns) {
            continue;
        }
        match cargo_gc(d, age, dry_run) {
            Ok((s, would)) => {
                summary = s;
                after = if dry_run { before.saturating_sub(would) } else { measured(d) };
            }
            // What it removed before it failed is counted.
            Err(e) => return (before.saturating_sub(if dry_run { after } else { measured(d) }), summary, Some(e)),
        }
    }
    (before.saturating_sub(after), summary, None)
}

/// What the cache holds by kind, measured again (after a collection).
fn scan_summary(cache: &Path, now: u64) -> serde_json::Map<String, Value> {
    scan(cache, now, None).by_kind().into_iter().map(|(k, n, b)| (k.name().to_string(), json!({ "count": n, "bytes": b }))).collect()
}

/// A collection's line in `LOG`: by kind and why, how many went and how many bytes; build
/// directories and program sources by name, as a build that comes back cold is traced to them
/// and they are few; Cargo's summary; and what the collection started from and aimed for.
fn log_line(c: &Collected, goal: &Goal, now: u64) -> Value {
    let mut groups: std::collections::BTreeMap<(Kind, &str), (u64, u64)> = Default::default();
    let mut named = vec![];
    let mut cargo = None;
    for r in &c.removed {
        let why = match &r.why {
            Why::Unusable(_) => "nothing could use it",
            Why::LeastRecentlyUsed => "used least recently",
            Why::Cargo(summary) => {
                cargo = Some(summary.clone());
                "Cargo"
            }
        };
        let g = groups.entry((r.kind, why)).or_default();
        g.0 += 1;
        g.1 += r.bytes;
        if matches!(r.kind, Kind::Build | Kind::Source) {
            named.push(json!({ "kind": r.kind.name(), "path": util::path_str(&r.path), "bytes": r.bytes, "why": r.why.to_string() }));
        }
    }
    let removed: Vec<Value> = groups.into_iter().map(|((kind, why), (count, bytes))| json!({ "kind": kind.name(), "why": why, "count": count, "bytes": bytes })).collect();
    json!({
        "format": LOG_FORMAT, "at": util::iso8601(now), "before": c.before, "after": c.after, "free_before": c.free_before, "kept_by_snapshots": c.kept,
        "goal": { "max_total": (goal.max_total != u64::MAX).then_some(goal.max_total), "min_free": (goal.min_free > 0).then_some(goal.min_free) },
        "removed": removed, "named": named, "cargo": cargo, "cargo_left": c.cargo_left, "failed": c.failed, "unmet": c.unmet,
    })
}

/// Records `why` the collection `out` fell short, after whatever it already says.
fn failed(out: &mut Collected, why: String) {
    out.failed = Some(match out.failed.take() {
        Some(earlier) => format!("{}; {}", earlier, why),
        None => why,
    });
}

fn log(cache: &Path, c: &Collected, goal: &Goal, now: u64) -> io::Result<()> {
    let p = cache.join(LOG);
    let mut lines: Vec<String> = fs::read_to_string(&p).unwrap_or_default().lines().filter(|l| serde_json::from_str::<Value>(l).is_ok_and(|v| v["format"] == json!(LOG_FORMAT))).map(str::to_string).collect();
    lines.push(log_line(c, goal, now).to_string());
    let keep = lines.len().saturating_sub(LOG_KEEP);
    util::write_atomic_fast(&p, (lines[keep..].join("\n") + "\n").as_bytes())
}

/// The last count (`usage.json`) as recorded, if there is one of `USAGE_FORMAT`.
pub fn usage(cache: &Path) -> Option<Value> {
    util::read_json(&cache.join(USAGE)).filter(|v| Count::of(v).is_some())
}

/// Recent collections, newest last.
pub fn recent(cache: &Path, n: usize) -> Vec<Value> {
    let text = fs::read_to_string(cache.join(LOG)).unwrap_or_default();
    let all: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["format"] == json!(LOG_FORMAT)).collect();
    all[all.len().saturating_sub(n)..].to_vec()
}

/// Whether a collection is due (see the module notes). Not again within the protection window
/// after one that could not bring the cache within its bounds: until then nothing it left has
/// become removable, and each collection would only compete with builds (Cargo's downloads are
/// collected under Cargo's package lock).
pub fn due(cache: &Path, policy: &Policy, volume: &Volume, now: u64) -> bool {
    let c = count(cache);
    if c.as_ref().is_some_and(|c| c.unmet && c.scanned_ns + PROTECTED_NS > now) {
        return false;
    }
    if free(cache, volume, now) < policy.min_free {
        return true;
    }
    match c {
        None => cache.exists(),
        Some(c) => {
            let lost = c.free_at_scan.saturating_sub(volume.space.available);
            c.total_bytes + lost > policy.max_bytes || c.scanned_ns + DAY_NS <= now
        }
    }
}

/// Whether a collection of QuickRust's cache is running (it holds `gc.lock`; Cargo's package lock
/// too while Cargo collects its downloads). Creates nothing.
pub fn collecting() -> bool {
    collecting_in(&util::cache_dir())
}

fn collecting_in(cache: &Path) -> bool {
    let lock = cache.join("gc.lock");
    lock.is_file() && matches!(util::ExclusiveLock::try_acquire(&lock), Ok(None))
}

/// After a command: starts `qrust gc --background` when a collection is due. Errors (no free
/// space to count, no process to start) are the command's to report as a note, never to fail on.
pub fn after_command(cfg: &Config, qrust: &Path) -> Result<bool, String> {
    let cache = util::cache_dir();
    let volume = volume(util::qrust_home().as_path())?;
    if !due(&cache, &policy(cfg, &volume.space), &volume, util::now_wall_ns()) {
        return Ok(false);
    }
    util::create_private_dir(&cache).map_err(|e| e.to_string())?;
    // Only one at a time: a running collection holds the lock, and a second exits at once.
    if util::ExclusiveLock::try_acquire(&cache.join("gc.lock")).map_err(|e| e.to_string())?.is_none() {
        return Ok(false);
    }
    let log = fs::File::create(cache.join("gc.log")).map_err(|e| e.to_string())?;
    let mut c = crate::resolve::qrust_command(qrust);
    c.args(["gc", "--background"]);
    crate::proc::spawn_detached(&mut c, log).map_err(|e| format!("cannot start {}: {}", qrust.display(), e))?;
    Ok(true)
}

/// Before a command: a cache never counted and still empty gets its first count without a scan
/// (nothing in it, the disk's free space now: what it grows by later is bounded by the free space
/// lost). When the disk is nearly full (a quarter of `cache.min-free`, as `free` counts it),
/// collects for at most ten seconds. `None` when nothing needed doing.
pub fn before_command(cfg: &Config) -> Option<Result<Collected, String>> {
    before_command_in(cfg, &util::cache_dir(), &util::qrust_home())
}

/// `before_command` for the cache at `cache` in QuickRust's home `home`.
fn before_command_in(cfg: &Config, cache: &Path, home: &Path) -> Option<Result<Collected, String>> {
    let cache = cache.to_path_buf();
    let empty = ["builds", "artifacts", "drivers", "src"].iter().all(|d| !cache.join(d).exists());
    if empty && count(&cache).is_none() {
        if let Ok(fs) = fs_space(home) {
            let _ = util::create_private_dir(&cache).and_then(|_| util::write_json_fast(&cache.join(USAGE), &json!({ "format": USAGE_FORMAT, "scanned_ns": util::now_wall_ns(), "total_bytes": 0, "free_at_scan": fs.available, "kept_by_snapshots": [], "counted": false, "unmet": false })));
        }
    }
    if !cache.exists() {
        return None;
    }
    let volume = volume(&cache).ok()?;
    let policy = policy(cfg, &volume.space);
    if free(&cache, &volume, util::now_wall_ns()) >= policy.min_free / 4 {
        return None;
    }
    let _lock = util::ExclusiveLock::try_acquire(&cache.join("gc.lock")).ok()??;
    Some(collect(&cache, None, &policy, None, false, Some(Instant::now() + Duration::from_secs(10))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> PathBuf {
        let d = std::env::temp_dir().join(format!("qrust-space-{}", util::new_id())).join("cache");
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(p: &Path, bytes: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; bytes]).unwrap();
    }

    fn age(p: &Path, secs: u64) {
        let t = std::time::SystemTime::now() - Duration::from_secs(secs);
        // Windows sets a time through a handle that may write attributes, and opens a directory only
        // with backup semantics.
        #[cfg(windows)]
        let f = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            fs::File::options().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(p)
        };
        #[cfg(not(windows))]
        let f = fs::File::open(p);
        f.unwrap().set_modified(t).unwrap();
    }

    /// A managed build directory of a workspace at `root`, last used `ago_secs` ago.
    fn build_dir(cache: &Path, name: &str, root: &Path, ago_secs: u64, bytes: usize) -> PathBuf {
        let d = cache.join("builds").join(name);
        file(&d.join("debug/deps/libx.rlib"), bytes);
        let used = util::now_wall_ns() - ago_secs * 1_000_000_000;
        util::write_json(&d.with_file_name(format!("{}.json", name)), &json!({ "format": "qrust-build-dir-1", "workspace_root": util::path_str(root), "created_ns": used, "last_used_ns": used })).unwrap();
        d
    }

    fn workspace(cache: &Path, name: &str) -> PathBuf {
        let root = cache.parent().unwrap().join(name);
        file(&root.join("Cargo.toml"), 10);
        root
    }

    // TEST0115: Resolves bounds
    #[test]
    fn test0115_resolves_bounds() {
        const GIB: u64 = 1 << 30;
        assert_eq!(resolve_space("auto", 1000 * GIB, (10.0, 10, 100)), 100 * GIB);
        assert_eq!(resolve_space("auto", 50 * GIB, (10.0, 10, 100)), 10 * GIB, "at least the floor");
        assert_eq!(resolve_space("auto", 300 * GIB, (10.0, 10, 100)), 30 * GIB);
        assert_eq!(resolve_space("25%", 200 * GIB, (10.0, 10, 100)), 50 * GIB);
        assert_eq!(resolve_space("12345", 200 * GIB, (10.0, 10, 100)), 12345);
    }

    // TEST0116: A file linked twice counts once and is freed with its last link
    #[cfg(unix)]
    #[test]
    fn test0116_a_file_linked_twice_counts_once_and_is_freed_with_its_last_link() {
        let c = cache();
        let ws = workspace(&c, "w");
        let entry = c.join("artifacts/e/k1/libx.rlib");
        file(&entry, 64 * 1024);
        util::write_json(&c.join("artifacts/e/k1/entry.json"), &json!({})).unwrap();
        let d = build_dir(&c, "w-1", &ws, 3 * 3600, 0);
        fs::remove_file(d.join("debug/deps/libx.rlib")).unwrap();
        fs::hard_link(&entry, d.join("debug/deps/libx.rlib")).unwrap();
        let s = scan(&c, util::now_wall_ns(), None);
        let blocks = fs::metadata(&entry).map(|m| allocated(&m)).unwrap();
        assert!(s.total() >= blocks && s.total() < 2 * blocks, "{} for a {}-byte file linked twice", s.total(), blocks);
        // Removing either alone frees nothing of it; both, all of it.
        let mut links = s.links.clone();
        let unit = |k: Kind| s.units.iter().find(|u| u.kind == k).unwrap().clone();
        assert!(links.release(&unit(Kind::Artifact)).bytes < blocks);
        assert!(links.release(&unit(Kind::Build)).bytes >= blocks);
    }

    // TEST0117: Orphans go first then the least recently used until within bounds
    #[test]
    fn test0117_orphans_go_first_then_the_least_recently_used_until_within_bounds() {
        let c = cache();
        let (a, b) = (workspace(&c, "a"), workspace(&c, "b"));
        let old = build_dir(&c, "a-1", &a, 5 * 3600, 400_000);
        let older = build_dir(&c, "b-1", &b, 9 * 3600, 400_000);
        let fresh = build_dir(&c, "a-2", &a, 60, 400_000);
        let gone = build_dir(&c, "gone-1", &c.parent().unwrap().join("nowhere"), 7 * 3600, 400_000);
        file(&c.join("downloads/abc.tar.xz.part"), 10);
        age(&c.join("downloads/abc.tar.xz.part"), 8 * 86_400);
        file(&c.join("downloads/fresh.tar.xz.part"), 10);
        // Within its size and with free disk: only what nothing can use goes.
        let roomy = Policy { max_bytes: u64::MAX, min_free: 0 };
        let dry = collect(&c, None, &roomy, None, true, None).unwrap();
        assert!(gone.exists(), "a dry run removes nothing");
        let done = collect(&c, None, &roomy, None, false, None).unwrap();
        assert_eq!(dry.removed.len(), done.removed.len());
        let removed: Vec<&Path> = done.removed.iter().map(|r| r.path.as_path()).collect();
        assert_eq!(removed, [gone.as_path(), c.join("downloads/abc.tar.xz.part").as_path()]);
        assert!(!gone.exists() && !gone.with_file_name("gone-1.json").exists());
        assert!(old.exists() && older.exists() && fresh.exists() && c.join("downloads/fresh.tar.xz.part").exists());
        // Over its size: the least recently used first, never what was used in the last minutes.
        let tight = Policy { max_bytes: 1, min_free: 0 };
        let done = collect(&c, None, &tight, None, false, None).unwrap();
        let removed: Vec<&Path> = done.removed.iter().map(|r| r.path.as_path()).collect();
        assert_eq!(removed, [older.as_path(), old.as_path()]);
        assert!(fresh.exists() && done.skipped_in_use == 1);
        assert!(done.after < done.before);
        assert!(usage(&c).is_some() && recent(&c, 5).len() == 2);
    }

    /// A process id that is no process: one that ran and was waited for.
    fn ended_process() -> u32 {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap()).arg("--list").stdout(std::process::Stdio::null()).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    // TEST0118: A held build directory stays while its holder runs and no longer
    #[test]
    fn test0118_a_held_build_directory_stays_while_its_holder_runs_and_no_longer() {
        let c = cache();
        let (a, b) = (workspace(&c, "a"), workspace(&c, "b"));
        let held = build_dir(&c, "a-1", &a, 9 * 3600, 400_000);
        let idle = build_dir(&c, "b-1", &b, 5 * 3600, 400_000);
        // Held by this process, and by one that has ended: the first is what counts.
        crate::builddir::hold(&held, std::process::id()).unwrap();
        let gone = ended_process();
        crate::builddir::hold(&held, gone).unwrap();
        let (mine, theirs) = (held.with_file_name(format!("a-1.hold-{}", std::process::id())), held.with_file_name(format!("a-1.hold-{}", gone)));
        assert!(mine.is_file() && theirs.is_file());
        age(&theirs, 3600);
        let tight = Policy { max_bytes: 1, min_free: 0 };
        let done = collect(&c, None, &tight, None, false, None).unwrap();
        // The older of the two stays, held; the other goes; and so does the record of a holder
        // that is gone.
        assert!(held.exists() && !idle.exists(), "{:?}", done.removed);
        assert!(mine.is_file() && !theirs.exists());
        assert_eq!(done.skipped_in_use, 1);

        // The holder ends (here: its record is of a process that has): the directory is collected.
        fs::remove_file(&mine).unwrap();
        crate::builddir::hold(&held, gone).unwrap();
        age(&theirs, 3600);
        collect(&c, None, &tight, None, false, None).unwrap();
        assert!(!held.exists() && !theirs.exists(), "a hold outlived its holder");
    }

    // TEST0119: A build directory being started is not a leftover
    #[test]
    fn test0119_a_build_directory_being_started_is_not_a_leftover() {
        // A command records its build directory before Cargo creates it.
        let c = cache();
        let a = workspace(&c, "a");
        let d = c.join("builds/a-1");
        crate::builddir::mark_used_in(&c.join("builds"), &d, &a);
        assert!(!d.exists() && d.with_file_name("a-1.json").exists());
        collect(&c, None, &Policy { max_bytes: 1, min_free: 0 }, None, false, None).unwrap();
        assert!(d.with_file_name("a-1.json").exists(), "the record of a directory about to be made was removed");
        // An old one, whose directory never came, goes.
        age(&d.with_file_name("a-1.json"), 2 * 3600);
        collect(&c, None, &Policy { max_bytes: u64::MAX, min_free: 0 }, None, false, None).unwrap();
        assert!(!d.with_file_name("a-1.json").exists());
    }

    // TEST0120: A directory in use is never removed
    #[cfg(unix)]
    #[test]
    fn test0120_a_directory_in_use_is_never_removed() {
        let c = cache();
        let a = workspace(&c, "a");
        let held = build_dir(&c, "a-1", &a, 5 * 3600, 1000);
        let building = build_dir(&c, "a-2", &a, 5 * 3600, 1000);
        let _in_use = crate::builddir::in_use(&held);
        file(&building.join("debug/.cargo-lock"), 0);
        let _cargo = util::ExclusiveLock::acquire(&building.join("debug/.cargo-lock")).unwrap();
        let done = collect(&c, None, &Policy { max_bytes: 1, min_free: 0 }, None, false, None).unwrap();
        assert!(done.removed.is_empty() && done.skipped_in_use == 2, "{:?}", done);
        assert!(held.exists() && building.exists());
    }

    // TEST0320: A directory held or built in after the scan stays
    #[test]
    fn test0320_a_directory_held_or_built_in_after_the_scan_stays() {
        let c = cache();
        let a = workspace(&c, "a");
        let held = build_dir(&c, "a-1", &a, 5 * 3600, 1000);
        let built = build_dir(&c, "a-2", &a, 5 * 3600, 1000);
        let idle = build_dir(&c, "a-3", &a, 5 * 3600, 1000);
        // What a collection decides from: three directories idle for hours, none held.
        let found = scan(&c, util::now_wall_ns(), None);
        let unit = |d: &Path| found.units.iter().find(|u| u.path == d).unwrap().clone();
        let now = util::now_wall_ns();
        assert!([&held, &built, &idle].iter().all(|d| unit(d).last_used_ns + PROTECTED_NS < now));
        // While it removes others, a run starts: it holds one and builds in another.
        crate::builddir::hold(&held, std::process::id()).unwrap();
        crate::builddir::mark_used_in(&c.join("builds"), &built, &a);
        assert_eq!(remove(&c, &unit(&held)), Ok(false), "a directory held since the scan was removed");
        assert_eq!(remove(&c, &unit(&built)), Ok(false), "a directory built in since the scan was removed");
        assert!(held.exists() && built.exists());
        // The one nothing touched goes, on the same scan.
        assert_eq!(remove(&c, &unit(&idle)), Ok(true));
        assert!(!idle.exists());
    }

    // TEST0319: A hold waits for a removal under way
    #[test]
    fn test0319_a_hold_waits_for_a_removal_under_way() {
        let c = cache();
        let a = workspace(&c, "a");
        let d = build_dir(&c, "a-1", &a, 5 * 3600, 1000);
        let record = d.with_file_name(format!("a-1.hold-{}", std::process::id()));
        // A collection removing the directory holds its lock until the directory is gone.
        let removing = util::ExclusiveLock::acquire(&d.with_file_name("a-1.lock")).unwrap();
        let (sent, held) = std::sync::mpsc::channel();
        let dir = d.clone();
        let holder = std::thread::spawn(move || sent.send(crate::builddir::hold(&dir, std::process::id())).unwrap());
        assert!(held.recv_timeout(Duration::from_millis(500)).is_err(), "a hold was recorded while its directory was being removed");
        assert!(!record.exists());
        drop(removing);
        assert_eq!(held.recv_timeout(Duration::from_secs(10)).unwrap(), Ok(()));
        holder.join().unwrap();
        assert!(record.is_file());
    }

    // TEST0121: Program sources count and go least recently used unless an install holds them
    #[cfg(unix)]
    #[test]
    fn test0121_program_sources_count_and_go_least_recently_used_unless_an_install_holds_them() {
        use crate::install::{copy_lock, COMPLETE};
        let c = cache();
        let a = workspace(&c, "a");
        let build = build_dir(&c, "a-1", &a, 3 * 3600, 1000);
        let source = |name: &str, ago_secs: u64| {
            let d = c.join("src").join(name);
            file(&d.join("src/main.rs"), 50_000);
            file(&d.join(COMPLETE), 0);
            age(&d.join(COMPLETE), ago_secs);
            d
        };
        let old = source("tool-1.0.0-abcd1234", 5 * 3600);
        let held = source("other-2.0.0-abcd1234", 7 * 3600);
        let _install = util::ExclusiveLock::acquire(&copy_lock(&held)).unwrap();
        // A copy that never finished, and the lock of a source that is gone.
        let part = c.join("src/.tool-1.1.0-abcd1234.77.part");
        file(&part.join("Cargo.toml"), 10);
        age(&part, 2 * 3600);
        let stray = c.join("src/gone-1.0.0-abcd1234.lock");
        file(&stray, 0);
        age(&stray, 2 * 3600);
        let counted = scan(&c, util::now_wall_ns(), None).by_kind();
        assert!(counted.iter().any(|&(k, n, b)| k == Kind::Source && n == 2 && b >= 100_000), "{:?}", counted);
        let done = collect(&c, None, &Policy { max_bytes: 1, min_free: 0 }, None, false, None).unwrap();
        let removed: Vec<&Path> = done.removed.iter().map(|r| r.path.as_path()).collect();
        assert_eq!(removed, [part.as_path(), stray.as_path(), old.as_path(), build.as_path()]);
        assert!(!old.exists() && !copy_lock(&old).exists());
        assert!(held.exists() && done.skipped_in_use == 1, "{:?}", done);
    }

    // TEST0122: A fresh cache is counted without a scan and then checked by its growth
    #[test]
    fn test0122_a_fresh_cache_is_counted_without_a_scan_and_then_checked_by_its_growth() {
        let home = std::env::temp_dir().join(format!("qrust-space-home-{}", util::new_id()));
        fs::create_dir_all(&home).unwrap();
        let cache = home.join("cache");
        // The first command of a fresh QuickRust home: no collection, a count of nothing.
        let first = before_command_in(&Config::defaults(), &cache, &home);
        assert!(first.is_none() || first.as_ref().is_some_and(|r| r.is_ok()));
        let u = usage(&cache).expect("counted");
        assert_eq!((u["total_bytes"].as_u64(), u["counted"].as_bool()), (Some(0), Some(false)));
        let p = Policy { max_bytes: u64::MAX / 4, min_free: 0 };
        let v = Volume { space: FsSpace { total: u64::MAX / 2, available: u["free_at_scan"].as_u64().unwrap() }, snapshots: vec![] };
        assert!(!due(&cache, &p, &v, util::now_wall_ns()), "not due right after its first count");
    }

    // TEST0123: A collection is due below the free space above the size or a day after the count
    #[test]
    fn test0123_a_collection_is_due_below_the_free_space_above_the_size_or_a_day_after_the_count() {
        let c = cache();
        let p = Policy { max_bytes: 1000, min_free: 100 };
        let now = util::now_wall_ns();
        assert!(due(&c, &p, &vol(10_000, 5_000, &[]), now), "never counted");
        util::write_json(&c.join(USAGE), &json!({ "format": 3, "scanned_ns": now, "total_bytes": 900, "free_at_scan": 5_000, "kept_by_snapshots": [], "unmet": false })).unwrap();
        assert!(!due(&c, &p, &vol(10_000, 5_000, &[]), now));
        assert!(due(&c, &p, &vol(10_000, 50, &[]), now), "below the free space");
        assert!(due(&c, &p, &vol(10_000, 4_800, &[]), now), "200 bytes lost since the count: past the size");
        assert!(due(&c, &p, &vol(10_000, 5_000, &[]), now + DAY_NS), "a day later");
        // A collection that could not get within the bounds is not repeated before what it left
        // can have become removable.
        util::write_json(&c.join(USAGE), &json!({ "format": 3, "scanned_ns": now, "total_bytes": 900, "free_at_scan": 50, "kept_by_snapshots": [], "unmet": true })).unwrap();
        assert!(!due(&c, &p, &vol(10_000, 50, &[]), now + PROTECTED_NS - 1));
        assert!(due(&c, &p, &vol(10_000, 50, &[]), now + PROTECTED_NS));
    }

    // TEST0124: A running collection is known by its lock
    #[test]
    fn test0124_a_running_collection_is_known_by_its_lock() {
        let c = cache();
        assert!(!collecting_in(&c) && !c.join("gc.lock").exists(), "asking makes no lock");
        let held = util::ExclusiveLock::acquire(&c.join("gc.lock")).unwrap();
        assert!(collecting_in(&c));
        drop(held);
        // A process another test starts at this moment holds the lock's file description from
        // its fork until its exec: released within moments, not at once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while collecting_in(&c) {
            assert!(std::time::Instant::now() < deadline, "the lock outlives its holder");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // TEST0125: What a unit in use holds is kept with it
    #[cfg(unix)]
    #[test]
    fn test0125_what_a_unit_in_use_holds_is_kept_with_it() {
        let c = cache();
        let ws = workspace(&c, "w");
        let entry = c.join("artifacts/e/k1/libx.rlib");
        file(&entry, 64 * 1024);
        util::write_json(&c.join("artifacts/e/k1/entry.json"), &json!({})).unwrap();
        age(&c.join("artifacts/e/k1/entry.json"), 5 * 3600);
        // A build directory used a minute ago links the entry's file.
        let d = build_dir(&c, "w-1", &ws, 60, 0);
        fs::remove_file(d.join("debug/deps/libx.rlib")).unwrap();
        fs::hard_link(&entry, d.join("debug/deps/libx.rlib")).unwrap();
        let tight = Policy { max_bytes: 1, min_free: 0 };
        let done = collect(&c, None, &tight, None, false, None).unwrap();
        assert!(done.removed.is_empty() && entry.exists(), "the entry went for nothing: {:?}", done);
        assert!(done.unmet);
        assert_eq!(usage(&c).unwrap()["unmet"], json!(true));
        // Once the build directory may go too, both go, the older first.
        let used = util::now_wall_ns() - 3 * 3600 * 1_000_000_000;
        util::write_json(&d.with_file_name("w-1.json"), &json!({ "format": "qrust-build-dir-1", "workspace_root": util::path_str(&ws), "created_ns": used, "last_used_ns": used })).unwrap();
        let done = collect(&c, None, &tight, None, false, None).unwrap();
        let removed: Vec<&Path> = done.removed.iter().map(|r| r.path.as_path()).collect();
        assert_eq!(removed, [c.join("artifacts/e/k1").as_path(), d.as_path()]);
        assert!(done.removed[1].bytes >= 64 * 1024 && !done.unmet, "{:?}", done);
    }

    /// Six build directories of one workspace, used one to six hours ago, with what removing each
    /// frees: the least recently used first.
    fn six_builds(c: &Path) -> Vec<(PathBuf, u64)> {
        let ws = workspace(c, "w");
        let dirs: Vec<PathBuf> = (1..=6).rev().map(|h| build_dir(c, &format!("w-{}", h), &ws, h * 3600, 300_000)).collect();
        let s = scan(c, util::now_wall_ns(), None);
        dirs.into_iter().map(|d| (d.clone(), s.units.iter().find(|u| u.path == d).unwrap().bytes())).collect()
    }

    /// A volume with `available` free, whose snapshots were taken at `snapshots`.
    fn vol(total: u64, available: u64, snapshots: &[u64]) -> Volume {
        Volume { space: FsSpace { total, available }, snapshots: snapshots.to_vec() }
    }

    /// A volume whose free space is `available` whatever is removed (others write as much as is
    /// freed, or a snapshot keeps it), with snapshots taken at `snapshots`.
    fn fixed_volume<'a>(available: &'a std::cell::Cell<u64>, snapshots: &'a [u64]) -> impl Fn() -> Result<Volume, String> + 'a {
        move || Ok(vol(1 << 40, available.get(), snapshots))
    }

    /// When a snapshot taken now is taken: after every file the test wrote changed (a file's
    /// times come from a coarser clock, which may lag this one by a few milliseconds).
    fn snapshot_now() -> u64 {
        std::thread::sleep(Duration::from_millis(50));
        let t = util::now_wall_ns();
        std::thread::sleep(Duration::from_millis(50));
        t
    }

    fn paths(c: &Collected) -> Vec<PathBuf> {
        c.removed.iter().map(|r| r.path.clone()).collect()
    }

    /// The bytes the count records as kept, by snapshot.
    fn recorded_kept(c: &Path) -> Vec<(u64, u64)> {
        usage(c).unwrap()["kept_by_snapshots"].as_array().unwrap().iter().map(|k| (k["snapshot_ns"].as_u64().unwrap(), k["bytes"].as_u64().unwrap())).collect()
    }

    // TEST0291: Below the free space a collection removes its shortfall though a snapshot keeps what it frees
    #[cfg(unix)]
    #[test]
    fn test0291_below_the_free_space_a_collection_removes_its_shortfall_though_a_snapshot_keeps_what_it_frees() {
        let c = cache();
        let b = six_builds(&c);
        let snap = [snapshot_now()];
        // The disk has as much free as the two least recently used directories and half the
        // third; the floor's goal (a quarter above it) is that much more again.
        let short = b[0].1 + b[1].1 + b[2].1 / 2;
        let m = (2 * short).div_ceil(5);
        let policy = Policy { max_bytes: u64::MAX, min_free: 4 * m };
        let disk = std::cell::Cell::new(short);
        let done = collect_on(&c, None, &policy, None, false, None, &fixed_volume(&disk, &snap)).unwrap();
        assert_eq!(paths(&done), [b[0].0.clone(), b[1].0.clone(), b[2].0.clone()], "not just the shortfall: {:?}", done);
        assert!(b[3..].iter().all(|(d, _)| d.exists()) && !done.unmet);
        // The snapshot keeps all of it: recorded under it, and counted as free, so nothing is due.
        let freed = b[0].1 + b[1].1 + b[2].1;
        assert_eq!((done.kept, recorded_kept(&c)), (freed, vec![(snap[0], freed)]));
        let now = util::now_wall_ns();
        assert_eq!(free(&c, &vol(1 << 40, short, &snap), now), short + freed);
        assert!(!due(&c, &policy, &vol(1 << 40, short, &snap), now), "due again on the same disk");
        // Once the snapshot is deleted, the disk's own figure is the free space.
        assert_eq!(free(&c, &vol(1 << 40, short, &[]), now), short);
        assert!(due(&c, &policy, &vol(1 << 40, short, &[]), now));
    }

    // TEST0292: What a snapshot keeps counts as free while that snapshot exists and for a day at most
    #[test]
    fn test0292_what_a_snapshot_keeps_counts_as_free_while_that_snapshot_exists_and_for_a_day_at_most() {
        let c = cache();
        let now = util::now_wall_ns();
        let (older, newer) = (now - 3_600 * 1_000_000_000, now - 60 * 1_000_000_000);
        let p = Policy { max_bytes: u64::MAX, min_free: 1000 };
        let kept = json!([{ "snapshot_ns": older, "removed_ns": now, "bytes": 400 }, { "snapshot_ns": newer, "removed_ns": now, "bytes": 100 }]);
        util::write_json(&c.join(USAGE), &json!({ "format": 3, "scanned_ns": now, "total_bytes": 0, "free_at_scan": 800, "kept_by_snapshots": kept, "unmet": false })).unwrap();
        let v = |available, snapshots: &[u64]| vol(1 << 40, available, snapshots);
        assert_eq!(free(&c, &v(800, &[older, newer]), now), 1300);
        assert!(!due(&c, &p, &v(800, &[older, newer]), now));
        // Each snapshot's bytes go with it.
        assert_eq!(free(&c, &v(800, &[older]), now), 1200);
        assert_eq!(free(&c, &v(800, &[newer]), now), 900);
        assert!(due(&c, &p, &v(800, &[newer]), now));
        // One taken since keeps none of it.
        assert_eq!(free(&c, &v(800, &[now]), now), 800);
        // Whatever the disk shows meanwhile, the snapshots keep what they keep.
        assert_eq!(free(&c, &v(300, &[older, newer]), now), 800);
        // A day after the removal the disk is taken at its word.
        assert_eq!(free(&c, &v(800, &[older, newer]), now + DAY_NS), 800);
        // A count of another format is no count, whatever it says: the disk's own figure, and a
        // collection is due to count again.
        util::write_json(&c.join(USAGE), &json!({ "format": 2, "scanned_ns": now, "total_bytes": 0, "free_at_scan": 800, "unseen_bytes": 400, "unmet": false })).unwrap();
        assert_eq!((free(&c, &v(800, &[older]), now), usage(&c)), (800, None));
        assert!(due(&c, &Policy { max_bytes: u64::MAX, min_free: 0 }, &v(5000, &[]), now));
        // So is one that lacks a field, or whose record of what is kept is not one.
        util::write_json(&c.join(USAGE), &json!({ "format": 3, "scanned_ns": now, "total_bytes": 0, "free_at_scan": 800, "unmet": false })).unwrap();
        assert_eq!(usage(&c), None);
        util::write_json(&c.join(USAGE), &json!({ "format": 3, "scanned_ns": now, "total_bytes": 0, "free_at_scan": 800, "kept_by_snapshots": [{ "bytes": 4 }], "unmet": false })).unwrap();
        assert_eq!(usage(&c), None);
    }

    // TEST0293: A collection after one a snapshot kept removes only what the disk lost since
    #[cfg(unix)]
    #[test]
    fn test0293_a_collection_after_one_a_snapshot_kept_removes_only_what_the_disk_lost_since() {
        let c = cache();
        let b = six_builds(&c);
        let snap = [snapshot_now()];
        let short = b[0].1 + b[1].1 + b[2].1 / 2;
        let m = (2 * short).div_ceil(5);
        let policy = Policy { max_bytes: u64::MAX, min_free: 4 * m };
        let disk = std::cell::Cell::new(short);
        let first = collect_on(&c, None, &policy, None, false, None, &fixed_volume(&disk, &snap)).unwrap();
        assert_eq!(paths(&first).len(), 3);
        // Builds then write enough that, with what the first removed, the disk is half the next
        // directory below the floor; the snapshot still keeps what the first removed.
        disk.set((4 * m).checked_sub(first.kept + b[3].1 / 2).expect("the disk cannot lose more than it has"));
        let now = util::now_wall_ns();
        assert!(due(&c, &policy, &vol(1 << 40, disk.get(), &snap), now));
        // The second gives back that, and the margin: two directories, not all that is left.
        let second = collect_on(&c, None, &policy, None, false, None, &fixed_volume(&disk, &snap)).unwrap();
        assert_eq!(paths(&second), [b[3].0.clone(), b[4].0.clone()], "{:?}", second);
        assert!(b[5].0.exists());
        assert_eq!(second.kept, first.kept + b[3].1 + b[4].1);
        assert_eq!(recorded_kept(&c), vec![(snap[0], second.kept)]);
    }

    // TEST0327: Space a busy disk without a snapshot did not show is not counted as free
    #[test]
    fn test0327_space_a_busy_disk_without_a_snapshot_did_not_show_is_not_counted_as_free() {
        let c = cache();
        let b = six_builds(&c);
        let short = b[0].1 + b[1].1 + b[2].1 / 2;
        let m = (2 * short).div_ceil(5);
        let policy = Policy { max_bytes: u64::MAX, min_free: 4 * m };
        // Other processes write as much as the collection frees: the disk never gains, and the
        // volume has no snapshot.
        let disk = std::cell::Cell::new(short);
        let done = collect_on(&c, None, &policy, None, false, None, &fixed_volume(&disk, &[])).unwrap();
        assert_eq!(paths(&done), [b[0].0.clone(), b[1].0.clone(), b[2].0.clone()], "{:?}", done);
        assert_eq!((done.kept, recorded_kept(&c)), (0, vec![]), "what others wrote was counted as kept by a snapshot");
        // The bounds go by the disk as it is: still below its floor, a collection is due.
        let now = util::now_wall_ns();
        assert_eq!(free(&c, &vol(1 << 40, short, &[]), now), short);
        assert!(due(&c, &policy, &vol(1 << 40, short, &[]), now), "the free-space floor no longer applies");
        let again = collect_on(&c, None, &policy, None, false, None, &fixed_volume(&disk, &[])).unwrap();
        assert!(!again.removed.is_empty() && again.kept == 0, "{:?}", again);
    }

    // TEST0328: Only what changed before the volume's newest snapshot counts as kept
    #[cfg(unix)]
    #[test]
    fn test0328_only_what_changed_before_the_volumes_newest_snapshot_counts_as_kept() {
        let c = cache();
        let ws = workspace(&c, "w");
        let earlier = snapshot_now();
        // Three directories written before the newest snapshot, three after it.
        let before: Vec<PathBuf> = (4..=6).rev().map(|h| build_dir(&c, &format!("w-{}", h), &ws, h * 3600, 300_000)).collect();
        let newest = snapshot_now();
        let after: Vec<PathBuf> = (1..=3).rev().map(|h| build_dir(&c, &format!("w-{}", h), &ws, h * 3600, 300_000)).collect();
        // The status change time is what counts: a modification time set back does not make a file
        // the snapshot keeps.
        age(&after[0].join("debug/deps/libx.rlib"), 5 * 86_400);
        let s = scan(&c, util::now_wall_ns(), None);
        let bytes = |dirs: &[PathBuf]| dirs.iter().map(|d| s.units.iter().find(|u| &u.path == d).unwrap().bytes()).sum::<u64>();
        let disk = std::cell::Cell::new(1 << 30);
        let snapshots = [earlier, newest];
        let done = collect_on(&c, None, &Policy { max_bytes: 1, min_free: 0 }, None, false, None, &fixed_volume(&disk, &snapshots)).unwrap();
        assert_eq!(paths(&done).len(), 6, "{:?}", done);
        assert_eq!(done.kept, bytes(&before), "{} of {} written before the snapshot", done.kept, bytes(&before));
        assert!(bytes(&after) > 0);
        assert_eq!(recorded_kept(&c), vec![(newest, bytes(&before))]);
    }

    // TEST0329: The snapshots of an APFS volume are listed from any directory on it
    #[cfg(target_os = "macos")]
    #[test]
    fn test0329_the_snapshots_of_an_apfs_volume_are_listed_from_any_directory_on_it() {
        // The sealed system volume is mounted from a snapshot of itself.
        let root = volume(Path::new("/")).unwrap();
        let now = util::now_wall_ns();
        let year_2020 = 1_577_836_800 * 1_000_000_000;
        assert!(!root.snapshots.is_empty() && root.snapshots.iter().all(|&t| t > year_2020 && t < now), "{:?}", root);
        // A directory below the root of its volume: the list is asked of the volume's root.
        let c = cache();
        assert!(volume(&c).is_ok_and(|v| v.snapshots.iter().all(|&t| t > year_2020 && t < now)), "{:?}", volume(&c));
        assert_eq!(volume(Path::new("/System/Library")).unwrap().snapshots, root.snapshots);
    }

    // TEST0294: Cargo's clean waits for every QuickRust process that runs Cargo
    #[test]
    fn test0294_cargos_clean_waits_for_every_quickrust_process_that_runs_cargo() {
        let c = cache();
        let runs = CargoRuns { dir: c.parent().unwrap().join("cargo-runs") };
        assert!(runs.idle().unwrap().is_ok(), "nobody runs Cargo");
        // This process runs Cargo; one that has ended left its record behind.
        let mine = runs.register(std::process::id(), || panic!("nothing holds the lock")).unwrap();
        let gone = ended_process();
        fs::write(runs.dir.join(gone.to_string()), b"").unwrap();
        assert_eq!(runs.idle().unwrap().err(), Some(vec![std::process::id()]));
        assert!(!runs.dir.join(gone.to_string()).exists(), "the record of a process that is gone stays");
        fs::remove_file(&mine).unwrap();
        // While Cargo's clean runs, a command that starts waits for it, and says so.
        let clean = runs.idle().unwrap().expect("idle once this process's record is gone");
        let waited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let starting = {
            let (runs, waited) = (runs.clone(), waited.clone());
            std::thread::spawn(move || runs.register(4_000_000_000, || waited.store(true, std::sync::atomic::Ordering::SeqCst)).unwrap())
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !waited.load(std::sync::atomic::Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the command did not wait for Cargo's clean");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(!starting.is_finished() && !runs.dir.join("4000000000").exists(), "registered while Cargo's clean ran");
        drop(clean);
        assert!(starting.join().unwrap().exists());
    }

    // TEST0295: A collection leaves Cargo's downloads while a QuickRust command runs Cargo
    #[cfg(unix)]
    #[test]
    fn test0295_a_collection_leaves_cargos_downloads_while_a_quickrust_command_runs_cargo() {
        let c = cache();
        let home = c.parent().unwrap();
        // A download older than Cargo's own age, and a Cargo that says what it was asked.
        let cargo_home = home.join("cargo");
        let old = cargo_home.join("registry/cache/index-1/greet-1.0.0.crate");
        file(&old, 10);
        age(&old, 60 * 86_400);
        let asked = home.join("cargo-asked");
        let cargo = home.join("cargo-bin");
        fs::write(&cargo, format!("#!/bin/sh\necho \"$@\" >> '{}'\necho '     Removed 0 files' >&2\n", asked.display())).unwrap();
        fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let d = Downloads { cargo_home, cargo, runs: CargoRuns { dir: home.join("cargo-runs") } };
        let mine = d.runs.register(std::process::id(), || {}).unwrap();
        let roomy = Policy { max_bytes: u64::MAX, min_free: 0 };
        let done = collect(&c, Some(&d), &roomy, None, false, None).unwrap();
        assert!(!asked.exists(), "Cargo's clean ran while a Cargo did: {:?}", fs::read_to_string(&asked));
        assert!(done.cargo_left.as_deref().is_some_and(|l| l.contains(&std::process::id().to_string())), "{:?}", done);
        assert!(recent(&c, 1)[0]["cargo_left"].is_string(), "{:?}", recent(&c, 1));
        // Once that command's Cargo has ended, the next collection runs Cargo's clean.
        fs::remove_file(mine).unwrap();
        let done = collect(&c, Some(&d), &roomy, None, false, None).unwrap();
        assert!(fs::read_to_string(&asked).unwrap().contains("clean gc"));
        assert!(done.cargo_left.is_none());
    }

    // TEST0298: A collection whose Cargo fails records what it removed all the same
    #[cfg(unix)]
    #[test]
    fn test0298_a_collection_whose_cargo_fails_records_what_it_removed_all_the_same() {
        let c = cache();
        let home = c.parent().unwrap();
        let b = six_builds(&c);
        let gone = build_dir(&c, "gone-1", &home.join("nowhere"), 7 * 3600, 1000);
        let cargo_home = home.join("cargo");
        let old = cargo_home.join("registry/cache/index-1/greet-1.0.0.crate");
        file(&old, 10);
        age(&old, 60 * 86_400);
        let cargo = home.join("cargo-bin");
        fs::write(&cargo, "#!/bin/sh\necho 'error: the package cache is broken' >&2\nexit 101\n").unwrap();
        fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let d = Downloads { cargo_home, cargo, runs: CargoRuns { dir: home.join("cargo-runs") } };
        let short = b[0].1 + b[1].1 / 2;
        let disk = std::cell::Cell::new(short);
        let snap = [snapshot_now()];
        let policy = Policy { max_bytes: u64::MAX, min_free: 4 * (2 * short).div_ceil(5) };
        let done = collect_on(&c, Some(&d), &policy, None, false, None, &fixed_volume(&disk, &snap)).unwrap();
        assert!(done.failed.as_deref().is_some_and(|e| e.contains("the package cache is broken")), "{:?}", done);
        assert!(!gone.exists() && !b[0].0.exists() && b[5].0.exists());
        // What it removed counts, though Cargo failed: the next check knows the snapshot keeps it.
        assert_eq!(recorded_kept(&c), vec![(snap[0], done.kept)]);
        assert!(done.kept >= b[0].1 + b[1].1, "{:?}", done);
        let line = &recent(&c, 1)[0];
        assert!(line["failed"].as_str().is_some_and(|e| e.contains("broken")) && line["named"].as_array().unwrap().len() == 3, "{}", line);
    }

    // TEST0299: A removal that fails stops the collection and what it did is recorded
    #[cfg(unix)]
    #[test]
    fn test0299_a_removal_that_fails_stops_the_collection_and_what_it_did_is_recorded() {
        let c = cache();
        let b = six_builds(&c);
        // An unfinished download nothing can use goes first; then a build directory cannot be
        // renamed away, as `tmp` is not a directory.
        let part = c.join("downloads/abc.tar.xz.part");
        file(&part, 50_000);
        age(&part, 8 * 86_400);
        file(&c.join("tmp"), 0);
        let done = collect(&c, None, &Policy { max_bytes: 1, min_free: 0 }, None, false, None).unwrap();
        assert!(done.failed.as_deref().is_some_and(|e| e.contains("tmp") || e.contains("exists")), "{:?}", done);
        assert_eq!(paths(&done), [part]);
        assert!(b.iter().all(|(d, _)| d.exists()));
        let line = &recent(&c, 1)[0];
        assert!(line["failed"].is_string() && line["removed"].as_array().unwrap().len() == 1, "{}", line);
        assert_eq!(usage(&c).unwrap()["total_bytes"].as_u64(), Some(done.after));
        // A collection that fails before it removes anything is logged too.
        let again = collect(&c, None, &Policy { max_bytes: 1, min_free: 0 }, None, false, None).unwrap();
        assert!(again.failed.is_some() && again.removed.is_empty(), "{:?}", again);
        let lines = recent(&c, 5);
        assert!(lines.len() == 2 && lines[1]["failed"].is_string() && lines[1]["removed"] == json!([]), "{:?}", lines);
    }

    // TEST0300: A collection that cannot record its count fails and says so
    #[test]
    fn test0300_a_collection_that_cannot_record_its_count_fails_and_says_so() {
        let c = cache();
        let b = six_builds(&c);
        // Where the count goes, a directory: what the snapshot keeps would be forgotten.
        fs::create_dir_all(c.join(USAGE).join("x")).unwrap();
        let short = b[0].1 / 2;
        let disk = std::cell::Cell::new(short);
        let snap = [snapshot_now()];
        let policy = Policy { max_bytes: u64::MAX, min_free: 4 * (2 * short).div_ceil(5) };
        let done = collect_on(&c, None, &policy, None, false, None, &fixed_volume(&disk, &snap)).unwrap();
        assert!(done.failed.as_deref().is_some_and(|e| e.contains("cannot record the count")), "{:?}", done);
        assert!(!b[0].0.exists());
        #[cfg(unix)]
        assert!(done.kept > 0, "{:?}", done);
        let line = &recent(&c, 1)[0];
        assert!(line["failed"].as_str().is_some_and(|e| e.contains("cannot record the count")), "{}", line);
    }

    // TEST0296: The collection log counts what went by kind and names only build directories
    #[test]
    fn test0296_the_collection_log_counts_what_went_by_kind_and_names_only_build_directories() {
        let c = cache();
        let ws = workspace(&c, "w");
        for i in 0..300 {
            let e = c.join(format!("artifacts/e/k{:03}", i));
            file(&e.join("libx.rlib"), 100);
            util::write_json(&e.join("entry.json"), &json!({})).unwrap();
            age(&e.join("entry.json"), 5 * 3600);
        }
        let d = build_dir(&c, "w-1", &ws, 3 * 3600, 1000);
        // A line as collections wrote them before: every removal by path.
        fs::write(c.join(LOG), "{\"at\":\"2026-10-21T09:26:39.766Z\",\"before\":1,\"after\":0,\"removed\":[{\"kind\":\"artifact cache\",\"path\":\"/x\",\"bytes\":1,\"why\":\"used least recently\"}]}\n").unwrap();
        let done = collect(&c, None, &Policy { max_bytes: 1, min_free: 0 }, None, false, None).unwrap();
        assert_eq!(done.removed.len(), 301);
        let text = fs::read_to_string(c.join(LOG)).unwrap();
        assert_eq!(text.lines().count(), 1, "a line of the old format stayed: {}", text);
        assert!(text.len() < 1500, "{} bytes for one collection: {}", text.len(), text);
        let line = &recent(&c, 5)[0];
        let group = |kind: &str| line["removed"].as_array().unwrap().iter().find(|g| g["kind"] == kind).cloned().unwrap_or_else(|| panic!("{}", line));
        assert_eq!((group("artifact cache")["count"].as_u64(), group("artifact cache")["why"].as_str()), (Some(300), Some("used least recently")));
        assert_eq!(group("build directories")["count"].as_u64(), Some(1));
        let named = line["named"].as_array().unwrap();
        assert_eq!((named.len(), named[0]["path"].as_str()), (1, Some(util::path_str(&d).as_str())));
        assert_eq!((line["before"].as_u64(), line["after"].as_u64()), (Some(done.before), Some(done.after)));
    }
}
