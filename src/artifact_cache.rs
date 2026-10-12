//! Haul's global artifact cache (fast mode).
//!
//! The rustc wrapper looks a compilation up before running rustc. On a hit it restores the
//! outputs into Cargo's output directory and replays rustc's stderr, so Cargo sees exactly
//! what a real compilation would have produced, minus the time. This first version caches
//! only units whose sources cannot change under a name (registry and git dependencies) and
//! that read nothing a build script produced outside `OUT_DIR`:
//!
//! - **key, level 1**: cache format version; rustc identity; every argument, with output
//!   and search paths removed and each `--extern` replaced by the content hash of the
//!   artifact it names; Cargo's `CARGO_*` variables; the content of `OUT_DIR`;
//! - **key, level 2**: level 1 plus the values of the environment variables rustc reported
//!   reading (`# env-dep:` in its dep-info), which are only known after one compilation;
//! - **store**: outputs are hard-linked into the entry when the cache shares their file
//!   system (rustc and the linkers write every output as a new file, so the next build of
//!   the unit leaves the entry's file alone), and copied otherwise. The dep-info is always
//!   copied: rustc rewrites it in place. Every file's content hash (BLAKE3) is kept with
//!   the entry, so its bookkeeping needs no fsync either;
//! - **hit**: outputs are copied (never linked: a later build of that unit, here or in
//!   another checkout, would see a modification time another checkout set) and so get a
//!   current modification time, which Cargo's staleness check expects; the dep-info and
//!   the replayed stderr have the output and `OUT_DIR` paths of the cached build replaced
//!   by this build's, because Cargo derives its own fingerprints and its pipelining
//!   (`artifact` notices) from them, and stores the diagnostics it replays as warnings.
//!   Every file is checked against its content hash as it is restored: a file rewritten in
//!   place through a link, torn by a crash or damaged on disk discards the entry.
//!
//! Any cache problem means "compile normally": the cache can only ever save time.
//!
//! Known difference: a crate that `include!`s files its build script generated is keyed by
//! the content of `OUT_DIR`, not its path, so a unit restored into another target directory
//! names the first one in panic locations and debug information for that generated code.

use crate::util;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const FORMAT: &str = "haul-artifact-cache-2";

/// One cacheable compilation, prepared before rustc runs.
pub struct Job {
    root: PathBuf,
    key1: String,
    out_dir: PathBuf,
    stem: String,
    out_dir_env: Option<String>,
    /// A workspace unit: its sources can change, so level 2 also hashes every input file.
    local: bool,
    /// Produces a linked output (binary, test executable, proc-macro).
    links: bool,
    cwd: PathBuf,
}

pub enum Lookup {
    /// Outputs restored and stderr to replay.
    Hit { stderr: Vec<u8>, entry: String },
    Miss,
}

fn value_of<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Option<&'a str> {
    let a = &args[*i];
    if a == flag {
        *i += 1;
        return args.get(*i).map(String::as_str);
    }
    a.strip_prefix(flag).map(|r| r.strip_prefix('=').unwrap_or(r))
}

/// Identity of a file's current content, without reading it: a rewrite changes its modification
/// time and usually its inode, as Cargo's own fingerprints assume.
fn content_identity(md: &fs::Metadata, path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        format!("{}:{}:{}:{}", md.dev(), md.ino(), md.len(), util::mtime_ns(md))
    }
    #[cfg(not(unix))]
    {
        format!("{}:{}:{}", util::path_str(path), md.len(), util::mtime_ns(md))
    }
}

fn memo_path(root: &Path, md: &fs::Metadata, path: &Path) -> PathBuf {
    root.join("h").join(util::digest_parts([content_identity(md, path)]))
}

/// Content hash of an artifact, remembered per content identity. Every unit hashes the
/// dependencies it links against, and a binary links them all: tens of megabytes per lookup.
fn content_hash(root: &Path, path: &Path) -> std::io::Result<String> {
    let md = fs::metadata(path)?;
    let memo = memo_path(root, &md, path);
    if let Ok(h) = fs::read_to_string(&memo) {
        if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(h);
        }
    }
    let h = util::blake3_file(path)?;
    remember_hash(root, path, &h)?;
    Ok(h)
}

/// Records the content hash of a file whose content is known (a restored artifact).
fn remember_hash(root: &Path, path: &Path, hash: &str) -> std::io::Result<()> {
    let md = fs::metadata(path)?;
    let memo = memo_path(root, &md, path);
    fs::create_dir_all(root.join("h"))?;
    util::write_atomic_fast(&memo, hash.as_bytes())
}

/// Outputs another unit can name in `--extern`, whose hashes are kept with a cache entry.
fn is_linkable(name: &str) -> bool {
    [".rlib", ".rmeta", ".so", ".dylib", ".dll"].iter().any(|e| name.ends_with(e))
}

/// Content digest of a directory tree: relative names and file contents, sorted.
fn tree_digest(root: &Path) -> Option<String> {
    let mut parts = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).ok()?.flatten() {
            let md = fs::symlink_metadata(e.path()).ok()?;
            let rel = util::path_str(e.path().strip_prefix(root).ok()?);
            if md.is_dir() {
                stack.push(e.path());
            } else {
                parts.push(format!("{}={}", rel, util::blake3_file(&e.path()).ok()?));
            }
        }
    }
    parts.sort();
    Some(util::digest_parts(parts))
}

/// Prepares a lookup, or says why this compilation is not cached.
/// `compiler` is the compiler's own account of itself (`rustc -vV`, see `rustc_info`).
pub fn prepare(root: Option<&Path>, compiler: &str, args: &[String]) -> Result<Job, String> {
    let root = root.ok_or("the artifact cache is off")?.to_path_buf();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| "not a Cargo unit")?;
    let local = !crate::wrapper::is_external_source(Path::new(&manifest_dir));
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut key = vec![FORMAT.to_string(), format!("rustc={}", util::sha256_hex(compiler.as_bytes()))];
    if local {
        // Workspace units carry the checkout's absolute path in their debug information: they are
        // reused within the same checkout (after a clean, across branch switches, in CI with a
        // fixed path), never into another one.
        key.push(format!("workspace={}", util::path_str(&cwd)));
    }
    let mut test = false;
    let mut scheduled_by_cargo = false;
    let (mut crate_name, mut out_dir, mut extra, mut types) = (None, None, String::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a.starts_with('@') {
            return Err("arguments come from an argument file".into());
        }
        if let Some(v) = value_of(args, &mut i, "--out-dir") {
            out_dir = Some(PathBuf::from(v));
        } else if let Some(v) = value_of(args, &mut i, "--crate-name") {
            crate_name = Some(v.to_string());
            key.push(format!("--crate-name={}", v));
        } else if let Some(v) = value_of(args, &mut i, "--crate-type") {
            types.extend(v.split(',').map(str::to_string));
            key.push(format!("--crate-type={}", v));
        } else if let Some(v) = value_of(args, &mut i, "--extern") {
            // The artifact a dependency resolves to, by content: identical inputs, identical output.
            match v.split_once('=') {
                Some((name, path)) => key.push(format!("--extern={}=sha:{}", name, content_hash(&root, Path::new(path)).map_err(|e| format!("dependency {} is unreadable: {}", path, e))?)),
                None => key.push(format!("--extern={}", v)),
            }
        } else if let Some(v) = value_of(args, &mut i, "-L") {
            // Where to look for dependencies is not what is compiled; native search paths are.
            if !v.starts_with("dependency=") {
                return Err("links against native libraries".into());
            }
        } else if value_of(args, &mut i, "-l").is_some() || a.starts_with("--print") {
            return Err("links native libraries or prints compiler information".into());
        } else if let Some(v) = value_of(args, &mut i, "-C") {
            if v.starts_with("incremental") {
                // rustc revalidates its incremental state against the inputs on every compilation,
                // so outputs may be restored without it; only whether it is on changes the output.
                key.push("-Cincremental".into());
                i += 1;
                continue;
            }
            if let Some(e) = v.strip_prefix("extra-filename=") {
                extra = e.to_string();
            }
            scheduled_by_cargo |= v.starts_with("metadata=");
            key.push(format!("-C{}", v));
        } else if a.starts_with("-Zthreads") {
            // How many threads rustc's frontend uses does not change what it produces.
        } else if a == "--test" {
            test = true;
            key.push(a.to_string());
        } else if value_of(args, &mut i, "--diagnostic-width").is_some() {
            // Terminal width of the build that ran first; never part of the identity.
        } else {
            key.push(args[i].clone());
        }
        i += 1;
    }
    // Build scripts run rustc themselves to probe the compiler (autocfg, version checks); those
    // must run every time. Cargo passes `-C metadata` to every unit it schedules, probes never.
    if !scheduled_by_cargo {
        return Err("a compiler probe run by a build script, not a unit Cargo scheduled".into());
    }
    let crate_name = crate_name.ok_or("no crate name")?;
    let out_dir = out_dir.ok_or("no output directory")?;
    if let Some(t) = types.iter().find(|t| !matches!(t.as_str(), "lib" | "rlib" | "proc-macro" | "bin")) {
        return Err(format!("crate type {} is not cached", t));
    }
    if (test || types.iter().any(|t| t == "bin")) && !local && !crate_name.starts_with("build_script_") {
        return Err("dependency binaries other than build scripts are not cached".into());
    }
    // What Cargo sets for the unit itself. Variables describing where this particular build
    // lives (target directory, Cargo home, current directory) are left out: when rustc or a
    // macro reads one of them through `env!`, dep-info reports it and level 2 keys on it.
    const UNIT_ENV: &[&str] = &["CARGO_PKG_", "CARGO_CRATE_NAME", "CARGO_MANIFEST_DIR", "CARGO_MANIFEST_PATH", "CARGO_MANIFEST_LINKS", "CARGO_PRIMARY_PACKAGE", "CARGO_BIN_NAME", "CARGO_CFG_"];
    let mut env: Vec<String> = std::env::vars().filter(|(k, _)| UNIT_ENV.iter().any(|p| k.starts_with(p))).map(|(k, v)| format!("{}={}", k, v)).collect();
    env.sort();
    key.extend(env);
    let out_dir_env = std::env::var("OUT_DIR").ok();
    if let Some(o) = &out_dir_env {
        key.push(format!("OUT_DIR=sha:{}", tree_digest(Path::new(o)).ok_or("OUT_DIR is unreadable")?));
    }
    let links = test || types.iter().any(|t| matches!(t.as_str(), "bin" | "proc-macro"));
    Ok(Job { root, key1: util::digest_parts(key), out_dir: cwd.join(out_dir), stem: format!("{}{}", crate_name, extra), out_dir_env, local, links, cwd })
}

/// Level 2: the values of the variables rustc read, and for workspace units the content of
/// every input file it read. `None` when an input has gone (a miss, then a fresh compilation).
fn level2(key1: &str, names: &[String], inputs: &[String], cwd: &Path) -> Option<String> {
    let mut parts = vec![env_digest(key1, names)];
    for f in inputs {
        // Files below OUT_DIR are covered by its content digest in level 1.
        if f.starts_with("<OUT_DIR>") {
            continue;
        }
        parts.push(format!("{}={}", f, util::blake3_file(&cwd.join(f)).ok()?));
    }
    Some(util::digest_parts(parts))
}

fn env_digest(key1: &str, names: &[String]) -> String {
    util::digest_parts(std::iter::once(key1.to_string()).chain(names.iter().map(|n| {
        // `include!(concat!(env!("OUT_DIR"), …))` reads OUT_DIR: what matters is the directory's
        // content, already in level 1; its path differs in every target directory.
        if n == "OUT_DIR" {
            return "OUT_DIR=<content>".to_string();
        }
        format!("{}={}", n, std::env::var(n).map(|v| format!("set:{}", v)).unwrap_or_else(|_| "unset".into()))
    })))
}

/// Replaces the cached build's paths with this build's (longest first).
fn rewrite(text: &[u8], pairs: &[(String, String)]) -> Vec<u8> {
    let mut s = String::from_utf8_lossy(text).into_owned();
    for (from, to) in pairs {
        if !from.is_empty() && from != to {
            s = s.replace(from.as_str(), to);
        }
    }
    s.into_bytes()
}

impl Job {
    /// A workspace unit (sources can change), as opposed to a dependency.
    pub fn is_local(&self) -> bool {
        self.local
    }

    /// Produces a linked output: binary, test executable or proc-macro.
    pub fn links(&self) -> bool {
        self.links
    }

    fn manifest(&self) -> PathBuf {
        self.root.join("m").join(format!("{}.json", self.key1))
    }

    /// Whether this unit compiled quickly before and was not stored for it: a small linked
    /// workspace output. It is compiled again after every clean; the parallel frontend would
    /// only add its start-up cost to it.
    pub fn known_quick(&self) -> bool {
        self.quick_marker().exists()
    }

    /// Records that this unit compiled quickly and was not stored for it.
    pub fn remember_quick(&self) -> std::io::Result<()> {
        fs::create_dir_all(self.root.join("q"))?;
        util::write_atomic_fast(&self.quick_marker(), b"")
    }

    fn quick_marker(&self) -> PathBuf {
        self.root.join("q").join(&self.key1)
    }

    fn entry_dir(&self, key2: &str) -> PathBuf {
        self.root.join("e").join(key2)
    }

    pub fn lookup(&self) -> Lookup {
        let Some(m) = util::read_json(&self.manifest()) else { return Lookup::Miss };
        let list = |k: &str| -> Vec<String> { m[k].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect() };
        let Some(key2) = level2(&self.key1, &list("env_deps"), &list("inputs"), &self.cwd) else { return Lookup::Miss };
        let dir = self.entry_dir(&key2);
        let Some(entry) = util::read_json(&dir.join("entry.json")) else { return Lookup::Miss };
        let pairs = vec![
            (util::vstr(&entry, "out_dir"), util::path_str(&self.out_dir)),
            (util::vstr(&entry, "out_dir_env"), self.out_dir_env.clone().unwrap_or_default()),
        ];
        let restore = || -> std::io::Result<Vec<u8>> {
            for f in entry["files"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let bytes = fs::read(dir.join(f))?;
                if Some(blake3::hash(&bytes).to_hex().as_str()) != entry["hashes"][f].as_str() {
                    return Err(std::io::Error::other(format!("{} does not have the content it was stored with", f)));
                }
                let bytes = if f.ends_with(".d") { rewrite(&bytes, &pairs) } else { bytes };
                // A fresh file, so Cargo's mtime-based staleness check sees a new output.
                util::write_atomic_fast(&self.out_dir.join(f), &bytes)?;
                // Its content is known: dependents hashing it for their own keys need not read it.
                if is_linkable(f) {
                    remember_hash(&self.root, &self.out_dir.join(f), entry["hashes"][f].as_str().unwrap_or_default())?;
                }
                #[cfg(unix)]
                if entry["executable"].as_array().into_iter().flatten().any(|x| x.as_str() == Some(f)) {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(self.out_dir.join(f), fs::Permissions::from_mode(0o755))?;
                }
            }
            Ok(rewrite(&fs::read(dir.join("stderr"))?, &pairs))
        };
        match restore() {
            Ok(stderr) => {
                // Recency for eviction: the least recently used entries go first.
                let _ = fs::File::options().write(true).open(dir.join("entry.json")).and_then(|f| f.set_modified(std::time::SystemTime::now()));
                Lookup::Hit { stderr, entry: key2 }
            }
            // A damaged entry is a miss; the compilation that follows overwrites the outputs and,
            // with the entry gone, stores it again.
            Err(_) => {
                discard(&self.root, &dir);
                Lookup::Miss
            }
        }
    }

    /// Stores the outputs of a successful compilation that started at `start_wall_ns`.
    pub fn store(&self, stderr: &[u8], start_wall_ns: u64) -> Result<(), String> {
        let dep_info = self.out_dir.join(format!("{}.d", self.stem));
        let text = fs::read_to_string(&dep_info).map_err(|e| format!("no dep-info: {}", e))?;
        let mut names: Vec<String> = text.lines().filter_map(|l| l.strip_prefix("# env-dep:")).map(|r| r.split_once('=').map(|(n, _)| n).unwrap_or(r).to_string()).collect();
        names.sort();
        names.dedup();
        // Inputs of a workspace unit, as rustc listed them (relative to the workspace root, or below OUT_DIR).
        let mut inputs = Vec::new();
        if self.local {
            let rule = text.lines().find(|l| !l.starts_with('#') && l.contains(": ")).ok_or("dep-info lists no inputs")?;
            for f in crate::wrapper::split_depinfo_paths(rule.split_once(": ").map(|(_, b)| b).unwrap_or("")) {
                let p = self.cwd.join(&f);
                // An input that changed while rustc ran: the outputs may not match its current content.
                let md = fs::metadata(&p).map_err(|e| format!("input {} is unreadable: {}", f, e))?;
                if util::mtime_ns(&md) >= start_wall_ns {
                    return Err(format!("{} changed during the compilation", f));
                }
                match self.out_dir_env.as_deref().and_then(|o| util::path_str(&p).strip_prefix(o).map(str::to_string)) {
                    Some(rel) => inputs.push(format!("<OUT_DIR>{}", rel)),
                    None => inputs.push(f),
                }
            }
            inputs.sort();
            inputs.dedup();
        }
        let key2 = level2(&self.key1, &names, &inputs, &self.cwd).ok_or("an input is unreadable")?;
        let dir = self.entry_dir(&key2);
        // The manifest always describes the latest compilation (its input list is what later
        // lookups hash), even when an identical entry was stored before.
        let manifest = json!({ "env_deps": names, "inputs": inputs });
        if dir.join("entry.json").exists() {
            return util::write_json_fast(&self.manifest(), &manifest).map_err(|e| e.to_string());
        }
        let candidates = [format!("lib{}.rlib", self.stem), format!("lib{}.rmeta", self.stem), format!("lib{}.so", self.stem), format!("lib{}.dylib", self.stem), format!("{}.dll", self.stem), self.stem.clone(), format!("{}.exe", self.stem), format!("{}.d", self.stem)];
        let tmp = self.root.join("tmp").join(format!("{}-{}", key2, std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        util::create_private_dir(&tmp).map_err(|e| e.to_string())?;
        let (mut files, mut executable, mut hashes) = (Vec::new(), Vec::new(), serde_json::Map::new());
        for name in candidates {
            let p = self.out_dir.join(&name);
            let Ok(md) = fs::metadata(&p) else { continue };
            // Only what this compilation wrote; older files of the same name are not its output.
            if !md.is_file() || util::mtime_ns(&md) + 1_000_000_000 < start_wall_ns {
                continue;
            }
            let dst = tmp.join(&name);
            if name.ends_with(".d") || fs::hard_link(&p, &dst).is_err() {
                fs::copy(&p, &dst).map_err(|e| e.to_string())?;
            }
            // What the entry holds, hashed from the entry itself. An output dependents link
            // against is hashed through the memo, and so is the target directory's file, for
            // their lookups; when the two are one linked file, the second is a memo hit.
            let hash = if is_linkable(&name) {
                let h = content_hash(&self.root, &dst).map_err(|e| e.to_string())?;
                content_hash(&self.root, &p).map_err(|e| e.to_string())?;
                h
            } else {
                util::blake3_file(&dst).map_err(|e| e.to_string())?
            };
            hashes.insert(name.clone(), json!(hash));
            if has_execute_permission(&md) {
                executable.push(name.clone());
            }
            files.push(name);
        }
        if !files.iter().any(|f| !f.ends_with(".d")) {
            let _ = fs::remove_dir_all(&tmp);
            return Err("the compilation left no output to cache".into());
        }
        fs::write(tmp.join("stderr"), stderr).map_err(|e| e.to_string())?;
        let entry = json!({ "files": files, "executable": executable, "hashes": hashes, "out_dir": util::path_str(&self.out_dir), "out_dir_env": self.out_dir_env, "env_deps": names, "inputs": inputs, "stored_at": util::iso8601(util::now_wall_ns()) });
        util::write_json_fast(&tmp.join("entry.json"), &entry).map_err(|e| e.to_string())?;
        fs::create_dir_all(self.root.join("e")).map_err(|e| e.to_string())?;
        // Another build may have stored the same entry meanwhile; either copy is the same.
        if fs::rename(&tmp, &dir).is_err() {
            let _ = fs::remove_dir_all(&tmp);
        }
        util::write_json_fast(&self.manifest(), &manifest).map_err(|e| e.to_string())
    }
}

/// Whether a file carries execute permission, which a restored copy must get back. Windows has
/// no such bit: executables are recognised by their extension there.
fn has_execute_permission(md: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        false
    }
}

/// Removes an entry. It is renamed away first, so a build looking it up concurrently finds it
/// whole or not at all. Returns whether it was removed.
fn discard(root: &Path, dir: &Path) -> bool {
    let doomed = root.join("tmp").join(format!("discard-{}-{}", std::process::id(), dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    if fs::create_dir_all(root.join("tmp")).and_then(|_| fs::rename(dir, &doomed)).is_ok() {
        let _ = fs::remove_dir_all(&doomed);
        return true;
    }
    false
}

/// Keeps the cache below `max_bytes` by removing the least recently used entries.
/// Returns (bytes before, bytes removed). Safe while builds run: an entry is renamed away
/// before it is deleted, and a build that finds it gone simply compiles.
pub fn evict(root: &Path, max_bytes: u64) -> (u64, u64) {
    let mut entries: Vec<(u64, u64, PathBuf)> = Vec::new();
    for e in fs::read_dir(root.join("e")).into_iter().flatten().flatten() {
        let dir = e.path();
        let used = fs::metadata(dir.join("entry.json")).map(|m| util::mtime_ns(&m)).unwrap_or(0);
        let bytes: u64 = fs::read_dir(&dir).into_iter().flatten().flatten().filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum();
        entries.push((used, bytes, dir));
    }
    let total: u64 = entries.iter().map(|e| e.1).sum();
    let mut over = total.saturating_sub(max_bytes);
    let mut removed = 0;
    entries.sort();
    for (_, bytes, dir) in entries {
        if over == 0 {
            break;
        }
        if discard(root, &dir) {
            removed += bytes;
            over = over.saturating_sub(bytes);
        }
    }
    // Remembered hashes and quick-unit markers are a few bytes each; those not refreshed for
    // two weeks name files or units long gone, and a live one is simply learned again.
    let cutoff = util::now_wall_ns().saturating_sub(14 * 86_400 * 1_000_000_000);
    for e in ["h", "q"].iter().flat_map(|d| fs::read_dir(root.join(d)).into_iter().flatten().flatten()) {
        if e.metadata().map(|m| util::mtime_ns(&m) < cutoff).unwrap_or(false) {
            let _ = fs::remove_file(e.path());
        }
    }
    (total, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembered_hashes_follow_the_file_content() {
        let root = std::env::temp_dir().join(format!("haul-memo-{}", util::new_id()));
        fs::create_dir_all(&root).unwrap();
        let f = root.join("libdep.rlib");
        fs::write(&f, b"first").unwrap();
        assert_eq!(content_hash(&root, &f).unwrap(), blake3::hash(b"first").to_hex().as_str());
        // Remembered: a planted wrong value for this exact file identity is what comes back,
        // which shows the file is not read again.
        remember_hash(&root, &f, &"0".repeat(64)).unwrap();
        assert_eq!(content_hash(&root, &f).unwrap(), "0".repeat(64));
        // A rewrite changes the identity: the new content is hashed.
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&f, b"second, longer").unwrap();
        assert_eq!(content_hash(&root, &f).unwrap(), blake3::hash(b"second, longer").to_hex().as_str());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn eviction_removes_least_recently_used_entries_first() {
        let root = std::env::temp_dir().join(format!("haul-evict-{}", util::new_id()));
        for (name, age_s) in [("old", 300), ("mid", 200), ("new", 100)] {
            let d = root.join("e").join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("lib.rlib"), vec![0u8; 1000]).unwrap();
            fs::write(d.join("entry.json"), "{}").unwrap();
            let t = std::time::SystemTime::now() - std::time::Duration::from_secs(age_s);
            fs::File::options().write(true).open(d.join("entry.json")).unwrap().set_modified(t).unwrap();
        }
        let (total, removed) = evict(&root, 2100);
        assert!(total >= 3000 && removed >= 1000);
        assert!(!root.join("e/old").exists() && root.join("e/mid").exists() && root.join("e/new").exists());
        assert_eq!(evict(&root, u64::MAX).1, 0);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rewrite_replaces_the_cached_builds_paths() {
        let pairs = vec![("/a/target/debug/deps".to_string(), "/b/t/debug/deps".to_string()), (String::new(), "/ignored".to_string())];
        let out = rewrite(b"{\"artifact\":\"/a/target/debug/deps/libx.rmeta\"}\n", &pairs);
        assert_eq!(String::from_utf8(out).unwrap(), "{\"artifact\":\"/b/t/debug/deps/libx.rmeta\"}\n");
    }
}
