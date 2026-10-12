//! Haul's cache keeps itself within its space.
//!
//! Everything under `~/.haul/cache` can be rebuilt or downloaded again: workspaces' build
//! directories, the artifact cache, compiler drivers, sources of installed programs, toolchain
//! downloads, remembered answers.
//! Haul bounds it two ways (`cache.max-size`, `cache.min-free`): it uses at most so much, and
//! leaves at least so much of the disk free. A collection removes, in order:
//!
//! 1. what nothing can use any more, whatever the space: build directories of workspaces that are
//!    gone, drivers of toolchains that are gone, stale downloads and memos, leftovers;
//! 2. while the cache is above its size or the disk below its free space, build directories,
//!    artifact entries and sources of installed programs, least recently used first;
//! 3. crates and git checkouts downloaded into Haul's Cargo home: Cargo removes them itself
//!    (`cargo -Zgc clean gc`, which knows when each was last used and waits for builds reading
//!    them), unused for its usual ages at every collection, and, while the bounds are still
//!    exceeded, those unused for a month, a week, a day, an hour, 15 minutes in turn. They come
//!    last: they need the network to come back.
//!
//! Nothing used in the last 15 minutes is removed, nor a build directory a command holds (its
//! lock) or Cargo is building in (Cargo's locks), nor a source an install holds (its lock), nor
//! an artifact entry whose files are mostly linked by something that stays (a build directory
//! in use: removing the entry would gain little space and lose it). A removal renames the thing away first, so
//! a build never sees it half deleted; a build that finds it gone builds it again.
//!
//! Sizes are blocks allocated, and a file with several links (the artifact cache links what it
//! restores into build directories) counts once, and is freed only with its last link.
//!
//! After a build Haul checks whether a collection is due (one `statvfs` and a small file read):
//! below the free space, above the size by the free space it has lost since the last count, or a
//! day after that count; but not within 15 minutes of one that could not get within the bounds
//! (a disk others fill), when nothing more has become removable. It then runs `haul gc
//! --background` detached, at the lowest priority.
//! When the disk is nearly full (a quarter of the free space Haul keeps) a command collects
//! first, for at most ten seconds. `haul gc` collects now.

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
const LOG: &str = "gc-log.jsonl";
const LOG_KEEP: usize = 200;

/// A filesystem's size and what is available to unprivileged users.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsSpace {
    pub total: u64,
    pub available: u64,
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
    /// Haul's Cargo home's registry and git downloads (removed by Cargo, see `cargo_gc`).
    Registry,
    /// A copy of an installed program's source (`haul install`).
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
    /// Its files with more than one link: identity, bytes, links in this unit.
    shared: Vec<((u64, u64), u64, u64)>,
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
}

/// Every file with several links, by identity: freed when its last link goes, and only if it
/// has none outside the cache.
#[derive(Clone, Default)]
struct Links {
    map: HashMap<(u64, u64), LinkCount>,
}

impl Links {
    /// The bytes freed by removing `unit`; the counts are updated as if it were removed.
    fn release(&mut self, unit: &Unit) -> u64 {
        let mut freed = unit.own_bytes;
        for (id, _, here) in &unit.shared {
            if let Some(c) = self.map.get_mut(id) {
                c.left = c.left.saturating_sub(*here);
                if c.left == 0 && c.in_cache >= c.nlink {
                    freed += c.bytes;
                }
            }
        }
        freed
    }
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

/// Measures `path` into `unit` (links not followed).
fn measure(path: &Path, unit: &mut Unit, all_links: &mut HashMap<(u64, u64), LinkCount>) {
    let mut stack = vec![path.to_path_buf()];
    let mut here: HashMap<(u64, u64), (u64, u64)> = HashMap::new();
    while let Some(p) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&p) else { continue };
        if md.is_dir() {
            unit.own_bytes += allocated(&md);
            stack.extend(fs::read_dir(&p).into_iter().flatten().flatten().map(|e| e.path()));
            continue;
        }
        match identity(&md) {
            Some((id, nlink)) => {
                let e = here.entry(id).or_insert((allocated(&md), 0));
                e.1 += 1;
                let g = all_links.entry(id).or_insert(LinkCount { bytes: allocated(&md), left: 0, in_cache: 0, nlink });
                g.left += 1;
                g.in_cache += 1;
            }
            None => unit.own_bytes += allocated(&md),
        }
    }
    unit.shared = here.into_iter().map(|(id, (bytes, n))| (id, bytes, n)).collect();
}

/// Everything in the cache at `cache`, measured.
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

/// Haul's Cargo home and a Cargo to collect its downloads with.
#[derive(Clone, Debug)]
pub struct Downloads {
    pub cargo_home: PathBuf,
    pub cargo: PathBuf,
}

/// Haul's Cargo home with the newest installed toolchain's Cargo; `None` without a toolchain.
pub fn downloads() -> Option<Downloads> {
    let layout = crate::toolchain::Layout::current();
    let newest = crate::toolchain::installed(&layout).into_iter().filter_map(|tc| Some((semver::Version::parse(&tc.record()?.version).ok()?, tc))).max_by(|a, b| a.0.cmp(&b.0))?;
    Some(Downloads { cargo_home: util::cargo_home(), cargo: newest.1.cargo() })
}

/// When the bounds need room from Cargo's downloads: what was used longer ago than each of these
/// is removed in turn, until there is enough. The last is the protection window: like the rest
/// of the cache, nothing used in it is removed (a size limit would let Cargo remove the crates a
/// build has just extracted, and the next build would extract them again).
const CARGO_AGES: &[&str] = &["30 days", "7 days", "1 day", "1 hour", "15 minutes"];

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

/// The bytes Haul's Cargo home's downloads take: registry, git.
fn download_bytes(d: &Downloads) -> (u64, u64) {
    let mut all = HashMap::new();
    let mut size = |sub: &str| {
        let mut u = Unit { kind: Kind::Registry, path: d.cargo_home.join(sub), companions: vec![], last_used_ns: 0, orphan: None, own_bytes: 0, shared: vec![] };
        measure(&d.cargo_home.join(sub), &mut u, &mut all);
        u.own_bytes + u.shared.iter().map(|(_, b, _)| b).sum::<u64>()
    };
    (size("registry"), size("git"))
}

pub fn scan(cache: &Path, now: u64) -> Scan {
    let mut units: Vec<Unit> = vec![];
    let mut all: HashMap<(u64, u64), LinkCount> = HashMap::new();
    let mut add = |kind: Kind, path: PathBuf, companions: Vec<PathBuf>, last_used_ns: u64, orphan: Option<String>| {
        let mut u = Unit { kind, path, companions, last_used_ns, orphan, own_bytes: 0, shared: vec![] };
        measure(&u.path.clone(), &mut u, &mut all);
        for c in u.companions.clone() {
            u.own_bytes += fs::symlink_metadata(&c).map(|m| allocated(&m)).unwrap_or(0);
        }
        units.push(u);
    };
    // Workspaces' build directories, with their records and locks.
    let builds = cache.join("builds");
    for p in entries(&builds) {
        let name = util::lossy(p.file_name().unwrap_or_default());
        if p.is_dir() {
            let companions = vec![p.with_file_name(format!("{}.json", name)), p.with_file_name(format!("{}.lock", name))];
            let (last_used, orphan) = match crate::builddir::meta(&p) {
                Some((root, _, used)) if root.join("Cargo.toml").is_file() => (used, None),
                Some((root, _, used)) => (used, Some(format!("its workspace {} is gone", root.display()))),
                None => (mtime(&p), Some("it has no record of its workspace".to_string())),
            };
            add(Kind::Build, p, companions, last_used, orphan);
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

/// A collection's outcome.
#[derive(Debug, Default)]
pub struct Collected {
    pub before: u64,
    pub after: u64,
    /// `(kind, path, bytes freed, why)`.
    pub removed: Vec<(Kind, PathBuf, u64, String)>,
    /// What would have been removed but is in use.
    pub skipped_in_use: usize,
    pub stopped_early: bool,
    /// The bounds were still exceeded at the end: everything else was in use or too recent.
    pub unmet: bool,
}

pub struct Goal {
    /// Remove until the cache is at most this big.
    pub max_total: u64,
    /// Remove until the disk has this much free.
    pub min_free: u64,
}

/// The goal for `policy` now: back under 90% of the size, and a quarter above the free space.
pub fn goal(policy: &Policy, total: u64, fs: &FsSpace) -> Goal {
    Goal { max_total: if total > policy.max_bytes { policy.max_bytes / 10 * 9 } else { u64::MAX }, min_free: if fs.available < policy.min_free { policy.min_free / 4 * 5 } else { 0 } }
}

/// Removes a build directory unless a command or Cargo uses it, or a program's source unless an
/// install uses it.
fn remove_locked_dir(cache: &Path, dir: &Path, companions: &[PathBuf], cargo_builds_in_it: bool) -> Result<bool, String> {
    let lock_path = companions.iter().find(|c| c.extension().is_some_and(|e| e == "lock"));
    let _ours = match lock_path {
        Some(l) => match util::ExclusiveLock::try_acquire(l).map_err(|e| e.to_string())? {
            Some(g) => Some(g),
            None => return Ok(false),
        },
        None => None,
    };
    if cargo_builds_in_it && cargo_building_in(dir) {
        return Ok(false);
    }
    let doomed = cache.join("tmp").join(format!("{}.{}", util::lossy(dir.file_name().unwrap_or_default()), std::process::id()));
    util::create_private_dir(&cache.join("tmp")).map_err(|e| e.to_string())?;
    fs::rename(dir, &doomed).map_err(|e| format!("cannot remove {}: {}", dir.display(), e))?;
    for c in companions {
        let _ = fs::remove_file(c);
    }
    let _ = fs::remove_dir_all(&doomed);
    Ok(true)
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
/// says (`haul gc --to`). With a `deadline`, Cargo's downloads are left alone (Cargo waits for
/// builds reading them).
pub fn collect(cache: &Path, downloads: Option<&Downloads>, policy: &Policy, max_total: Option<u64>, dry_run: bool, deadline: Option<Instant>) -> Result<Collected, String> {
    let now = util::now_wall_ns();
    let mut scan = scan(cache, now);
    let (registry_before, git_before) = downloads.map(download_bytes).unwrap_or((0, 0));
    let mut out = Collected { before: scan.total() + registry_before + git_before, ..Default::default() };
    let mut total = out.before;
    let fs_now = || fs_space(cache).unwrap_or(FsSpace { total: 0, available: u64::MAX });
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
        if dry_run || remove(cache, &u)? {
            total = total.saturating_sub(freed);
            out.removed.push((u.kind, u.path.clone(), freed, u.orphan.clone().unwrap_or_default()));
        } else {
            out.skipped_in_use += 1;
        }
    }
    // 2: least recently used, while the bounds are exceeded.
    let mut goal = goal(policy, total, &fs_now());
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
    let mut available = fs_now().available;
    let mut freed_since_check = 0u64;
    for u in candidates {
        if total <= goal.max_total && available >= goal.min_free {
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
        if dry_run || remove(cache, &u)? {
            total = total.saturating_sub(freed);
            out.removed.push((u.kind, u.path.clone(), freed, "used least recently".into()));
            freed_since_check += freed;
            available = if dry_run { available.saturating_add(freed) } else if freed_since_check >= 1 << 30 {
                freed_since_check = 0;
                fs_now().available
            } else {
                available.saturating_add(freed)
            };
        } else {
            out.skipped_in_use += 1;
        }
    }
    // 3: Cargo's downloads, by its ages, and down to what the bounds leave if still needed.
    if let (Some(d), None) = (downloads, deadline) {
        let before = registry_before + git_before;
        let measured = |d: &Downloads| {
            let (r, g) = download_bytes(d);
            r + g
        };
        // Cargo's own ages, then what was used longer ago than each of CARGO_AGES while the
        // bounds need room (a dry run goes by what Cargo says it would remove).
        let (mut summary, mut would) = cargo_gc(d, None, dry_run)?;
        let mut after = if dry_run { before.saturating_sub(would) } else { measured(d) };
        for age in CARGO_AGES {
            let freed = before.saturating_sub(after);
            let needed = total.saturating_sub(freed).saturating_sub(goal.max_total).max(goal.min_free.saturating_sub(available.saturating_add(freed)));
            if needed == 0 {
                break;
            }
            (summary, would) = cargo_gc(d, Some(age), dry_run)?;
            after = if dry_run { before.saturating_sub(would) } else { measured(d) };
        }
        let freed = before.saturating_sub(after);
        total = total.saturating_sub(freed);
        available = available.saturating_add(freed);
        if freed > 0 {
            out.removed.push((Kind::Registry, d.cargo_home.clone(), freed, format!("Cargo: {}", summary)));
        }
    }
    out.after = total;
    out.unmet = total > goal.max_total || available < goal.min_free;
    if !dry_run {
        let fs = fs_now();
        let mut by_kind: serde_json::Map<String, Value> = scan_summary(cache, now);
        if let Some(d) = downloads {
            let (r, g) = download_bytes(d);
            by_kind.insert(Kind::Registry.name().to_string(), json!({ "count": 2, "bytes": r + g }));
        }
        let usage = json!({ "format": 1, "scanned_ns": now, "total_bytes": total, "free_at_scan": fs.available, "fs_total": fs.total, "max_bytes": policy.max_bytes, "min_free": policy.min_free, "by_kind": by_kind, "counted": true, "unmet": out.unmet });
        let _ = util::write_json_fast(&cache.join(USAGE), &usage);
        if !out.removed.is_empty() {
            log(cache, &out, now);
        }
    }
    Ok(out)
}

/// What the cache holds by kind, measured again (after a collection).
fn scan_summary(cache: &Path, now: u64) -> serde_json::Map<String, Value> {
    scan(cache, now).by_kind().into_iter().map(|(k, n, b)| (k.name().to_string(), json!({ "count": n, "bytes": b }))).collect()
}

fn log(cache: &Path, c: &Collected, now: u64) {
    let p = cache.join(LOG);
    let mut lines: Vec<String> = fs::read_to_string(&p).unwrap_or_default().lines().map(str::to_string).collect();
    let removed: Vec<Value> = c.removed.iter().map(|(k, path, b, why)| json!({ "kind": k.name(), "path": util::path_str(path), "bytes": b, "why": why })).collect();
    lines.push(json!({ "at": util::iso8601(now), "before": c.before, "after": c.after, "removed": removed }).to_string());
    let keep = lines.len().saturating_sub(LOG_KEEP);
    let _ = util::write_atomic_fast(&p, (lines[keep..].join("\n") + "\n").as_bytes());
}

/// The last count (`usage.json`), if any.
pub fn usage(cache: &Path) -> Option<Value> {
    util::read_json(&cache.join(USAGE)).filter(|v| v["format"] == json!(1))
}

/// Recent collections, newest last.
pub fn recent(cache: &Path, n: usize) -> Vec<Value> {
    let text = fs::read_to_string(cache.join(LOG)).unwrap_or_default();
    let all: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    all[all.len().saturating_sub(n)..].to_vec()
}

/// Whether a collection is due (see the module notes). Not again within the protection window
/// after one that could not bring the cache within its bounds: until then nothing it left has
/// become removable, and each collection would only compete with builds (Cargo's downloads are
/// collected under Cargo's package lock).
pub fn due(cache: &Path, policy: &Policy, fs: &FsSpace, now: u64) -> bool {
    let u = usage(cache);
    if u.as_ref().is_some_and(|u| u["unmet"] == json!(true) && u["scanned_ns"].as_u64().unwrap_or(0) + PROTECTED_NS > now) {
        return false;
    }
    if fs.available < policy.min_free {
        return true;
    }
    match u {
        None => cache.exists(),
        Some(u) => {
            let total = u["total_bytes"].as_u64().unwrap_or(0);
            let lost = u["free_at_scan"].as_u64().unwrap_or(fs.available).saturating_sub(fs.available);
            total + lost > policy.max_bytes || u["scanned_ns"].as_u64().unwrap_or(0) + DAY_NS <= now
        }
    }
}

/// After a command: starts `haul gc --background` when a collection is due. Errors (no free
/// space to count, no process to start) are the command's to report as a note, never to fail on.
pub fn after_command(cfg: &Config, haul: &Path) -> Result<bool, String> {
    let cache = util::cache_dir();
    let fs = fs_space(util::haul_home().as_path()).map_err(|e| format!("cannot measure the free space of {}: {}", util::haul_home().display(), e))?;
    if !due(&cache, &policy(cfg, &fs), &fs, util::now_wall_ns()) {
        return Ok(false);
    }
    util::create_private_dir(&cache).map_err(|e| e.to_string())?;
    // Only one at a time: a running collection holds the lock, and a second exits at once.
    if util::ExclusiveLock::try_acquire(&cache.join("gc.lock")).map_err(|e| e.to_string())?.is_none() {
        return Ok(false);
    }
    let log = fs::File::create(cache.join("gc.log")).map_err(|e| e.to_string())?;
    let mut c = std::process::Command::new(haul);
    c.args(["gc", "--background"]);
    crate::proc::spawn_detached(&mut c, log).map_err(|e| format!("cannot start {}: {}", haul.display(), e))?;
    Ok(true)
}

/// Before a command: a cache never counted and still empty gets its first count without a scan
/// (nothing in it, the disk's free space now: what it grows by later is bounded by the free space
/// lost). When the disk is nearly full (a quarter of `cache.min-free`), collects for at most ten
/// seconds. `None` when nothing needed doing.
pub fn before_command(cfg: &Config) -> Option<Result<Collected, String>> {
    before_command_in(cfg, &util::cache_dir(), &util::haul_home())
}

/// `before_command` for the cache at `cache` in Haul's home `home`.
fn before_command_in(cfg: &Config, cache: &Path, home: &Path) -> Option<Result<Collected, String>> {
    let cache = cache.to_path_buf();
    let empty = ["builds", "artifacts", "drivers", "src"].iter().all(|d| !cache.join(d).exists());
    if empty && usage(&cache).is_none() {
        if let Ok(fs) = fs_space(home) {
            let _ = util::create_private_dir(&cache).and_then(|_| util::write_json_fast(&cache.join(USAGE), &json!({ "format": 1, "scanned_ns": util::now_wall_ns(), "total_bytes": 0, "free_at_scan": fs.available, "counted": false })));
        }
    }
    if !cache.exists() {
        return None;
    }
    let fs = fs_space(&cache).ok()?;
    let policy = policy(cfg, &fs);
    if fs.available >= policy.min_free / 4 {
        return None;
    }
    let _lock = util::ExclusiveLock::try_acquire(&cache.join("gc.lock")).ok()??;
    Some(collect(&cache, None, &policy, None, false, Some(Instant::now() + Duration::from_secs(10))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> PathBuf {
        let d = std::env::temp_dir().join(format!("haul-space-{}", util::new_id())).join("cache");
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(p: &Path, bytes: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; bytes]).unwrap();
    }

    fn age(p: &Path, secs: u64) {
        let t = std::time::SystemTime::now() - Duration::from_secs(secs);
        fs::File::open(p).or_else(|_| fs::File::options().write(true).open(p)).unwrap().set_modified(t).unwrap();
    }

    /// A managed build directory of a workspace at `root`, last used `ago_secs` ago.
    fn build_dir(cache: &Path, name: &str, root: &Path, ago_secs: u64, bytes: usize) -> PathBuf {
        let d = cache.join("builds").join(name);
        file(&d.join("debug/deps/libx.rlib"), bytes);
        let used = util::now_wall_ns() - ago_secs * 1_000_000_000;
        util::write_json(&d.with_file_name(format!("{}.json", name)), &json!({ "format": "haul-build-dir-1", "workspace_root": util::path_str(root), "created_ns": used, "last_used_ns": used })).unwrap();
        d
    }

    fn workspace(cache: &Path, name: &str) -> PathBuf {
        let root = cache.parent().unwrap().join(name);
        file(&root.join("Cargo.toml"), 10);
        root
    }

    #[test]
    fn resolves_bounds() {
        const GIB: u64 = 1 << 30;
        assert_eq!(resolve_space("auto", 1000 * GIB, (10.0, 10, 100)), 100 * GIB);
        assert_eq!(resolve_space("auto", 50 * GIB, (10.0, 10, 100)), 10 * GIB, "at least the floor");
        assert_eq!(resolve_space("auto", 300 * GIB, (10.0, 10, 100)), 30 * GIB);
        assert_eq!(resolve_space("25%", 200 * GIB, (10.0, 10, 100)), 50 * GIB);
        assert_eq!(resolve_space("12345", 200 * GIB, (10.0, 10, 100)), 12345);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_linked_twice_counts_once_and_is_freed_with_its_last_link() {
        let c = cache();
        let ws = workspace(&c, "w");
        let entry = c.join("artifacts/e/k1/libx.rlib");
        file(&entry, 64 * 1024);
        util::write_json(&c.join("artifacts/e/k1/entry.json"), &json!({})).unwrap();
        let d = build_dir(&c, "w-1", &ws, 3 * 3600, 0);
        fs::remove_file(d.join("debug/deps/libx.rlib")).unwrap();
        fs::hard_link(&entry, d.join("debug/deps/libx.rlib")).unwrap();
        let s = scan(&c, util::now_wall_ns());
        let blocks = fs::metadata(&entry).map(|m| allocated(&m)).unwrap();
        assert!(s.total() >= blocks && s.total() < 2 * blocks, "{} for a {}-byte file linked twice", s.total(), blocks);
        // Removing either alone frees nothing of it; both, all of it.
        let mut links = s.links.clone();
        let unit = |k: Kind| s.units.iter().find(|u| u.kind == k).unwrap().clone();
        assert!(links.release(&unit(Kind::Artifact)) < blocks);
        assert!(links.release(&unit(Kind::Build)) >= blocks);
    }

    #[test]
    fn orphans_go_first_then_the_least_recently_used_until_within_bounds() {
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
        let removed: Vec<&Path> = done.removed.iter().map(|r| r.1.as_path()).collect();
        assert_eq!(removed, [gone.as_path(), c.join("downloads/abc.tar.xz.part").as_path()]);
        assert!(!gone.exists() && !gone.with_file_name("gone-1.json").exists());
        assert!(old.exists() && older.exists() && fresh.exists() && c.join("downloads/fresh.tar.xz.part").exists());
        // Over its size: the least recently used first, never what was used in the last minutes.
        let tight = Policy { max_bytes: 1, min_free: 0 };
        let done = collect(&c, None, &tight, None, false, None).unwrap();
        let removed: Vec<&Path> = done.removed.iter().map(|r| r.1.as_path()).collect();
        assert_eq!(removed, [older.as_path(), old.as_path()]);
        assert!(fresh.exists() && done.skipped_in_use == 1);
        assert!(done.after < done.before);
        assert!(usage(&c).is_some() && recent(&c, 5).len() == 2);
    }

    #[test]
    fn a_build_directory_being_started_is_not_a_leftover() {
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

    #[cfg(unix)]
    #[test]
    fn a_directory_in_use_is_never_removed() {
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

    #[cfg(unix)]
    #[test]
    fn program_sources_count_and_go_least_recently_used_unless_an_install_holds_them() {
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
        let counted = scan(&c, util::now_wall_ns()).by_kind();
        assert!(counted.iter().any(|&(k, n, b)| k == Kind::Source && n == 2 && b >= 100_000), "{:?}", counted);
        let done = collect(&c, None, &Policy { max_bytes: 1, min_free: 0 }, None, false, None).unwrap();
        let removed: Vec<&Path> = done.removed.iter().map(|r| r.1.as_path()).collect();
        assert_eq!(removed, [part.as_path(), stray.as_path(), old.as_path(), build.as_path()]);
        assert!(!old.exists() && !copy_lock(&old).exists());
        assert!(held.exists() && done.skipped_in_use == 1, "{:?}", done);
    }

    #[test]
    fn a_fresh_cache_is_counted_without_a_scan_and_then_checked_by_its_growth() {
        let home = std::env::temp_dir().join(format!("haul-space-home-{}", util::new_id()));
        fs::create_dir_all(&home).unwrap();
        let cache = home.join("cache");
        // The first command of a fresh Haul home: no collection, a count of nothing.
        let first = before_command_in(&Config::defaults(), &cache, &home);
        assert!(first.is_none() || first.as_ref().is_some_and(|r| r.is_ok()));
        let u = usage(&cache).expect("counted");
        assert_eq!((u["total_bytes"].as_u64(), u["counted"].as_bool()), (Some(0), Some(false)));
        let p = Policy { max_bytes: u64::MAX / 4, min_free: 0 };
        let fs = FsSpace { total: u64::MAX / 2, available: u["free_at_scan"].as_u64().unwrap() };
        assert!(!due(&cache, &p, &fs, util::now_wall_ns()), "not due right after its first count");
    }

    #[test]
    fn a_collection_is_due_below_the_free_space_above_the_size_or_a_day_after_the_count() {
        let c = cache();
        let p = Policy { max_bytes: 1000, min_free: 100 };
        let now = util::now_wall_ns();
        assert!(due(&c, &p, &FsSpace { total: 10_000, available: 5_000 }, now), "never counted");
        util::write_json(&c.join(USAGE), &json!({ "format": 1, "scanned_ns": now, "total_bytes": 900, "free_at_scan": 5_000 })).unwrap();
        assert!(!due(&c, &p, &FsSpace { total: 10_000, available: 5_000 }, now));
        assert!(due(&c, &p, &FsSpace { total: 10_000, available: 50 }, now), "below the free space");
        assert!(due(&c, &p, &FsSpace { total: 10_000, available: 4_800 }, now), "200 bytes lost since the count: past the size");
        assert!(due(&c, &p, &FsSpace { total: 10_000, available: 5_000 }, now + DAY_NS), "a day later");
        // A collection that could not get within the bounds is not repeated before what it left
        // can have become removable.
        util::write_json(&c.join(USAGE), &json!({ "format": 1, "scanned_ns": now, "total_bytes": 900, "free_at_scan": 50, "unmet": true })).unwrap();
        assert!(!due(&c, &p, &FsSpace { total: 10_000, available: 50 }, now + PROTECTED_NS - 1));
        assert!(due(&c, &p, &FsSpace { total: 10_000, available: 50 }, now + PROTECTED_NS));
    }

    #[cfg(unix)]
    #[test]
    fn what_a_unit_in_use_holds_is_kept_with_it() {
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
        util::write_json(&d.with_file_name("w-1.json"), &json!({ "format": "haul-build-dir-1", "workspace_root": util::path_str(&ws), "created_ns": used, "last_used_ns": used })).unwrap();
        let done = collect(&c, None, &tight, None, false, None).unwrap();
        let removed: Vec<&Path> = done.removed.iter().map(|r| r.1.as_path()).collect();
        assert_eq!(removed, [c.join("artifacts/e/k1").as_path(), d.as_path()]);
        assert!(done.removed[1].2 >= 64 * 1024 && !done.unmet, "{:?}", done);
    }
}
