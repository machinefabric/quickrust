//! Shared harness for the integration suites: isolated state, a shim directory
//! in front of the real Cargo, and small fixture workspaces without registry
//! dependencies so the suites run offline.

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

static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Environment that would make the wrapped and unwrapped runs differ for reasons
/// unrelated to Haul (it leaks in from the `cargo test` that runs this suite).
const SCRUB: &[&str] = &[
    "CARGO", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_INCREMENTAL", "RUSTUP_TOOLCHAIN", "CARGO_MAKEFLAGS", "MAKEFLAGS", "CARGO_MANIFEST_DIR", "CARGO_PKG_NAME", "CARGO_PKG_VERSION", "CARGO_PRIMARY_PACKAGE", "CARGO_CRATE_NAME", "OUT_DIR", "CI", "GITHUB_ACTIONS",
    "RUST_RECURSION_COUNT", "CARGO_BUILD_TARGET",
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
        Sandbox { home: dir.join("haul-home"), shim_dir, upstream_path, dir }
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
        // A Cargo home of the sandbox's own: the machine's `$CARGO_HOME/config.toml` (a global
        // `build.target-dir`, a rustc wrapper, profile overrides) must not reach the fixtures, and
        // `cargo login` in the suite must not touch real credentials. The fixtures need no registry.
        c.env("CARGO_HOME", self.dir.join("cargo-home"));
        c
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

    /// The real Cargo with no Haul anywhere on PATH: the compatibility oracle.
    pub fn upstream(&self, cwd: &Path) -> Command {
        let mut c = self.base(Path::new("cargo"), cwd);
        c.env("PATH", &self.upstream_path);
        c
    }

    /// `cargo ARGS` through the shim, run inside a pseudo-terminal (util-linux `script`), as a
    /// developer at a terminal would. `None` where `script` is not available.
    pub fn cargo_in_terminal(&self, cwd: &Path, args: &str) -> Option<Command> {
        self.in_terminal(cwd, args, &self.path_with_shim())
    }

    /// The same with the real Cargo and no Haul on PATH: the oracle for terminal behaviour.
    pub fn upstream_in_terminal(&self, cwd: &Path, args: &str) -> Option<Command> {
        self.in_terminal(cwd, args, &self.upstream_path)
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

/// Every regular file below `root`, relative, sorted.
pub fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().filter_map(|e| e.ok()) {
            let md = fs::symlink_metadata(e.path()).unwrap();
            if md.is_dir() {
                stack.push(e.path());
            } else {
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
