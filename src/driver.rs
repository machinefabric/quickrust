//! Haul's compiler driver (`driver/` in this repository): rustc itself — the toolchain's own
//! `librustc_driver`, its LLVM, its release — run through Haul's callbacks, which is how Haul
//! changes how the compiler works without changing what it builds.
//!
//! The compiler's internal API differs between releases, so the driver is built for each
//! toolchain, against that toolchain's `rustc-dev` component, and only used once it reports
//! exactly the compiler's own `rustc -vV`. It lives in `~/.haul/drivers/<key>/`, the key naming
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
/// Next to where the driver would be: why it could not be built.
const BUILD_FAILED: &str = "build-failed";
/// Next to where the driver would be, while a background build runs.
const BUILDING: &str = ".building";

/// Where the driver for the compiler with this `rustc -vV` lives, built or not.
pub fn dir(version_verbose: &str) -> Result<PathBuf, String> {
    let field = |k: &str| version_verbose.lines().find_map(|l| l.strip_prefix(k)).map(str::trim).ok_or_else(|| format!("`rustc -vV` has no `{}` line", k.trim_end_matches(": ")));
    let (commit, host) = (field("commit-hash: ")?, field("host: ")?);
    let source = &util::digest_parts(std::iter::once(MANIFEST.to_string()).chain(SOURCES.iter().flat_map(|(name, text)| [name.to_string(), text.to_string()])))[..16];
    Ok(util::haul_home().join("drivers").join(format!("{}-{}-{}", &commit[..commit.len().min(12)], host, source)))
}

/// The driver for the compiler with this `rustc -vV`, when it has been built.
pub fn find(version_verbose: &str) -> Option<PathBuf> {
    let exe = dir(version_verbose).ok()?.join(format!("{}{}", BINARY, std::env::consts::EXE_SUFFIX));
    util::is_executable(&exe).then_some(exe)
}

/// Why the driver for the compiler with this `rustc -vV` could not be built, when it could not.
pub fn build_failure(version_verbose: &str) -> Option<String> {
    fs::read_to_string(dir(version_verbose).ok()?.join(BUILD_FAILED)).ok()
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

/// The toolchain directory of a rustup-managed compiler (`…/toolchains/<name>/bin/rustc`).
fn rustup_toolchain(rustc: &Path) -> Option<(PathBuf, String)> {
    let dir = rustc.parent()?.parent()?;
    let name = dir.file_name()?.to_str()?.to_string();
    (dir.parent()?.file_name()? == "toolchains").then(|| (dir.to_path_buf(), name))
}

/// Starts building the driver for the compiler at `rustc` (whose `rustc -vV` is
/// `version_verbose`) in the background, with `haul` (Haul's own binary) running `haul driver
/// build`, unless it is built, being built, or could not be built before (`haul driver build`
/// tries again). Returns whether a build started.
// The build outlives this process on purpose; init reaps it.
#[allow(clippy::zombie_processes)]
pub fn start_build(haul: &Path, rustc: &Path, version_verbose: &str) -> Result<bool, String> {
    if find(version_verbose).is_some() {
        return Ok(false);
    }
    let target = dir(version_verbose)?;
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
    c.args(["driver", "build", "--rustc", &util::path_str(rustc)]).stdin(Stdio::null()).stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(log);
    // A session of its own: it outlives the build that started it, and that build's terminal
    // signals (Ctrl-C) do not reach it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    match c.spawn() {
        Ok(_) => Ok(true),
        Err(e) => {
            let _ = fs::remove_file(&marker);
            Err(format!("cannot start {}: {}", haul.display(), e))
        }
    }
}

/// Builds the driver for the compiler at `rustc` (a toolchain binary, not rustup's proxy) and
/// installs it where `find` looks; one build at a time. Installs the toolchain's `rustc-dev`
/// component through rustup when it is missing. A failure is recorded, and background builds
/// (`start_build`) do not try again; this one always does.
pub fn build(rustc: &Path) -> Result<PathBuf, String> {
    let vv = crate::rustc_info::version_verbose(&util::haul_home().join("rustc"), &util::path_str(rustc))?;
    let target = dir(&vv)?;
    let _lock = util::ExclusiveLock::acquire(&target.join(".build.lock")).map_err(|e| format!("cannot lock {}: {}", target.display(), e))?;
    let result = match find(&vv) {
        Some(exe) => Ok(exe),
        None => build_locked(rustc, &vv, &target),
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
    let (toolchain, name) = rustup_toolchain(rustc).ok_or_else(|| format!("{} is not a rustup toolchain's compiler; the driver is built against rustup's rustc-dev component", rustc.display()))?;
    let host = vv.lines().find_map(|l| l.strip_prefix("host: ")).unwrap_or_default().trim().to_string();
    let compiler_libs = toolchain.join("lib").join("rustlib").join(&host).join("lib");
    let has_rustc_dev = fs::read_dir(&compiler_libs).into_iter().flatten().flatten().any(|e| e.file_name().to_string_lossy().starts_with("librustc_driver-") && e.file_name().to_string_lossy().ends_with(".rlib"));
    if !has_rustc_dev {
        let out = Command::new("rustup").args(["component", "add", "rustc-dev", "--toolchain", &name]).stdin(Stdio::null()).output().map_err(|e| format!("cannot run rustup to install rustc-dev: {}", e))?;
        if !out.status.success() {
            return Err(format!("rustup could not install rustc-dev for {}: {}", name, String::from_utf8_lossy(&out.stderr).trim()));
        }
    }
    // A scratch crate: the driver's sources, built with the toolchain's own Cargo.
    let work = util::haul_home().join("drivers").join(format!("build-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(work.join("src")).map_err(|e| e.to_string())?;
    fs::write(work.join("Cargo.toml"), MANIFEST).map_err(|e| e.to_string())?;
    for (name, text) in SOURCES {
        fs::write(work.join("src").join(name), text).map_err(|e| e.to_string())?;
    }
    let lib = toolchain.join("lib");
    let rustflags = format!("-L {} -C link-arg=-Wl,-rpath,{} -Zon-broken-pipe=kill", util::path_str(&lib), util::path_str(&lib));
    let out = Command::new(toolchain.join("bin").join(format!("cargo{}", std::env::consts::EXE_SUFFIX)))
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
    let says = Command::new(&built).arg("-vV").stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
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
