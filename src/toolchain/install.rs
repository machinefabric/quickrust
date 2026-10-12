//! Installing toolchains and components from release archives.
//!
//! An archive (rust-installer format) holds one directory with a `components` file naming the
//! components inside, and for each a `<component>/manifest.in` listing its files (`file:<path>`)
//! and directories (`dir:<path>`) relative to the toolchain root. Installing a component moves
//! those into the toolchain, records the list in `lib/rustlib/manifest-<component>` and the name
//! in `lib/rustlib/components`, as rustup does, so tools that read them keep working.
//!
//! A new toolchain is assembled in `toolchains/.staging/<id>.<pid>` and renamed into place
//! complete. Components added to an installed toolchain (which builds may be using) go through a
//! journal, `lib/rustlib/.haul-adding`, written before anything is moved; the toolchain's record
//! (`haul-toolchain.json`) is the commit point. An add that was interrupted is undone the next
//! time the toolchain's lock is taken. Two processes installing the same toolchain wait for one
//! another on `toolchains/.locks/<id>.lock`; the second finds the work done.

use super::dist::{Component, Manifest};
use super::file::Profile;
use super::Layout;
use crate::{archive, net, util};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub const RECORD: &str = "haul-toolchain.json";
const JOURNAL: &str = "lib/rustlib/.haul-adding";
/// Components being removed: once named here, a removal is carried through.
const REMOVING: &str = "lib/rustlib/.haul-removing";
/// The largest archive content unpacked (rustc-dev is the largest component, about 1 GiB).
const MAX_UNPACKED: u64 = 8 << 30;
const PARALLEL: usize = 4;

/// What Haul knows about a toolchain it installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub version: String,
    pub date: String,
    pub host: String,
    pub profile: String,
    /// `(package, target)` of each installed archive, with the components it held.
    pub components: Vec<(String, String, Vec<String>)>,
    pub installed_ns: u64,
}

impl Record {
    pub fn has(&self, pkg: &str, target: &str) -> bool {
        self.components.iter().any(|(p, t, _)| p == pkg && (t == target || t == "*"))
    }

    fn to_json(&self) -> Value {
        let comps: Vec<Value> = self.components.iter().map(|(p, t, n)| json!({"pkg": p, "target": t, "names": n})).collect();
        json!({"format": 1, "id": self.id, "version": self.version, "date": self.date, "host": self.host, "profile": self.profile, "components": comps, "installed_ns": self.installed_ns})
    }

    fn save(&self, dir: &Path) -> Result<(), String> {
        util::write_json(&dir.join(RECORD), &self.to_json()).map_err(|e| format!("cannot write {}: {}", dir.join(RECORD).display(), e))
    }
}

pub fn read_record(dir: &Path) -> Result<Record, String> {
    let p = dir.join(RECORD);
    let v = util::read_json(&p).ok_or_else(|| format!("{} is not a toolchain Haul installed (no readable {}); `haul toolchain remove` deletes it", dir.display(), RECORD))?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("{}: no `{}`", p.display(), k));
    let mut components = vec![];
    for c in v.get("components").and_then(Value::as_array).ok_or_else(|| format!("{}: no components", p.display()))? {
        let f = |k: &str| c.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("{}: a component without `{}`", p.display(), k));
        let names = c.get("names").and_then(Value::as_array).map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect()).unwrap_or_default();
        components.push((f("pkg")?, f("target")?, names));
    }
    Ok(Record { id: s("id")?, version: s("version")?, date: s("date")?, host: s("host")?, profile: s("profile")?, components, installed_ns: v.get("installed_ns").and_then(Value::as_u64).unwrap_or(0) })
}

/// Progress of an install, reported on the calling thread.
pub trait Report {
    /// Another process is installing the same toolchain; this one waits for it.
    fn waiting(&mut self, id: &str);
    /// Archives done of the total, bytes received so far, and the archive last advanced.
    fn progress(&mut self, done: usize, total: usize, bytes: u64, current: &str);
}

/// Holds a toolchain's install lock, after undoing an interrupted component add.
pub struct Locked {
    _lock: util::ExclusiveLock,
}

pub fn lock(layout: &Layout, id: &str, report: &mut dyn Report) -> Result<Locked, String> {
    let path = layout.toolchains().join(".locks").join(format!("{}.lock", id));
    let lock = match util::ExclusiveLock::try_acquire(&path).map_err(|e| format!("cannot lock {}: {}", path.display(), e))? {
        Some(l) => l,
        None => {
            report.waiting(id);
            util::ExclusiveLock::acquire(&path).map_err(|e| format!("cannot lock {}: {}", path.display(), e))?
        }
    };
    // Whoever left these held this lock and is gone.
    remove_prefixed(&layout.toolchains().join(".staging"), &format!("{}.", id));
    let dir = layout.toolchains().join(id);
    if dir.join(JOURNAL).exists() {
        undo_add(&dir)?;
    }
    if dir.join(REMOVING).exists() {
        finish_removal(&dir)?;
    }
    Ok(Locked { _lock: lock })
}

/// Installs toolchain `id` from `manifest`: the given components, assembled apart and renamed
/// into place. The caller holds the toolchain's lock and has checked it is not installed.
#[allow(clippy::too_many_arguments)]
pub fn install(layout: &Layout, _locked: &Locked, id: &str, host: &str, profile: Profile, manifest: &Manifest, components: &[Component], server: &str, client: &net::Client, report: &mut dyn Report) -> Result<PathBuf, String> {
    let dest = layout.toolchains().join(id);
    if dest.exists() {
        return Err(format!("{} exists but is not a complete toolchain; `haul toolchain remove {}` deletes it", dest.display(), id));
    }
    let staging_root = layout.toolchains().join(".staging");
    util::create_private_dir(&staging_root).map_err(|e| format!("cannot create {}: {}", staging_root.display(), e))?;
    let staging = staging_root.join(format!("{}.{}", id, std::process::id()));
    let out = (|| {
        fs::create_dir_all(staging.join("lib/rustlib")).map_err(|e| format!("cannot create {}: {}", staging.display(), e))?;
        let unpacked = fetch_and_unpack(layout, &staging_root, &format!("{}.{}.unpack", id, std::process::id()), components, server, client, host, report)?;
        let mut record = Record { id: id.to_string(), version: manifest.version().to_string(), date: manifest.date().to_string(), host: host.to_string(), profile: profile.name().to_string(), components: vec![], installed_ns: util::now_wall_ns() };
        for (c, dir) in components.iter().zip(&unpacked) {
            let names = place(dir, &staging, None)?;
            record.components.push((c.pkg.clone(), c.target.clone(), names));
            let _ = fs::remove_dir_all(dir);
        }
        let write = |rel: &str, bytes: &[u8]| fs::write(staging.join(rel), bytes).map_err(|e| format!("cannot write {}: {}", rel, e));
        write("lib/rustlib/channel-manifest.toml", &manifest.bytes)?;
        write("lib/rustlib/rust-installer-version", b"3\n")?;
        record.save(&staging)?;
        fs::rename(&staging, &dest).map_err(|e| format!("cannot move the toolchain into {}: {}", dest.display(), e))?;
        forget_downloads(layout, components);
        Ok(dest.clone())
    })();
    if out.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    remove_prefixed(&staging_root, &format!("{}.{}.unpack", id, std::process::id()));
    out
}

/// Adds components to the installed toolchain in `dir`, journaled (see the module notes). The
/// caller holds the toolchain's lock; `components` come from the toolchain's own manifest.
pub fn add(layout: &Layout, _locked: &Locked, dir: &Path, components: &[Component], server: &str, client: &net::Client, report: &mut dyn Report) -> Result<(), String> {
    let mut record = read_record(dir)?;
    let staging_root = layout.toolchains().join(".staging");
    util::create_private_dir(&staging_root).map_err(|e| format!("cannot create {}: {}", staging_root.display(), e))?;
    let prefix = format!("{}.add.{}", record.id, std::process::id());
    let out = (|| {
        let unpacked = fetch_and_unpack(layout, &staging_root, &prefix, components, server, client, &record.host, report)?;
        // The journal names everything about to be moved in, before anything is; none of it may
        // exist (undoing the add removes what the journal names).
        let mut journal = String::new();
        for (c, d) in components.iter().zip(&unpacked) {
            for (name, entries) in read_components(d)? {
                for (_, rel) in entries {
                    if fs::symlink_metadata(dir.join(&rel)).is_ok() {
                        return Err(format!("{} would replace {}, which another component installed", name, rel));
                    }
                    journal.push_str(&format!("{}\t{}\t{}\t{}\n", c.pkg, c.target, name, rel));
                }
            }
        }
        util::write_atomic(&dir.join(JOURNAL), journal.as_bytes()).map_err(|e| format!("cannot write the install journal: {}", e))?;
        for (c, d) in components.iter().zip(&unpacked) {
            let names = place(d, dir, Some(&record))?;
            record.components.push((c.pkg.clone(), c.target.clone(), names));
        }
        record.save(dir)?;
        fs::remove_file(dir.join(JOURNAL)).map_err(|e| format!("cannot remove the install journal: {}", e))?;
        forget_downloads(layout, components);
        Ok(())
    })();
    remove_prefixed(&staging_root, &prefix);
    if out.is_err() && dir.join(JOURNAL).exists() {
        undo_add(dir)?;
    }
    out
}

/// Removes the archives `(package, target)` from the installed toolchain in `dir`: they leave the
/// record first (a removal once begun is carried through, see `finish_removal`), then their files
/// go. The caller holds the toolchain's lock.
pub fn remove_components(_locked: &Locked, dir: &Path, which: &[(String, String)]) -> Result<(), String> {
    let mut record = read_record(dir)?;
    let mut names = vec![];
    for (pkg, target) in which {
        let at = record.components.iter().position(|(p, t, _)| p == pkg && t == target).ok_or_else(|| format!("{} has no {} for {}", record.id, pkg, target))?;
        if matches!(pkg.as_str(), "rustc" | "cargo") || (pkg == "rust-std" && *target == record.host) {
            return Err(format!("{} for {} is part of every toolchain; `haul toolchain remove {}` removes the toolchain", pkg, target, record.id));
        }
        names.extend(record.components.remove(at).2);
    }
    let listing: String = names.iter().map(|n| format!("{}\n", n)).collect();
    util::write_atomic(&dir.join(REMOVING), listing.as_bytes()).map_err(|e| format!("cannot write the removal journal: {}", e))?;
    record.save(dir)?;
    finish_removal(dir)
}

/// Carries a removal through: the files of every component the journal names, its manifest and
/// its line in `components`.
fn finish_removal(dir: &Path) -> Result<(), String> {
    let journal = fs::read_to_string(dir.join(REMOVING)).map_err(|e| format!("cannot read the removal journal: {}", e))?;
    let names: Vec<&str> = journal.lines().filter(|l| !l.is_empty()).collect();
    let rustlib = dir.join("lib/rustlib");
    for name in &names {
        let manifest = rustlib.join(format!("manifest-{}", name));
        if let Ok(text) = fs::read_to_string(&manifest) {
            for line in text.lines() {
                let Some((_, rel)) = line.split_once(':') else { continue };
                let path = dir.join(rel);
                let _ = match fs::symlink_metadata(&path) {
                    Ok(md) if md.is_dir() => fs::remove_dir_all(&path),
                    Ok(_) => fs::remove_file(&path),
                    Err(_) => Ok(()),
                };
            }
            fs::remove_file(&manifest).map_err(|e| format!("cannot remove {}: {}", manifest.display(), e))?;
        }
    }
    let listed = rustlib.join("components");
    let text = fs::read_to_string(&listed).unwrap_or_default();
    let kept: String = text.lines().filter(|l| !names.contains(l)).map(|l| format!("{}\n", l)).collect();
    util::write_atomic(&listed, kept.as_bytes()).map_err(|e| format!("cannot write {}: {}", listed.display(), e))?;
    fs::remove_file(dir.join(REMOVING)).map_err(|e| format!("cannot remove the removal journal: {}", e))
}

/// Undoes an interrupted add: whatever the journal names that the record does not hold.
fn undo_add(dir: &Path) -> Result<(), String> {
    let record = read_record(dir)?;
    let journal = fs::read_to_string(dir.join(JOURNAL)).map_err(|e| format!("cannot read the install journal: {}", e))?;
    let mut undone: Vec<String> = vec![];
    for line in journal.lines() {
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        let [pkg, target, name, rel] = parts[..] else { return Err(format!("{}: unreadable line {:?}", dir.join(JOURNAL).display(), line)) };
        if record.components.iter().any(|(p, t, _)| p == pkg && t == target) {
            continue;
        }
        let path = dir.join(rel);
        match fs::symlink_metadata(&path) {
            Ok(md) if md.is_dir() => fs::remove_dir_all(&path),
            Ok(_) => fs::remove_file(&path),
            Err(_) => Ok(()),
        }
        .map_err(|e| format!("cannot undo {}: {}", path.display(), e))?;
        if !undone.iter().any(|n| n == name) {
            undone.push(name.to_string());
        }
    }
    for name in &undone {
        let _ = fs::remove_file(dir.join("lib/rustlib").join(format!("manifest-{}", name)));
    }
    let listed = dir.join("lib/rustlib/components");
    if let Ok(text) = fs::read_to_string(&listed) {
        let kept: String = text.lines().filter(|l| !undone.iter().any(|n| n == l)).map(|l| format!("{}\n", l)).collect();
        util::write_atomic(&listed, kept.as_bytes()).map_err(|e| format!("cannot write {}: {}", listed.display(), e))?;
    }
    fs::remove_file(dir.join(JOURNAL)).map_err(|e| format!("cannot remove the install journal: {}", e))
}

/// Downloads the archives (verified, into the download cache) and unpacks each into
/// `<staging_root>/<prefix>.<n>`, `PARALLEL` at a time; reports progress on this thread.
#[allow(clippy::too_many_arguments)]
fn fetch_and_unpack(layout: &Layout, staging_root: &Path, prefix: &str, components: &[Component], server: &str, client: &net::Client, host: &str, report: &mut dyn Report) -> Result<Vec<PathBuf>, String> {
    enum Msg {
        Bytes(usize, u64),
        Done(usize, Result<PathBuf, String>),
    }
    let downloads = layout.downloads();
    fs::create_dir_all(&downloads).map_err(|e| format!("cannot create {}: {}", downloads.display(), e))?;
    let (tx, rx) = mpsc::channel();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<Option<Result<PathBuf, String>>> = components.iter().map(|_| None).collect();
    std::thread::scope(|scope| {
        for _ in 0..PARALLEL.min(components.len()) {
            let tx = tx.clone();
            let next = &next;
            let downloads = &downloads;
            scope.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(c) = components.get(i) else { return };
                let archive_path = downloads.join(format!("{}.tar.xz", c.sha256));
                let out = (|| {
                    if !archive_path.exists() {
                        let tx = tx.clone();
                        client.download(&super::dist::on_server(&c.url, server), &archive_path, &c.sha256, &mut |n, _| { let _ = tx.send(Msg::Bytes(i, n)); }).map_err(|e| e.to_string())?;
                    }
                    let dir = staging_root.join(format!("{}.{}", prefix, i));
                    let _ = fs::remove_dir_all(&dir);
                    archive::unpack_file(&archive_path, archive::Compression::Xz, c.root(), &dir, MAX_UNPACKED).map_err(|e| format!("cannot unpack {}: {}", c.url, e))?;
                    Ok(dir)
                })();
                let failed = out.is_err();
                let _ = tx.send(Msg::Done(i, out));
                if failed {
                    // Others finish what they started; nothing new is begun.
                    next.store(components.len(), std::sync::atomic::Ordering::SeqCst);
                    return;
                }
            });
        }
        drop(tx);
        let mut bytes = vec![0u64; components.len()];
        let mut done = 0;
        report.progress(0, components.len(), 0, "");
        for msg in rx {
            match msg {
                Msg::Bytes(i, n) => {
                    bytes[i] = n;
                    report.progress(done, components.len(), bytes.iter().sum(), &components[i].label(host));
                }
                Msg::Done(i, r) => {
                    done += 1;
                    results[i] = Some(r);
                    report.progress(done, components.len(), bytes.iter().sum(), &components[i].label(host));
                }
            }
        }
    });
    let mut dirs = vec![];
    let mut first_error = None;
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Some(Ok(d)) => dirs.push(d),
            Some(Err(e)) => {
                first_error.get_or_insert(e);
            }
            None => {
                first_error.get_or_insert_with(|| format!("{} was not downloaded", components[i].url));
            }
        }
    }
    match first_error {
        Some(e) => {
            for d in dirs {
                let _ = fs::remove_dir_all(d);
            }
            Err(e)
        }
        None => Ok(dirs),
    }
}

/// One component of an unpacked archive: its name and its entries (`file`/`dir`, relative path).
type Listing = (String, Vec<(String, String)>);

/// The components an unpacked archive holds.
fn read_components(unpacked: &Path) -> Result<Vec<Listing>, String> {
    let listed = fs::read_to_string(unpacked.join("components")).map_err(|e| format!("{}: no components list: {}", unpacked.display(), e))?;
    let mut out = vec![];
    for name in listed.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !super::spec::is_link_name(name) {
            return Err(format!("{}: `{}` is not a component name", unpacked.display(), name));
        }
        let manifest = unpacked.join(name).join("manifest.in");
        let text = fs::read_to_string(&manifest).map_err(|e| format!("cannot read {}: {}", manifest.display(), e))?;
        let mut entries = vec![];
        for line in text.lines().filter(|l| !l.is_empty()) {
            let (kind, rel) = line.split_once(':').ok_or_else(|| format!("{}: unreadable line {:?}", manifest.display(), line))?;
            let plain = Path::new(rel).components().all(|c| matches!(c, std::path::Component::Normal(_)));
            if !matches!(kind, "file" | "dir") || rel.is_empty() || !plain {
                return Err(format!("{}: unreadable line {:?}", manifest.display(), line));
            }
            entries.push((kind.to_string(), rel.to_string()));
        }
        out.push((name.to_string(), entries));
    }
    Ok(out)
}

/// Moves the components of an unpacked archive into the toolchain `into`, records each, and
/// returns their names. A path another component installed already is an error.
fn place(unpacked: &Path, into: &Path, existing: Option<&Record>) -> Result<Vec<String>, String> {
    let mut names = vec![];
    for (name, entries) in read_components(unpacked)? {
        if existing.is_some_and(|r| r.components.iter().any(|(_, _, n)| n.contains(&name))) {
            return Err(format!("{} has component {} already", into.display(), name));
        }
        let mut listed = String::new();
        for (kind, rel) in &entries {
            let from = unpacked.join(&name).join(rel);
            let to = into.join(rel);
            if fs::symlink_metadata(&to).is_ok() {
                return Err(format!("{} would replace {}, which another component installed", name, rel));
            }
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
            }
            fs::rename(&from, &to).map_err(|e| format!("cannot install {}: {}", rel, e))?;
            listed.push_str(&format!("{}:{}\n", kind, rel));
        }
        let rustlib = into.join("lib/rustlib");
        fs::create_dir_all(&rustlib).map_err(|e| format!("cannot create {}: {}", rustlib.display(), e))?;
        util::write_atomic_fast(&rustlib.join(format!("manifest-{}", name)), listed.as_bytes()).map_err(|e| format!("cannot record {}: {}", name, e))?;
        let mut f = fs::OpenOptions::new().create(true).append(true).open(rustlib.join("components")).map_err(|e| format!("cannot record {}: {}", name, e))?;
        writeln!(f, "{}", name).map_err(|e| format!("cannot record {}: {}", name, e))?;
        names.push(name);
    }
    Ok(names)
}

/// Drops the archives of an install that succeeded (one that failed keeps them for the next try).
fn forget_downloads(layout: &Layout, components: &[Component]) {
    for c in components {
        let _ = fs::remove_file(layout.downloads().join(format!("{}.tar.xz", c.sha256)));
    }
}

fn remove_prefixed(dir: &Path, prefix: &str) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(prefix) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
}
