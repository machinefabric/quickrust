//! Command classification and upstream Cargo resolution.
//!
//! Classification never consumes or rewrites arguments (FR-001.1): it only
//! peeks at them to decide how much observation a command gets.

use crate::config::Config;
use crate::util::{self, canon, lossy};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Build,
    Check,
    Test,
    Bench,
    Run,
    Doc,
    Rustc,
    Clippy,
    Fix,
    Clean,
    /// Registry, packaging and project-scaffolding commands (COMP-010).
    Passthrough,
    /// Read-only queries and help output: delegated with no session at all.
    Query,
    /// Not a built-in command: an external subcommand.
    External,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Family::Build => "build",
            Family::Check => "check",
            Family::Test => "test",
            Family::Bench => "bench",
            Family::Run => "run",
            Family::Doc => "doc",
            Family::Rustc => "rustc",
            Family::Clippy => "clippy",
            Family::Fix => "fix",
            Family::Clean => "clean",
            Family::Passthrough => "passthrough",
            Family::Query => "query",
            Family::External => "external",
        }
    }

    /// Build-affecting commands that are observed through the rustc wrapper (COMP-011).
    pub fn builds(self) -> bool {
        matches!(self, Family::Build | Family::Check | Family::Test | Family::Bench | Family::Run | Family::Doc | Family::Rustc | Family::Clippy | Family::Fix)
    }
}

pub fn family_of(command: &str) -> Family {
    match command {
        "build" | "b" => Family::Build,
        "check" | "c" => Family::Check,
        "test" | "t" => Family::Test,
        "bench" => Family::Bench,
        "run" | "r" => Family::Run,
        "doc" | "d" | "rustdoc" => Family::Doc,
        "rustc" => Family::Rustc,
        "clippy" => Family::Clippy,
        "fix" => Family::Fix,
        "clean" => Family::Clean,
        "add" | "remove" | "rm" | "update" | "fetch" | "vendor" | "generate-lockfile" | "publish" | "package" | "login" | "logout" | "owner" | "yank" | "install" | "uninstall" | "new" | "init" => Family::Passthrough,
        "metadata" | "tree" | "search" | "info" | "locate-project" | "pkgid" | "verify-project" | "read-manifest" | "version" | "help" | "config" | "report" | "fmt" => Family::Query,
        _ => Family::External,
    }
}

#[derive(Debug, Clone)]
pub struct Invocation {
    /// `+toolchain` override, without the plus.
    pub toolchain: Option<String>,
    pub command: Option<String>,
    pub family: Family,
    /// The command an alias expands to, when the family came from an alias.
    pub alias_of: Option<String>,
    pub manifest_path: Option<PathBuf>,
    pub target_dir: Option<PathBuf>,
    pub targets: Vec<String>,
    /// Raw values of `--config` arguments, in order.
    pub configs: Vec<String>,
    /// `-C <dir>` global option.
    pub chdir: Option<PathBuf>,
    pub json_messages: bool,
    pub release: bool,
}

/// Value of `--flag value` / `--flag=value` at position `i`, if `args[i]` is that flag.
fn flag_value(args: &[OsString], i: usize, long: &str) -> Option<String> {
    let a = args[i].to_string_lossy();
    if a == long {
        return args.get(i + 1).map(|v| lossy(v));
    }
    a.strip_prefix(long).and_then(|r| r.strip_prefix('=')).map(str::to_string)
}

pub fn classify(args: &[OsString]) -> Invocation {
    let mut inv = Invocation {
        toolchain: None,
        command: None,
        family: Family::Query,
        alias_of: None,
        manifest_path: None,
        target_dir: None,
        targets: Vec::new(),
        configs: Vec::new(),
        chdir: None,
        json_messages: false,
        release: false,
    };
    let mut i = 0;
    if let Some(first) = args.first().map(|a| a.to_string_lossy()) {
        if let Some(tc) = first.strip_prefix('+') {
            inv.toolchain = Some(tc.to_string());
            i = 1;
        }
    }
    // Skip global options to find the subcommand; value-taking globals consume one more token.
    let mut cmd_index = None;
    while i < args.len() {
        let a = args[i].to_string_lossy();
        if !a.starts_with('-') {
            cmd_index = Some(i);
            break;
        }
        match a.as_ref() {
            "--config" | "--color" | "--explain" | "-Z" => i += 1,
            "-C" => {
                inv.chdir = args.get(i + 1).map(PathBuf::from);
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    if let Some(ci) = cmd_index {
        let cmd = lossy(&args[ci]);
        inv.family = family_of(&cmd);
        inv.command = Some(cmd);
    }
    for i in 0..args.len() {
        let a = args[i].to_string_lossy();
        if a == "--" {
            break;
        }
        if let Some(v) = flag_value(args, i, "--manifest-path") {
            inv.manifest_path = Some(PathBuf::from(v));
        } else if let Some(v) = flag_value(args, i, "--target-dir") {
            inv.target_dir = Some(PathBuf::from(v));
        } else if let Some(v) = flag_value(args, i, "--target") {
            inv.targets.push(v);
        } else if let Some(v) = flag_value(args, i, "--config") {
            inv.configs.push(v);
        } else if let Some(v) = flag_value(args, i, "--message-format") {
            inv.json_messages |= v.split(',').any(|p| p.starts_with("json"));
        } else if a == "--release" || a == "-r" {
            inv.release = true;
        } else if let Some(v) = flag_value(args, i, "--profile") {
            inv.release |= v == "release";
        }
    }
    inv
}

/// The small part of Cargo's configuration Haul must know to stay transparent:
/// a configured rustc wrapper (which our `RUSTC_WRAPPER` would otherwise mask,
/// FR-100.4) and aliases (to classify commands). Parsed for analysis and
/// wrapper preservation only; Cargo remains the execution truth (COMP-052).
#[derive(Debug, Default, Clone)]
pub struct CargoConfigSubset {
    pub rustc_wrapper: Option<(String, String)>,
    pub aliases: Vec<(String, String)>,
    pub target_dir: Option<PathBuf>,
    /// Profile settings the user made in configuration, as `(path below [profile], origin)`,
    /// e.g. `("dev.package.*.debug", "cargo config /w/.cargo/config.toml")`.
    pub profile_keys: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// Every leaf key path below a TOML value, dot-joined (`dev.package.*.debug`).
fn flatten_keys(prefix: &str, v: &toml::Value, out: &mut Vec<String>) {
    match v {
        toml::Value::Table(t) => {
            for (k, x) in t {
                flatten_keys(&if prefix.is_empty() { k.clone() } else { format!("{}.{}", prefix, k) }, x, out);
            }
        }
        _ => out.push(prefix.to_string()),
    }
}

/// Profile settings made in the manifest at `ws_root` (only the workspace root's profiles count).
pub fn manifest_profile_keys(ws_root: &Path) -> Vec<(String, String)> {
    let manifest = ws_root.join("Cargo.toml");
    let mut keys = Vec::new();
    if let Some(p) = std::fs::read_to_string(&manifest).ok().and_then(|t| t.parse::<toml::Table>().ok()).and_then(|d| d.get("profile").cloned()) {
        flatten_keys("", &p, &mut keys);
    }
    keys.into_iter().map(|k| (k, format!("manifest {}", manifest.display()))).collect()
}

/// Profile settings made through `CARGO_PROFILE_<NAME>_<KEY>`, normalised as `name_key` in lower case.
/// The environment cannot name packages, so these never contain `package`-level paths.
pub fn env_profile_keys() -> Vec<(String, String)> {
    std::env::vars().filter_map(|(k, _)| k.strip_prefix("CARGO_PROFILE_").map(|rest| (rest.to_ascii_lowercase(), format!("env {}", k)))).collect()
}

fn config_files(cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    let mut push = |dir: PathBuf, out: &mut Vec<PathBuf>| {
        for name in ["config.toml", "config"] {
            let p = dir.join(name);
            if p.is_file() {
                let c = canon(&p);
                if !seen.contains(&c) {
                    seen.push(c);
                    out.push(p);
                }
                break;
            }
        }
    };
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        push(d.join(".cargo"), &mut out);
        dir = d.parent();
    }
    if let Some(home) = util::cargo_home() {
        push(home, &mut out);
    }
    out
}

/// Cargo `ConfigRelativePath` semantics: a bare name is looked up on `PATH`,
/// anything with a separator is relative to the directory holding `.cargo`.
fn config_relative(value: &str, config_file: &Path) -> String {
    if value.contains('/') || value.contains('\\') {
        let p = Path::new(value);
        if p.is_absolute() {
            return value.to_string();
        }
        if let Some(base) = config_file.parent().and_then(Path::parent) {
            return util::path_str(&base.join(p));
        }
    }
    value.to_string()
}

fn alias_string(v: &toml::Value) -> Option<String> {
    match v {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Array(a) => Some(a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")),
        _ => None,
    }
}

pub fn cargo_config_subset(cwd: &Path, cli_configs: &[String]) -> CargoConfigSubset {
    let mut out = CargoConfigSubset::default();
    let absorb = |doc: &toml::Table, origin: &Path, out: &mut CargoConfigSubset| {
        if let Some(build) = doc.get("build").and_then(|b| b.as_table()) {
            if out.rustc_wrapper.is_none() {
                if let Some(w) = build.get("rustc-wrapper").and_then(|v| v.as_str()) {
                    out.rustc_wrapper = Some((config_relative(w, origin), format!("cargo config {}", origin.display())));
                }
            }
            if out.target_dir.is_none() {
                if let Some(t) = build.get("target-dir").and_then(|v| v.as_str()) {
                    out.target_dir = Some(PathBuf::from(config_relative(&format!("./{}", t), origin)));
                }
            }
        }
        if let Some(p) = doc.get("profile") {
            let mut keys = Vec::new();
            flatten_keys("", p, &mut keys);
            out.profile_keys.extend(keys.into_iter().map(|k| (k, format!("cargo config {}", origin.display()))));
        }
        if let Some(aliases) = doc.get("alias").and_then(|b| b.as_table()) {
            for (k, v) in aliases {
                if !out.aliases.iter().any(|(n, _)| n == k) {
                    if let Some(s) = alias_string(v) {
                        out.aliases.push((k.clone(), s));
                    }
                }
            }
        }
    };
    // Highest precedence first: --config, then environment, then files nearest-first.
    for raw in cli_configs.iter().rev() {
        match raw.parse::<toml::Table>() {
            Ok(doc) => absorb(&doc, &cwd.join(".cargo").join("<--config>"), &mut out),
            Err(_) => {
                let p = Path::new(raw);
                match std::fs::read_to_string(p).ok().and_then(|t| t.parse::<toml::Table>().ok()) {
                    Some(doc) => absorb(&doc, p, &mut out),
                    None => out.notes.push(format!("--config {:?} could not be interpreted for analysis", raw)),
                }
            }
        }
    }
    if out.rustc_wrapper.is_none() {
        if let Ok(w) = std::env::var("CARGO_BUILD_RUSTC_WRAPPER") {
            out.rustc_wrapper = Some((w, "env CARGO_BUILD_RUSTC_WRAPPER".into()));
        }
    }
    for (name, value) in std::env::vars() {
        if let Some(alias) = name.strip_prefix("CARGO_ALIAS_") {
            out.aliases.push((alias.to_ascii_lowercase().replace('_', "-"), value));
        }
    }
    for file in config_files(cwd) {
        match std::fs::read_to_string(&file).ok().and_then(|t| t.parse::<toml::Table>().ok()) {
            Some(doc) => absorb(&doc, &file, &mut out),
            None => out.notes.push(format!("{} could not be parsed for analysis", file.display())),
        }
    }
    out
}

/// Re-classifies an external-looking command through user aliases (up to a few hops).
pub fn apply_aliases(inv: &mut Invocation, subset: &CargoConfigSubset) {
    let mut name = match (&inv.command, inv.family) {
        (Some(c), Family::External) => c.clone(),
        _ => return,
    };
    for _ in 0..4 {
        let Some((_, expansion)) = subset.aliases.iter().find(|(n, _)| *n == name) else { return };
        let Some(first) = expansion.split_whitespace().find(|t| !t.starts_with('-')) else { return };
        let fam = family_of(first);
        if fam != Family::External {
            inv.family = fam;
            inv.alias_of = Some(first.to_string());
            inv.release |= expansion.split_whitespace().any(|t| t == "--release" || t == "-r");
            return;
        }
        name = first.to_string();
    }
}

#[derive(Debug, Clone)]
pub struct Upstream {
    pub program: PathBuf,
    /// The program is a concrete toolchain binary, so a leading `+toolchain` must not be forwarded.
    pub strip_toolchain_arg: bool,
    pub env: Vec<(String, String)>,
    pub rule: &'static str,
}

/// Canonical path and identity of the running executable.
pub fn self_identity() -> (PathBuf, Option<(u64, u64)>) {
    let exe = std::env::current_exe().map(|p| canon(&p)).unwrap_or_default();
    let id = util::file_identity(&exe);
    (exe, id)
}

fn is_self(candidate: &Path) -> bool {
    let (exe, id) = self_identity();
    canon(candidate) == exe || (id.is_some() && util::file_identity(candidate) == id)
}

fn first_exe(dir: &Path, name: &str) -> Option<PathBuf> {
    util::exe_candidates(dir, name).into_iter().find(|c| util::is_executable(c))
}

/// The first `cargo` on `PATH` that is not Haul: what the shell would run without Haul.
fn path_cargo() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|d| !d.as_os_str().is_empty()).find_map(|d| first_exe(&d, "cargo").filter(|c| !is_self(c)))
}

/// Resolves the Cargo a command would have run without Haul; never returns Haul itself.
///
/// 1. `HAUL_REAL_CARGO`;
/// 2. `compatibility.real-cargo`;
/// 3. the first `cargo` on `PATH` that is not Haul — normally rustup's proxy, which applies
///    `+toolchain`, `RUSTUP_TOOLCHAIN`, directory overrides and toolchain files itself
///    (COMP-012); fast mode learns its choice once and calls that toolchain's Cargo directly;
/// 4. when no other Cargo is on `PATH` because Haul occupies rustup's proxy slot,
///    `rustup which cargo`;
/// 5. an error, never a recursion.
pub fn resolve_upstream(inv: &Invocation, cfg: &Config) -> Result<Upstream, String> {
    if let Some(p) = std::env::var_os(crate::ENV_REAL_CARGO).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        if is_self(&p) {
            return Err(format!("{} points at Haul itself ({}); it must name the upstream Cargo binary", crate::ENV_REAL_CARGO, p.display()));
        }
        if !util::is_executable(&p) {
            return Err(format!("{}={} is not an executable file", crate::ENV_REAL_CARGO, p.display()));
        }
        return Ok(Upstream { program: p, strip_toolchain_arg: false, env: vec![], rule: "explicit HAUL_REAL_CARGO" });
    }
    let configured = cfg.str("compatibility.real-cargo");
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if !p.is_absolute() || !util::is_executable(&p) || is_self(&p) {
            return Err(format!("compatibility.real-cargo = {:?} (from {}) must be an absolute path to the upstream Cargo binary", configured, cfg.origin("compatibility.real-cargo")));
        }
        return Ok(Upstream { program: p, strip_toolchain_arg: false, env: vec![], rule: "configured compatibility.real-cargo" });
    }
    if let Some(cargo) = path_cargo() {
        // Fast mode: rustup's choice, learned once for these inputs, without the proxy's ~5 ms.
        if cfg.str("mode") == "fast" {
            if let Some((direct, env)) = std::env::current_dir().ok().and_then(|cwd| crate::toolchain_cache::resolve(&cargo, inv.toolchain.as_deref(), &cwd)) {
                return Ok(Upstream { program: direct, strip_toolchain_arg: inv.toolchain.is_some(), env, rule: "rustup's toolchain, resolved once (proxy skipped)" });
            }
        }
        return Ok(Upstream { program: cargo, strip_toolchain_arg: false, env: vec![], rule: "first cargo on PATH" });
    }
    // Haul in rustup's proxy slot (machine-wide install): ask rustup directly.
    if let Some(rustup) = util::which("rustup") {
        let mut cmd = Command::new(&rustup);
        cmd.arg("which").arg("cargo");
        if let Some(tc) = &inv.toolchain {
            cmd.arg("--toolchain").arg(tc);
        }
        match cmd.output() {
            Ok(out) if out.status.success() => {
                let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
                if util::is_executable(&p) && !is_self(&p) {
                    let env = inv.toolchain.iter().map(|t| ("RUSTUP_TOOLCHAIN".to_string(), t.clone())).collect();
                    return Ok(Upstream { program: p, strip_toolchain_arg: inv.toolchain.is_some(), env, rule: "rustup which cargo" });
                }
            }
            Ok(out) if inv.toolchain.is_some() => {
                return Err(format!("rustup could not select toolchain `{}`: {}", inv.toolchain.as_deref().unwrap_or(""), String::from_utf8_lossy(&out.stderr).trim()));
            }
            _ => {}
        }
    }
    Err(format!(
        "could not find an upstream Cargo to delegate to.\n  tried: {}, compatibility.real-cargo, PATH (excluding this shim), rustup\n  fix: install Rust (https://rustup.rs) or set {}=/absolute/path/to/cargo",
        crate::ENV_REAL_CARGO,
        crate::ENV_REAL_CARGO
    ))
}

/// Detects that the "upstream Cargo" we just executed is Haul again (COMP-022).
/// The guard carries the delegating process id; a legitimate nested invocation
/// has a build script or Cargo itself as its parent instead.
pub fn recursion_detected() -> bool {
    let Ok(guard) = std::env::var(crate::ENV_EXEC_GUARD) else { return false };
    let Some((pid, _session)) = guard.split_once(':') else { return false };
    let ppid = crate::proc::parent_pid();
    ppid != 0 && pid.parse::<u32>().ok() == Some(ppid)
}

/// The rustc wrapper the user already configured, if any, with where it came from.
/// An empty `RUSTC_WRAPPER` means "none", exactly as Cargo treats it.
pub fn existing_rustc_wrapper(subset: &CargoConfigSubset) -> Option<(String, String)> {
    match std::env::var("RUSTC_WRAPPER") {
        Ok(v) if !v.is_empty() => Some((v, "env RUSTC_WRAPPER".into())),
        Ok(_) => None,
        Err(_) => subset.rustc_wrapper.clone().filter(|(w, _)| !w.is_empty()),
    }
}

/// Nearest directory at or above `start` that looks like a Cargo workspace root.
pub fn guess_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut nearest: Option<PathBuf> = None;
    let mut dir = Some(start);
    while let Some(d) = dir {
        let manifest = d.join("Cargo.toml");
        if manifest.is_file() {
            let is_ws = std::fs::read_to_string(&manifest).map(|t| t.lines().any(|l| l.trim_start().starts_with("[workspace"))).unwrap_or(false);
            if is_ws {
                return Some(d.to_path_buf());
            }
            if nearest.is_none() {
                nearest = Some(d.to_path_buf());
            }
        }
        dir = d.parent();
    }
    nearest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn classifies_without_consuming() {
        let inv = classify(&os(&["+nightly", "--config", "a=1", "-Zfoo", "test", "-p", "core", "--features", "foo"]));
        assert_eq!(inv.toolchain.as_deref(), Some("nightly"));
        assert_eq!(inv.command.as_deref(), Some("test"));
        assert_eq!(inv.family, Family::Test);
        assert_eq!(inv.configs, vec!["a=1"]);
    }

    #[test]
    fn global_value_flags_are_skipped() {
        let inv = classify(&os(&["--color", "always", "-C", "sub", "b", "--release"]));
        assert_eq!(inv.command.as_deref(), Some("b"));
        assert_eq!(inv.family, Family::Build);
        assert!(inv.release);
        assert_eq!(inv.chdir, Some(PathBuf::from("sub")));
    }

    #[test]
    fn unknown_and_bare_invocations() {
        assert_eq!(classify(&os(&["nextest", "run"])).family, Family::External);
        assert_eq!(classify(&os(&["--version"])).family, Family::Query);
        assert_eq!(classify(&os(&[])).command, None);
        assert_eq!(classify(&os(&["publish", "--dry-run"])).family, Family::Passthrough);
        assert_eq!(classify(&os(&["clean"])).family, Family::Clean);
    }

    #[test]
    fn sees_json_and_paths() {
        let inv = classify(&os(&["build", "--message-format=json-diagnostic-rendered-ansi", "--manifest-path", "x/Cargo.toml", "--target", "wasm32-unknown-unknown"]));
        assert!(inv.json_messages);
        assert_eq!(inv.manifest_path, Some(PathBuf::from("x/Cargo.toml")));
        assert_eq!(inv.targets, vec!["wasm32-unknown-unknown"]);
        // Arguments after `--` belong to the program being run.
        assert!(!classify(&os(&["run", "--", "--message-format=json"])).json_messages);
    }

    #[test]
    fn aliases_reclassify() {
        let mut inv = classify(&os(&["xb"]));
        let subset = CargoConfigSubset { aliases: vec![("xb".into(), "build --release".into())], ..Default::default() };
        apply_aliases(&mut inv, &subset);
        assert_eq!(inv.family, Family::Build);
        assert!(inv.release);
    }
}
