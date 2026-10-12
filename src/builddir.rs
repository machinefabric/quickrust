//! Build directories: Haul's, not the project's.
//!
//! A workspace builds into `~/.haul/cache/builds/<name>-<id>/` (`<id>`: the workspace's id, as
//! its records use), never into `target/` beside its sources. Haul tells Cargo, and every tool
//! that runs under it — external subcommands, build tools that ask `cargo metadata`, an
//! `xtask` run by `cargo run` that calls `$CARGO` — through `CARGO_TARGET_DIR` and
//! `CARGO_BUILD_BUILD_DIR`, marked as Haul's by `HAUL_MANAGED_TARGET_DIR`. The directory is
//! disposable: Haul's cache may delete it when it has not been used for a while, and the next
//! build recreates it.
//!
//! A target directory the user chose is used as it is and never managed: `--target-dir`,
//! `--config build.target-dir=…`, or `CARGO_TARGET_DIR` / `CARGO_BUILD_TARGET_DIR` in the
//! environment (unless set by Haul for another command, which a nested command for another
//! workspace replaces). `build.target-dir` in Cargo configuration files is overridden.
//!
//! The workspace is Cargo's answer (`cargo locate-project --workspace`), remembered for the
//! directory asked from and the manifests above it (any change to one of them asks again).

use crate::engine::Engine;
use crate::resolve::Invocation;
use crate::util;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Marks the target directory variables as set by Haul (their value: the managed directory).
pub const ENV_MANAGED: &str = "HAUL_MANAGED_TARGET_DIR";
const META_FORMAT: &str = "haul-build-dir-1";

/// The target directory of a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub dir: PathBuf,
    /// Haul's (in its cache) rather than one the user chose.
    pub managed: bool,
}

/// Where Haul's build directories live.
pub fn builds_dir() -> PathBuf {
    util::cache_dir().join("builds")
}

/// The managed build directory of the workspace rooted at `root`.
pub fn managed_dir(root: &Path) -> PathBuf {
    let name: String = root.file_name().map(util::lossy).unwrap_or_default().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(24).collect();
    let name = if name.is_empty() { "workspace".to_string() } else { name };
    builds_dir().join(format!("{}-{}", name, crate::store::workspace_id(root)))
}

/// The directory Cargo starts from for `inv` run in `cwd`: `-C`, then `--manifest-path`'s.
pub fn start_dir(inv: &Invocation, cwd: &Path) -> PathBuf {
    let base = inv.chdir.as_ref().map(|c| cwd.join(c)).unwrap_or_else(|| cwd.to_path_buf());
    match inv.manifest_path.as_ref().and_then(|m| base.join(m).parent().map(Path::to_path_buf)) {
        Some(d) => d,
        None => base,
    }
}

/// The workspace `start` belongs to (its root directory), as Cargo says; `None` outside any.
pub fn workspace_root(engine: &Engine, start: &Path) -> Result<Option<PathBuf>, String> {
    let memo = memo_path(start);
    if let Some(v) = util::read_json(&memo) {
        if v["format"] == json!(1) {
            return Ok(v["root"].as_str().map(PathBuf::from));
        }
    }
    if !start.is_dir() {
        return Ok(None);
    }
    let mut cmd = engine.command(&["locate-project".into(), "--workspace".into(), "--message-format".into(), "plain".into()], &crate::resolve::classify(&[]));
    cmd.current_dir(start).stdin(std::process::Stdio::null()).env(crate::ENV_EXEC_GUARD, format!("{}:locate", std::process::id()));
    let out = cmd.output().map_err(|e| format!("cannot run {}: {}", engine.cargo().display(), e))?;
    let root = if out.status.success() {
        let manifest = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        Some(manifest.parent().map(Path::to_path_buf).ok_or_else(|| format!("`cargo locate-project` named {}", manifest.display()))?)
    } else if String::from_utf8_lossy(&out.stderr).contains("could not find `Cargo.toml`") {
        None
    } else {
        // A broken manifest: Cargo reports it to the command itself; nothing is remembered.
        return Ok(None);
    };
    let _ = util::create_private_dir(memo.parent().unwrap_or(&memo)).and_then(|_| util::write_json_fast(&memo, &json!({ "format": 1, "start": util::path_str(start), "root": root.as_ref().map(|r| util::path_str(r)) })));
    Ok(root)
}

/// Remembered per start directory and the state of every `Cargo.toml` from there up.
fn memo_path(start: &Path) -> PathBuf {
    let mut parts = vec![util::path_str(start)];
    for d in start.ancestors() {
        let m = d.join("Cargo.toml");
        parts.push(match fs::metadata(&m) {
            Ok(md) => format!("{}:{}:{}", util::path_str(&m), md.len(), util::mtime_ns(&md)),
            Err(_) => format!("{}:-", util::path_str(&m)),
        });
    }
    util::memo_dir("roots").join(format!("{}.json", util::digest_parts(parts)))
}

/// The target directory for `inv`: one the user chose, or the workspace's managed one; `None`
/// outside any workspace (nothing is built there).
pub fn choose(inv: &Invocation, cwd: &Path, root: Option<&Path>) -> Option<Target> {
    choose_in(inv, cwd, root, &|k| std::env::var_os(k).filter(|v| !v.is_empty()))
}

/// `choose` with the environment given.
fn choose_in(inv: &Invocation, cwd: &Path, root: Option<&Path>, env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<Target> {
    if let Some(t) = &inv.target_dir {
        return Some(Target { dir: cwd.join(t), managed: false });
    }
    if let Some(v) = inv.configs.iter().rev().find_map(|c| c.strip_prefix("build.target-dir=")) {
        return Some(Target { dir: cwd.join(v.trim().trim_matches('"').trim_matches('\'')), managed: false });
    }
    let ours = env(ENV_MANAGED);
    for var in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
        if let Some(v) = env(var) {
            if ours.as_ref() != Some(&v) {
                return Some(Target { dir: cwd.join(v), managed: false });
            }
        }
    }
    root.map(|r| Target { dir: managed_dir(r), managed: true })
}

/// What tells Cargo and the tools under it where to build.
pub fn environment(target: &Target) -> Vec<(String, String)> {
    if !target.managed {
        return vec![];
    }
    let dir = util::path_str(&target.dir);
    vec![("CARGO_TARGET_DIR".into(), dir.clone()), ("CARGO_BUILD_BUILD_DIR".into(), dir.clone()), (ENV_MANAGED.into(), dir)]
}

fn meta_path(dir: &Path) -> PathBuf {
    let mut name = dir.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".json");
    dir.with_file_name(name)
}

/// Notes that the managed directory `dir` of the workspace `root` is in use (written at most
/// once a minute): its age is what Haul's cache goes by.
pub fn mark_used(dir: &Path, root: &Path) {
    let meta = meta_path(dir);
    let now = util::now_wall_ns();
    let previous = util::read_json(&meta).filter(|v| v["format"] == json!(META_FORMAT));
    if previous.as_ref().and_then(|v| v["last_used_ns"].as_u64()).is_some_and(|t| t + 60_000_000_000 > now) {
        return;
    }
    let created = previous.as_ref().and_then(|v| v["created_ns"].as_u64()).unwrap_or(now);
    let _ = util::create_private_dir(&builds_dir()).and_then(|_| util::write_json_fast(&meta, &json!({ "format": META_FORMAT, "workspace_root": util::path_str(root), "created_ns": created, "last_used_ns": now })));
}

/// A managed directory's record: its workspace root and when it was last used.
pub fn meta(dir: &Path) -> Option<(PathBuf, u64, u64)> {
    let v: Value = util::read_json(&meta_path(dir)).filter(|v| v["format"] == json!(META_FORMAT))?;
    Some((PathBuf::from(v["workspace_root"].as_str()?), v["created_ns"].as_u64()?, v["last_used_ns"].as_u64()?))
}

/// Held while a command uses a managed directory: Haul's cache never removes one in use.
pub struct InUse {
    _lock: Option<util::SharedLock>,
}

pub fn in_use(dir: &Path) -> InUse {
    let mut name = dir.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".lock");
    InUse { _lock: util::SharedLock::acquire(&dir.with_file_name(name)).ok() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::classify;
    use std::ffi::OsString;

    fn inv(args: &[&str]) -> Invocation {
        classify(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn the_users_choice_wins_and_otherwise_the_workspace_gets_a_directory_of_its_own() {
        let cwd = Path::new("/w/app");
        let root = Path::new("/w/app");
        let none = |_: &str| None;
        let choose = |i: &Invocation, cwd: &Path, root: Option<&Path>| choose_in(i, cwd, root, &none);
        let managed = choose(&inv(&["build"]), cwd, Some(root)).unwrap();
        assert!(managed.managed && managed.dir.starts_with(builds_dir()));
        assert!(managed.dir.file_name().unwrap().to_string_lossy().starts_with("app-"));
        assert_eq!(choose(&inv(&["build", "--target-dir", "out"]), cwd, Some(root)), Some(Target { dir: cwd.join("out"), managed: false }));
        assert_eq!(choose(&inv(&["--config", "build.target-dir=\"/x/t\"", "build"]), cwd, Some(root)), Some(Target { dir: PathBuf::from("/x/t"), managed: false }));
        assert_eq!(choose(&inv(&["new", "x"]), cwd, None), None, "outside a workspace there is nothing to build");
        assert_ne!(managed_dir(Path::new("/a/app")), managed_dir(Path::new("/b/app")), "same name, different workspaces");
        assert!(environment(&Target { dir: cwd.join("out"), managed: false }).is_empty());
        let env = environment(&managed);
        assert_eq!(env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", ENV_MANAGED]);
        // The user's variable is theirs; Haul's own, set for an enclosing command (perhaps of
        // another workspace), is replaced.
        let theirs = |k: &str| (k == "CARGO_TARGET_DIR").then(|| std::ffi::OsString::from("/mine"));
        assert_eq!(choose_in(&inv(&["build"]), cwd, Some(root), &theirs), Some(Target { dir: PathBuf::from("/mine"), managed: false }));
        let outer = managed_dir(Path::new("/w/other"));
        let inherited = |k: &str| matches!(k, "CARGO_TARGET_DIR" | ENV_MANAGED).then(|| outer.clone().into_os_string());
        assert_eq!(choose_in(&inv(&["build"]), cwd, Some(root), &inherited), Some(managed.clone()));
    }

    #[test]
    fn names_are_safe_and_bounded() {
        let d = managed_dir(Path::new("/w/my project (copy) with a very long name indeed"));
        let name = d.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("my_project__copy__with_a-"), "{}", name);
        assert_eq!(managed_dir(Path::new("/")).file_name().unwrap().to_string_lossy().split('-').next(), Some("workspace"));
    }
}
