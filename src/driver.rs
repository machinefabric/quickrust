//! QuickRust's compiler driver (`driver/` in this repository): rustc itself — the toolchain's own
//! `librustc_driver`, its LLVM, its release — run through QuickRust's callbacks, which is how QuickRust
//! changes how the compiler works without changing what it builds.
//!
//! The compiler's internal API differs between releases, so the driver is built for each
//! toolchain, against that toolchain's `rustc-dev` component, and only used once it reports
//! exactly the compiler's own `rustc -vV`. It lives in `~/.qrust/cache/drivers/<key>/`, the key naming
//! the compiler (commit, host), its toolchain and the driver's source.
//!
//! Only a build that compiles with it builds it, and that build says how it went. A toolchain the
//! driver cannot be built for keeps the stock compiler, and the reason is recorded in
//! `drivers/could-not-build/`, once per compiler, toolchain and QuickRust version: builds do not
//! try again until one of them changes (`qrust driver build` does).

use crate::util;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Set for the driver: compute the crate hash from what dependents can observe.
pub const ENV_INTERFACE_HASH: &str = "QRUST_DRIVER_INTERFACE_HASH";
/// Set for the driver: the descriptor of a pipe on which it says its outputs are written.
pub const ENV_READY_FD: &str = "QRUST_DRIVER_READY_FD";
/// Set for the driver: where to keep the objects of a unit QuickRust relinks.
pub const ENV_KEEP_OBJECTS: &str = "QRUST_DRIVER_KEEP_OBJECTS";

const MANIFEST: &str = include_str!("../driver/Cargo.toml");
const SOURCES: [(&str, &str); 2] = [("main.rs", include_str!("../driver/src/main.rs")), ("early_link.rs", include_str!("../driver/src/early_link.rs"))];
/// The binary's name inside its directory.
const BINARY: &str = "qrust-rustc-driver";
/// Releases whose compiler interface the driver's sources adapt to: the driver is built with
/// `--cfg qrust_rustc_ge_<minor>` for each one its compiler is (rustc's internal interface changes
/// between releases).
const INTERFACE_BREAKS: &[u32] = &[99, 100];
/// How a failure to get the toolchain's `rustc-dev` component begins.
const NO_RUSTC_DEV: &str = "rustc-dev cannot be installed";
/// In a driver's directory: the toolchain it was built for (`{"sysroot": …}`).
pub const COMPILER: &str = "compiler.json";
/// In `drivers/`: why a driver could not be built, one record per compiler, toolchain and QuickRust
/// version (`failure_key`).
pub const COULD_NOT_BUILD: &str = "could-not-build";
/// The version of QuickRust whose driver a failure record is about.
const QUICKRUST: &str = env!("CARGO_PKG_VERSION");

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

/// The release a compiler names (`1.100.0`), or its first line when it names none.
fn release(version_verbose: &str) -> String {
    version_verbose.lines().find_map(|l| l.strip_prefix("release: ")).or_else(|| version_verbose.lines().next()).unwrap_or_default().trim().to_string()
}

/// The toolchain a compiler is in, by name: its directory's (`1.100.0-x86_64-unknown-linux-gnu`,
/// a linked toolchain's directory). The same wherever QuickRust's home is.
fn toolchain_name(compiler: &Compiler) -> String {
    util::lossy(compiler.sysroot.file_name().unwrap_or(compiler.sysroot.as_os_str()))
}

/// Which failure record answers for this compiler: its own account, its toolchain's name, and the
/// driver QuickRust `quickrust` builds (its version and the driver's source). Not where the
/// toolchain is: a home that moved (a cache carried to another machine) has the same toolchains,
/// and the driver fails for them as it did.
fn failure_key(version_verbose: &str, toolchain: &str, quickrust: &str) -> Result<String, String> {
    let field = |k: &str| version_verbose.lines().find_map(|l| l.strip_prefix(k)).map(str::trim).ok_or_else(|| format!("`rustc -vV` has no `{}` line", k.trim_end_matches(": ")));
    let (commit, host) = (field("commit-hash: ")?, field("host: ")?);
    let digest = util::digest_parts([version_verbose.to_string(), toolchain.to_string(), quickrust.to_string(), MANIFEST.to_string()].into_iter().chain(SOURCES.iter().flat_map(|(name, text)| [name.to_string(), text.to_string()])));
    Ok(format!("{}-{}-{}", &commit[..commit.len().min(12)], host, &digest[..16]))
}

/// Where the failure record for this compiler is, built or not.
fn failure_path(compiler: &Compiler) -> Result<PathBuf, String> {
    Ok(util::cache_dir().join("drivers").join(COULD_NOT_BUILD).join(failure_key(&compiler.version_verbose, &toolchain_name(compiler), QUICKRUST)?))
}

/// Why the driver could not be built for a compiler, as recorded.
#[derive(Clone, Debug)]
pub struct Failure {
    /// The compiler's release (`1.100.0`).
    pub release: String,
    /// Its toolchain, by name.
    pub toolchain: String,
    /// Everything the build said about it.
    pub error: String,
    /// Where it is recorded.
    pub record: PathBuf,
}

impl Failure {
    /// The error in a line: its first, and the first error the compiler reported with how many
    /// more there were (a driver that does not compile says dozens of lines).
    pub fn summary(&self) -> String {
        summarize(&self.error)
    }
}

fn summarize(error: &str) -> String {
    let head = error.lines().next().unwrap_or_default().trim();
    let errors: Vec<&str> = error.lines().skip(1).map(str::trim).filter(|l| (l.starts_with("error[") || l.starts_with("error:")) && !l.starts_with("error: could not compile") && !l.starts_with("error: aborting")).collect();
    match errors.split_first() {
        None => head.to_string(),
        Some((first, [])) => format!("{} {}", head, first),
        Some((first, rest)) => format!("{} {}, and {} more error{}", head, first, rest.len(), if rest.len() == 1 { "" } else { "s" }),
    }
}

/// Why the driver for this compiler could not be built, when this QuickRust recorded that it could
/// not.
pub fn build_failure(compiler: &Compiler) -> Option<Failure> {
    let record = failure_path(compiler).ok()?;
    let v = util::read_json(&record)?;
    let field = |k: &str| v[k].as_str().map(str::to_string);
    Some(Failure { release: field("release")?, toolchain: field("toolchain")?, error: field("error")?, record })
}

/// How getting the driver for a compiler went.
pub enum Outcome {
    /// It was built before.
    Found(PathBuf),
    /// It is built now, for this release.
    Built(PathBuf, String),
    /// It could not be built now.
    Failed(Failure),
    /// It could not be built before, by this QuickRust for this compiler: not tried again.
    FailedBefore(Failure),
}

/// The driver for the compiler at `rustc` (a toolchain's own binary): the one built, or built now
/// and installed where `find` looks, one build at a time. Adds the `rustc-dev` component to a
/// toolchain QuickRust installed when it is missing; any other compiler must have it. A failure is
/// recorded; one recorded before is not tried again unless `force` (`qrust driver build`).
pub fn obtain(rustc: &Path, force: bool) -> Result<Outcome, String> {
    let compiler = Compiler::of(&util::path_str(rustc))?;
    let target = dir(&compiler)?;
    let record = failure_path(&compiler)?;
    // Asked before the lock, so that a build finding either leaves the cache as it is, and again
    // under it: another build may have just built it, or failed to.
    let known = || match (find(&compiler), build_failure(&compiler)) {
        (Some(exe), _) => Some(Outcome::Found(exe)),
        (None, Some(failure)) if !force => Some(Outcome::FailedBefore(failure)),
        _ => None,
    };
    if let Some(outcome) = known() {
        return Ok(outcome);
    }
    util::create_private_dir(&target).map_err(|e| format!("cannot create {}: {}", target.display(), e))?;
    let _lock = util::ExclusiveLock::acquire(&target.join(".build.lock")).map_err(|e| format!("cannot lock {}: {}", target.display(), e))?;
    if let Some(outcome) = known() {
        return Ok(outcome);
    }
    match build_locked(rustc, &compiler.version_verbose, &target) {
        Ok(exe) => {
            // Which toolchain it is for: once that is gone, QuickRust's cache removes the driver.
            util::write_json_fast(&target.join(COMPILER), &serde_json::json!({ "sysroot": util::path_str(&compiler.sysroot) })).map_err(|e| format!("cannot write {}: {}", target.join(COMPILER).display(), e))?;
            match fs::remove_file(&record) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("cannot remove {}: {}", record.display(), e)),
                _ => {}
            }
            Ok(Outcome::Built(exe, release(&compiler.version_verbose)))
        }
        Err(error) => {
            let failure = Failure { release: release(&compiler.version_verbose), toolchain: toolchain_name(&compiler), error, record: record.clone() };
            let json = serde_json::json!({ "quickrust": QUICKRUST, "release": failure.release, "toolchain": failure.toolchain, "sysroot": util::path_str(&compiler.sysroot), "error": failure.error });
            util::create_private_dir(record.parent().unwrap_or(&record)).and_then(|_| util::write_json_fast(&record, &json)).map_err(|e| format!("cannot record why the driver could not be built in {}: {}", record.display(), e))?;
            Ok(Outcome::Failed(failure))
        }
    }
}

/// Whether builds under `cfg` compile with the driver (early cut-off: fast mode with the artifact
/// cache, `fast.early-cutoff = "auto"`).
pub fn wanted(cfg: &crate::config::Config) -> bool {
    cfg.str("mode") == "fast" && cfg.str("cache.artifacts") == "on" && cfg.str("fast.early-cutoff") == "auto"
}

/// The driver for the compiler at `rustc`, got now for a command about to compile with it (or to
/// prepare a toolchain for builds), shown as a phase; the command's output says how it went. A
/// recorded failure is not tried again (`obtain`).
pub fn prepare(rustc: &Path) -> Option<PathBuf> {
    let mut phase = crate::ui::Phase::start();
    if let Ok(c) = Compiler::of(&util::path_str(rustc)) {
        if let Some(exe) = find(&c) {
            return Some(exe);
        }
        phase.status(&format!("Preparing QuickRust's compiler driver for Rust {}", release(&c.version_verbose)), true);
    }
    let outcome = obtain(rustc, false);
    phase.clear_line();
    report(&outcome, phase.elapsed_ns());
    match outcome {
        Ok(Outcome::Found(exe) | Outcome::Built(exe, _)) => Some(exe),
        _ => None,
    }
}

/// Says how getting the driver went, in the output of the command that needed it.
pub fn report(outcome: &Result<Outcome, String>, elapsed_ns: u64) {
    use crate::ui::{message, say, Tag, INDENT};
    match outcome {
        Ok(Outcome::Found(_)) => {}
        Ok(Outcome::Built(_, release)) => say(Tag::Ok, &message(&format!("Compiler driver for Rust {} ready", release), &[], Some(elapsed_ns))),
        Ok(Outcome::Failed(f)) => {
            say(Tag::Warn, &format!("QuickRust's compiler driver could not be built for Rust {} ({}), so builds with it have no early cut-off: {}", f.release, f.toolchain, f.summary()));
            eprintln!("{}the whole report is in {}; builds try again with another Rust or another QuickRust, and `qrust driver build` tries now", INDENT, util::tilde(&f.record));
        }
        Ok(Outcome::FailedBefore(f)) => say(Tag::Note, &format!("QuickRust's compiler driver could not be built for Rust {} ({}), so this build has no early cut-off: `qrust driver status` says why, and `qrust driver build` tries again", f.release, f.toolchain)),
        Err(e) => say(Tag::Warn, &format!("QuickRust's compiler driver cannot be built, so this build has no early cut-off: {}", e)),
    }
}

fn build_locked(rustc: &Path, vv: &str, target: &Path) -> Result<PathBuf, String> {
    let vv = vv.to_string();
    let toolchain = rustc.parent().and_then(Path::parent).ok_or_else(|| format!("{} is not in a toolchain's bin directory", rustc.display()))?.to_path_buf();
    let name = util::lossy(toolchain.file_name().unwrap_or_default());
    // The toolchain's rustc-dev component: added to a toolchain QuickRust installed (from its own
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
                return Err(format!("{} for {}: the toolchain does not have it, and QuickRust only adds components to toolchains it installed", NO_RUSTC_DEV, name));
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
    let cfgs: String = INTERFACE_BREAKS.iter().map(|b| format!(" --check-cfg cfg(qrust_rustc_ge_{b}){}", if minor >= *b { format!(" --cfg qrust_rustc_ge_{b}") } else { String::new() })).collect();
    // It finds the toolchain's libraries by an rpath on Unix; on Windows by `PATH` (see
    // `engine::LIBRARY_PATH`), which Cargo's environment and `run_against` give it.
    let rpath = if cfg!(windows) { String::new() } else { format!(" -C link-arg=-Wl,-rpath,{}", util::path_str(&lib)) };
    let rustflags = format!("-L {}{} -Zon-broken-pipe=kill{}", util::path_str(&lib), rpath, cfgs);
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
        return Err(format!("the driver does not compile against {}:\n{}", name, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let built = work.join("target").join("release").join(format!("{}{}", BINARY, std::env::consts::EXE_SUFFIX));
    // It must be the compiler it was built against, by its own account.
    let says = run_against(&built, rustc).arg("-vV").stdin(Stdio::null()).output().map_err(|e| format!("cannot run the driver built for {}: {}", name, e))?;
    if !says.status.success() {
        let _ = fs::remove_dir_all(&work);
        return Err(format!("the driver built for {} does not run ({}): {}", name, says.status, String::from_utf8_lossy(&says.stderr).trim()));
    }
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

/// The driver at `exe`, run as the compiler at `rustc` is run: with that toolchain's libraries
/// first on `engine::LIBRARY_PATH`.
fn run_against(exe: &Path, rustc: &Path) -> Command {
    let mut cmd = Command::new(exe);
    if let Some((_, Some(lib))) = crate::engine::toolchain_program(rustc).get_envs().find(|(k, _)| *k == crate::engine::LIBRARY_PATH) {
        cmd.env(crate::engine::LIBRARY_PATH, lib);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    // TEST0050: Two toolchains of one release get drivers of their own
    #[test]
    fn test0050_two_toolchains_of_one_release_get_drivers_of_their_own() {
        let vv = "rustc 1.98.1 (48a229cea 2026-09-01)\nbinary: rustc\ncommit-hash: 48a229ceaefd4985c50990b14116b6d856af0985\nhost: x86_64-unknown-linux-gnu\nrelease: 1.98.1\n".to_string();
        let at = |sysroot: &str| dir(&Compiler { version_verbose: vv.clone(), sysroot: PathBuf::from(sysroot) }).unwrap();
        assert_ne!(at("/qrust/toolchains/1.98.1-x86_64-unknown-linux-gnu"), at("/qrust/toolchains/dev"));
        assert_eq!(at("/qrust/toolchains/1.98.1-x86_64-unknown-linux-gnu"), at("/qrust/toolchains/1.98.1-x86_64-unknown-linux-gnu"));
    }

    // TEST0357: A driver that could not be built is answered for by its compiler, toolchain and QuickRust, not by where they are
    #[test]
    fn test0357_a_driver_that_could_not_be_built_is_answered_for_by_its_compiler_toolchain_and_quickrust() {
        let vv = |commit: &str, release: &str| format!("rustc {release} (0ef1bc386 2026-11-09)\nbinary: rustc\ncommit-hash: {commit}\ncommit-date: 2026-11-09\nhost: x86_64-unknown-linux-gnu\nrelease: {release}\nLLVM version: 21.1.3\n");
        let v100 = vv("0ef1bc3860599d3b8f41c6c42c3e81d1d7e9b2a4", "1.100.0");
        let key = failure_key(&v100, "1.100.0-x86_64-unknown-linux-gnu", "1.190.0").unwrap();
        assert!(key.starts_with("0ef1bc386059-x86_64-unknown-linux-gnu-"), "{}", key);
        // The same toolchain in a home that moved: its record is found where it was made. The
        // driver itself is not shared (it loads the toolchain's libraries by their path).
        let compiler = |sysroot: &str| Compiler { version_verbose: v100.clone(), sysroot: PathBuf::from(sysroot) };
        let (here, moved) = (compiler("/home/a/.qrust/toolchains/1.100.0-x86_64-unknown-linux-gnu"), compiler("/home/b/.qrust/toolchains/1.100.0-x86_64-unknown-linux-gnu"));
        assert_eq!(failure_key(&here.version_verbose, &toolchain_name(&here), QUICKRUST).unwrap(), failure_key(&moved.version_verbose, &toolchain_name(&moved), QUICKRUST).unwrap());
        assert_ne!(dir(&here).unwrap(), dir(&moved).unwrap());
        // Another QuickRust, another compiler, or another toolchain of the same release: tried again.
        assert_ne!(failure_key(&v100, "1.100.0-x86_64-unknown-linux-gnu", "1.190.1").unwrap(), key);
        assert_ne!(failure_key(&vv("0ef1bc3860599d3b8f41c6c42c3e81d1d7e9b2a5", "1.100.1"), "1.100.0-x86_64-unknown-linux-gnu", "1.190.0").unwrap(), key);
        assert_ne!(failure_key(&v100, "dev", "1.190.0").unwrap(), key);
        assert!(failure_key("rustc 1.100.0\n", "x", "1.190.0").unwrap_err().contains("commit-hash"));
    }

    // TEST0358: A driver that does not compile is summed up by its first error and how many more there are
    #[test]
    fn test0358_a_driver_that_does_not_compile_is_summed_up_by_its_first_error_and_how_many_more() {
        let error = "the driver does not compile against 1.100.0-x86_64-unknown-linux-gnu:\nerror[E0407]: method `replaced_intrinsics` is not a member of trait `CodegenBackend`\n  --> src/early_link.rs:120:5\n    |\nwarning: unused import\nerror[E0603]: enum `CrateType` is private\nerror[E0053]: method `init` has an incompatible type for trait\nerror: aborting due to 3 previous errors\nerror: could not compile `qrust-rustc-driver` (bin \"qrust-rustc-driver\")";
        assert_eq!(summarize(error), "the driver does not compile against 1.100.0-x86_64-unknown-linux-gnu: error[E0407]: method `replaced_intrinsics` is not a member of trait `CodegenBackend`, and 2 more errors");
        assert_eq!(summarize("the driver does not compile against x:\nerror: linking with `cc` failed: exit status: 1"), "the driver does not compile against x: error: linking with `cc` failed: exit status: 1");
        let one = "rustc-dev cannot be installed for mine: the toolchain does not have it, and QuickRust only adds components to toolchains it installed";
        assert_eq!(summarize(one), one);
    }

    // TEST0051: The release names the interface the driver adapts to
    #[test]
    fn test0051_the_release_names_the_interface_the_driver_adapts_to() {
        let vv = |release: &str| format!("rustc {release} (abc 2026-09-28)\nbinary: rustc\nrelease: {release}\n");
        assert_eq!(release_minor(&vv("1.99.0")), Some(99));
        assert_eq!(release_minor(&vv("1.98.1")), Some(98));
        assert_eq!(release_minor(&vv("1.100.0-nightly")), Some(100));
        assert_eq!(release_minor("rustc 1.99.0\n"), None);
    }
}
