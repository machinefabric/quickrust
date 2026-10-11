//! The sealed environment every timed command runs in.
//!
//! Commands start from an empty environment. Cargo gets its own `CARGO_HOME` without a
//! `config.toml`, an explicit target directory and a pinned toolchain, so nothing from the
//! machine's own Cargo configuration (a global `build.target-dir`, a rustc wrapper, profile
//! overrides) can leak into a measurement or let two variants share build output.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Sealed {
    pub cargo_home: PathBuf,
    pub rustup_home: PathBuf,
    /// rustup's `cargo` proxy: upstream Cargo for the pinned toolchain.
    pub cargo: PathBuf,
    pub toolchain: String,
    path: String,
    home: String,
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

impl Sealed {
    pub fn new(work: &Path, toolchain: &str) -> Result<Sealed, String> {
        let home = std::env::var("HOME").map_err(|_| "HOME is not set")?;
        let rustup = which("rustup").ok_or("rustup is not on PATH; the benchmark pins toolchains through it")?;
        let cargo = rustup.with_file_name("cargo");
        if !cargo.is_file() {
            return Err(format!("no rustup cargo proxy next to {}", rustup.display()));
        }
        let rustup_home = std::env::var_os("RUSTUP_HOME").map(PathBuf::from).unwrap_or_else(|| Path::new(&home).join(".rustup"));
        // Only directories that hold rustup's proxies and ordinary system tools; never a Haul shim.
        let mut dirs = vec![rustup.parent().unwrap_or(Path::new("/")).to_path_buf()];
        dirs.extend(["/usr/local/bin", "/usr/bin", "/bin"].iter().map(PathBuf::from));
        let path = std::env::join_paths(dirs).map_err(|e| e.to_string())?.to_string_lossy().into_owned();
        Ok(Sealed { cargo_home: work.join("cargo-home"), rustup_home, cargo, toolchain: toolchain.to_string(), path, home })
    }

    /// A command with the sealed environment. `online` only for fetching.
    pub fn command(&self, program: &Path, cwd: &Path, target_dir: &Path, online: bool) -> Command {
        let mut c = Command::new(program);
        c.current_dir(cwd).stdin(Stdio::null()).env_clear();
        for (k, v) in [
            ("PATH", self.path.as_str()),
            ("HOME", self.home.as_str()),
            ("LANG", "C.UTF-8"),
            ("RUSTUP_TOOLCHAIN", self.toolchain.as_str()),
            ("CARGO_TERM_COLOR", "never"),
            ("CARGO_TERM_PROGRESS_WHEN", "never"),
            ("CARGO_CACHE_AUTO_CLEAN_FREQUENCY", "never"),
        ] {
            c.env(k, v);
        }
        c.env("RUSTUP_HOME", &self.rustup_home).env("CARGO_HOME", &self.cargo_home).env("CARGO_TARGET_DIR", target_dir);
        if !online {
            c.env("CARGO_NET_OFFLINE", "true");
        }
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            c.env("TMPDIR", tmp);
        }
        c
    }

    /// First line of `program -V` under the sealed environment.
    pub fn version(&self, program: &str, cwd: &Path) -> String {
        let p = self.cargo.with_file_name(program);
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
