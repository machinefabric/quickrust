//! Build-directory inventory and garbage-collection planning (FR-110).
//!
//! The scan streams directory entries and keeps only per-category and
//! per-group aggregates, never one record per file (NFR-005, NFR-007).
//! Symlinks are never followed (FR-110.4, SEC-018). Planning is read-only:
//! this release computes what could be reclaimed and deletes nothing (FR-110.6).

use crate::util::{self, path_str};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const CATEGORIES: &[&str] = &[
    "final", "dependency", "metadata", "incremental", "debug", "build-script-binary", "build-script-output", "proc-macro", "fingerprint", "documentation", "package-output", "compiler-cache",
    "cargo-registry-cache", "cargo-git-cache", "container-cache", "unknown-generated",
];

/// What Haul knows about build units from observed runs; sharpens classification and liveness.
#[derive(Default)]
pub struct Knowledge {
    /// Artifact stem (`name-hash`) → wall-clock time the unit was last built or linked against.
    pub last_used_ns: HashMap<String, u64>,
    pub proc_macro_stems: HashSet<String>,
    /// Artifact stem → last observed compile duration.
    pub cost_ns: HashMap<String, u64>,
}

pub struct Options {
    pub stale_days: u64,
    pub now_ns: u64,
    pub size_budget: Option<u64>,
}

#[derive(Default, Clone, Copy)]
struct Tally {
    files: u64,
    logical: u64,
    allocated: u64,
}

impl Tally {
    fn add(&mut self, logical: u64, allocated: u64) {
        self.files += 1;
        self.logical += logical;
        self.allocated += allocated;
    }
    fn json(&self) -> Value {
        json!({ "files": self.files, "logical_bytes": self.logical, "allocated_bytes": self.allocated })
    }
}

#[derive(Default)]
struct Group {
    tally: Tally,
    newest_mtime_ns: u64,
}

/// Every name of one hard-linked inode: (relative path components, logical bytes, allocated bytes, mtime).
type LinkedNames = Vec<(Vec<String>, u64, u64, u64)>;

const LAYOUT_DIRS: &[&str] = &["deps", "build", "incremental", ".fingerprint", "examples"];

/// Unit stem (`name-hash`) of an artifact file name in `deps/`.
pub fn artifact_stem(file_name: &str) -> &str {
    let base = file_name.split('.').next().unwrap_or(file_name);
    base.strip_prefix("lib").unwrap_or(base)
}

/// Category and aggregation group of one file, from its path relative to the scan root.
pub fn classify(rel: &[&str], known: &Knowledge) -> (&'static str, String) {
    let name = rel.last().copied().unwrap_or("");
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if rel.iter().any(|c| c.ends_with(".dSYM")) {
        return ("debug", rel[..rel.len().min(3)].join("/"));
    }
    if let Some(i) = rel.iter().take(3).position(|c| LAYOUT_DIRS.contains(c)) {
        let prefix = rel[..=i].join("/");
        let sub = rel.get(i + 1).copied().unwrap_or("");
        return match rel[i] {
            "incremental" => ("incremental", format!("{}/{}", prefix, sub)),
            ".fingerprint" => ("fingerprint", format!("unit:{}", sub)),
            "examples" => (if ext == "d" { "fingerprint" } else { "final" }, prefix),
            "build" => {
                let group = format!("{}/{}", prefix, sub);
                if rel.get(i + 2) == Some(&"out") {
                    ("build-script-output", group)
                } else if name.starts_with("build_script_") || name.starts_with("build-script-") {
                    ("build-script-binary", group)
                } else {
                    ("build-script-output", group)
                }
            }
            _ => {
                let stem = artifact_stem(name);
                let group = format!("unit:{}", stem);
                let cat = match ext {
                    "rmeta" => "metadata",
                    "rlib" | "a" | "lib" => "dependency",
                    "d" => "fingerprint",
                    "o" | "dwo" | "dwp" | "pdb" => "debug",
                    "dylib" | "so" | "dll" => {
                        if known.proc_macro_stems.contains(stem) {
                            "proc-macro"
                        } else {
                            "dependency"
                        }
                    }
                    "" | "exe" | "wasm" => "final",
                    _ => "unknown-generated",
                };
                (cat, group)
            }
        };
    }
    if rel.first() == Some(&"doc") || rel.get(1) == Some(&"doc") {
        return ("documentation", "doc".into());
    }
    if rel.first() == Some(&"package") || rel.get(1) == Some(&"package") {
        return ("package-output", "package".into());
    }
    if matches!(name, ".rustc_info.json" | "CACHEDIR.TAG" | ".cargo-lock" | ".cargo-build-lock") {
        return ("fingerprint", "cargo-bookkeeping".into());
    }
    // Uplifted outputs sit directly in `<profile>/` or `<triple>/<profile>/`.
    if rel.len() == 2 || rel.len() == 3 {
        return (if ext == "d" { "fingerprint" } else { "final" }, rel[..rel.len() - 1].join("/"));
    }
    ("unknown-generated", rel[..rel.len().min(2)].join("/"))
}

fn allocated_bytes(md: &fs::Metadata) -> u64 {
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

fn hardlink_key(md: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (md.nlink() > 1).then(|| (md.dev(), md.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        None
    }
}

/// Whether a running Cargo holds the build lock in `dir` (evidence of an active build).
fn cargo_lock_held(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        for name in [".cargo-lock", ".cargo-build-lock"] {
            if let Ok(f) = fs::File::open(dir.join(name)) {
                let fd = f.as_raw_fd();
                if unsafe { libc::flock(fd, libc::LOCK_SH | libc::LOCK_NB) } != 0 {
                    return true;
                }
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                }
            }
        }
        false
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

fn reclaimability(category: &str, group: &str, newest_mtime_ns: u64, known: &Knowledge, cutoff_ns: u64) -> (&'static str, &'static str) {
    match category {
        "final" => ("required", "build output the user asked for"),
        "unknown-generated" => ("uncertain", "not recognised as Cargo output; never a deletion candidate"),
        "documentation" | "package-output" | "incremental" => {
            if newest_mtime_ns < cutoff_ns {
                ("rebuildable", "regenerated on demand and not modified within the staleness window")
            } else {
                ("reusable", "recently produced")
            }
        }
        "compiler-cache" | "cargo-registry-cache" | "cargo-git-cache" | "container-cache" => ("reusable", "shared cache managed by its own tool"),
        _ => {
            let used = group.strip_prefix("unit:").and_then(|s| known.last_used_ns.get(s)).copied();
            match used {
                Some(t) if t >= cutoff_ns => ("reusable", "unit used by a recent recorded build"),
                Some(_) if newest_mtime_ns < cutoff_ns => ("rebuildable", "unit not used by any recorded build within the staleness window"),
                None if newest_mtime_ns < cutoff_ns => ("rebuildable", "not referenced by any recorded build and older than the staleness window"),
                _ => ("reusable", "recently produced"),
            }
        }
    }
}

/// Adds one file to its category and group.
#[allow(clippy::too_many_arguments)]
fn account(rel: &[&str], logical: u64, allocated: u64, mtime_ns: u64, known: &Knowledge, cats: &mut BTreeMap<&'static str, Tally>, total: &mut Tally, groups: &mut HashMap<(&'static str, String), Group>) {
    // Haul's own benchmark/experiment directories are complete target directories one level down.
    let inner = if matches!(rel.first(), Some(&"haul-bench") | Some(&"haul-exp")) && rel.len() > 2 { &rel[2..] } else { rel };
    let (cat, group) = classify(inner, known);
    let group = if inner.len() != rel.len() { format!("{}/{}/{}", rel[0], rel[1], group) } else { group };
    cats.entry(cat).or_default().add(logical, allocated);
    total.add(logical, allocated);
    let g = groups.entry((cat, group)).or_default();
    g.tally.add(logical, allocated);
    g.newest_mtime_ns = g.newest_mtime_ns.max(mtime_ns);
}

/// Scans one build/target directory.
pub fn scan(root: &Path, known: &Knowledge, opts: &Options) -> Value {
    let mut cats: BTreeMap<&'static str, Tally> = BTreeMap::new();
    let mut groups: HashMap<(&'static str, String), Group> = HashMap::new();
    let mut linked: HashMap<(u64, u64), LinkedNames> = HashMap::new();
    let mut total = Tally::default();
    let mut hardlink_dup = 0u64;
    let mut symlinks = 0u64;
    let mut errors: Vec<String> = Vec::new();
    let mut error_count = 0u64;
    let mut lock_dirs: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let note_err = |what: String, errors: &mut Vec<String>, count: &mut u64| {
        *count += 1;
        if errors.len() < 20 {
            errors.push(what);
        }
    };
    while let Some(dir) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                note_err(format!("{}: {}", dir.display(), e), &mut errors, &mut error_count);
                continue;
            }
        };
        for entry in rd {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    note_err(format!("{}: {}", dir.display(), e), &mut errors, &mut error_count);
                    continue;
                }
            };
            let path = entry.path();
            // symlink_metadata: a link is an entry of its own and is never traversed.
            let md = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    note_err(format!("{}: {}", path.display(), e), &mut errors, &mut error_count);
                    continue;
                }
            };
            if md.is_dir() {
                stack.push(path);
                continue;
            }
            let rel_path = path.strip_prefix(root).unwrap_or(&path);
            let rel_owned: Vec<String> = rel_path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            let rel: Vec<&str> = rel_owned.iter().map(String::as_str).collect();
            let (logical, allocated) = (md.len(), allocated_bytes(&md));
            if md.file_type().is_symlink() {
                symlinks += 1;
                cats.entry("unknown-generated").or_default().add(logical, allocated);
                total.add(logical, allocated);
                groups.entry(("unknown-generated", "symlinks".into())).or_default().tally.add(logical, allocated);
                continue;
            }
            if matches!(rel.last(), Some(&".cargo-lock") | Some(&".cargo-build-lock")) {
                if let Some(p) = path.parent() {
                    if !lock_dirs.iter().any(|d| d == p) {
                        lock_dirs.push(p.to_path_buf());
                    }
                }
            }
            if let Some(key) = hardlink_key(&md) {
                // Decided after the walk, once every name of the inode is known.
                linked.entry(key).or_default().push((rel_owned.clone(), logical, allocated, util::mtime_ns(&md)));
                continue;
            }
            account(&rel, logical, allocated, util::mtime_ns(&md), known, &mut cats, &mut total, &mut groups);
        }
    }
    // Cargo uplifts outputs from `deps/` by hard link. One inode is counted once (FR-110.3), under
    // the name that identifies its build unit, whichever name the walk happened to reach first.
    for (_, mut names) in linked {
        let pick = names.iter().position(|n| n.0.iter().any(|c| c == "deps")).unwrap_or(0);
        let (rel_owned, logical, allocated, mtime) = names.swap_remove(pick);
        hardlink_dup += names.iter().map(|n| n.1).sum::<u64>();
        let rel: Vec<&str> = rel_owned.iter().map(String::as_str).collect();
        account(&rel, logical, allocated, mtime, known, &mut cats, &mut total, &mut groups);
    }

    let cutoff = opts.now_ns.saturating_sub(opts.stale_days * 86_400 * 1_000_000_000);
    let cargo_active = lock_dirs.iter().any(|d| cargo_lock_held(d));
    // A session's build directory can be redirected by its environment, so every other
    // live Haul session is treated as a possible owner of this one (NFR-026).
    let haul_sessions: Vec<Value> = crate::store::active_sessions().into_iter().filter(|s| s["pid"].as_u64() != Some(std::process::id() as u64)).collect();
    let active = cargo_active || !haul_sessions.is_empty();

    let mut by_reclaim: BTreeMap<&'static str, Tally> = BTreeMap::new();
    let mut candidates: Vec<Value> = Vec::new();
    let mut candidate_bytes = 0u64;
    let (mut regen_ns, mut regen_known, mut regen_unknown) = (0u64, 0u64, 0u64);
    let mut counted_units: HashSet<String> = HashSet::new();
    for ((cat, group), g) in &groups {
        let (class, reason) = reclaimability(cat, group, g.newest_mtime_ns, known, cutoff);
        let t = by_reclaim.entry(class).or_default();
        t.files += g.tally.files;
        t.logical += g.tally.logical;
        t.allocated += g.tally.allocated;
        if class != "rebuildable" {
            continue;
        }
        candidate_bytes += g.tally.allocated;
        let stem = group.strip_prefix("unit:");
        if let Some(stem) = stem {
            if counted_units.insert(stem.to_string()) {
                match known.cost_ns.get(stem) {
                    Some(ns) => {
                        regen_ns += ns;
                        regen_known += 1;
                    }
                    None => regen_unknown += 1,
                }
            }
        }
        candidates.push(json!({
            "category": cat,
            "group": group,
            "files": g.tally.files,
            "logical_bytes": g.tally.logical,
            "allocated_bytes": g.tally.allocated,
            "age_days": opts.now_ns.saturating_sub(g.newest_mtime_ns) / (86_400 * 1_000_000_000),
            "reason": reason,
            "regeneration_cost_ns": stem.and_then(|s| known.cost_ns.get(s)),
        }));
    }
    candidates.sort_by(|a, b| b["allocated_bytes"].as_u64().cmp(&a["allocated_bytes"].as_u64()).then_with(|| a["group"].as_str().cmp(&b["group"].as_str())));
    let candidate_groups = candidates.len();
    candidates.truncate(200);

    let mut categories = serde_json::Map::new();
    for c in CATEGORIES {
        if let Some(t) = cats.get(c) {
            categories.insert((*c).into(), t.json());
        }
    }
    let reclaimable_now = if active { 0 } else { candidate_bytes };
    json!({
        "root": path_str(root),
        "scanned_at": util::iso8601(opts.now_ns),
        "complete": error_count == 0,
        "errors": { "count": error_count, "samples": errors },
        "files": total.files,
        "logical_bytes": total.logical,
        "allocated_bytes": total.allocated,
        "hardlink_duplicate_bytes_excluded": hardlink_dup,
        "symlinks_not_followed": symlinks,
        "categories": categories,
        "reclaimability": by_reclaim.iter().map(|(k, t)| ((*k).to_string(), t.json())).collect::<serde_json::Map<_, _>>(),
        "active": {
            "cargo_lock_held": cargo_active,
            "haul_sessions": haul_sessions.iter().map(|s| s["session"].clone()).collect::<Vec<_>>(),
        },
        "gc_plan": {
            "mode": "dry-run",
            "stale_days": opts.stale_days,
            "candidate_groups": candidate_groups,
            "candidate_bytes": candidate_bytes,
            "reclaimable_now_bytes": reclaimable_now,
            "blocked_by": if active { json!("an active build owns this directory; nothing may be removed while it runs") } else { Value::Null },
            "regeneration_estimate": {
                "kind": if regen_known > 0 { "measured" } else { "unknown" },
                "compile_time_ns": if regen_known > 0 { json!(regen_ns) } else { Value::Null },
                "units_with_known_cost": regen_known,
                "units_with_unknown_cost": regen_unknown,
            },
            "size_budget": opts.size_budget.map(|b| json!({
                "target_bytes": b,
                "current_bytes": total.allocated,
                "after_plan_bytes": total.allocated.saturating_sub(candidate_bytes),
                "met_by_plan": total.allocated.saturating_sub(candidate_bytes) <= b,
            })),
            "candidates": candidates,
            "candidates_truncated": candidate_groups > 200,
        },
        "notes": [
            "copy-on-write clones (reflinks) cannot be detected portably; cloned files are counted at full size",
        ],
    })
}

/// Total size of a shared cache directory, reported under a single category.
pub fn scan_flat(root: &Path, category: &'static str) -> Option<Value> {
    if !root.is_dir() {
        return None;
    }
    let mut t = Tally::default();
    let mut errors = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            errors += 1;
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            match fs::symlink_metadata(e.path()) {
                Ok(md) if md.is_dir() => stack.push(e.path()),
                Ok(md) => t.add(md.len(), allocated_bytes(&md)),
                Err(_) => errors += 1,
            }
        }
    }
    let mut v = t.json();
    v["category"] = json!(category);
    v["root"] = json!(path_str(root));
    v["complete"] = json!(errors == 0);
    Some(v)
}

/// Shared caches outside the workspace: sccache, Cargo registry and git checkouts.
pub fn scan_global_caches() -> Vec<Value> {
    let mut out = Vec::new();
    let sccache = std::env::var_os("SCCACHE_DIR").map(PathBuf::from).or_else(|| {
        util::home_dir().map(|h| if cfg!(target_os = "macos") { h.join("Library/Caches/Mozilla.sccache") } else { h.join(".cache/sccache") })
    });
    if let Some(v) = sccache.and_then(|d| scan_flat(&d, "compiler-cache")) {
        out.push(v);
    }
    if let Some(home) = util::cargo_home() {
        out.extend(scan_flat(&home.join("registry"), "cargo-registry-cache"));
        out.extend(scan_flat(&home.join("git"), "cargo-git-cache"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(path: &str) -> &'static str {
        let parts: Vec<&str> = path.split('/').collect();
        classify(&parts, &Knowledge::default()).0
    }

    #[test]
    fn classifies_cargo_layout() {
        assert_eq!(cat("debug/deps/libfoo-0123.rmeta"), "metadata");
        assert_eq!(cat("debug/deps/libfoo-0123.rlib"), "dependency");
        assert_eq!(cat("debug/deps/foo-0123.d"), "fingerprint");
        assert_eq!(cat("debug/deps/foo-0123.abc.rcgu.o"), "debug");
        assert_eq!(cat("debug/deps/foo-0123"), "final");
        assert_eq!(cat("debug/incremental/foo-xyz/s-1/dep-graph.bin"), "incremental");
        assert_eq!(cat("debug/build/foo-0123/out/gen.rs"), "build-script-output");
        assert_eq!(cat("debug/build/foo-0456/build_script_build-0456"), "build-script-binary");
        assert_eq!(cat("debug/.fingerprint/foo-0123/lib-foo.json"), "fingerprint");
        assert_eq!(cat("x86_64-unknown-linux-gnu/release/deps/libfoo-1.rlib"), "dependency");
        assert_eq!(cat("debug/app"), "final");
        assert_eq!(cat("debug/app.dSYM/Contents/Resources/DWARF/app"), "debug");
        assert_eq!(cat("doc/foo/index.html"), "documentation");
        assert_eq!(cat("package/foo-0.1.0.crate"), "package-output");
        assert_eq!(cat("some/unrelated/deep/tree/file.bin"), "unknown-generated");
    }

    #[test]
    fn proc_macro_identity_comes_from_observed_units() {
        let mut k = Knowledge::default();
        assert_eq!(classify(&["debug", "deps", "libmac-9.dylib"], &k).0, "dependency");
        k.proc_macro_stems.insert("mac-9".into());
        assert_eq!(classify(&["debug", "deps", "libmac-9.dylib"], &k).0, "proc-macro");
    }

    fn touch_old(p: &Path, days: u64) {
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
        fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn plan_is_conservative_and_never_follows_symlinks() {
        let base = std::env::temp_dir().join(format!("haul-inv-{}", util::new_id()));
        let root = base.join("target");
        let outside = base.join("outside");
        fs::create_dir_all(root.join("debug/deps")).unwrap();
        fs::create_dir_all(root.join("debug/odd/x/y")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious"), vec![0u8; 4096]).unwrap();
        fs::write(root.join("debug/deps/libold-1.rlib"), vec![0u8; 10_000]).unwrap();
        fs::write(root.join("debug/deps/libnew-2.rlib"), vec![0u8; 20_000]).unwrap();
        fs::write(root.join("debug/deps/liblive-3.rlib"), vec![0u8; 30_000]).unwrap();
        fs::write(root.join("debug/app"), vec![0u8; 5_000]).unwrap();
        fs::write(root.join("debug/odd/x/y/mystery"), vec![0u8; 7_000]).unwrap();
        touch_old(&root.join("debug/deps/libold-1.rlib"), 90);
        touch_old(&root.join("debug/deps/liblive-3.rlib"), 90);
        touch_old(&root.join("debug/odd/x/y/mystery"), 90);
        touch_old(&root.join("debug/app"), 90);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("debug/escape")).unwrap();

        let now = util::now_wall_ns();
        let mut known = Knowledge::default();
        known.last_used_ns.insert("live-3".into(), now);
        let v = scan(&root, &known, &Options { stale_days: 30, now_ns: now, size_budget: None });

        let groups: Vec<&str> = v["gc_plan"]["candidates"].as_array().unwrap().iter().map(|c| c["group"].as_str().unwrap()).collect();
        assert_eq!(groups, vec!["unit:old-1"], "only the stale, unreferenced unit is a candidate");
        assert_eq!(v["reclaimability"]["required"]["logical_bytes"], 5_000);
        assert!(v["reclaimability"]["uncertain"]["logical_bytes"].as_u64().unwrap() >= 7_000);
        assert_eq!(v["reclaimability"]["reusable"]["logical_bytes"], 50_000);
        // The symlink target's 4096 bytes must not be counted or traversed.
        assert!(v["logical_bytes"].as_u64().unwrap() < 72_000 + 1024);
        #[cfg(unix)]
        assert_eq!(v["symlinks_not_followed"], 1);
        assert!(outside.join("precious").exists());
        assert_eq!(v["gc_plan"]["mode"], "dry-run");
        fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_are_counted_once() {
        let root = std::env::temp_dir().join(format!("haul-hl-{}", util::new_id()));
        fs::create_dir_all(root.join("debug/deps")).unwrap();
        fs::write(root.join("debug/deps/app-1"), vec![0u8; 8_000]).unwrap();
        fs::hard_link(root.join("debug/deps/app-1"), root.join("debug/app")).unwrap();
        let v = scan(&root, &Knowledge::default(), &Options { stale_days: 30, now_ns: util::now_wall_ns(), size_budget: None });
        assert_eq!(v["logical_bytes"], 8_000);
        assert_eq!(v["hardlink_duplicate_bytes_excluded"], 8_000);
        fs::remove_dir_all(&root).ok();
    }
}
