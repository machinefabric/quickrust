//! Build directories: QuickRust's, not the project's.
//!
//! A workspace builds into `~/.qrust/cache/builds/<name>-<id>/` (`<id>`: the workspace's id, as
//! its records use), never into `target/` beside its sources. QuickRust tells Cargo, and every tool
//! that runs under it — external subcommands, build tools that ask `cargo metadata`, an
//! `xtask` run by `cargo run` that calls `$CARGO` — through `CARGO_TARGET_DIR` and
//! `CARGO_BUILD_BUILD_DIR`, marked as QuickRust's by `QRUST_MANAGED_TARGET_DIR`. The directory is
//! disposable: QuickRust's cache may delete it when it has not been used for a while, and the next
//! build recreates it.
//!
//! A workspace built in more than one configuration that must not share output — the same
//! sources against another registry, with another feature set, for another release channel — is
//! given a build directory per configuration by naming each a variant (`build.variant`,
//! `QRUST_BUILD_VARIANT`): `builds/<name>-<id>.<variant>/`. Each is a build directory like any
//! other: Cargo's, locked while in use, counted in QuickRust's cache and collected with it. Without
//! variants the configurations take turns in one directory, and each overwrites the binaries of
//! the last.
//!
//! A program that uses what a build left there — a test harness starting the binaries it built
//! an hour ago, a packager reading them — holds the directory while it runs
//! (`qrust path --hold-for <pid>`): QuickRust's cache treats a held directory as in use, and
//! the hold ends with the process, whether it says so or not.
//!
//! A target directory the user chose is used as it is and never managed: `--target-dir`,
//! `--config build.target-dir=…`, or `CARGO_TARGET_DIR` / `CARGO_BUILD_TARGET_DIR` in the
//! environment (unless set by QuickRust for another command, which a nested command for another
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

/// Marks the target directory variables as set by QuickRust (their value: the managed directory).
pub const ENV_MANAGED: &str = "QRUST_MANAGED_TARGET_DIR";
const META_FORMAT: &str = "qrust-build-dir-1";

/// The target directory of a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub dir: PathBuf,
    /// QuickRust's (in its cache) rather than one the user chose.
    pub managed: bool,
}

/// Where QuickRust's build directories live.
pub fn builds_dir() -> PathBuf {
    util::cache_dir().join("builds")
}

/// The longest name a build variant may have.
const VARIANT_MAX: usize = 32;

/// The build variant `cfg` names (`build.variant`), `None` when it names none. A variant is part
/// of a directory's name, so it is a plain word: letters, digits, `-` and `_`.
pub fn variant(cfg: &crate::config::Config) -> Result<Option<String>, String> {
    variant_named(cfg.str("build.variant"))
}

fn variant_named(name: &str) -> Result<Option<String>, String> {
    if name.is_empty() {
        return Ok(None);
    }
    if name.len() > VARIANT_MAX || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!("invalid build variant {:?} (`build.variant`): a variant is at most {} letters, digits, `-` or `_`", name, VARIANT_MAX));
    }
    Ok(Some(name.to_string()))
}

/// The managed build directory of the workspace rooted at `root`: its one directory, or that of
/// the build variant `variant`.
pub fn managed_dir(root: &Path, variant: Option<&str>) -> PathBuf {
    let name: String = root.file_name().map(util::lossy).unwrap_or_default().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(24).collect();
    let name = if name.is_empty() { "workspace".to_string() } else { name };
    let id = crate::store::workspace_id(root);
    builds_dir().join(match variant {
        // `.` is in neither a workspace's name here nor a variant's: what follows it is the variant.
        Some(v) => format!("{}-{}.{}", name, id, v),
        None => format!("{}-{}", name, id),
    })
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
    let out = cmd.output().map_err(|e| format!("cannot run {}: {}", engine.program.display(), e))?;
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

/// The target directory for `inv`: one the user chose, or the workspace's managed one (that of
/// `variant`, when one is named); `None` outside any workspace (nothing is built there).
pub fn choose(inv: &Invocation, cwd: &Path, root: Option<&Path>, variant: Option<&str>) -> Option<Target> {
    choose_in(inv, cwd, root, variant, &|k| std::env::var_os(k).filter(|v| !v.is_empty()))
}

/// `choose` with the environment given.
fn choose_in(inv: &Invocation, cwd: &Path, root: Option<&Path>, variant: Option<&str>, env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<Target> {
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
    root.map(|r| Target { dir: managed_dir(r, variant), managed: true })
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
/// once a minute): its age is what QuickRust's cache goes by.
pub fn mark_used(dir: &Path, root: &Path) {
    mark_used_in(&builds_dir(), dir, root)
}

/// `mark_used`, with QuickRust's build directories in `builds`.
pub fn mark_used_in(builds: &Path, dir: &Path, root: &Path) {
    let meta = meta_path(dir);
    let now = util::now_wall_ns();
    let previous = util::read_json(&meta).filter(|v| v["format"] == json!(META_FORMAT));
    if previous.as_ref().and_then(|v| v["last_used_ns"].as_u64()).is_some_and(|t| t + 60_000_000_000 > now) {
        return;
    }
    let created = previous.as_ref().and_then(|v| v["created_ns"].as_u64()).unwrap_or(now);
    let _ = util::create_private_dir(builds).and_then(|_| util::write_json_fast(&meta, &json!({ "format": META_FORMAT, "workspace_root": util::path_str(root), "created_ns": created, "last_used_ns": now })));
}

/// A managed directory's record: its workspace root and when it was last used.
pub fn meta(dir: &Path) -> Option<(PathBuf, u64, u64)> {
    let v: Value = util::read_json(&meta_path(dir)).filter(|v| v["format"] == json!(META_FORMAT))?;
    Some((PathBuf::from(v["workspace_root"].as_str()?), v["created_ns"].as_u64()?, v["last_used_ns"].as_u64()?))
}

const HOLD: &str = ".hold-";

/// Records that the process `pid` holds the managed directory `dir`: one empty file per holder
/// beside it, `<dir>.hold-<pid>`, so holders never write the same file.
///
/// A holder does not release its hold: it ends, and its record is then of nobody. A collection
/// removes such records, but a cache inside its bounds is never collected, and a tool that asks
/// for a held path at every step left dozens of them a run beside the build directories. So each
/// new hold first clears the records of holders that are gone.
pub fn hold(dir: &Path, pid: u32) -> Result<(), String> {
    let builds = dir.parent().ok_or_else(|| format!("{} is not a build directory", dir.display()))?;
    let mut name = dir.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!("{}{}", HOLD, pid));
    let file = dir.with_file_name(name);
    util::create_private_dir(builds).map_err(|e| format!("cannot record a hold on {}: {}", dir.display(), e))?;
    release_ended_holders(builds);
    fs::write(&file, b"").map_err(|e| format!("cannot record a hold on {}: {}", dir.display(), e))
}

/// Removes the hold records in `builds` whose holder is no longer running. One that cannot be
/// removed is left for the next hold or the next collection, which read it as what it is: the
/// record of a process that is gone holds nothing.
fn release_ended_holders(builds: &Path) {
    let Ok(entries) = fs::read_dir(builds) else {
        return;
    };
    for entry in entries.flatten() {
        if hold_of(&util::lossy(&entry.file_name())).is_some_and(|(_, holder)| !crate::proc::pid_alive(holder)) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// What a file name beside the build directories says, if it is a hold's: the directory's name
/// and the holder.
pub fn hold_of(file_name: &str) -> Option<(&str, u32)> {
    let (dir, pid) = file_name.rsplit_once(HOLD)?;
    if dir.is_empty() || pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((dir, pid.parse().ok()?))
}

/// Held while a command uses a managed directory: QuickRust's cache never removes one in use.
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
        let choose = |i: &Invocation, cwd: &Path, root: Option<&Path>| choose_in(i, cwd, root, None, &none);
        let managed = choose(&inv(&["build"]), cwd, Some(root)).unwrap();
        assert!(managed.managed && managed.dir.starts_with(builds_dir()));
        assert!(managed.dir.file_name().unwrap().to_string_lossy().starts_with("app-"));
        assert_eq!(choose(&inv(&["build", "--target-dir", "out"]), cwd, Some(root)), Some(Target { dir: cwd.join("out"), managed: false }));
        assert_eq!(choose(&inv(&["--config", "build.target-dir=\"/x/t\"", "build"]), cwd, Some(root)), Some(Target { dir: PathBuf::from("/x/t"), managed: false }));
        assert_eq!(choose(&inv(&["new", "x"]), cwd, None), None, "outside a workspace there is nothing to build");
        assert_ne!(managed_dir(Path::new("/a/app"), None), managed_dir(Path::new("/b/app"), None), "same name, different workspaces");
        assert!(environment(&Target { dir: cwd.join("out"), managed: false }).is_empty());
        let env = environment(&managed);
        assert_eq!(env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", ENV_MANAGED]);
        // The user's variable is theirs; QuickRust's own, set for an enclosing command (perhaps of
        // another workspace), is replaced.
        let theirs = |k: &str| (k == "CARGO_TARGET_DIR").then(|| std::ffi::OsString::from("/mine"));
        assert_eq!(choose_in(&inv(&["build"]), cwd, Some(root), None, &theirs), Some(Target { dir: PathBuf::from("/mine"), managed: false }));
        let outer = managed_dir(Path::new("/w/other"), None);
        let inherited = |k: &str| matches!(k, "CARGO_TARGET_DIR" | ENV_MANAGED).then(|| outer.clone().into_os_string());
        assert_eq!(choose_in(&inv(&["build"]), cwd, Some(root), None, &inherited), Some(managed.clone()));
    }

    #[test]
    fn a_variant_is_a_build_directory_of_its_own_beside_the_workspaces() {
        let cwd = Path::new("/w/app");
        let none = |_: &str| None;
        let plain = choose_in(&inv(&["build"]), cwd, Some(cwd), None, &none).unwrap();
        let staging = choose_in(&inv(&["build"]), cwd, Some(cwd), Some("staging-nightly"), &none).unwrap();
        let prod = choose_in(&inv(&["build"]), cwd, Some(cwd), Some("prod-nightly"), &none).unwrap();
        assert!(staging.managed && prod.managed);
        assert_ne!(staging.dir, plain.dir, "a variant does not build into the workspace's own directory");
        assert_ne!(staging.dir, prod.dir, "nor into another variant's");
        // Beside it, under the workspace's own name: the cache finds, sizes and collects it as one.
        assert_eq!(staging.dir.parent(), plain.dir.parent());
        let (plain_name, staging_name) = (plain.dir.file_name().unwrap().to_string_lossy().into_owned(), staging.dir.file_name().unwrap().to_string_lossy().into_owned());
        assert_eq!(staging_name, format!("{}.staging-nightly", plain_name));
        // The same variant of the same workspace is the same directory, every time.
        assert_eq!(managed_dir(cwd, Some("staging-nightly")), staging.dir);
        // A directory the user chose is theirs, whatever variant is named.
        assert_eq!(choose_in(&inv(&["build", "--target-dir", "out"]), cwd, Some(cwd), Some("staging-nightly"), &none), Some(Target { dir: cwd.join("out"), managed: false }));
        // A command nested in a variant's build (for the same workspace) builds where it does.
        let inherited = |k: &str| matches!(k, "CARGO_TARGET_DIR" | ENV_MANAGED).then(|| staging.dir.clone().into_os_string());
        assert_eq!(choose_in(&inv(&["build"]), cwd, Some(cwd), Some("staging-nightly"), &inherited), Some(staging.clone()));
    }

    #[test]
    fn a_hold_names_its_directory_and_its_holder() {
        assert_eq!(hold_of("app-fca2211b77f340ae.hold-4242"), Some(("app-fca2211b77f340ae", 4242)));
        // A variant's directory is held under its whole name, whatever the variant is called.
        assert_eq!(hold_of("app-fca2211b77f340ae.staging.hold-7"), Some(("app-fca2211b77f340ae.staging", 7)));
        assert_eq!(hold_of("app-fca2211b77f340ae.hold-1.hold-9"), Some(("app-fca2211b77f340ae.hold-1", 9)));
        for other in ["app-fca2211b77f340ae.json", "app-fca2211b77f340ae.lock", "app-fca2211b77f340ae.hold-", "app-fca2211b77f340ae.hold-x1", ".hold-12", "app.hold-99999999999999999999"] {
            assert_eq!(hold_of(other), None, "{}", other);
        }
    }

    #[test]
    fn a_new_hold_clears_the_records_of_holders_that_are_gone() {
        let builds = std::env::temp_dir().join(format!("qrust-holds-{}", util::new_id())).join("builds");
        let (app, lib) = (builds.join("app-fca2211b77f340ae"), builds.join("lib-0011223344556677.staging"));
        // A process that ran and was waited for: its id is nobody's now.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap()).arg("--list").stdout(std::process::Stdio::null()).spawn().unwrap();
        let gone = child.id();
        child.wait().unwrap();
        let me = std::process::id();
        let record = |dir: &Path, pid: u32| dir.with_file_name(format!("{}.hold-{}", dir.file_name().unwrap().to_string_lossy(), pid));

        hold(&app, me).unwrap();
        // What a holder that ended left behind, on this directory and on another.
        fs::write(record(&app, gone), b"").unwrap();
        fs::write(record(&lib, gone), b"").unwrap();
        // Files that are not holds, and are not touched.
        fs::write(builds.join("app-fca2211b77f340ae.json"), b"{}").unwrap();
        fs::write(builds.join("app-fca2211b77f340ae.lock"), b"").unwrap();

        hold(&lib, me).unwrap();
        let mut left: Vec<String> = fs::read_dir(&builds).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(
            left,
            [
                format!("app-fca2211b77f340ae.hold-{}", me),
                "app-fca2211b77f340ae.json".to_string(),
                "app-fca2211b77f340ae.lock".to_string(),
                format!("lib-0011223344556677.staging.hold-{}", me),
            ],
            "the records of the holder that is gone are cleared, and nothing else"
        );
        fs::remove_dir_all(builds.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_variant_is_a_plain_word() {
        assert_eq!(variant_named(""), Ok(None));
        assert_eq!(variant_named("st-n_cu9"), Ok(Some("st-n_cu9".to_string())));
        assert_eq!(variant_named(&"v".repeat(VARIANT_MAX)), Ok(Some("v".repeat(VARIANT_MAX))));
        for bad in ["a.b", "a/b", "..", "a b", "a\\b", "é", &"v".repeat(VARIANT_MAX + 1)] {
            let e = variant_named(bad).unwrap_err();
            assert!(e.starts_with(&format!("invalid build variant {:?}", bad)), "{}", e);
        }
    }

    #[test]
    fn names_are_safe_and_bounded() {
        let d = managed_dir(Path::new("/w/my project (copy) with a very long name indeed"), None);
        let name = d.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("my_project__copy__with_a-"), "{}", name);
        assert_eq!(managed_dir(Path::new("/"), None).file_name().unwrap().to_string_lossy().split('-').next(), Some("workspace"));
    }
}
