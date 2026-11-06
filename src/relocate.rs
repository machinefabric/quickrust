//! Where QuickRust's cache is, and moving it (`qrust cache move`).
//!
//! The cache is in QuickRust's home (`$QRUST_HOME/cache`) unless `qrust cache move` put it
//! elsewhere; the move records where in `$QRUST_HOME/cache.json`, which nothing else writes. A
//! record that cannot be read, or that names a directory that is not there (on a disk that is not
//! mounted), ends every command that needs the cache: building into a new, empty cache instead
//! would lose the one that was moved, and fill the disk under the mount point of one that is not
//! mounted.
//!
//! Every process that uses the cache holds `$QRUST_HOME/cache.lock` shared from its first use of
//! it to its end, and reads the record under it (`settle`). Cargo that QuickRust replaces itself
//! with takes the lock over (`keep_across_exec`); a toolchain program run through a proxy does
//! not: it uses none of the cache, and an editor's rust-analyzer would otherwise hold it for
//! hours. A move holds the lock exclusively: nothing uses the cache while it moves, and a command
//! that starts meanwhile waits for it, then finds the cache where it went. A move waits for the
//! commands using the cache, saying which. It stops, saying why, while a build directory is held
//! (`qrust path --hold-for`) or a Cargo QuickRust did not start is building in one: neither ends
//! with a QuickRust command.
//!
//! On its own volume the cache is renamed. To another it is copied with what Cargo's and
//! QuickRust's freshness go by: each file's content, permissions and modification time, to the
//! nanosecond; symbolic links as links; a file with several links in the cache (the artifact cache
//! shares files with build directories) as one file with as many. A copy that does not fit is not
//! started. The record is written once the copy is complete, and the old cache is removed after
//! it. The count the cache's bounds go by (`usage.json`) is of the old disk: it is dropped, and
//! the cache is counted again on its new one.
//!
//! A move that stops half way (the machine loses power, the process is killed) is finished or
//! undone by the next process that finds itself alone with the cache: the move writes what it
//! is doing to `$QRUST_HOME/cache-move.json` first, and removes it last.
//!
//! Nothing in the cache names where it is, but what Cargo and rustc wrote into build directories.
//! Cargo reads its own records relative to the target directory, so a moved build directory is
//! as fresh as it was. A symbolic link to something in the cache by its absolute path is pointed
//! at the same thing where it went. A binary's objects kept for relinking are named by their old
//! path in its `.rlink`: its next rebuild compiles it (see `artifact_cache::Job::prepare_relink`).

use crate::util;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const RECORD: &str = "cache.json";
const RECORD_FORMAT: u64 = 1;
const LOCK: &str = "cache.lock";
const JOURNAL: &str = "cache-move.json";
const JOURNAL_FORMAT: u64 = 1;

/// Where QuickRust's cache is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub dir: PathBuf,
    /// Put there by `qrust cache move`, rather than in QuickRust's home.
    pub moved: bool,
}

/// The cache's place in QuickRust's home `home`, where it is unless it was moved.
fn default_dir_in(home: &Path) -> PathBuf {
    home.join("cache")
}

/// Where QuickRust's home `home` records its cache to be (it may not be there).
pub fn place_in(home: &Path) -> Result<Place, String> {
    let path = home.join(RECORD);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Place { dir: default_dir_in(home), moved: false }),
        Err(e) => return Err(format!("cannot read {}, which says where QuickRust's cache is: {}", path.display(), e)),
    };
    let dir = serde_json::from_slice::<Value>(&bytes).ok().filter(|v| v["format"] == json!(RECORD_FORMAT)).and_then(|v| v["dir"].as_str().map(PathBuf::from)).filter(|d| d.is_absolute());
    match dir {
        Some(dir) => Ok(Place { dir, moved: true }),
        None => Err(format!("{} does not say where QuickRust's cache is (`qrust cache move` writes it); without it the cache is {}", path.display(), default_dir_in(home).display())),
    }
}

/// Where QuickRust's cache is recorded to be.
pub fn place() -> Result<Place, String> {
    place_in(&util::qrust_home())
}

fn missing(dir: &Path) -> String {
    format!("QuickRust's cache is at {} (`qrust cache move` put it there), which does not exist: if it is on a disk that is not mounted, mount it; `qrust cache move DIR` starts an empty one in DIR", dir.display())
}

/// The cache's lock as this process holds it (`None`: QuickRust's home could not hold one, or
/// the process let it go).
static HELD: Mutex<Option<util::SharedLock>> = Mutex::new(None);
/// This process holds the cache's lock exclusively, to move the cache.
static MOVING: AtomicBool = AtomicBool::new(false);

/// Where QuickRust's cache is, for the rest of this process, which holds the cache's lock shared
/// from now on (see the module notes).
pub fn settle() -> Result<PathBuf, String> {
    // It would wait for its own exclusive lock.
    assert!(!MOVING.load(Ordering::SeqCst), "QuickRust's cache was used by the command moving it");
    let home = util::qrust_home();
    let lock = home.join(LOCK);
    if home.join(JOURNAL).exists() {
        finish_interrupted(&home)?;
    }
    let held = match util::SharedLock::try_acquire(&lock) {
        Ok(Some(l)) => Some(l),
        Ok(None) => {
            crate::ui::note("waiting for QuickRust's cache to finish moving");
            util::SharedLock::acquire(&lock).ok()
        }
        // A home QuickRust cannot write holds no lock, and so no move either: a move writes its
        // lock and its record there.
        Err(_) => None,
    };
    *HELD.lock().unwrap_or_else(|e| e.into_inner()) = held;
    let place = place_in(&home)?;
    if place.moved && !place.dir.is_dir() {
        return Err(missing(&place.dir));
    }
    Ok(place.dir)
}

/// Lets the program this process is about to replace itself with (Cargo, which builds in the
/// cache) hold the cache's lock in its place.
pub fn keep_across_exec() {
    if let Some(lock) = HELD.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        lock.keep_across_exec();
    }
}

/// Lets go of the cache's lock before this process runs a program that uses none of the cache
/// (a toolchain program through its proxy), for as long as that program runs: on Windows this
/// process waits for it, where elsewhere it replaces itself with it.
pub fn let_go() {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).take();
}

/// What a move did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum How {
    /// The cache was there already.
    AlreadyThere,
    /// There was no cache to move: it starts there, empty.
    Nothing,
    /// Renamed, on its volume.
    Renamed,
    /// Copied to another volume, and the old one removed: so many files and bytes.
    Copied { files: u64, bytes: u64 },
}

#[derive(Clone, Debug)]
pub struct Moved {
    pub from: PathBuf,
    pub to: PathBuf,
    pub how: How,
}

/// The directory `arg` (given in `cwd`) names, absolute, through its parent's canonical path: its
/// parent must exist.
pub fn destination(arg: &Path, cwd: &Path) -> Result<PathBuf, String> {
    let joined = cwd.join(arg);
    if let Ok(dir) = util::canonical(&joined) {
        return Ok(dir);
    }
    let name = match joined.file_name() {
        Some(n) if !matches!(joined.components().next_back(), Some(std::path::Component::ParentDir | std::path::Component::CurDir)) => n.to_os_string(),
        _ => return Err(format!("{} names no directory to move QuickRust's cache to", arg.display())),
    };
    let parent = joined.parent().ok_or_else(|| format!("{} names no directory to move QuickRust's cache to", arg.display()))?;
    let parent = util::canonical(parent).map_err(|e| format!("cannot move QuickRust's cache into {}: {}", parent.display(), e))?;
    Ok(parent.join(name))
}

/// What a move tells while it runs.
pub enum Progress<'a> {
    /// It waits for the commands using the cache, which this says.
    Waiting(&'a str),
    /// A copy has written so many bytes, of how many.
    Copied { done: u64, total: u64 },
}

/// Moves QuickRust's cache to `to` (absolute, as `destination` gives it), which must not exist or
/// be an empty directory, and records it there for every later command. It waits for the commands
/// using the cache to finish.
pub fn move_to(to: &Path, progress: &mut dyn FnMut(Progress)) -> Result<Moved, String> {
    let home = util::qrust_home();
    let lock_path = home.join(LOCK);
    let cannot_lock = |e: io::Error| format!("cannot lock {}: {}", lock_path.display(), e);
    let _lock = match util::ExclusiveLock::try_acquire(&lock_path).map_err(cannot_lock)? {
        Some(l) => l,
        None => {
            progress(Progress::Waiting(&in_use()));
            util::ExclusiveLock::acquire(&lock_path).map_err(cannot_lock)?
        }
    };
    let _moving = Moving::start();
    if home.join(JOURNAL).exists() {
        finish_interrupted_locked(&home)?;
    }
    let from = place_in(&home)?.dir;
    let moved = |how| Ok(Moved { from: from.clone(), to: to.to_path_buf(), how });
    let from_canon = util::canonical(&from).ok();
    if from_canon.as_deref() == Some(to) || from == to {
        return moved(How::AlreadyThere);
    }
    if let Some(inside) = from_canon.as_deref().filter(|f| to.starts_with(f)) {
        return Err(format!("{} is inside QuickRust's cache ({})", to.display(), inside.display()));
    }
    let to_existed = match fs::symlink_metadata(to) {
        Ok(md) if md.is_dir() => {
            let mut entries = fs::read_dir(to).map_err(|e| format!("cannot read {}: {}", to.display(), e))?;
            if entries.next().is_some() {
                return Err(format!("{} is not empty: QuickRust's cache moves to a directory that does not exist or is empty", to.display()));
            }
            true
        }
        Ok(_) => return Err(format!("{} is not a directory", to.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(format!("cannot read {}: {}", to.display(), e)),
    };
    if !from.is_dir() {
        util::create_private_dir(to).map_err(|e| format!("cannot create {}: {}", to.display(), e))?;
        record(&home, to)?;
        return moved(How::Nothing);
    }
    if let Some(why) = busy(&from) {
        return Err(why);
    }
    let journal = Journal { from: from.clone(), to: to.to_path_buf(), to_existed, copy: false };
    journal.write(&home)?;
    if to_existed {
        fs::remove_dir(to).map_err(|e| format!("cannot replace the empty directory {}: {}", to.display(), e))?;
    }
    // A link into the cache by its absolute path names it as it was given or as it is.
    let froms: Vec<PathBuf> = std::iter::once(from.clone()).chain(from_canon.clone().filter(|c| *c != from)).collect();
    match fs::rename(&from, to) {
        Ok(()) => {
            record(&home, to)?;
            Journal::remove(&home)?;
            retarget_links(to, &froms, to).map_err(|e| format!("QuickRust's cache is now {}, but {}", to.display(), e))?;
            return moved(How::Renamed);
        }
        Err(e) if !crosses_devices(&e) => {
            let failed = format!("cannot move {} to {}: {}", from.display(), to.display(), e);
            return Err(and_undo(failed, &journal, &home));
        }
        Err(_) => {}
    }
    // Another volume: copied.
    let total = crate::space::scan(&from, util::now_wall_ns(), None).total();
    let parent = to.parent().unwrap_or(to);
    let room = crate::space::fs_space(parent).map_err(|e| format!("cannot measure the free space of {}: {}", parent.display(), e))?.available;
    if total > room {
        let failed = format!("QuickRust's cache takes {}, and {} has {} free", util::human_bytes(total), parent.display(), util::human_bytes(room));
        return Err(and_undo(failed, &journal, &home));
    }
    let journal = Journal { copy: true, ..journal };
    journal.write(&home)?;
    let mut copy = Copy { froms: &froms, to, linked: HashMap::new(), files: 0, bytes: 0, total, progress };
    if let Err(e) = copy.dir(&from, to) {
        return Err(and_undo(format!("cannot copy QuickRust's cache to {}: {}", to.display(), e), &journal, &home));
    }
    let (files, bytes) = (copy.files, copy.bytes);
    // A count of the old disk's space: the cache is counted again on this one.
    let usage = to.join(crate::space::USAGE);
    fs::remove_file(&usage).or_else(|e| if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).map_err(|e| format!("cannot remove {}: {}", usage.display(), e))?;
    record(&home, to)?;
    fs::remove_dir_all(&from).map_err(|e| format!("QuickRust's cache is now {}, and its old copy {} could not be removed: {}", to.display(), from.display(), e))?;
    Journal::remove(&home)?;
    moved(How::Copied { files, bytes })
}

/// Why a move failed, after undoing what it did; and why that failed too, if it did (the next
/// process alone with the cache tries again, see `finish_interrupted`).
fn and_undo(failed: String, journal: &Journal, home: &Path) -> String {
    match journal.undo(home) {
        Ok(()) => failed,
        Err(e) => format!("{}; and undoing the move failed: {}", failed, e),
    }
}

/// While this process moves the cache (see `MOVING`).
struct Moving;

impl Moving {
    fn start() -> Moving {
        MOVING.store(true, Ordering::SeqCst);
        Moving
    }
}

impl Drop for Moving {
    fn drop(&mut self) {
        MOVING.store(false, Ordering::SeqCst);
    }
}

/// Who holds the cache's lock: the QuickRust commands that are running, as far as they say.
fn in_use() -> String {
    let sessions = crate::store::active_sessions();
    if sessions.is_empty() {
        return "waiting for the QuickRust commands using the cache to finish".to_string();
    }
    let list: Vec<String> = sessions.iter().map(|s| format!("process {} in {}", s["pid"], util::vstr(s, "cwd"))).collect();
    format!("waiting for the QuickRust commands using the cache to finish: {}", list.join(", "))
}

/// Why the cache at `cache` cannot move now, when it cannot: a running program holds one of its
/// build directories, or a Cargo QuickRust did not start is building in one.
fn busy(cache: &Path) -> Option<String> {
    let builds = crate::builddir::builds_dir_in(cache);
    for dir in fs::read_dir(&builds).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        if crate::builddir::held(&dir) {
            return Some(format!("a running program holds the build directory {} (`qrust path --hold-for`); QuickRust's cache moves when it has ended", dir.display()));
        }
        if crate::space::cargo_building_in(&dir) {
            return Some(format!("a Cargo is building in {}; QuickRust's cache moves when it has finished", dir.display()));
        }
    }
    None
}

/// Whether a rename failed only because it would cross volumes.
fn crosses_devices(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(windows)]
    {
        const ERROR_NOT_SAME_DEVICE: i32 = 17;
        e.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE)
    }
}

/// Records that the cache is at `to`: no record when that is its place in QuickRust's home.
fn record(home: &Path, to: &Path) -> Result<(), String> {
    let path = home.join(RECORD);
    let default = default_dir_in(home);
    if to == default || util::canonical(&default).ok().as_deref() == Some(to) {
        return match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot remove {}: {}", path.display(), e)),
        };
    }
    util::write_json(&path, &json!({ "format": RECORD_FORMAT, "dir": util::path_str(to) })).map_err(|e| format!("cannot write {}: {}", path.display(), e))
}

/// A move under way, as `$QRUST_HOME/cache-move.json` records it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Journal {
    from: PathBuf,
    to: PathBuf,
    /// `to` was an empty directory before the move.
    to_existed: bool,
    /// Copied (to another volume), rather than renamed.
    copy: bool,
}

impl Journal {
    fn write(&self, home: &Path) -> Result<(), String> {
        let path = home.join(JOURNAL);
        util::write_json(&path, &json!({ "format": JOURNAL_FORMAT, "from": util::path_str(&self.from), "to": util::path_str(&self.to), "to_existed": self.to_existed, "copy": self.copy })).map_err(|e| format!("cannot write {}: {}", path.display(), e))
    }

    fn read(home: &Path) -> Result<Option<Journal>, String> {
        let path = home.join(JOURNAL);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {}", path.display(), e)),
        };
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{} is not a record of a move of QuickRust's cache: {}", path.display(), e))?;
        let journal = (v["format"] == json!(JOURNAL_FORMAT))
            .then(|| Some(Journal { from: PathBuf::from(v["from"].as_str()?), to: PathBuf::from(v["to"].as_str()?), to_existed: v["to_existed"].as_bool()?, copy: v["copy"].as_bool()? }))
            .flatten();
        journal.map(Some).ok_or_else(|| format!("{} is not a record of a move of QuickRust's cache", path.display()))
    }

    fn remove(home: &Path) -> Result<(), String> {
        let path = home.join(JOURNAL);
        fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {}", path.display(), e))
    }

    /// Puts things back as they were before this move, which did not get as far as its record:
    /// what was copied goes, and the empty destination is there again if it was.
    fn undo(&self, home: &Path) -> Result<(), String> {
        if self.copy {
            match fs::remove_dir_all(&self.to) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot remove the unfinished copy of QuickRust's cache in {}: {}", self.to.display(), e)),
            }
        }
        if self.to_existed && !self.to.exists() {
            fs::create_dir(&self.to).map_err(|e| format!("cannot create {} again: {}", self.to.display(), e))?;
        }
        Journal::remove(home)
    }
}

/// Finishes or undoes a move that stopped half way, when this process is alone with the cache;
/// otherwise the process that is will.
fn finish_interrupted(home: &Path) -> Result<(), String> {
    let lock = home.join(LOCK);
    match util::ExclusiveLock::try_acquire(&lock) {
        Ok(Some(_alone)) => finish_interrupted_locked(home),
        Ok(None) | Err(_) => Ok(()),
    }
}

fn finish_interrupted_locked(home: &Path) -> Result<(), String> {
    let Some(journal) = Journal::read(home)? else { return Ok(()) };
    let recorded = place_in(home)?.dir;
    if recorded == journal.to {
        // Recorded: only the old copy was left to remove.
        if journal.copy && journal.from.exists() {
            crate::ui::note(&format!("removing the old copy of QuickRust's cache, {}, which a move left", journal.from.display()));
            fs::remove_dir_all(&journal.from).map_err(|e| format!("cannot remove the old copy of QuickRust's cache, {}: {}", journal.from.display(), e))?;
        }
        return Journal::remove(home);
    }
    if !journal.copy && !journal.from.exists() && journal.to.is_dir() {
        // Renamed, not yet recorded.
        record(home, &journal.to)?;
        return Journal::remove(home);
    }
    journal.undo(home)
}

/// The target a link to `target` has once the cache moved from any of `froms` (one directory, as
/// given and as it is) to `to`: one into the old cache by its absolute path names the same thing
/// in the new one; `None` for any other.
fn retarget(target: &Path, froms: &[PathBuf], to: &Path) -> Option<PathBuf> {
    froms.iter().find_map(|f| target.strip_prefix(f).ok()).map(|rest| to.join(rest))
}

/// Points every symbolic link under `dir` that names the cache that moved from `froms` to `to` by
/// its absolute path at the same thing in `to`.
fn retarget_links(dir: &Path, froms: &[PathBuf], to: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("cannot read {}: {}", dir.display(), e))? {
        let path = entry.map_err(|e| format!("cannot read {}: {}", dir.display(), e))?.path();
        let md = fs::symlink_metadata(&path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        if md.is_dir() {
            retarget_links(&path, froms, to)?;
        } else if md.file_type().is_symlink() {
            let target = fs::read_link(&path).map_err(|e| format!("cannot read the link {}: {}", path.display(), e))?;
            if let Some(new) = retarget(&target, froms, to) {
                remove_link(&path, &md).and_then(|_| make_link(&new, &path, &md)).map_err(|e| format!("the link {} to {} could not be pointed at {}: {}", path.display(), target.display(), new.display(), e))?;
            }
        }
    }
    Ok(())
}

/// A copy of the cache to another volume (see the module notes).
struct Copy<'a> {
    /// Where the cache was (as given and as it is), and where it goes.
    froms: &'a [PathBuf],
    to: &'a Path,
    /// Files with more than one link, by identity: where their first link was copied to.
    linked: HashMap<(u64, u64), PathBuf>,
    files: u64,
    bytes: u64,
    total: u64,
    progress: &'a mut dyn FnMut(Progress),
}

impl Copy<'_> {
    /// Copies the directory `from` to `to`, which is created.
    fn dir(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        let md = fs::symlink_metadata(from).map_err(|e| format!("{}: {}", from.display(), e))?;
        util::create_private_dir(to).map_err(|e| format!("{}: {}", to.display(), e))?;
        for entry in fs::read_dir(from).map_err(|e| format!("{}: {}", from.display(), e))? {
            let entry = entry.map_err(|e| format!("{}: {}", from.display(), e))?;
            let (src, dst) = (entry.path(), to.join(entry.file_name()));
            let md = fs::symlink_metadata(&src).map_err(|e| format!("{}: {}", src.display(), e))?;
            let kind = md.file_type();
            if kind.is_dir() {
                self.dir(&src, &dst)?;
            } else if kind.is_symlink() {
                let target = fs::read_link(&src).map_err(|e| format!("{}: {}", src.display(), e))?;
                let target = retarget(&target, self.froms, self.to).unwrap_or(target);
                make_link(&target, &dst, &md).map_err(|e| format!("{}: {}", src.display(), e))?;
            } else if kind.is_file() {
                self.file(&src, &dst, &md).map_err(|e| format!("{}: {}", src.display(), e))?;
            } else {
                return Err(format!("{} is not a file, a directory or a symbolic link", src.display()));
            }
        }
        // Last: its entries change its modification time, and its permissions may forbid them.
        set_modified(to, &md).map_err(|e| format!("{}: {}", to.display(), e))?;
        fs::set_permissions(to, md.permissions()).map_err(|e| format!("{}: {}", to.display(), e))
    }

    fn file(&mut self, src: &Path, dst: &Path, md: &fs::Metadata) -> io::Result<()> {
        if let Some(id) = util::linked_identity(src, md)? {
            if let Some(first) = self.linked.get(&id) {
                return fs::hard_link(first, dst);
            }
            self.linked.insert(id, dst.to_path_buf());
        }
        // Content and permissions.
        fs::copy(src, dst)?;
        set_modified(dst, md)?;
        self.files += 1;
        self.bytes += md.len();
        (self.progress)(Progress::Copied { done: self.bytes, total: self.total });
        Ok(())
    }
}

/// Gives `path` the modification time `md` has.
fn set_modified(path: &Path, md: &fs::Metadata) -> io::Result<()> {
    let times = fs::FileTimes::new().set_modified(md.modified()?);
    #[cfg(unix)]
    let file = fs::File::open(path)?;
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        // Without it a directory cannot be opened at all.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        fs::File::options().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)?
    };
    file.set_times(times)
}

/// A symbolic link at `dst` to `target`, of the kind of the link `md` describes (on Windows, a
/// link to a directory is not one to a file).
fn make_link(target: &Path, dst: &Path, md: &fs::Metadata) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = md;
        std::os::unix::fs::symlink(target, dst)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if md.file_type().is_symlink_dir() {
            std::os::windows::fs::symlink_dir(target, dst)
        } else {
            std::os::windows::fs::symlink_file(target, dst)
        }
    }
}

/// Removes the symbolic link at `path` that `md` describes (on Windows, a link to a directory is
/// removed as a directory).
fn remove_link(path: &Path, md: &fs::Metadata) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = md;
        fs::remove_file(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if md.file_type().is_symlink_dir() {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("qrust-relocate-{}-{}", name, util::new_id()));
        fs::create_dir_all(&d).unwrap();
        util::canonical(&d).unwrap()
    }

    fn mtime(p: &Path) -> std::time::SystemTime {
        fs::symlink_metadata(p).unwrap().modified().unwrap()
    }

    // TEST0332: A copy keeps contents times permissions and links
    #[test]
    fn test0332_a_copy_keeps_contents_times_permissions_and_links() {
        let root = scratch("copy");
        let (from, to) = (root.join("from"), root.join("to"));
        fs::create_dir_all(from.join("builds/app/debug/deps")).unwrap();
        fs::create_dir_all(from.join("artifacts/e/k1")).unwrap();
        let rlib = from.join("artifacts/e/k1/libutil.rlib");
        fs::write(&rlib, b"rlib bytes").unwrap();
        // The artifact cache and a build directory share one file.
        fs::hard_link(&rlib, from.join("builds/app/debug/deps/libutil.rlib")).unwrap();
        fs::write(from.join("builds/app/debug/deps/app.d"), b"dep-info").unwrap();
        let readonly = from.join("builds/app/debug/.fingerprint");
        fs::write(&readonly, b"fp").unwrap();
        let mut perms = fs::metadata(&readonly).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&readonly, perms).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("deps/app.d", from.join("builds/app/debug/link")).unwrap();
            // A link into the cache by its absolute path, and one out of it.
            std::os::unix::fs::symlink(from.join("builds/app/debug/deps"), from.join("builds/app/debug/app.dSYM")).unwrap();
            std::os::unix::fs::symlink("/elsewhere/entirely", from.join("builds/app/debug/out")).unwrap();
        }
        // Times a copy would not give them: an hour ago, to the nanosecond.
        let then = std::time::SystemTime::now() - std::time::Duration::from_nanos(3_600_000_000_123);
        for p in [from.join("builds/app/debug/deps/app.d"), rlib.clone()] {
            fs::File::options().write(true).open(&p).unwrap().set_times(fs::FileTimes::new().set_modified(then)).unwrap();
        }
        fs::File::open(from.join("builds/app/debug/deps")).unwrap().set_times(fs::FileTimes::new().set_modified(then)).unwrap();

        let mut seen = 0;
        let froms = [from.clone()];
        let mut copy = Copy {
            froms: &froms,
            to: &to,
            linked: HashMap::new(),
            files: 0,
            bytes: 0,
            total: 0,
            progress: &mut |p| {
                if let Progress::Copied { done, .. } = p {
                    seen = done;
                }
            },
        };
        copy.dir(&from, &to).unwrap();
        assert_eq!((copy.files, copy.bytes), (3, 10 + 8 + 2), "a file with two links is copied once");
        assert_eq!(seen, 20);

        assert_eq!(fs::read(to.join("builds/app/debug/deps/app.d")).unwrap(), b"dep-info");
        for p in ["builds/app/debug/deps/app.d", "artifacts/e/k1/libutil.rlib", "builds/app/debug/deps/libutil.rlib", "builds/app/debug/deps"] {
            assert_eq!(mtime(&to.join(p)), mtime(&from.join(p)), "{} keeps its modification time", p);
            assert_eq!(mtime(&to.join(p)), then, "{}", p);
        }
        assert_eq!(util::linked_identity(&to.join("artifacts/e/k1/libutil.rlib"), &fs::metadata(to.join("artifacts/e/k1/libutil.rlib")).unwrap()).unwrap(), util::linked_identity(&to.join("builds/app/debug/deps/libutil.rlib"), &fs::metadata(to.join("builds/app/debug/deps/libutil.rlib")).unwrap()).unwrap(), "still one file with two links");
        assert!(util::linked_identity(&to.join("builds/app/debug/deps/libutil.rlib"), &fs::metadata(to.join("builds/app/debug/deps/libutil.rlib")).unwrap()).unwrap().is_some());
        assert!(fs::metadata(to.join("builds/app/debug/.fingerprint")).unwrap().permissions().readonly());
        #[cfg(unix)]
        {
            assert_eq!(fs::read_link(to.join("builds/app/debug/link")).unwrap(), Path::new("deps/app.d"), "a link is copied as a link");
            assert_eq!(fs::read_link(to.join("builds/app/debug/app.dSYM")).unwrap(), to.join("builds/app/debug/deps"), "a link into the cache follows it");
            assert_eq!(fs::read_link(to.join("builds/app/debug/out")).unwrap(), Path::new("/elsewhere/entirely"), "a link out of it is left as it is");
        }

        let mut perms = fs::metadata(&readonly).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&readonly, perms.clone()).unwrap();
        fs::set_permissions(to.join("builds/app/debug/.fingerprint"), perms).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    // TEST0333: The record says where the cache is and nothing else is taken for one
    #[test]
    fn test0333_the_record_says_where_the_cache_is_and_nothing_else_is_taken_for_one() {
        let home = scratch("record");
        assert_eq!(place_in(&home).unwrap(), Place { dir: home.join("cache"), moved: false }, "no record: in the home");
        let elsewhere = home.join("elsewhere");
        record(&home, &elsewhere).unwrap();
        assert_eq!(place_in(&home).unwrap(), Place { dir: elsewhere, moved: true });
        record(&home, &home.join("cache")).unwrap();
        assert!(!home.join(RECORD).exists(), "back in the home: no record");
        for bad in [r#"{"format":1,"dir":"relative/dir"}"#, r#"{"format":2,"dir":"/x"}"#, r#"{"format":1}"#, "not json"] {
            fs::write(home.join(RECORD), bad).unwrap();
            let e = place_in(&home).unwrap_err();
            assert!(e.contains("does not say where QuickRust's cache is"), "{}: {}", bad, e);
        }
        fs::remove_dir_all(&home).unwrap();
    }

    // TEST0334: An interrupted move is finished or undone
    #[test]
    fn test0334_an_interrupted_move_is_finished_or_undone() {
        let home = scratch("journal");
        let (from, to) = (home.join("cache"), home.join("moved"));

        // A copy that stopped before its record: what it copied goes, the empty directory it
        // replaced is back, and the cache stays where it was.
        fs::create_dir_all(from.join("builds")).unwrap();
        fs::create_dir_all(to.join("builds")).unwrap();
        Journal { from: from.clone(), to: to.clone(), to_existed: true, copy: true }.write(&home).unwrap();
        finish_interrupted_locked(&home).unwrap();
        assert!(to.is_dir() && fs::read_dir(&to).unwrap().next().is_none(), "the destination is empty again");
        assert!(from.join("builds").is_dir());
        assert_eq!(place_in(&home).unwrap().dir, from);
        assert!(!home.join(JOURNAL).exists());

        // A copy that was recorded: the old copy is removed.
        fs::create_dir_all(to.join("builds")).unwrap();
        record(&home, &to).unwrap();
        Journal { from: from.clone(), to: to.clone(), to_existed: false, copy: true }.write(&home).unwrap();
        finish_interrupted_locked(&home).unwrap();
        assert!(!from.exists() && to.join("builds").is_dir());
        assert_eq!(place_in(&home).unwrap().dir, to);

        // A rename that was not recorded: it is.
        let again = home.join("again");
        fs::rename(&to, &again).unwrap();
        Journal { from: to.clone(), to: again.clone(), to_existed: false, copy: false }.write(&home).unwrap();
        finish_interrupted_locked(&home).unwrap();
        assert_eq!(place_in(&home).unwrap(), Place { dir: again.clone(), moved: true });
        assert!(!home.join(JOURNAL).exists());
        fs::remove_dir_all(&home).unwrap();
    }

    // TEST0335: A destination is a directory whose parent exists
    #[test]
    fn test0335_a_destination_is_a_directory_whose_parent_exists() {
        let root = scratch("dest");
        assert_eq!(destination(Path::new("new"), &root).unwrap(), root.join("new"));
        assert!(destination(&root.join("a/../new"), Path::new("/")).unwrap_err().contains("cannot move QuickRust's cache into"), "a parent that does not exist");
        fs::create_dir_all(root.join("a")).unwrap();
        assert_eq!(destination(&root.join("a/../new"), Path::new("/")).unwrap(), root.join("new"));
        assert_eq!(destination(Path::new("a"), &root).unwrap(), root.join("a"), "an existing directory, as it is");
        assert!(destination(Path::new(".."), &root.join("missing")).is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
