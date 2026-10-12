//! The engine: the Cargo of the toolchain a command uses, which Haul runs for it with the
//! environment Haul gives every build.
//!
//! - `CARGO_HOME` is Haul's (`$HAUL_HOME/cargo`): registry index and sources, git checkouts and
//!   Cargo's own configuration live there for every project on the machine.
//! - `RUSTUP_TOOLCHAIN` names the toolchain, so that a nested `cargo` or `rustc` (a build script,
//!   an external subcommand) reaching Haul's proxies uses the same one.
//! - `RUSTC` and `RUSTDOC` are the toolchain's, unless the user set them to something else.
//! - `PATH` starts with Haul's `bin`: Haul as `cargo`, the toolchain proxies (`PROXIES`) and
//!   installed programs, so that a nested `cargo` (a build script's, an external subcommand's)
//!   runs through Haul and a nested `rustfmt` through the same toolchain. The toolchain's own
//!   `bin` comes last.
//! - The toolchain's `lib` comes first on the dynamic library path (`LD_LIBRARY_PATH`;
//!   `DYLD_FALLBACK_LIBRARY_PATH` on macOS), as rustup sets it: Cargo puts the toolchain's
//!   `lib/rustlib/<host>/lib` there for build scripts, and with the `rustc-dev` component that
//!   holds a copy of `librustc_driver` which finds LLVM only through the toolchain's `lib`.
//! - Cargo never deletes cached downloads on its own (`CARGO_CACHE_AUTO_CLEAN_FREQUENCY=never`):
//!   Haul manages that space.
//!
//! `+<toolchain>` is consumed here and never reaches Cargo.

use crate::config::Config;
use crate::resolve::Invocation;
use crate::toolchain::{self, Layout, Needs, Selection, Source, Toolchain};
use crate::util;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Marks the toolchain directory Haul set `RUSTC`/`RUSTDOC`/`PATH` for, so that a nested
/// invocation can tell Haul's settings from the user's.
pub const ENV_ENGINE: &str = "HAUL_ENGINE";

/// The toolchain programs Haul's `bin` holds as proxies (links to the `haul` binary): run as one
/// of these, Haul runs that program of the toolchain the directory uses. `cargo` is Haul itself.
pub const PROXIES: &[&str] = &["rustc", "rustdoc", "rustfmt", "cargo-fmt", "cargo-clippy", "clippy-driver", "rust-analyzer", "rust-gdb", "rust-gdbgui", "rust-lldb", "cargo-miri"];

/// Runs toolchain program `tool` (see `PROXIES`) with `args`; a leading `+toolchain` chooses the
/// toolchain, as for `cargo`.
pub fn run_proxy(tool: &str, args: Vec<OsString>) -> ! {
    let fail = |e: String| -> ! { crate::session::fail(&e) };
    let cwd = std::env::current_dir().unwrap_or_else(|e| fail(format!("cannot determine the current directory: {}", e)));
    let cfg = Config::load(&cwd).unwrap_or_else(|e| fail(format!("invalid Haul configuration: {}", e)));
    let plus = args.first().and_then(|a| a.to_str()).and_then(|a| a.strip_prefix('+')).map(str::to_string);
    let engine = for_tool(tool, plus.as_deref(), &cwd, &cfg).unwrap_or_else(|e| fail(e));
    let program = engine.toolchain.bin(tool);
    if !program.is_file() {
        fail(format!("toolchain {} has no {}", engine.toolchain.id, tool));
    }
    let mut cmd = engine.tool(tool);
    cmd.args(&args[usize::from(plus.is_some())..]);
    let err = crate::proc::exec(&mut cmd);
    fail(format!("could not run {}: {}", program.display(), err))
}

/// Haul's Cargo home.
pub fn cargo_home() -> PathBuf {
    util::cargo_home()
}

/// Haul's `bin`: its own binaries, the toolchain proxies and installed programs.
pub fn bin_dir() -> PathBuf {
    util::haul_home().join("bin")
}

#[derive(Debug, Clone)]
pub struct Engine {
    pub toolchain: Toolchain,
    pub selected_by: Source,
    pub env: Vec<(String, String)>,
}

impl Engine {
    pub fn cargo(&self) -> PathBuf {
        self.toolchain.cargo()
    }

    pub fn rustc(&self) -> PathBuf {
        self.toolchain.rustc()
    }

    /// Cargo with `args` (without a leading `+toolchain`) and the engine's environment.
    pub fn command(&self, args: &[OsString], inv: &Invocation) -> Command {
        let mut cmd = Command::new(self.cargo());
        cmd.args(&args[usize::from(inv.toolchain.is_some())..]);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }

    /// A tool of the toolchain with the engine's environment.
    pub fn tool(&self, name: &str) -> Command {
        let mut cmd = Command::new(self.toolchain.bin(name));
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }
}

/// The engine for a Cargo command run in `cwd`: its toolchain (see `toolchain`), installed with
/// the standard library of every `--target` it builds for.
pub fn for_command(inv: &Invocation, cwd: &Path, cfg: &Config) -> Result<Engine, String> {
    let dir = inv.chdir.as_ref().map(|c| cwd.join(c)).unwrap_or_else(|| cwd.to_path_buf());
    let targets = inv.targets.iter().filter(|t| toolchain::spec::is_triple(t)).cloned().collect();
    start(&dir, inv.toolchain.as_deref(), &Needs { targets, ..Default::default() }, cfg)
}

/// The engine for a toolchain program run through Haul's proxy (`rustc`, `rustfmt`, …), with the
/// component that provides it.
pub fn for_tool(tool: &str, plus: Option<&str>, cwd: &Path, cfg: &Config) -> Result<Engine, String> {
    let component = match tool {
        "rustfmt" | "cargo-fmt" => Some("rustfmt"),
        "cargo-clippy" | "clippy-driver" => Some("clippy"),
        "rust-analyzer" => Some("rust-analyzer"),
        "cargo-miri" | "miri" => Some("miri"),
        _ => None,
    };
    start(cwd, plus, &Needs { components: component.into_iter().map(str::to_string).collect(), ..Default::default() }, cfg)
}

fn start(dir: &Path, plus: Option<&str>, needs: &Needs, cfg: &Config) -> Result<Engine, String> {
    let layout = Layout::current();
    let sel: Selection = toolchain::select(&layout, dir, plus)?;
    let mut progress = crate::ui::ToolchainProgress::new(&sel.spec.to_string());
    ensure_proxies();
    let tc = match toolchain::ensure(&layout, &sel, needs, cfg.str("toolchain.dist-server"), &mut progress) {
        Ok(tc) => tc,
        Err(e) => {
            progress.fail(&e);
            return Err(format!("toolchain {} (from {}): {}", sel.spec, sel.source, e));
        }
    };
    progress.finish(&tc);
    let env = environment(&tc);
    Ok(Engine { toolchain: tc, selected_by: sel.source, env })
}

/// The environment of commands run with toolchain `tc` (see the module notes).
fn environment(tc: &Toolchain) -> Vec<(String, String)> {
    let inherited_engine = std::env::var_os(ENV_ENGINE).map(PathBuf::from);
    // A variable set by an enclosing Haul for its toolchain is Haul's to replace; one the user set
    // to something else stays.
    let ours = |var: &str| match std::env::var_os(var).filter(|v| !v.is_empty()) {
        None => true,
        Some(v) => inherited_engine.as_ref().is_some_and(|e| Path::new(&v).starts_with(e)),
    };
    let mut env = vec![
        ("CARGO_HOME".to_string(), util::path_str(&cargo_home())),
        ("RUSTUP_TOOLCHAIN".to_string(), tc.id.clone()),
        ("CARGO_CACHE_AUTO_CLEAN_FREQUENCY".to_string(), "never".to_string()),
        (ENV_ENGINE.to_string(), util::path_str(&tc.dir)),
    ];
    for (var, tool) in [("RUSTC", "rustc"), ("RUSTDOC", "rustdoc")] {
        if ours(var) {
            env.push((var.to_string(), util::path_str(&tc.bin(tool))));
        }
    }
    let previous_bin = inherited_engine.map(|e| e.join("bin"));
    let mut path: Vec<PathBuf> = vec![bin_dir()];
    if let Some(p) = std::env::var_os("PATH") {
        path.extend(std::env::split_paths(&p).filter(|d| *d != bin_dir() && *d != tc.dir.join("bin") && Some(d) != previous_bin.as_ref()));
    }
    path.push(tc.dir.join("bin"));
    if let Ok(joined) = std::env::join_paths(path) {
        env.push(("PATH".to_string(), joined.to_string_lossy().into_owned()));
    }
    if !cfg!(windows) {
        let var = if cfg!(target_os = "macos") { "DYLD_FALLBACK_LIBRARY_PATH" } else { "LD_LIBRARY_PATH" };
        let previous_lib = std::env::var_os(ENV_ENGINE).map(|e| PathBuf::from(e).join("lib"));
        let mut libs = vec![tc.dir.join("lib")];
        match std::env::var_os(var).filter(|v| !v.is_empty()) {
            Some(v) => libs.extend(std::env::split_paths(&v).filter(|d| *d != tc.dir.join("lib") && Some(d) != previous_lib.as_ref())),
            // macOS's own default, which setting the variable would otherwise replace.
            None if cfg!(target_os = "macos") => libs.extend(util::home_dir().map(|h| h.join("lib")).into_iter().chain(["/usr/local/lib", "/usr/lib"].iter().map(PathBuf::from))),
            None => {}
        }
        if let Ok(joined) = std::env::join_paths(libs) {
            env.push((var.to_string(), joined.to_string_lossy().into_owned()));
        }
    }
    env
}

/// Haul's `bin` holds `cargo` and the proxies, each Haul itself: made here when missing (or
/// left dangling by a Haul binary that is gone), pointing at the running binary. `haul setup`
/// installs Haul there for good.
fn ensure_proxies() {
    let bin = bin_dir();
    let cargo = util::exe_candidates(&bin, "cargo").remove(0);
    if std::fs::metadata(&cargo).is_ok() {
        return;
    }
    let (me, _) = crate::resolve::self_identity();
    if util::create_private_dir(&bin).is_err() {
        return;
    }
    for name in std::iter::once("cargo").chain(PROXIES.iter().copied()) {
        let link = util::exe_candidates(&bin, name).remove(0);
        // Made where nothing is, or where a link leads nowhere; anything else is left alone.
        let dangling = std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) && std::fs::metadata(&link).is_err();
        if std::fs::symlink_metadata(&link).is_ok() && !dangling {
            continue;
        }
        let staged = bin.join(format!(".{}.{}", name, std::process::id()));
        let _ = std::fs::remove_file(&staged);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&me, &staged);
        #[cfg(not(unix))]
        let made = std::fs::hard_link(&me, &staged);
        if made.and_then(|_| std::fs::rename(&staged, &link)).is_err() {
            let _ = std::fs::remove_file(&staged);
        }
    }
}
