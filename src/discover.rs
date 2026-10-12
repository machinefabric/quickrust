//! Workspace, toolchain and environment discovery (FR-010). Read-only.
//!
//! Workspace facts come from the toolchain's `cargo metadata --format-version 1`;
//! Haul never resolves dependencies itself and never lets discovery touch the
//! network or the lockfile (`--locked --offline`).

use crate::config::Config;
use crate::resolve;
use crate::util::{self, canon, path_str};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A Command for the Cargo of the toolchain `cwd` uses (or `toolchain`), guarded against
/// recursion.
pub fn cargo_command(cfg: &Config, cwd: &Path, toolchain: Option<&str>) -> Result<Command, String> {
    let args: Vec<std::ffi::OsString> = toolchain.map(|t| format!("+{}", t).into()).into_iter().collect();
    let inv = resolve::classify(&args);
    let engine = crate::engine::for_command(&inv, cwd, cfg)?;
    let mut cmd = engine.command(&args, &inv);
    cmd.current_dir(cwd).env(crate::ENV_EXEC_GUARD, format!("{}:discover", std::process::id())).stdin(Stdio::null());
    Ok(cmd)
}

fn run_capture(mut cmd: Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("could not run {:?}: {}", cmd.get_program(), e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().find(|l| !l.trim().is_empty()).unwrap_or("command failed").trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub struct Metadata {
    pub value: Value,
    /// Whether the dependency graph (`resolve`) is present.
    pub with_deps: bool,
    /// Why the dependency graph is missing, when it is.
    pub deps_unavailable: Option<String>,
}

/// Upstream `cargo metadata`. The full graph is requested without network or
/// lockfile writes; if that is impossible, members-only metadata is returned
/// and the reason is carried along so reports can say what is missing.
pub fn metadata(cfg: &Config, cwd: &Path, toolchain: Option<&str>, manifest_path: Option<&Path>) -> Result<Metadata, String> {
    let base = |extra: &[&str]| -> Result<Command, String> {
        let mut cmd = cargo_command(cfg, cwd, toolchain)?;
        cmd.args(["metadata", "--format-version", "1"]).args(extra);
        if let Some(m) = manifest_path {
            cmd.arg("--manifest-path").arg(m);
        }
        Ok(cmd)
    };
    let parse = |text: String| -> Result<Value, String> {
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("unreadable cargo metadata: {}", e))?;
        if v["version"] != json!(1) {
            return Err(format!("unsupported cargo metadata format version {}", v["version"]));
        }
        Ok(v)
    };
    match run_capture(base(&["--locked", "--offline"])?).and_then(parse) {
        Ok(value) => Ok(Metadata { value, with_deps: true, deps_unavailable: None }),
        Err(full_err) => {
            let value = run_capture(base(&["--no-deps"])?).and_then(parse)?;
            Ok(Metadata { value, with_deps: false, deps_unavailable: Some(format!("dependency graph not available offline with the current lockfile: {}", full_err)) })
        }
    }
}

fn git_dir(root: &Path) -> Option<PathBuf> {
    let mut dir = Some(root);
    while let Some(d) = dir {
        let dot = d.join(".git");
        if dot.is_dir() {
            return Some(dot);
        }
        if dot.is_file() {
            // Linked worktree: `.git` is a file pointing at the real git directory.
            let text = fs::read_to_string(&dot).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            return Some(d.join(target));
        }
        dir = d.parent();
    }
    None
}

/// HEAD revision read straight from the git directory: no process spawn on the build path.
pub fn git_head_fast(root: &Path) -> Option<String> {
    let dir = git_dir(root)?;
    let head = fs::read_to_string(dir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        return Some(head.to_string());
    };
    let common = fs::read_to_string(dir.join("commondir")).ok().map(|c| dir.join(c.trim())).unwrap_or_else(|| dir.clone());
    for base in [&dir, &common] {
        if let Ok(rev) = fs::read_to_string(base.join(reference)) {
            return Some(rev.trim().to_string());
        }
    }
    let packed = fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|l| l.strip_suffix(reference).map(|rev| rev.trim().to_string())).filter(|r| !r.is_empty())
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Revision, dirtiness and repository identity. Without Git the source state is
/// described by a filesystem snapshot digest instead (FR-010.4, ASM-012).
pub fn source_state(root: &Path, skip: &[PathBuf]) -> Value {
    if let Some(rev) = git(root, &["rev-parse", "HEAD"]) {
        let status = git(root, &["status", "--porcelain"]);
        let common = git(root, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        return json!({
            "vcs": "git",
            "revision": rev,
            "dirty": status.as_ref().map(|s| !s.is_empty()),
            "dirty_files": status.as_ref().map(|s| s.lines().count()),
            "branch": git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD"),
            "repository_id": common.map(|c| util::sha256_hex(c.as_bytes())[..16].to_string()),
            "observed": "at analysis time",
        });
    }
    let snap = source_snapshot(root, skip);
    json!({
        "vcs": null,
        "revision": null,
        "dirty": null,
        "snapshot_digest": snap.0,
        "snapshot_files": snap.1,
        "observed": "at analysis time",
        "unavailable": { "revision": "not a git repository", "dirty": "no VCS; compare snapshot_digest between runs instead" },
    })
}

/// Digest over `(relative path, size, mtime)` of the source tree, skipping VCS, build output
/// and the given files or directories.
pub fn source_snapshot(root: &Path, skip: &[PathBuf]) -> (String, u64) {
    let mut entries: Vec<String> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let skip: Vec<PathBuf> = skip.iter().map(|p| canon(p)).collect();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let Ok(md) = fs::symlink_metadata(&p) else { continue };
            let name = e.file_name();
            if md.is_dir() {
                if name == ".git" || name == ".haul" || p.join("CACHEDIR.TAG").exists() || skip.contains(&canon(&p)) {
                    continue;
                }
                stack.push(p);
            } else if md.is_file() {
                if !skip.is_empty() && skip.contains(&canon(&p)) {
                    continue;
                }
                let rel = p.strip_prefix(root).unwrap_or(&p);
                entries.push(format!("{}\t{}\t{}", rel.display(), md.len(), util::mtime_ns(&md)));
            }
        }
    }
    entries.sort();
    let n = entries.len() as u64;
    (util::digest_parts(entries), n)
}

fn field<'a>(verbose: &'a str, key: &str) -> Option<&'a str> {
    verbose.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(": ")))
}

/// Cargo and rustc identity. `rustc_hint` is the compiler path Cargo actually
/// invoked (observed by the wrapper); when present it is the execution truth.
pub fn toolchain_fingerprint(cfg: &Config, cwd: &Path, toolchain: Option<&str>, rustc_hint: Option<&str>) -> Value {
    // Analysing a backlog of runs asks for the same compiler many times; the answer for an
    // observed compiler binary does not change within one process.
    static MEMO: std::sync::Mutex<Vec<(String, Value)>> = std::sync::Mutex::new(Vec::new());
    let Some(hint) = rustc_hint else { return probe_toolchain(cfg, cwd, toolchain, None) };
    let key = format!("{}|{}", hint, toolchain.unwrap_or(""));
    if let Some((_, v)) = MEMO.lock().ok().and_then(|m| m.iter().find(|(k, _)| *k == key).cloned()) {
        return v;
    }
    let v = probe_toolchain(cfg, cwd, toolchain, Some(hint));
    if let Ok(mut m) = MEMO.lock() {
        m.push((key, v.clone()));
    }
    v
}

fn probe_toolchain(cfg: &Config, cwd: &Path, toolchain: Option<&str>, rustc_hint: Option<&str>) -> Value {
    let direct = |program: &Path, args: &[&str]| {
        let mut c = Command::new(program);
        c.args(args).current_dir(cwd).stdin(Stdio::null());
        run_capture(c)
    };
    let hinted = rustc_hint.map(PathBuf::from).filter(|p| p.is_absolute() && util::is_executable(p));
    let (cargo_v, rustc_vv, source) = match &hinted {
        Some(rustc) => {
            let sibling = rustc.parent().and_then(|d| util::exe_candidates(d, "cargo").into_iter().find(|c| util::is_executable(c)));
            let cargo_v = match sibling {
                Some(c) => direct(&c, &["-V"]),
                None => cargo_command(cfg, cwd, toolchain).and_then(|mut c| {
                    c.arg("-V");
                    run_capture(c)
                }),
            };
            (cargo_v, direct(rustc, &["-vV"]), "rustc path observed by the wrapper")
        }
        None => {
            let args: Vec<std::ffi::OsString> = toolchain.map(|t| format!("+{}", t).into()).into_iter().collect();
            match crate::engine::for_command(&resolve::classify(&args), cwd, cfg) {
                Ok(engine) => (direct(&engine.cargo(), &["-V"]), direct(&engine.rustc(), &["-vV"]), "toolchain selected for the working directory at analysis time"),
                Err(e) => (Err(e.clone()), Err(e), "toolchain selected for the working directory at analysis time"),
            }
        }
    };
    let verbose = rustc_vv.as_deref().unwrap_or("");
    let cargo_version = cargo_v.as_ref().ok().map(|s| s.trim().to_string());
    let nightly = cargo_version.as_deref().map(|v| v.contains("nightly")).unwrap_or(false);
    let mut capabilities = Vec::new();
    if cargo_version.is_some() {
        capabilities.extend(["cargo_metadata_v1", "cargo_json_messages", "rustc_wrapper", "rustc_workspace_wrapper"]);
    }
    if nightly {
        // Recorded as available only; no P0 behaviour depends on unstable interfaces (OBJ-033, CFG-004).
        capabilities.extend(["nightly_unit_graph", "nightly_build_analysis"]);
    }
    let mut unavailable = serde_json::Map::new();
    if let Err(e) = &cargo_v {
        unavailable.insert("cargo_version".into(), json!(e));
    }
    if let Err(e) = &rustc_vv {
        unavailable.insert("rustc_version".into(), json!(e));
    }
    json!({
        "cargo_version": cargo_version,
        "rustc_version": verbose.lines().next().filter(|l| !l.is_empty()),
        "rustc_release": field(verbose, "release"),
        "rustc_commit": field(verbose, "commit-hash"),
        "llvm_version": field(verbose, "LLVM version"),
        "host": field(verbose, "host"),
        "channel": if nightly { "nightly" } else { "stable-or-beta" },
        "capabilities": capabilities,
        "source": source,
        "unavailable": unavailable,
    })
}

#[cfg(target_os = "macos")]
fn sysctl(name: &str) -> Option<Vec<u8>> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut len: libc::size_t = 0;
    unsafe {
        if libc::sysctlbyname(cname.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        if libc::sysctlbyname(cname.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        buf.truncate(len);
        Some(buf)
    }
}

fn cpu_and_memory() -> (Option<String>, Option<u64>) {
    #[cfg(target_os = "macos")]
    {
        let cpu = sysctl("machdep.cpu.brand_string").map(|b| String::from_utf8_lossy(&b).trim_end_matches('\0').to_string());
        let mem = sysctl("hw.memsize").filter(|b| b.len() == 8).map(|b| u64::from_ne_bytes(b[..8].try_into().unwrap_or([0; 8])));
        (cpu, mem)
    }
    #[cfg(target_os = "linux")]
    {
        let cpu = fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| t.lines().find(|l| l.starts_with("model name") || l.starts_with("Model")).and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string())));
        let mem = fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| t.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1).and_then(|kb| kb.parse::<u64>().ok())))
            .map(|kb| kb * 1024);
        (cpu, mem)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        (std::env::var("PROCESSOR_IDENTIFIER").ok(), None)
    }
}

/// Filesystem type holding `path`, where the platform exposes it cheaply.
pub fn filesystem_type(path: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let existing = path.ancestors().find(|p| p.exists())?;
        let c = std::ffi::CString::new(existing.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        let name: Vec<u8> = st.f_fstypename.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
        Some(String::from_utf8_lossy(&name).into_owned())
    }
    #[cfg(target_os = "linux")]
    {
        let target = canon(path.ancestors().find(|p| p.exists())?);
        let mounts = fs::read_to_string("/proc/self/mountinfo").ok()?;
        let mut best: Option<(usize, String)> = None;
        for line in mounts.lines() {
            let Some((left, right)) = line.split_once(" - ") else { continue };
            let (Some(mount_point), Some(fstype)) = (left.split_whitespace().nth(4), right.split_whitespace().next()) else { continue };
            if target.starts_with(mount_point) && best.as_ref().map(|(l, _)| mount_point.len() > *l).unwrap_or(true) {
                best = Some((mount_point.len(), fstype.to_string()));
            }
        }
        best.map(|(_, t)| t)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        None
    }
}

/// Machine description used to decide whether two runs are comparable (FR-160.3).
pub fn environment_fingerprint(build_dir: Option<&Path>) -> Value {
    let (cpu, mem) = cpu_and_memory();
    let cores = std::thread::available_parallelism().map(|n| n.get() as u64).ok();
    let fs_type = build_dir.and_then(filesystem_type);
    let class_parts = [
        std::env::consts::OS.to_string(),
        std::env::consts::ARCH.to_string(),
        cpu.clone().unwrap_or_default(),
        cores.map(|c| c.to_string()).unwrap_or_default(),
        mem.map(|m| (m >> 30).to_string()).unwrap_or_default(),
    ];
    let mut unavailable = serde_json::Map::new();
    if cpu.is_none() {
        unavailable.insert("cpu_model".into(), json!("not exposed on this platform"));
    }
    if mem.is_none() {
        unavailable.insert("memory_bytes".into(), json!("not exposed on this platform"));
    }
    if fs_type.is_none() {
        unavailable.insert("filesystem".into(), json!("build directory unknown or filesystem type not exposed"));
    }
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpu_model": cpu,
        "logical_cpus": cores,
        "memory_bytes": mem,
        "filesystem": fs_type,
        "ci": util::is_ci(),
        "class": util::digest_parts(class_parts)[..12].to_string(),
        "unavailable": unavailable,
    })
}

fn file_digest(p: &Path) -> Option<String> {
    util::sha256_file(p).ok().map(|h| format!("sha256:{}", &h[..32]))
}

/// Members, manifests, lockfile and output directories of the workspace (FR-010.1, FR-010.2).
pub fn workspace_snapshot(meta: &Value) -> Value {
    let root = PathBuf::from(meta["workspace_root"].as_str().unwrap_or(""));
    let members: Vec<&str> = meta["workspace_members"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut manifests = serde_json::Map::new();
    let mut parts = Vec::new();
    let root_manifest = root.join("Cargo.toml");
    let mut manifest_paths: Vec<PathBuf> = vec![root_manifest];
    for pkg in meta["packages"].as_array().into_iter().flatten() {
        if members.contains(&pkg["id"].as_str().unwrap_or("")) {
            if let Some(m) = pkg["manifest_path"].as_str() {
                let p = PathBuf::from(m);
                if !manifest_paths.contains(&p) {
                    manifest_paths.push(p);
                }
            }
        }
    }
    for p in &manifest_paths {
        if let Some(d) = file_digest(p) {
            let rel = p.strip_prefix(&root).unwrap_or(p).display().to_string();
            parts.push(format!("{}={}", rel, d));
            manifests.insert(rel, json!(d));
        }
    }
    let lock = file_digest(&root.join("Cargo.lock"));
    let target_dir = meta["target_directory"].as_str().map(str::to_string);
    let build_dir = meta["build_directory"].as_str().map(str::to_string).or_else(|| target_dir.clone());
    json!({
        "id": format!("sha256:{}", crate::store::workspace_id(&root)),
        "root": path_str(&root),
        "members": members.len(),
        "member_ids": members,
        "virtual": !meta["packages"].as_array().map(|pkgs| pkgs.iter().any(|p| p["manifest_path"].as_str() == Some(path_str(&root.join("Cargo.toml")).as_str()))).unwrap_or(false),
        "manifest_digests": manifests,
        "manifest_digest": util::digest_parts(parts),
        "lockfile_digest": lock,
        "target_directory": target_dir,
        "build_directory": build_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_head_from_plain_and_packed_refs() {
        let dir = std::env::temp_dir().join(format!("haul-git-{}", util::new_id()));
        fs::create_dir_all(dir.join(".git/refs/heads")).unwrap();
        fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(dir.join(".git/refs/heads/main"), "abc123\n").unwrap();
        assert_eq!(git_head_fast(&dir).as_deref(), Some("abc123"));
        fs::remove_file(dir.join(".git/refs/heads/main")).unwrap();
        fs::write(dir.join(".git/packed-refs"), "# pack-refs\nfeed42 refs/heads/main\n").unwrap();
        assert_eq!(git_head_fast(&dir).as_deref(), Some("feed42"));
        fs::write(dir.join(".git/HEAD"), "0123detached\n").unwrap();
        assert_eq!(git_head_fast(&dir).as_deref(), Some("0123detached"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn snapshot_changes_with_content_and_ignores_build_output() {
        let dir = std::env::temp_dir().join(format!("haul-snap-{}", util::new_id()));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("target/CACHEDIR.TAG"), "x").unwrap();
        fs::write(dir.join("src/lib.rs"), "a").unwrap();
        let a = source_snapshot(&dir, &[]);
        fs::write(dir.join("target/junk"), "zzz").unwrap();
        assert_eq!(source_snapshot(&dir, &[]), a, "build output must not affect the source snapshot");
        fs::write(dir.join("src/lib.rs"), "ab").unwrap();
        assert_ne!(source_snapshot(&dir, &[]).0, a.0);
        fs::remove_dir_all(&dir).ok();
    }
}
