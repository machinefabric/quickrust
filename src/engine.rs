//! The engine: the Cargo of the toolchain a command uses, which QuickRust runs for it with the
//! environment QuickRust gives every build.
//!
//! - Cargo runs as `toolchain::engine_program`, a name it does not take for its own, with `CARGO`
//!   set to QuickRust's `cargo`: Cargo hands that on to everything it starts, so a nested build through
//!   `$CARGO` (Clippy's, nextest's, a build script's, a test's) runs through QuickRust too.
//! - `CARGO_HOME` is QuickRust's (`$QRUST_HOME/cargo`): registry index and sources, git checkouts and
//!   Cargo's own configuration live there for every project on the machine.
//! - `RUSTUP_TOOLCHAIN` names the toolchain, so that a nested `cargo` or `rustc` (a build script,
//!   an external subcommand) reaching QuickRust's proxies uses the same one.
//! - `RUSTC` and `RUSTDOC` are the toolchain's, unless the user set them to something else.
//! - `PATH` starts with QuickRust's `bin`: QuickRust as `cargo`, the toolchain proxies (`PROXIES`) and
//!   installed programs, so that a nested `cargo` (a build script's, an external subcommand's)
//!   runs through QuickRust and a nested `rustfmt` through the same toolchain. The toolchain's own
//!   `bin` comes last.
//! - The toolchain's `lib` comes first on the dynamic library path (`LD_LIBRARY_PATH`;
//!   `DYLD_FALLBACK_LIBRARY_PATH` on macOS), as rustup sets it: Cargo puts the toolchain's
//!   `lib/rustlib/<host>/lib` there for build scripts, and with the `rustc-dev` component that
//!   holds a copy of `librustc_driver` which finds LLVM only through the toolchain's `lib`.
//! - Cargo never deletes cached downloads on its own (`CARGO_CACHE_AUTO_CLEAN_FREQUENCY=never`):
//!   QuickRust manages that space.
//!
//! `+<toolchain>` is consumed here and never reaches Cargo.

use crate::config::Config;
use crate::resolve::Invocation;
use crate::toolchain::{self, Layout, Needs, Selection, Source, Toolchain};
use crate::util;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Marks the toolchain directory QuickRust set `RUSTC`/`RUSTDOC`/`PATH` for, so that a nested
/// invocation can tell QuickRust's settings from the user's.
pub const ENV_ENGINE: &str = "QRUST_ENGINE";

/// The toolchain programs QuickRust's `bin` holds as proxies (links to the `qrust` binary): run as one
/// of these, QuickRust runs that program of the toolchain the directory uses. `cargo` is QuickRust itself.
pub const PROXIES: &[&str] = &["rustc", "rustdoc", "rustfmt", "cargo-fmt", "cargo-clippy", "clippy-driver", "rust-analyzer", "rust-gdb", "rust-gdbgui", "rust-lldb", "cargo-miri"];

/// Runs toolchain program `tool` (see `PROXIES`) with `args`; a leading `+toolchain` chooses the
/// toolchain, as for `cargo`.
pub fn run_proxy(tool: &str, args: Vec<OsString>) -> ! {
    let fail = |e: String| -> ! { crate::session::fail(&e) };
    let cwd = std::env::current_dir().unwrap_or_else(|e| fail(format!("cannot determine the current directory: {}", e)));
    let cfg = Config::load(&cwd).unwrap_or_else(|e| fail(format!("invalid QuickRust configuration: {}", e)));
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

/// QuickRust's Cargo home.
pub fn cargo_home() -> PathBuf {
    util::cargo_home()
}

/// QuickRust's `bin`: its own binaries, the toolchain proxies and installed programs.
pub fn bin_dir() -> PathBuf {
    util::qrust_home().join("bin")
}

/// QuickRust as `cargo`, in its `bin`.
fn qrust_cargo() -> PathBuf {
    util::exe_candidates(&bin_dir(), "cargo").remove(0)
}

#[derive(Debug, Clone)]
pub struct Engine {
    pub toolchain: Toolchain,
    pub selected_by: Source,
    /// The channel that chose the toolchain, when it can move to a newer release
    /// (`stable-<host>`; see `toolchain::start_update`).
    pub channel: Option<String>,
    /// The workspace the command runs in (Cargo's account), for a Cargo command.
    pub workspace_root: Option<PathBuf>,
    /// Its target directory (see `builddir`), for a Cargo command in a workspace.
    pub target: Option<crate::builddir::Target>,
    /// What runs as Cargo (`toolchain::engine_program`).
    pub program: PathBuf,
    pub env: Vec<(String, String)>,
}

impl Engine {
    /// The toolchain's `cargo`.
    pub fn cargo(&self) -> PathBuf {
        self.toolchain.cargo()
    }

    pub fn rustc(&self) -> PathBuf {
        self.toolchain.rustc()
    }

    /// Cargo with `args` (without a leading `+toolchain`) and the engine's environment. Its
    /// `argv[0]` is QuickRust's `cargo`, which Cargo takes for its own path and name (`CARGO`, and
    /// `Usage: cargo build` in its help and errors); Windows has no separate `argv[0]`, and there
    /// Cargo goes by `CARGO` and names itself after the file it runs as.
    pub fn command(&self, args: &[OsString], inv: &Invocation) -> Command {
        let mut cmd = Command::new(&self.program);
        #[cfg(unix)]
        if self.program != self.cargo() {
            std::os::unix::process::CommandExt::arg0(&mut cmd, qrust_cargo());
        }
        cmd.args(&args[usize::from(inv.toolchain.is_some())..]);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }

    /// The toolchain's own Cargo, run as itself with the engine's environment but for `CARGO`:
    /// what it starts calls it back directly (`qrust upstream`).
    pub fn upstream_command(&self, args: &[OsString], inv: &Invocation) -> Command {
        let mut cmd = Command::new(self.cargo());
        cmd.args(&args[usize::from(inv.toolchain.is_some())..]);
        for (k, v) in self.env.iter().filter(|(k, _)| k != "CARGO") {
            cmd.env(k, v);
        }
        cmd
    }

    /// A tool of the toolchain with the engine's environment (`CARGO` only for Cargo's
    /// subcommands, which call Cargo back).
    pub fn tool(&self, name: &str) -> Command {
        let mut cmd = Command::new(self.toolchain.bin(name));
        for (k, v) in self.env.iter().filter(|(k, _)| k != "CARGO" || name.starts_with("cargo-")) {
            cmd.env(k, v);
        }
        cmd
    }
}

/// The engine for a Cargo command run in `cwd`: its toolchain (see `toolchain`), installed with
/// the standard library of every `--target` it builds for, and its workspace's target directory
/// (see `builddir`).
pub fn for_command(inv: &Invocation, cwd: &Path, cfg: &Config) -> Result<Engine, String> {
    let dir = inv.chdir.as_ref().map(|c| cwd.join(c)).unwrap_or_else(|| cwd.to_path_buf());
    let targets = inv.targets.iter().filter(|t| toolchain::spec::is_triple(t)).map(|t| toolchain::spec::rust_target(t).to_string()).collect();
    let mut engine = start(&dir, inv.toolchain.as_deref(), &Needs { targets, ..Default::default() }, cfg)?;
    let root = crate::builddir::workspace_root(&engine, &crate::builddir::start_dir(inv, cwd))?;
    let variant = crate::builddir::variant(cfg)?;
    let target = crate::builddir::choose(inv, cwd, root.as_deref(), variant.as_deref());
    if let Some(t) = &target {
        engine.env.extend(crate::builddir::environment(t));
    }
    engine.workspace_root = root;
    engine.target = target;
    Ok(engine)
}

/// The engine for a toolchain program run through QuickRust's proxy (`rustc`, `rustfmt`, …), with the
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
    toolchain::mark_used(&tc);
    let program = toolchain::engine_program(&tc).map_err(|e| format!("toolchain {}: {}", tc.id, e))?;
    let mut env = environment(&tc);
    if program != tc.cargo() {
        env.push(("CARGO".to_string(), util::path_str(&qrust_cargo())));
    }
    Ok(Engine { channel: toolchain::floating_key(&sel.spec), toolchain: tc, selected_by: sel.source, workspace_root: None, target: None, program, env })
}

/// The variable the toolchain's programs find their shared libraries by: the dynamic library
/// path on Unix; `PATH` on Windows, where a program's DLLs are looked for.
pub const LIBRARY_PATH: &str = if cfg!(windows) {
    "PATH"
} else if cfg!(target_os = "macos") {
    "DYLD_FALLBACK_LIBRARY_PATH"
} else {
    "LD_LIBRARY_PATH"
};

/// Where the shared libraries of the toolchain in `dir` are (`librustc_driver`, LLVM): its `lib`
/// on Unix; on Windows its `bin`, beside its programs.
pub fn library_dir(dir: &Path) -> PathBuf {
    dir.join(if cfg!(windows) { "bin" } else { "lib" })
}

/// A toolchain program (`<toolchain>/bin/<program>`), or a program linked against the toolchain's
/// libraries (QuickRust's compiler driver), run directly: the toolchain's libraries come first on
/// `LIBRARY_PATH`. An inherited path (rustup puts its toolchain's `lib` there for everything it
/// runs, `cargo test` included) could otherwise hand it another toolchain's `librustc_driver` of
/// the same release, and with it that toolchain as its sysroot.
pub fn toolchain_program(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    if let Some(lib) = program.parent().and_then(Path::parent).map(library_dir) {
        cmd.env(LIBRARY_PATH, libraries_first(&lib));
    }
    cmd
}

/// `LIBRARY_PATH` as inherited, with `lib` first.
fn libraries_first(lib: &Path) -> OsString {
    let mut libs = vec![lib.to_path_buf()];
    libs.extend(std::env::var_os(LIBRARY_PATH).iter().flat_map(std::env::split_paths).filter(|d| d != lib));
    std::env::join_paths(libs).unwrap_or_else(|_| lib.as_os_str().to_os_string())
}

/// The environment of commands run with toolchain `tc` (see the module notes).
fn environment(tc: &Toolchain) -> Vec<(String, String)> {
    let inherited_engine = std::env::var_os(ENV_ENGINE).map(PathBuf::from);
    // A variable set by an enclosing QuickRust for its toolchain is QuickRust's to replace; one the user set
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
    // On Windows `LIBRARY_PATH` is `PATH`, which has the toolchain's `bin` (its DLLs) already.
    if !cfg!(windows) {
        let var = LIBRARY_PATH;
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

/// QuickRust's `bin` holds `cargo` and the proxies, each QuickRust itself: made here when missing (or
/// left dangling by a QuickRust binary that is gone), pointing at the running binary. `qrust setup`
/// installs QuickRust there for good.
fn ensure_proxies() {
    let bin = bin_dir();
    let cargo = qrust_cargo();
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
