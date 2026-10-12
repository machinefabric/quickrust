//! Haul's compiler driver (`driver/` in this repository): rustc itself — the toolchain's own
//! `librustc_driver`, its LLVM, its release — run through Haul's callbacks, which is how Haul
//! changes how the compiler works without changing what it builds.
//!
//! The compiler's internal API differs between releases, so the driver is built for each
//! toolchain, against that toolchain's `rustc-dev` component, and only used once it reports
//! exactly the compiler's own `rustc -vV`. It lives in `~/.haul/cache/drivers/<key>/`, the key naming
//! the compiler (commit, host) and the driver's source. A toolchain the driver cannot be built
//! for keeps the stock compiler; the reason is recorded next to where the driver would be.

use crate::util;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Set for the driver: compute the crate hash from what dependents can observe.
pub const ENV_INTERFACE_HASH: &str = "HAUL_DRIVER_INTERFACE_HASH";
/// Set for the driver: the descriptor of a pipe on which it says its outputs are written.
pub const ENV_READY_FD: &str = "HAUL_DRIVER_READY_FD";
/// Set for the driver: where to keep the objects of a unit Haul relinks.
pub const ENV_KEEP_OBJECTS: &str = "HAUL_DRIVER_KEEP_OBJECTS";

const MANIFEST: &str = include_str!("../driver/Cargo.toml");
const SOURCES: [(&str, &str); 2] = [("main.rs", include_str!("../driver/src/main.rs")), ("early_link.rs", include_str!("../driver/src/early_link.rs"))];
/// The binary's name inside its directory.
const BINARY: &str = "haul-rustc-driver";
/// Releases whose compiler interface the driver's sources adapt to: the driver is built with
/// `--cfg haul_rustc_ge_<minor>` for each one its compiler is (rustc's internal interface changes
/// between releases).
const INTERFACE_BREAKS: &[u32] = &[99];
/// How a failure to get the toolchain's `rustc-dev` component begins.
pub const NO_RUSTC_DEV: &str = "rustc-dev cannot be installed";
/// In a driver's directory: the toolchain it was built for (`{"sysroot": …}`).
pub const COMPILER: &str = "compiler.json";
/// Next to where the driver would be: why it could not be built.
const BUILD_FAILED: &str = "build-failed";
/// Next to where the driver would be, while a background build runs.
const BUILDING: &str = ".building";

/// A compiler as the driver is built for it: its own account (`rustc -vV`) and its toolchain
/// (`--print sysroot`). Two toolchains can share a release and a commit (an installed release and
/// a linked build of it), but not their files: the driver loads `librustc_driver` and the standard library
/// from the toolchain it was built in.
pub struct Compiler {
    pub version_verbose: String,
    pub sysroot: PathBuf,
}

impl Compiler {
    /// The compiler at `rustc` (a path, or a name on `PATH`), asked once per binary.
    pub fn of(rustc: &str) -> Result<Compiler, String> {
        let memo = util::memo_dir("rustc");
        Ok(Compiler { version_verbose: crate::rustc_info::version_verbose(&memo, rustc)?, sysroot: crate::rustc_info::sysroot(&memo, rustc)? })
    }
}

/// Where the driver for this compiler lives, built or not.
pub fn dir(compiler: &Compiler) -> Result<PathBuf, String> {
    let vv = &compiler.version_verbose;
    let field = |k: &str| vv.lines().find_map(|l| l.strip_prefix(k)).map(str::trim).ok_or_else(|| format!("`rustc -vV` has no `{}` line", k.trim_end_matches(": ")));
    let (commit, host) = (field("commit-hash: ")?, field("host: ")?);
    let source = util::digest_parts(std::iter::once(MANIFEST.to_string()).chain(SOURCES.iter().flat_map(|(name, text)| [name.to_string(), text.to_string()])).chain(std::iter::once(util::path_str(&util::canon(&compiler.sysroot)))));
    Ok(util::cache_dir().join("drivers").join(format!("{}-{}-{}", &commit[..commit.len().min(12)], host, &source[..16])))
}

/// The driver for this compiler, when it has been built.
pub fn find(compiler: &Compiler) -> Option<PathBuf> {
    let exe = dir(compiler).ok()?.join(format!("{}{}", BINARY, std::env::consts::EXE_SUFFIX));
    util::is_executable(&exe).then_some(exe)
}

/// Why the driver for this compiler could not be built, when it could not.
pub fn build_failure(compiler: &Compiler) -> Option<String> {
    fs::read_to_string(dir(compiler).ok()?.join(BUILD_FAILED)).ok()
}

/// Where failures of the driver at `exe` are recorded, per unit (see the wrapper).
fn failures(exe: &Path) -> PathBuf {
    exe.with_file_name("failed")
}

/// Whether the driver at `exe` failed on this unit before: it is then compiled by rustc, until
/// a new driver (another compiler or another driver source) takes over.
pub fn failed_before(exe: &Path, unit: &str) -> bool {
    failures(exe).join(unit).exists()
}

/// Records that the driver at `exe` failed on this unit, with what it printed; returns where.
pub fn record_failure(exe: &Path, unit: &str, report: &[u8]) -> Result<PathBuf, String> {
    let dir = failures(exe);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    let path = dir.join(unit);
    util::write_atomic_fast(&path, report).map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
    Ok(path)
}

/// The minor version of a compiler's release (`release: 1.99.0`, `1.100.0-nightly`).
fn release_minor(version_verbose: &str) -> Option<u32> {
    let release = version_verbose.lines().find_map(|l| l.strip_prefix("release: "))?;
    release.trim().split('.').nth(1)?.parse().ok()
}

/// Starts building the driver for the compiler at `rustc` (described by `compiler`) in the
/// background, with `haul` (Haul's own binary) running `haul driver
/// build`, unless it is built, being built, or could not be built before (`haul driver build`
/// tries again). Returns whether a build started.
pub fn start_build(haul: &Path, rustc: &Path, compiler: &Compiler) -> Result<bool, String> {
    if find(compiler).is_some() {
        return Ok(false);
    }
    let target = dir(compiler)?;
    if target.join(BUILD_FAILED).exists() {
        return Ok(false);
    }
    util::create_private_dir(&target).map_err(|e| e.to_string())?;
    // One build at a time. A marker older than an hour is from a build that did not finish.
    let marker = target.join(BUILDING);
    if let Ok(md) = fs::metadata(&marker) {
        if util::mtime_ns(&md) + 3_600 * 1_000_000_000 > util::now_wall_ns() {
            return Ok(false);
        }
        let _ = fs::remove_file(&marker);
    }
    match fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(format!("cannot create {}: {}", marker.display(), e)),
    }
    let log = fs::File::create(target.join("build.log")).map_err(|e| e.to_string())?;
    let mut c = Command::new(haul);
    c.args(["driver", "build", "--rustc", &util::path_str(rustc)]);
    // It outlives the build that started it, and that build's terminal signals (Ctrl-C) do not
    // reach it.
    match crate::proc::spawn_detached(&mut c, log) {
        Ok(()) => Ok(true),
        Err(e) => {
            let _ = fs::remove_file(&marker);
            Err(format!("cannot start {}: {}", haul.display(), e))
        }
    }
}

/// Whether builds under `cfg` compile with the driver (early cut-off: fast mode with the artifact
/// cache, `fast.early-cutoff = "auto"`).
pub fn wanted(cfg: &crate::config::Config) -> bool {
    cfg.str("mode") == "fast" && cfg.str("cache.artifacts") == "on" && cfg.str("fast.early-cutoff") == "auto"
}

/// Builds the driver for a toolchain just installed (`haul setup`, a channel's new release),
/// before any build uses the toolchain: the driver is part of what an artifact-cache entry is
/// keyed by, so what builds store without it, builds with it never find. Shown as a phase. A
/// failure is a note, not an error: the driver only saves time, and builds try again.
pub fn prepare(tc: &crate::toolchain::Toolchain) {
    let rustc = tc.dir.join("bin").join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    let release = tc.record().map(|r| r.version).unwrap_or_else(|| tc.id.clone());
    if Compiler::of(&util::path_str(&rustc)).ok().and_then(|c| find(&c)).is_some() {
        return;
    }
    let mut phase = crate::ui::Phase::start();
    phase.status(&format!("Preparing Haul's compiler driver for Rust {}", release), true);
    match build(&rustc) {
        Ok(_) => phase.finish(true, &format!("Compiler driver for Rust {} ready", release)),
        Err(e) => {
            phase.clear_line();
            crate::ui::notice_driver_unavailable(&e);
        }
    }
}

/// Builds the driver for the compiler at `rustc` (a toolchain's own binary) and installs it where
/// `find` looks; one build at a time. Adds the `rustc-dev` component to a toolchain Haul installed
/// when it is missing; any other compiler must have it. A failure is recorded, and background builds
/// (`start_build`) do not try again; this one always does.
pub fn build(rustc: &Path) -> Result<PathBuf, String> {
    let compiler = Compiler::of(&util::path_str(rustc))?;
    let target = dir(&compiler)?;
    let _lock = util::ExclusiveLock::acquire(&target.join(".build.lock")).map_err(|e| format!("cannot lock {}: {}", target.display(), e))?;
    // Which toolchain it is for: once that is gone, Haul's cache removes the driver.
    let _ = util::write_json_fast(&target.join(COMPILER), &serde_json::json!({ "sysroot": util::path_str(&compiler.sysroot) }));
    let result = match find(&compiler) {
        Some(exe) => Ok(exe),
        None => build_locked(rustc, &compiler.version_verbose, &target),
    };
    match &result {
        Ok(_) => {
            let _ = fs::remove_file(target.join(BUILD_FAILED));
        }
        Err(e) => {
            let _ = fs::write(target.join(BUILD_FAILED), format!("{}\n", e));
        }
    }
    let _ = fs::remove_file(target.join(BUILDING));
    result
}

fn build_locked(rustc: &Path, vv: &str, target: &Path) -> Result<PathBuf, String> {
    let vv = vv.to_string();
    let toolchain = rustc.parent().and_then(Path::parent).ok_or_else(|| format!("{} is not in a toolchain's bin directory", rustc.display()))?.to_path_buf();
    let name = util::lossy(toolchain.file_name().unwrap_or_default());
    // The toolchain's rustc-dev component: added to a toolchain Haul installed (from its own
    // release; nothing when it has it), required of any other (listed in its components).
    let layout = crate::toolchain::Layout::current();
    match crate::toolchain::owning(&layout, rustc) {
        Some(tc) => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let server = crate::config::Config::load(&cwd).map(|c| c.str("toolchain.dist-server").to_string()).unwrap_or_else(|_| crate::toolchain::dist::UPSTREAM.to_string());
            let mut progress = crate::ui::ToolchainProgress::new(&tc.id);
            match crate::toolchain::ensure_components(&layout, &tc, &["rustc-dev".to_string()], &server, &mut progress) {
                Ok(()) => progress.finish(&tc),
                Err(e) => {
                    progress.fail(&e);
                    return Err(format!("{} for {}: {}", NO_RUSTC_DEV, name, e));
                }
            }
        }
        None => {
            let listed = fs::read_to_string(toolchain.join("lib/rustlib/components")).unwrap_or_default();
            if !listed.lines().any(|l| l.trim() == "rustc-dev") {
                return Err(format!("{} for {}: the toolchain does not have it, and Haul only adds components to toolchains it installed", NO_RUSTC_DEV, name));
            }
        }
    }
    // A scratch crate: the driver's sources, built with the toolchain's own Cargo.
    let work = util::cache_dir().join("drivers").join(format!("build-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(work.join("src")).map_err(|e| e.to_string())?;
    fs::write(work.join("Cargo.toml"), MANIFEST).map_err(|e| e.to_string())?;
    for (name, text) in SOURCES {
        fs::write(work.join("src").join(name), text).map_err(|e| e.to_string())?;
    }
    let lib = toolchain.join("lib");
    let minor = release_minor(&vv).ok_or_else(|| format!("`rustc -vV` of {} names no release", name))?;
    let cfgs: String = INTERFACE_BREAKS.iter().map(|b| format!(" --check-cfg cfg(haul_rustc_ge_{b}){}", if minor >= *b { format!(" --cfg haul_rustc_ge_{b}") } else { String::new() })).collect();
    let rustflags = format!("-L {} -C link-arg=-Wl,-rpath,{} -Zon-broken-pipe=kill{}", util::path_str(&lib), util::path_str(&lib), cfgs);
    let out = crate::engine::toolchain_program(&toolchain.join("bin").join(format!("cargo{}", std::env::consts::EXE_SUFFIX)))
        .args(["build", "--release", "--quiet"])
        .current_dir(&work)
        .env("RUSTC", rustc)
        .env("RUSTC_BOOTSTRAP", "1")
        .env("RUSTFLAGS", rustflags)
        .env("CARGO_TARGET_DIR", work.join("target"))
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run the toolchain's cargo: {}", e))?;
    if !out.status.success() {
        let _ = fs::remove_dir_all(&work);
        return Err(format!("the driver does not build against {}: {}", name, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let built = work.join("target").join("release").join(format!("{}{}", BINARY, std::env::consts::EXE_SUFFIX));
    // It must be the compiler it was built against, by its own account.
    let mut says = Command::new(&built);
    // The driver links the toolchain's own libraries, found as rustc finds them.
    if let Some(lib) = crate::engine::toolchain_program(rustc).get_envs().find(|(k, _)| *k == crate::engine::LIBRARY_PATH).and_then(|(_, v)| v) {
        says.env(crate::engine::LIBRARY_PATH, lib);
    }
    let says = says.arg("-vV").stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if String::from_utf8_lossy(&says.stdout) != vv {
        let _ = fs::remove_dir_all(&work);
        return Err(format!("the driver built for {} does not report the compiler's own `rustc -vV`", name));
    }
    let exe = target.join(format!("{}{}", BINARY, std::env::consts::EXE_SUFFIX));
    fs::create_dir_all(target).map_err(|e| e.to_string())?;
    let staged = target.join(format!(".{}.{}", BINARY, std::process::id()));
    fs::copy(&built, &staged).and_then(|_| fs::rename(&staged, &exe)).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&work);
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_toolchains_of_one_release_get_drivers_of_their_own() {
        let vv = "rustc 1.98.1 (48a229cea 2026-09-01)\nbinary: rustc\ncommit-hash: 48a229ceaefd4985c50990b14116b6d856af0985\nhost: x86_64-unknown-linux-gnu\nrelease: 1.98.1\n".to_string();
        let at = |sysroot: &str| dir(&Compiler { version_verbose: vv.clone(), sysroot: PathBuf::from(sysroot) }).unwrap();
        assert_ne!(at("/haul/toolchains/1.98.1-x86_64-unknown-linux-gnu"), at("/haul/toolchains/dev"));
        assert_eq!(at("/haul/toolchains/1.98.1-x86_64-unknown-linux-gnu"), at("/haul/toolchains/1.98.1-x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn the_release_names_the_interface_the_driver_adapts_to() {
        let vv = |release: &str| format!("rustc {release} (abc 2026-09-28)\nbinary: rustc\nrelease: {release}\n");
        assert_eq!(release_minor(&vv("1.99.0")), Some(99));
        assert_eq!(release_minor(&vv("1.98.1")), Some(98));
        assert_eq!(release_minor(&vv("1.100.0-nightly")), Some(100));
        assert_eq!(release_minor("rustc 1.99.0\n"), None);
    }
}
