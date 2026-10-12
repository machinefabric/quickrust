//! Shared harness for the integration suites: isolated state, a shim directory with Haul as
//! `cargo`, and small fixture workspaces without registry dependencies.
//!
//! Every sandbox has a Haul home of its own, except for its toolchains: those come from one store
//! shared by every test run (`HAULTEST_TOOLCHAINS`, default `~/.cache/haul-test-toolchains`),
//! which Haul fills itself the first time a test needs a toolchain (that needs the network).

#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub struct Sandbox {
    pub dir: PathBuf,
    pub shim_dir: PathBuf,
    pub home: PathBuf,
    upstream_path: OsString,
}

/// Where the suites' toolchains are kept between runs.
pub fn toolchain_store() -> PathBuf {
    match std::env::var_os("HAULTEST_TOOLCHAINS").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME").expect("HOME is set")).join(".cache/haul-test-toolchains"),
    }
}

static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Environment that would make the wrapped and unwrapped runs differ for reasons
/// unrelated to Haul (it leaks in from the `cargo test` that runs this suite).
const SCRUB: &[&str] = &[
    "CARGO", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_INCREMENTAL", "RUSTUP_TOOLCHAIN", "CARGO_MAKEFLAGS", "MAKEFLAGS", "CARGO_MANIFEST_DIR", "CARGO_PKG_NAME", "CARGO_PKG_VERSION", "CARGO_PRIMARY_PACKAGE", "CARGO_CRATE_NAME", "OUT_DIR", "CI", "GITHUB_ACTIONS",
    "RUST_RECURSION_COUNT", "CARGO_BUILD_TARGET", "CARGO_HOME", "RUSTDOC",
];

impl Sandbox {
    pub fn new(name: &str) -> Sandbox {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("haul-it-{}-{}-{}", name, std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Canonical so that paths printed by Cargo and paths built here agree (/tmp vs /private/tmp).
        let dir = fs::canonicalize(&dir).unwrap();
        let shim_dir = dir.join("shim");
        fs::create_dir_all(&shim_dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_haul"), shim_dir.join("cargo")).unwrap();
        let upstream_path = std::env::var_os("PATH").unwrap();
        let home = dir.join("haul-home");
        fs::create_dir_all(&home).unwrap();
        let store = toolchain_store();
        fs::create_dir_all(&store).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&store, home.join("toolchains")).unwrap();
        Sandbox { home, shim_dir, upstream_path, dir }
    }

    fn base(&self, program: &Path, cwd: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(cwd).stdin(Stdio::null());
        for (k, _) in std::env::vars_os() {
            let k = k.to_string_lossy().into_owned();
            if SCRUB.contains(&k.as_str()) || k.starts_with("HAUL_") || k.starts_with("CARGO_PKG_") || k.starts_with("CARGO_BIN_EXE_") {
                c.env_remove(&k);
            }
        }
        c.env("HAUL_HOME", &self.home).env("CARGO_TERM_COLOR", "never");
        // Early cut-off builds Haul's compiler driver in the background when it is missing: the
        // tests that need the driver turn it on and build it themselves.
        c.env("HAUL_EARLY_CUTOFF", "off");
        // The suites' toolchains change only when a test changes them.
        c.env("HAUL_TOOLCHAIN_AUTO_UPDATE", "off");
        c
    }

    /// Haul's Cargo home in this sandbox (registry sources, credentials).
    pub fn cargo_home(&self) -> PathBuf {
        self.home.join("cargo")
    }

    /// The target directory of the workspace `cwd` is in: Haul's, in its cache (`haul path`).
    pub fn target_dir(&self, cwd: &Path) -> PathBuf {
        let out = run(self.haul(cwd).arg("path"));
        assert!(out.status.success(), "no target directory: {}", String::from_utf8_lossy(&out.stderr));
        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
    }

    /// The toolchain Haul uses in `cwd` (installed into the shared store on first use).
    pub fn toolchain_dir(&self, cwd: &Path) -> PathBuf {
        let out = run(self.haul(cwd).args(["toolchain", "which"]));
        assert!(out.status.success(), "no toolchain: {}", String::from_utf8_lossy(&out.stderr));
        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
    }

    /// The oracle's environment: the toolchain's own Cargo, as Haul runs it, without Haul.
    fn oracle_path(&self, cwd: &Path) -> (PathBuf, OsString) {
        let tc = self.toolchain_dir(cwd);
        let path = std::env::join_paths(std::iter::once(tc.join("bin")).chain(std::env::split_paths(&self.upstream_path))).unwrap();
        (tc, path)
    }

    pub fn path_with_shim(&self) -> OsString {
        let mut paths = vec![self.shim_dir.clone()];
        paths.extend(std::env::split_paths(&self.upstream_path));
        std::env::join_paths(paths).unwrap()
    }

    /// `cargo` resolved through the Haul shim, as a user with the shim first on PATH would run it.
    pub fn cargo(&self, cwd: &Path) -> Command {
        let mut c = self.base(&self.shim_dir.join("cargo"), cwd);
        c.env("PATH", self.path_with_shim());
        c
    }

    /// The toolchain's own Cargo with no Haul anywhere: the compatibility oracle. It gets the
    /// Cargo home, compiler, PATH and (in a workspace) target directory Haul gives the Cargo it
    /// runs.
    pub fn upstream(&self, cwd: &Path) -> Command {
        let (tc, path) = self.oracle_path(cwd);
        let mut c = self.base(&tc.join("bin/cargo"), cwd);
        Self::oracle_env(&mut c, &tc, &self.cargo_home());
        self.oracle_target(&mut c, cwd);
        c.env("PATH", path);
        c
    }

    /// Haul's target directory for `cwd`'s workspace, given to the oracle (none outside one).
    fn oracle_target(&self, c: &mut Command, cwd: &Path) {
        let out = run(self.haul(cwd).arg("path"));
        if out.status.success() {
            let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
            c.env("CARGO_TARGET_DIR", &dir).env("CARGO_BUILD_BUILD_DIR", &dir);
        }
    }

    /// What Haul sets for the Cargo it runs, set for the oracle too.
    fn oracle_env(c: &mut Command, tc: &Path, cargo_home: &Path) {
        c.env("CARGO_HOME", cargo_home).env("RUSTC", tc.join("bin/rustc")).env("RUSTDOC", tc.join("bin/rustdoc"));
        c.env("RUSTUP_TOOLCHAIN", tc.file_name().unwrap()).env("CARGO_CACHE_AUTO_CLEAN_FREQUENCY", "never");
        c.env(if cfg!(target_os = "macos") { "DYLD_FALLBACK_LIBRARY_PATH" } else { "LD_LIBRARY_PATH" }, tc.join("lib"));
    }

    /// `cargo ARGS` through the shim, run inside a pseudo-terminal (util-linux `script`), as a
    /// developer at a terminal would. `None` where `script` is not available.
    pub fn cargo_in_terminal(&self, cwd: &Path, args: &str) -> Option<Command> {
        self.in_terminal(cwd, args, &self.path_with_shim())
    }

    /// The same with the toolchain's own Cargo and no Haul: the oracle for terminal behaviour.
    pub fn upstream_in_terminal(&self, cwd: &Path, args: &str) -> Option<Command> {
        let (tc, path) = self.oracle_path(cwd);
        let mut c = self.in_terminal(cwd, args, &path)?;
        Self::oracle_env(&mut c, &tc, &self.cargo_home());
        self.oracle_target(&mut c, cwd);
        Some(c)
    }

    fn in_terminal(&self, cwd: &Path, args: &str, path: &OsString) -> Option<Command> {
        if !cfg!(target_os = "linux") || Command::new("script").arg("--version").output().is_err() {
            return None;
        }
        let mut c = self.base(Path::new("script"), cwd);
        c.env("PATH", path).env("COLUMNS", "120").env("TERM", "xterm");
        c.args(["-qec", &format!("cargo {}", args), "/dev/null"]);
        Some(c)
    }

    /// Any program in the sandbox's environment, with the shim first on PATH (a build tool that
    /// runs `cargo`, for instance).
    pub fn with_shim(&self, program: &str, cwd: &Path) -> Command {
        let mut c = self.base(Path::new(program), cwd);
        c.env("PATH", self.path_with_shim());
        c
    }

    pub fn haul(&self, cwd: &Path) -> Command {
        let mut c = self.base(Path::new(env!("CARGO_BIN_EXE_haul")), cwd);
        c.env("PATH", self.path_with_shim());
        c
    }

    pub fn file(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        p
    }

    /// A single library package with a binary.
    pub fn package(&self, rel: &str, name: &str) -> PathBuf {
        self.file(&format!("{}/Cargo.toml", rel), &format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\na = []\nb = []\n", name));
        self.file(&format!("{}/src/lib.rs", rel), "pub fn add(x: u32, y: u32) -> u32 { x + y }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(2, 2), 4);\n    }\n}\n");
        self.file(&format!("{}/src/main.rs", rel), &format!("fn main() {{\n    println!(\"{{}}\", {}::add(1, 2));\n}}\n", name.replace('-', "_")));
        self.dir.join(rel)
    }

    /// A virtual workspace: `util` (lib), `gen` (lib with a build script), `mac` (proc macro), `app` (binary using all).
    pub fn workspace(&self, rel: &str) -> PathBuf {
        self.file(&format!("{}/Cargo.toml", rel), "[workspace]\nmembers = [\"util\", \"gen\", \"mac\", \"app\"]\nresolver = \"2\"\n");
        self.file(&format!("{}/util/Cargo.toml", rel), "[package]\nname = \"util\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nextra = []\n");
        self.file(&format!("{}/util/src/lib.rs", rel), "pub fn double(x: u32) -> u32 { x * 2 }\n");
        self.file(&format!("{}/gen/Cargo.toml", rel), "[package]\nname = \"gen\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n");
        self.file(&format!("{}/gen/build.rs", rel), "fn main() {\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(format!(\"{out}/gen.rs\"), \"pub const GENERATED: u32 = 7;\").unwrap();\n}\n");
        self.file(&format!("{}/gen/src/lib.rs", rel), "include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n");
        self.file(&format!("{}/mac/Cargo.toml", rel), "[package]\nname = \"mac\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n");
        self.file(&format!("{}/mac/src/lib.rs", rel), "use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn answer(_input: TokenStream) -> TokenStream {\n    \"42u32\".parse().unwrap()\n}\n");
        self.file(&format!("{}/app/Cargo.toml", rel), "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nutil = { path = \"../util\" }\ngen = { path = \"../gen\" }\nmac = { path = \"../mac\" }\n");
        self.file(&format!("{}/app/src/main.rs", rel), "fn main() {\n    println!(\"{}\", util::double(gen::GENERATED) + mac::answer!());\n}\n");
        self.dir.join(rel)
    }

    /// Path of the single recorded workspace store.
    pub fn store_dirs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(self.home.join("workspaces")).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
        v.sort();
        v
    }

    /// Session records of every workspace, oldest first.
    pub fn sessions(&self) -> Vec<serde_json::Value> {
        let mut files: Vec<PathBuf> = Vec::new();
        for d in self.store_dirs() {
            if let Ok(rd) = fs::read_dir(d.join("sessions")) {
                files.extend(rd.filter_map(|e| e.ok()).map(|e| e.path()));
            }
        }
        files.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
        files.iter().map(|p| serde_json::from_slice(&fs::read(p).unwrap()).unwrap()).collect()
    }

    /// The file of a session's record.
    #[allow(dead_code)]
    pub fn session_path(&self, id: &str) -> PathBuf {
        self.store_dirs().into_iter().map(|d| d.join("sessions").join(format!("{}.json", id))).find(|p| p.is_file()).unwrap_or_else(|| panic!("no record of session {}", id))
    }

    /// The report of the latest (or referenced) build run, as `haul report --format json` exports it.
    pub fn report(&self, cwd: &Path, reference: &str) -> serde_json::Value {
        let out = run(self.haul(cwd).args(["report", "--format", "json", "--run", reference]));
        assert!(out.status.success(), "haul report failed: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("report is JSON")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

pub fn run(cmd: &mut Command) -> Output {
    cmd.output().unwrap_or_else(|e| panic!("could not run {:?}: {}", cmd, e))
}

pub fn ok(cmd: &mut Command) -> Output {
    let out = run(cmd);
    assert!(out.status.success(), "{:?} failed with {:?}\nstdout: {}\nstderr: {}", cmd, out.status, String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    out
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Every regular file below `root`, relative, sorted (links are not followed or listed).
pub fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().filter_map(|e| e.ok()) {
            let md = fs::symlink_metadata(e.path()).unwrap();
            if md.is_dir() {
                stack.push(e.path());
            } else if md.is_file() {
                out.push(e.path().strip_prefix(root).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

/// Recursively copies a directory (fixtures are small).
pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().filter_map(|e| e.ok()) {
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            fs::copy(e.path(), &target).unwrap();
        }
    }
}

pub fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git").current_dir(cwd).args(["-c", "user.name=haul-test", "-c", "user.email=haul@example.invalid", "-c", "commit.gpgsign=false"]).args(args).output().expect("git is required for this test");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

/// How Haul reports that the toolchain's `rustc-dev` component, which its compiler driver is
/// built against, cannot be installed (`haul driver build` / `haul driver status`).
pub const NO_RUSTC_DEV: &str = "rustc-dev cannot be installed";

/// Builds Haul's compiler driver for the toolchain `cwd` uses; its path. `None` (with the reason
/// on stderr) only where the toolchain's `rustc-dev` component cannot be installed (its files
/// clash with another component's in some releases' packaging): the driver cannot exist there.
/// Any other failure to build it fails the test.
pub fn build_driver(sb: &Sandbox, cwd: &Path) -> Option<PathBuf> {
    let built = run(sb.haul(cwd).args(["driver", "build"]));
    if !built.status.success() && stderr(&built).contains(NO_RUSTC_DEV) {
        eprintln!("not run: Haul's compiler driver cannot be built here: {}", stderr(&built).trim());
        return None;
    }
    assert!(built.status.success(), "{}", stderr(&built));
    Some(PathBuf::from(stdout(&built).trim()))
}

#[cfg(unix)]
pub fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
/// Wall time, peak resident memory, success and stdout of a command. The child is reaped with
/// `wait4`, which reports its resource usage; `Child::wait` would discard it. The peak counts the
/// child from its start: a child forked from a test process that holds much memory counts that
/// memory until it executes, so each measurement lives in a test binary of its own.
#[allow(clippy::zombie_processes)]
pub fn measure(cmd: &mut Command) -> (f64, u64, bool, Vec<u8>) {
    let started = std::time::Instant::now();
    let child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let pid = child.id() as libc::pid_t;
    let mut out = child.stdout.unwrap();
    let mut stdout = Vec::new();
    std::io::Read::read_to_end(&mut out, &mut stdout).unwrap();
    let mut status = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::wait4(pid, &mut status, 0, &mut usage) }, pid);
    (started.elapsed().as_secs_f64(), usage.ru_maxrss as u64 * 1024, libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0, stdout)
}
