//! Haul's global artifact cache (fast mode).
//!
//! The rustc wrapper looks a compilation up before running rustc. On a hit it restores the
//! outputs into Cargo's output directory and replays rustc's stderr, so Cargo sees exactly
//! what a real compilation would have produced, minus the time. This first version caches
//! only units whose sources cannot change under a name (registry and git dependencies) and
//! that read nothing a build script produced outside `OUT_DIR`:
//!
//! - **key, level 1**: cache format version; rustc identity (and Haul's compiler driver, when
//!   it compiles the unit); every argument, with output and search paths removed and each
//!   `--extern` replaced by the identity of the artifact it names; Cargo's `CARGO_*`
//!   variables; the content of `OUT_DIR`. An artifact's identity is its content hash, except
//!   for the metadata a unit that links nothing compiles against when the driver built it:
//!   then it is the crate's interface hash (see `driver/`), which an edit inside a function
//!   body its dependents cannot observe leaves unchanged. Their entries are then hits: the
//!   rebuild stops at the edited crate (early cut-off), up to the units that link. A unit
//!   that links (binary, test, procedural macro) is keyed instead by what it links: each
//!   rlib's content and, recursively, the rlibs it was compiled against, which the linker
//!   finds through the search path without the command line naming them;
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

const FORMAT: &str = "haul-artifact-cache-3";

/// How far past the start of a crate's metadata root its crate hash may lie: the root begins
/// with the target triple (or a whole JSON target specification), then the hash.
const ROOT_HEADER_WINDOW: u64 = 64 * 1024;

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
    /// `foreign_out_dir`: the unit includes code its build script generated, and was built
    /// with this other `OUT_DIR`, which panic locations and debug information in that code name.
    Hit { stderr: Vec<u8>, entry: String, foreign_out_dir: Option<String> },
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

fn link_memo_path(root: &Path, md: &fs::Metadata, path: &Path) -> PathBuf {
    root.join("l").join(util::digest_parts([content_identity(md, path)]))
}

/// Records what the rlib a unit just produced (compiled or restored) was compiled against: the
/// dependencies named on its command line, which `link_identity` follows once every rlib a
/// linking unit needs is complete (a dependency's may still be in code generation when this
/// unit finishes). Nothing to record for a unit that produces no rlib.
pub fn record_link_inputs(root: &Path, args: &[String]) -> Result<(), String> {
    let (mut out_dir, mut name, mut extra, mut deps) = (None, None, "", Vec::new());
    let mut i = 0;
    while i < args.len() {
        if let Some(v) = value_of(args, &mut i, "--out-dir") {
            out_dir = Some(v);
        } else if let Some(v) = value_of(args, &mut i, "--crate-name") {
            name = Some(v);
        } else if let Some(v) = value_of(args, &mut i, "--extern") {
            if let Some((n, p)) = v.split_once('=') {
                deps.push(json!([n, util::path_str(&std::env::current_dir().map_err(|e| e.to_string())?.join(p))]));
            }
        } else if let Some(v) = value_of(args, &mut i, "-C") {
            if let Some(e) = v.strip_prefix("extra-filename=") {
                extra = e;
            }
        }
        i += 1;
    }
    let (Some(out_dir), Some(name)) = (out_dir, name) else { return Ok(()) };
    let rlib = Path::new(out_dir).join(format!("lib{}{}.rlib", name, extra));
    let Ok(md) = fs::metadata(&rlib) else { return Ok(()) };
    fs::create_dir_all(root.join("l")).map_err(|e| e.to_string())?;
    util::write_json_fast(&link_memo_path(root, &md, &rlib), &json!({ "deps": deps })).map_err(|e| e.to_string())
}

/// What a unit that links the artifact at `path` links, by content: an rlib's (named directly or
/// through its metadata) content and, recursively, what the dependencies it was compiled against
/// link; a dynamic library's or procedural macro's own content, which holds what it linked.
/// `None` when an rlib's inputs were never recorded (it was not built through this cache).
fn link_identity(root: &Path, path: &Path, seen: &mut std::collections::HashMap<PathBuf, String>) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let path = match name.strip_suffix(".rmeta") {
        Some(stem) => path.with_file_name(format!("{}.rlib", stem)),
        None => path.to_path_buf(),
    };
    if let Some(id) = seen.get(&path) {
        return Some(id.clone());
    }
    let content = content_hash(root, &path).ok()?;
    let id = if path.extension().is_some_and(|e| e == "rlib") {
        let memo_path = link_memo_path(root, &fs::metadata(&path).ok()?, &path);
        let memo = util::read_json(&memo_path)?;
        // In use: eviction drops records not refreshed for two weeks, and this one cannot be
        // learned again without compiling the rlib anew.
        let day_ago = util::now_wall_ns().saturating_sub(86_400 * 1_000_000_000);
        if fs::metadata(&memo_path).map(|m| util::mtime_ns(&m) < day_ago).unwrap_or(false) {
            let _ = fs::File::options().write(true).open(&memo_path).and_then(|f| f.set_modified(std::time::SystemTime::now()));
        }
        let mut parts = vec![format!("content={}", content)];
        for dep in memo["deps"].as_array()? {
            let (n, p) = (dep[0].as_str()?, dep[1].as_str()?);
            parts.push(format!("{}={}", n, link_identity(root, Path::new(p), seen)?));
        }
        util::digest_parts(parts)
    } else {
        content
    };
    seen.insert(path, id.clone());
    Some(id)
}

/// Outputs another unit can name in `--extern`, whose hashes are kept with a cache entry.
fn is_linkable(name: &str) -> bool {
    [".rlib", ".rmeta", ".so", ".dylib", ".dll"].iter().any(|e| name.ends_with(e))
}

/// What decides whether rustc's MIR inliner runs, which copies other crates' function bodies
/// into this one's MIR (and so into its metadata).
#[derive(Default)]
struct MirInlining {
    opt_level: String,
    mir_opt_level: Option<String>,
    inline_mir: Option<String>,
}

impl MirInlining {
    /// As rustc decides it (`Inline::is_enabled`); a setting it cannot read counts as inlining.
    fn inlines(&self, incremental: bool) -> bool {
        if let Some(v) = &self.inline_mir {
            return !matches!(v.as_str(), "no" | "n" | "off" | "false");
        }
        let opt_level = if self.opt_level.is_empty() { "0" } else { self.opt_level.as_str() };
        let mir_opt_level = match &self.mir_opt_level {
            Some(l) => match l.parse::<u32>() {
                Ok(l) => l,
                Err(_) => return true,
            },
            None if opt_level == "0" => 1,
            None => 2,
        };
        match mir_opt_level {
            0 | 1 => false,
            2 => matches!(opt_level, "2" | "3") && !incremental,
            _ => true,
        }
    }
}

/// The interface hash of the crate whose metadata is at `rmeta`, as Haul's compiler driver
/// recorded it next to the metadata (`lib<crate><extra>.haul-svh`), when that metadata carries
/// it. The metadata is what dependents compile against, so the record counts only if the
/// metadata's own root names the same crate hash: a record left by an earlier compilation, or a
/// metadata file written by another compiler since, is not this crate's interface.
fn interface_identity(rmeta: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let stem = rmeta.file_name()?.to_str()?.strip_suffix(".rmeta")?;
    let svh = u128::from_str_radix(fs::read_to_string(rmeta.with_file_name(format!("{}.haul-svh", stem))).ok()?.trim(), 16).ok()?;
    // `rust\0\0\0` and the format version, then the root's position (64-bit little-endian).
    let mut f = fs::File::open(rmeta).ok()?;
    let mut head = [0u8; 16];
    f.read_exact(&mut head).ok()?;
    if &head[..7] != b"rust\0\0\0" {
        return None;
    }
    let root = u64::from_le_bytes(head[8..].try_into().ok()?);
    f.seek(SeekFrom::Start(root)).ok()?;
    let mut window = Vec::new();
    f.take(ROOT_HEADER_WINDOW).read_to_end(&mut window).ok()?;
    // The crate hash is encoded as its sixteen little-endian bytes.
    let hash = svh.to_le_bytes();
    window.windows(hash.len()).any(|w| w == hash).then(|| format!("{:032x}", svh))
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
/// `compiler` is the compiler's own account of itself (`rustc -vV`, see `rustc_info`);
/// `driver`, Haul's compiler driver when it compiles this unit instead of rustc.
pub fn prepare(root: Option<&Path>, compiler: &str, driver: Option<&Path>, args: &[String]) -> Result<Job, String> {
    let root = root.ok_or("the artifact cache is off")?.to_path_buf();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| "not a Cargo unit")?;
    let local = !crate::wrapper::is_external_source(Path::new(&manifest_dir));
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut key = vec![FORMAT.to_string(), format!("rustc={}", util::sha256_hex(compiler.as_bytes()))];
    if let Some(d) = driver {
        // The driver's directory names the compiler and the driver's own source.
        key.push(format!("driver={}", d.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    }
    if local {
        // Workspace units carry the checkout's absolute path in their debug information: they are
        // reused within the same checkout (after a clean, across branch switches, in CI with a
        // fixed path), never into another one.
        key.push(format!("workspace={}", util::path_str(&cwd)));
    }
    let mut test = false;
    let mut scheduled_by_cargo = false;
    let mut incremental = false;
    let (mut crate_name, mut out_dir, mut extra, mut types, mut externs) = (None, None, String::new(), Vec::new(), Vec::new());
    let mut mir = MirInlining::default();
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
            // Keyed below, once it is known whether this unit links.
            externs.push(v.to_string());
        } else if let Some(v) = value_of(args, &mut i, "-L") {
            // Where to look for dependencies is not what is compiled; native search paths are.
            if !v.starts_with("dependency=") {
                return Err("links against native libraries".into());
            }
        } else if value_of(args, &mut i, "-l").is_some() || a.starts_with("--print") {
            return Err("links native libraries or prints compiler information".into());
        } else if let Some(v) = value_of(args, &mut i, "-C") {
            if v.starts_with("incremental") {
                incremental = true;
                // rustc revalidates its incremental state against the inputs on every compilation,
                // so outputs may be restored without it; only whether it is on changes the output.
                key.push("-Cincremental".into());
                i += 1;
                continue;
            }
            if let Some(e) = v.strip_prefix("extra-filename=") {
                extra = e.to_string();
            }
            if let Some(level) = v.strip_prefix("opt-level=") {
                mir.opt_level = level.to_string();
            }
            scheduled_by_cargo |= v.starts_with("metadata=");
            key.push(format!("-C{}", v));
        } else if a.starts_with("-Zthreads") {
            // How many threads rustc's frontend uses does not change what it produces.
        } else if let Some(v) = value_of(args, &mut i, "-Z") {
            if let Some(level) = v.strip_prefix("mir-opt-level=") {
                mir.mir_opt_level = Some(level.to_string());
            }
            if let Some(inline) = v.strip_prefix("inline-mir=") {
                mir.inline_mir = Some(inline.to_string());
            }
            key.push(format!("-Z{}", v));
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
    // The artifact each dependency resolves to: identical inputs, identical output. A unit that
    // links compiles the dependencies' code into its output, and the code of everything they
    // were compiled against: it is keyed by what it links.
    let mut seen = std::collections::HashMap::new();
    for v in externs {
        match v.split_once('=') {
            Some((name, path)) if links => match link_identity(&root, Path::new(path), &mut seen) {
                Some(id) => key.push(format!("--extern={}=link:{}", name, id)),
                None => return Err(format!("what dependency {} links was not recorded", path)),
            },
            Some((name, path)) => {
                // Nor may a unit inline their MIR: it would carry spans whose expansions the
                // interface hash identifies by kind, not by number (see `driver/`).
                let interface = if driver.is_some() && !mir.inlines(incremental) { interface_identity(Path::new(path)) } else { None };
                match interface {
                    Some(svh) => key.push(format!("--extern={}=svh:{}", name, svh)),
                    None => key.push(format!("--extern={}=sha:{}", name, content_hash(&root, Path::new(path)).map_err(|e| format!("dependency {} is unreadable: {}", path, e))?)),
                }
            }
            None => key.push(format!("--extern={}", v)),
        }
    }
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

    /// Whether this unit compiled quickly from scratch before and was not stored for it: a small
    /// linked workspace output. It is compiled again after every clean; the parallel frontend
    /// would only add its start-up cost to it.
    pub fn known_quick(&self) -> bool {
        self.quick_marker().exists()
    }

    /// Records that this unit compiled quickly from scratch and was not stored for it.
    pub fn remember_quick(&self) -> std::io::Result<()> {
        fs::create_dir_all(self.root.join("q"))?;
        util::write_atomic_fast(&self.quick_marker(), b"")
    }

    /// Withdraws the record: the unit was just compiled from scratch, not quickly.
    pub fn forget_quick(&self) {
        let _ = fs::remove_file(self.quick_marker());
    }

    /// Keyed by the unit as Cargo names it (its output directory and file stem, which carries
    /// Cargo's unit hash), not by the cache key: a unit stays small across edits of its own
    /// sources and its dependencies.
    fn quick_marker(&self) -> PathBuf {
        self.root.join("q").join(util::digest_parts([util::path_str(&self.out_dir), self.stem.clone()]))
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
                let built_in = util::vstr(&entry, "out_dir_env");
                let reads_out_dir = entry["env_deps"].as_array().into_iter().flatten().any(|n| n == "OUT_DIR");
                let foreign_out_dir = (reads_out_dir && !built_in.is_empty() && Some(&built_in) != self.out_dir_env.as_ref()).then_some(built_in);
                Lookup::Hit { stderr, entry: key2, foreign_out_dir }
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
        // With the interface hash Haul's compiler driver recorded, which dependents key on.
        let candidates = [format!("lib{}.rlib", self.stem), format!("lib{}.rmeta", self.stem), format!("lib{}.haul-svh", self.stem), format!("lib{}.so", self.stem), format!("lib{}.dylib", self.stem), format!("{}.dll", self.stem), self.stem.clone(), format!("{}.exe", self.stem), format!("{}.d", self.stem)];
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
    // Remembered hashes, link inputs and quick-unit markers are a few bytes each; those not refreshed for
    // two weeks name files or units long gone, and a live one is simply learned again.
    let cutoff = util::now_wall_ns().saturating_sub(14 * 86_400 * 1_000_000_000);
    for e in ["h", "l", "q"].iter().flat_map(|d| fs::read_dir(root.join(d)).into_iter().flatten().flatten()) {
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
    fn mir_inlining_is_decided_as_rustc_decides_it() {
        let m = |opt: &str, mir: Option<&str>, inline: Option<&str>| MirInlining { opt_level: opt.into(), mir_opt_level: mir.map(Into::into), inline_mir: inline.map(Into::into) };
        // Debug builds (`opt-level` 0, the default) and size-optimised ones do not inline MIR.
        assert!(!m("", None, None).inlines(false));
        assert!(!m("0", None, None).inlines(false));
        assert!(!m("s", None, None).inlines(false));
        assert!(!m("1", None, None).inlines(false));
        // Optimised builds do, except incremental ones.
        assert!(m("3", None, None).inlines(false));
        assert!(m("2", None, None).inlines(false));
        assert!(!m("3", None, None).inlines(true));
        // Explicit settings win; one that cannot be read counts as inlining.
        assert!(m("0", None, Some("yes")).inlines(true));
        assert!(!m("3", None, Some("no")).inlines(false));
        assert!(m("0", Some("3"), None).inlines(true));
        assert!(!m("3", Some("1"), None).inlines(false));
        assert!(m("0", Some("high"), None).inlines(false));
    }

    #[test]
    fn an_interface_hash_counts_only_when_the_metadata_carries_it() {
        let dir = std::env::temp_dir().join(format!("haul-iface-{}", util::new_id()));
        fs::create_dir_all(&dir).unwrap();
        let svh: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
        // Metadata as rustc lays it out: header, the root's position, …, the root (target triple, crate hash).
        let mut rmeta = b"rust\0\0\0\x0a".to_vec();
        let root = 64u64;
        rmeta.extend_from_slice(&root.to_le_bytes());
        rmeta.resize(root as usize, 0xaa);
        rmeta.extend_from_slice(b"\x1ax86_64-unknown-linux-gnu");
        rmeta.extend_from_slice(&svh.to_le_bytes());
        let path = dir.join("libdep-0123.rmeta");
        fs::write(&path, &rmeta).unwrap();
        let record = dir.join("libdep-0123.haul-svh");
        fs::write(&record, format!("{:032x}\n", svh)).unwrap();
        assert_eq!(interface_identity(&path), Some(format!("{:032x}", svh)));
        // A record left by an earlier compilation, or metadata another compiler wrote since.
        fs::write(&record, format!("{:032x}\n", svh ^ 1)).unwrap();
        assert_eq!(interface_identity(&path), None);
        fs::write(&record, format!("{:032x}\n", svh)).unwrap();
        let mut other = rmeta.clone();
        let at = other.len() - 1;
        other[at] ^= 0xff;
        fs::write(&path, &other).unwrap();
        assert_eq!(interface_identity(&path), None);
        // The hash only counts in the root, not anywhere in the file.
        let mut elsewhere = rmeta[..root as usize].to_vec();
        elsewhere[20..36].copy_from_slice(&svh.to_le_bytes());
        elsewhere.extend_from_slice(b"\x1ax86_64-unknown-linux-gnu");
        fs::write(&path, &elsewhere).unwrap();
        assert_eq!(interface_identity(&path), None);
        // Not metadata at all, or no record.
        fs::write(&path, b"not metadata").unwrap();
        assert_eq!(interface_identity(&path), None);
        fs::write(&path, &rmeta).unwrap();
        fs::remove_file(&record).unwrap();
        assert_eq!(interface_identity(&path), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn what_a_unit_links_follows_every_rlib_below_it() {
        let dir = std::env::temp_dir().join(format!("haul-link-{}", util::new_id()));
        let (root, deps) = (dir.join("cache"), dir.join("deps"));
        fs::create_dir_all(&deps).unwrap();
        let unit = |name: &str, content: &str, externs: &[&str]| {
            fs::write(deps.join(format!("lib{}.rlib", name)), content).unwrap();
            fs::write(deps.join(format!("lib{}.rmeta", name)), content).unwrap();
            let mut args = vec!["--crate-name".to_string(), name.to_string(), "--out-dir".to_string(), util::path_str(&deps)];
            for e in externs {
                args.push("--extern".into());
                args.push(format!("{}={}", e, util::path_str(&deps.join(format!("lib{}.rmeta", e)))));
            }
            record_link_inputs(&root, &args).unwrap();
        };
        let linked = |name: &str| link_identity(&root, &deps.join(format!("lib{}.rmeta", name)), &mut std::collections::HashMap::new());
        unit("base", "base v1", &[]);
        unit("mid", "mid", &["base"]);
        let before = linked("mid").unwrap();
        assert_eq!(linked("mid").unwrap(), before);
        // `base` changes; `mid`'s own output does not (its interface did not change): what links
        // `mid` links the new `base`.
        std::thread::sleep(std::time::Duration::from_millis(5));
        unit("base", "base v2", &[]);
        assert_ne!(linked("mid").unwrap(), before);
        // An rlib whose inputs were never recorded: unknown, never guessed.
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(deps.join("libbase.rlib"), "base v3").unwrap();
        assert_eq!(linked("mid"), None);
        fs::remove_dir_all(&dir).ok();
    }

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
