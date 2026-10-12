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

const MANIFEST: &str = include_str!("../driver/Cargo.toml");
const SOURCE: &str = include_str!("../driver/src/main.rs");
/// The binary's name inside its directory.
const BINARY: &str = "haul-rustc-driver";

/// Where the driver for the compiler with this `rustc -vV` lives, built or not.
pub fn dir(version_verbose: &str) -> Result<PathBuf, String> {
    let field = |k: &str| version_verbose.lines().find_map(|l| l.strip_prefix(k)).map(str::trim).ok_or_else(|| format!("`rustc -vV` has no `{}` line", k.trim_end_matches(": ")));
    let (commit, host) = (field("commit-hash: ")?, field("host: ")?);
    let source = &util::digest_parts([MANIFEST.to_string(), SOURCE.to_string()])[..16];
    Ok(util::haul_home().join("drivers").join(format!("{}-{}-{}", &commit[..commit.len().min(12)], host, source)))
}

/// The driver for the compiler with this `rustc -vV`, when it has been built.
pub fn find(version_verbose: &str) -> Option<PathBuf> {
    let exe = dir(version_verbose).ok()?.join(format!("{}{}", BINARY, std::env::consts::EXE_SUFFIX));
    util::is_executable(&exe).then_some(exe)
}

/// The toolchain directory of a rustup-managed compiler (`…/toolchains/<name>/bin/rustc`).
fn rustup_toolchain(rustc: &Path) -> Option<(PathBuf, String)> {
    let dir = rustc.parent()?.parent()?;
    let name = dir.file_name()?.to_str()?.to_string();
    (dir.parent()?.file_name()? == "toolchains").then(|| (dir.to_path_buf(), name))
}

/// Builds the driver for the compiler at `rustc` (a toolchain binary, not rustup's proxy) and
/// installs it where `find` looks. Installs the toolchain's `rustc-dev` component through rustup
/// when it is missing.
pub fn build(rustc: &Path) -> Result<PathBuf, String> {
    let vv = crate::rustc_info::version_verbose(&util::haul_home().join("rustc"), &util::path_str(rustc))?;
    let target = dir(&vv)?;
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
    fs::write(work.join("src/main.rs"), SOURCE).map_err(|e| e.to_string())?;
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
    fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    let staged = target.join(format!(".{}.{}", BINARY, std::process::id()));
    fs::copy(&built, &staged).and_then(|_| fs::rename(&staged, &exe)).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&work);
    Ok(exe)
}
