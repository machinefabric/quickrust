//! The sealed environment every timed command runs in.
//!
//! Commands start from an empty environment. The toolchain is pinned (`--toolchain`) and installed
//! by Haul into the work directory's toolchain store, which every Haul home of the run links;
//! plain Cargo runs that toolchain's own `cargo` directly. Cargo gets a Cargo home without a
//! `config.toml` (`<work>/cargo-home`, which every Haul home of the run links as its own) and an
//! explicit target directory, so nothing from the machine's own configuration (a global
//! `build.target-dir`, a rustc wrapper, profile overrides) can leak into a measurement or let two
//! variants share build output.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Sealed {
    pub cargo_home: PathBuf,
    /// Haul's toolchain store (`toolchains/` of a Haul home), shared by every Haul home of the run.
    pub toolchains: PathBuf,
    /// The pinned toolchain's directory.
    pub toolchain_dir: PathBuf,
    /// The pinned toolchain's own Cargo.
    pub cargo: PathBuf,
    /// The pinned toolchain, as given.
    pub toolchain: String,
    path: String,
    home: String,
}

impl Sealed {
    /// Installs the pinned toolchain with `haul` (into `<work>/toolchain-home`) and seals the
    /// environment around it.
    pub fn new(work: &Path, toolchain: &str, haul: &Path) -> Result<Sealed, String> {
        let home = std::env::var("HOME").map_err(|_| "HOME is not set")?;
        let toolchain_home = work.join("toolchain-home");
        let out = Command::new(haul)
            .args(["toolchain", "which", "--toolchain", toolchain])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HAUL_HOME", &toolchain_home)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("cannot run {}: {}", haul.display(), e))?;
        if !out.status.success() {
            return Err(format!("Haul could not install toolchain {}", toolchain));
        }
        let toolchain_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        // Only the toolchain and ordinary system tools; never a Haul shim.
        let dirs = [toolchain_dir.join("bin"), PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin"), PathBuf::from("/bin")];
        let path = std::env::join_paths(dirs).map_err(|e| e.to_string())?.to_string_lossy().into_owned();
        Ok(Sealed { cargo_home: work.join("cargo-home"), toolchains: toolchain_home.join("toolchains"), cargo: toolchain_dir.join("bin").join("cargo"), toolchain_dir, toolchain: toolchain.to_string(), path, home })
    }

    /// Makes `home` a Haul home of this run: it links the shared toolchain store and Cargo home.
    pub fn haul_home(&self, home: &Path) -> Result<(), String> {
        std::fs::create_dir_all(home).map_err(|e| format!("cannot create {}: {}", home.display(), e))?;
        std::fs::create_dir_all(&self.cargo_home).map_err(|e| format!("cannot create {}: {}", self.cargo_home.display(), e))?;
        for (link, to) in [(home.join("toolchains"), &self.toolchains), (home.join("cargo"), &self.cargo_home)] {
            if std::fs::symlink_metadata(&link).is_err() {
                std::os::unix::fs::symlink(to, &link).map_err(|e| format!("cannot link {}: {}", link.display(), e))?;
            }
        }
        Ok(())
    }

    /// A command with the sealed environment. `online` only for fetching.
    pub fn command(&self, program: &Path, cwd: &Path, target_dir: &Path, online: bool) -> Command {
        let mut c = Command::new(program);
        c.current_dir(cwd).stdin(Stdio::null()).env_clear();
        for (k, v) in [
            ("PATH", self.path.as_str()),
            ("HOME", self.home.as_str()),
            ("LANG", "C.UTF-8"),
            // Haul selects the pinned toolchain by it; plain Cargo runs that toolchain's binary.
            ("RUSTUP_TOOLCHAIN", self.toolchain.as_str()),
            ("CARGO_TERM_COLOR", "never"),
            ("CARGO_TERM_PROGRESS_WHEN", "never"),
            ("CARGO_CACHE_AUTO_CLEAN_FREQUENCY", "never"),
        ] {
            c.env(k, v);
        }
        c.env("CARGO_HOME", &self.cargo_home).env("RUSTC", self.toolchain_dir.join("bin/rustc")).env("RUSTDOC", self.toolchain_dir.join("bin/rustdoc")).env("CARGO_TARGET_DIR", target_dir);
        if !online {
            c.env("CARGO_NET_OFFLINE", "true");
        }
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            c.env("TMPDIR", tmp);
        }
        c
    }

    /// First line of `program -V` of the pinned toolchain.
    pub fn version(&self, program: &str, cwd: &Path) -> String {
        let p = self.toolchain_dir.join("bin").join(program);
        self.command(&p, cwd, &cwd.join("target"), false)
            .arg("-V")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "unavailable".into())
    }
}

/// Cargo reads `.cargo/config.toml` from every ancestor of the working directory. The work
/// directory must have none, or the machine's configuration would reach the measurements.
pub fn check_no_ancestor_config(work: &Path) -> Result<(), String> {
    for dir in work.ancestors() {
        for name in ["config.toml", "config"] {
            let p = dir.join(".cargo").join(name);
            if p.is_file() {
                return Err(format!("{} would apply to every build under {}; choose another --work-dir", p.display(), work.display()));
            }
        }
    }
    Ok(())
}
