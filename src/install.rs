//! `haul install` / `cargo install`: programs built from crates and put into Haul's `bin`
//! (`~/.haul/bin`, or `<root>/bin` with `--root`), by Haul itself.
//!
//! - Which release of a crate: Cargo's resolver decides, as for a dependency (a probe manifest
//!   depending on its binaries — an artifact dependency, `-Zbindeps`, unlocked with
//!   `RUSTC_BOOTSTRAP=1`: Cargo drops a plain dependency on a crate without a library — then
//!   `cargo metadata`), so registries, source replacement, yanked releases and the toolchain's
//!   Rust version count exactly as in a build. Its source then comes from Haul's
//!   Cargo home, copied into Haul's cache.
//! - `--git` clones with the system `git`; `--path` builds the package where it is.
//! - It is built like any build Haul runs (release profile unless `--debug` / `--profile`), with
//!   Haul's output, artifact cache and build directories. As by `cargo install`, a crate's
//!   dependencies are resolved afresh, and with `--locked` from the lockfile published with it
//!   (kept aside in the copy as `.haul-published.lock`); `--path` builds the workspace with its own
//!   `Cargo.lock`, as `cargo build` does. A copy is locked while it is prepared and built
//!   (`<copy>.lock`), and the cache removes copies least recently used like build directories.
//! - Its binaries (or `--bin`, `--example`) are put into the `bin` directory, never over one of
//!   Haul's own names, nor over another package's or an unknown file without `--force`.
//!   `installs.json` in Haul's home records what each package installed, for `--list`,
//!   `uninstall` and knowing an install is already there.

use crate::config::Config;
use crate::resolve::{self, Invocation};
use crate::util;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

const RECORD: &str = "installs.json";
/// The lockfile a crate was published with, in Haul's copy of it.
const PUBLISHED_LOCK: &str = ".haul-published.lock";
/// In a complete copy of a published package; its time is when an install last used the copy.
pub const COMPLETE: &str = ".haul-complete";

/// Names in Haul's `bin` that are Haul.
fn reserved(name: &str) -> bool {
    matches!(name, "cargo" | "haul" | "haul-rustc-wrapper") || crate::engine::PROXIES.contains(&name)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// `(name, version requirement)`; empty for `--path` / `--git` without a name.
    pub crates: Vec<(String, Option<String>)>,
    pub git: Option<String>,
    pub git_ref: Option<(String, String)>,
    pub path: Option<PathBuf>,
    pub registry: Option<String>,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    pub locked: bool,
    pub force: bool,
    pub bins: Vec<String>,
    pub all_bins: bool,
    pub examples: Vec<String>,
    pub all_examples: bool,
    pub root: Option<PathBuf>,
    pub profile: String,
    pub target: Option<String>,
    /// Passed to the build as they are (`-j`, `--offline`, `--config`, `--ignore-rust-version`, `-q`, `-v`).
    pub build_args: Vec<String>,
    pub list: bool,
    pub no_track: bool,
}

fn need(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i).cloned().ok_or_else(|| format!("{} needs a value", flag))
}

/// Parses `cargo install`'s arguments (after `install`). An argument it does not know is an error.
pub fn parse_install(args: &[String]) -> Result<Request, String> {
    let mut r = Request { profile: "release".into(), ..Default::default() };
    let mut version: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a, None),
        };
        let value = |i: &mut usize| -> Result<String, String> { inline.clone().map(Ok).unwrap_or_else(|| need(args, i, flag)) };
        match flag {
            "--version" | "--vers" => version = Some(value(&mut i)?),
            "--git" => r.git = Some(value(&mut i)?),
            "--branch" | "--tag" | "--rev" => {
                if r.git_ref.is_some() {
                    return Err("only one of --branch, --tag and --rev".into());
                }
                r.git_ref = Some((flag.trim_start_matches("--").to_string(), value(&mut i)?));
            }
            "--path" => r.path = Some(PathBuf::from(value(&mut i)?)),
            "--registry" => r.registry = Some(value(&mut i)?),
            "--index" => return Err("--index is not supported: name the registry in Cargo's configuration and use --registry".into()),
            "-F" | "--features" => r.features.extend(value(&mut i)?.split([',', ' ']).filter(|f| !f.is_empty()).map(str::to_string)),
            "--all-features" => r.all_features = true,
            "--no-default-features" => r.no_default_features = true,
            "--locked" => r.locked = true,
            "-f" | "--force" => r.force = true,
            "--bin" => r.bins.push(value(&mut i)?),
            "--bins" => r.all_bins = true,
            "--example" => r.examples.push(value(&mut i)?),
            "--examples" => r.all_examples = true,
            "--root" => r.root = Some(PathBuf::from(value(&mut i)?)),
            "--profile" => r.profile = value(&mut i)?,
            "--debug" => r.profile = "dev".into(),
            "--target" => r.target = Some(value(&mut i)?),
            "--list" => r.list = true,
            "--no-track" => r.no_track = true,
            "-j" | "--jobs" | "--config" => {
                let v = value(&mut i)?;
                r.build_args.extend([flag.to_string(), v]);
            }
            "--offline" | "--frozen" | "--ignore-rust-version" | "-q" | "--quiet" | "-v" | "-vv" | "--verbose" => r.build_args.push(flag.to_string()),
            f if f.starts_with('-') => return Err(format!("`haul install` has no option `{}`", f)),
            spec => {
                let (name, ver) = match spec.split_once('@') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (spec.to_string(), None),
                };
                if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
                    return Err(format!("`{}` is not a crate name", spec));
                }
                r.crates.push((name, ver));
            }
        }
        i += 1;
    }
    if let Some(v) = version {
        match r.crates.as_mut_slice() {
            [(_, req @ None)] => *req = Some(v),
            [_] => return Err("the version is given twice (`name@version` and --version)".into()),
            _ => return Err("--version needs exactly one crate".into()),
        }
    }
    if r.git.is_some() as u8 + r.path.is_some() as u8 > 1 {
        return Err("--git and --path exclude each other".into());
    }
    if r.git_ref.is_some() && r.git.is_none() {
        return Err("--branch, --tag and --rev go with --git".into());
    }
    if !r.list && r.crates.is_empty() && r.git.is_none() && r.path.is_none() {
        return Err("`haul install` needs a crate, --git or --path (or --list)".into());
    }
    Ok(r)
}

/// A version requirement as `cargo install` reads it: a bare version means exactly that one.
fn requirement(v: &Option<String>) -> String {
    match v {
        None => "*".into(),
        Some(v) if v.chars().next().is_some_and(|c| c.is_ascii_digit()) && semver::Version::parse(v).is_ok() => format!("={}", v),
        Some(v) => v.clone(),
    }
}

/// The root binaries go under (`<root>/bin`): `--root`, else Haul's home.
fn root_of(r: &Request, cwd: &Path) -> Result<PathBuf, String> {
    if std::env::var_os("CARGO_INSTALL_ROOT").is_some_and(|v| !v.is_empty()) && r.root.is_none() {
        return Err("CARGO_INSTALL_ROOT is set; Haul installs into its own bin (~/.haul/bin) unless --root says otherwise".into());
    }
    Ok(match &r.root {
        Some(p) => cwd.join(p),
        None => util::haul_home(),
    })
}

/// The record of installs, by root and package.
fn load_record() -> Value {
    util::read_json(&util::haul_home().join(RECORD)).filter(|v| v["format"] == json!(1)).unwrap_or_else(|| json!({ "format": 1, "roots": {} }))
}

fn save_record(v: &Value) -> Result<(), String> {
    util::write_json(&util::haul_home().join(RECORD), v).map_err(|e| format!("cannot record the install: {}", e))
}

/// `haul install …` / `haul uninstall …`: what a session runs for them.
pub fn run(args: &[OsString], inv: &Invocation, cwd: &Path, cfg: &Config) -> Result<i32, String> {
    let at = inv.command_index.ok_or("no command")?;
    let rest: Vec<String> = args[at + 1..].iter().map(|a| util::lossy(a)).collect();
    let _lock = util::ExclusiveLock::acquire(&util::haul_home().join("installs.lock")).map_err(|e| format!("cannot lock the install record: {}", e))?;
    match inv.command.as_deref() {
        Some("uninstall") => uninstall(&rest, cwd),
        _ => {
            let r = parse_install(&rest)?;
            let root = root_of(&r, cwd)?;
            if r.list {
                list(&root);
                return Ok(0);
            }
            install(&r, inv, cwd, cfg, &root)
        }
    }
}

fn list(root: &Path) {
    let rec = load_record();
    let pkgs = rec["roots"][util::path_str(root)].as_object().cloned().unwrap_or_default();
    for (name, p) in pkgs {
        println!("{} v{} ({}):", name, util::vstr(&p, "version"), util::vstr(&p["source"], "display"));
        for b in p["bins"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            println!("    {}", b);
        }
    }
}

/// Where a package's source is, resolved.
struct Source {
    name: String,
    version: String,
    /// The package directory to build.
    dir: PathBuf,
    /// How the record names it: `{"kind": …, "display": …}`.
    record: Value,
    /// The workspace of Haul's copy (registry, git), whose `Cargo.lock` the install chooses;
    /// `None` for a workspace of the user's (`--path`).
    copy_root: Option<PathBuf>,
    /// The copy's lock, held until the install is done.
    _lock: Option<util::ExclusiveLock>,
}

/// The lock of a copy in Haul's cache (`<copy>.lock`), which the cache's collection respects.
pub fn copy_lock(dir: &Path) -> PathBuf {
    dir.with_file_name(format!("{}.lock", util::lossy(dir.file_name().unwrap_or_default())))
}

fn install(r: &Request, inv: &Invocation, cwd: &Path, cfg: &Config, root: &Path) -> Result<i32, String> {
    let mut failed = vec![];
    let mut installed = 0;
    let targets: Vec<(String, Option<String>)> = if r.crates.is_empty() { vec![(String::new(), None)] } else { r.crates.clone() };
    let many = targets.len() > 1;
    let all = crate::ui::Phase::start();
    for (name, req) in &targets {
        let attempt = crate::ui::Phase::start();
        match install_one(r, name, req, inv, cwd, cfg, root) {
            Ok(_) => installed += 1,
            Err(e) if many => {
                attempt.finish(false, &format!("{}: {}", if name.is_empty() { "the package" } else { name }, e));
                failed.push(name.clone());
            }
            Err(e) => return Err(e),
        }
    }
    if many {
        let summary = format!("Installed {} of {}{}", installed, targets.len(), if failed.is_empty() { String::new() } else { format!(" · failed: {}", failed.join(", ")) });
        all.finish(failed.is_empty(), &summary);
    }
    Ok(if failed.is_empty() { 0 } else { 101 })
}

#[allow(clippy::too_many_arguments)]
fn install_one(r: &Request, name: &str, req: &Option<String>, inv: &Invocation, cwd: &Path, cfg: &Config, root: &Path) -> Result<(), String> {
    let mut phase = crate::ui::Phase::start();
    phase.status(&format!("Resolving {}", if name.is_empty() { "the package" } else { name }), true);
    let source = match (&r.path, &r.git) {
        (Some(p), _) => local(&cwd.join(p), name, inv, cwd, cfg)?.0,
        (None, Some(url)) => git(url, r.git_ref.as_ref(), name, inv, cwd, cfg)?,
        (None, None) => registry(name, req, r, inv, cfg)?,
    };
    let features = feature_record(r);
    let prior = load_record()["roots"][util::path_str(root)][&source.name].clone();
    let same = |p: &Value| p["version"] == json!(source.version) && p["source"] == source.record && p["features"] == features && p["profile"] == json!(r.profile) && p["target"] == json!(r.target);
    if !r.force && same(&prior) && prior["bins"].as_array().is_some_and(|b| b.iter().filter_map(Value::as_str).all(|b| root.join("bin").join(exe(b, r.target.as_deref())).is_file())) {
        phase.finish(true, &format!("{} {} is installed already (--force installs it again)", source.name, source.version));
        return Ok(());
    }
    // What it builds: the package's binaries (or those asked for), as `cargo install` picks them.
    let engine = crate::engine::for_command(&resolve::classify(&["build".into()]), &source.dir, cfg)?;
    let meta = metadata(&engine, &source.dir.join("Cargo.toml"), true)?;
    let pkg = meta["packages"].as_array().and_then(|p| p.iter().find(|p| p["name"] == json!(source.name))).ok_or_else(|| format!("{} has no package {}", source.dir.display(), source.name))?.clone();
    let wanted = select_targets(&pkg, r)?;
    // Built like any build Haul runs.
    let mut build: Vec<OsString> = inv.toolchain.iter().map(|t| OsString::from(format!("+{}", t))).collect();
    build.extend(["build", "--manifest-path"].map(OsString::from));
    build.push(source.dir.join("Cargo.toml").into_os_string());
    build.extend(["--profile".into(), OsString::from(&r.profile)]);
    for (kind, n) in &wanted {
        build.extend([OsString::from(if kind == "bin" { "--bin" } else { "--example" }), OsString::from(n)]);
    }
    if !r.features.is_empty() {
        build.extend(["--features".into(), r.features.join(",").into()]);
    }
    if r.all_features {
        build.push("--all-features".into());
    }
    if r.no_default_features {
        build.push("--no-default-features".into());
    }
    if let Some(t) = &r.target {
        build.extend(["--target".into(), t.into()]);
    }
    match &source.copy_root {
        // Haul's copy: Cargo resolves afresh, or with --locked from the published lockfile.
        Some(root) => {
            let lock = root.join("Cargo.lock");
            match fs::remove_file(&lock) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot remove {}: {}", lock.display(), e)),
            }
            let published = root.join(PUBLISHED_LOCK);
            if r.locked && published.is_file() {
                fs::copy(&published, &lock).map_err(|e| format!("cannot restore {}'s published lockfile: {}", source.name, e))?;
                build.push("--locked".into());
            } else if r.locked {
                phase.line(&format!("note: {} {} was published without a lockfile; --locked has nothing to hold it to", source.name, source.version));
            }
        }
        None if r.locked => build.push("--locked".into()),
        None => {}
    }
    build.extend(r.build_args.iter().map(OsString::from));
    phase.clear_line();
    let mut spec = crate::session::Spec::new(build, crate::session::Mode::Explicit);
    spec.cwd = Some(source.dir.clone());
    let out = crate::session::execute(&spec)?;
    if !out.exit.success() {
        return Err(format!("{} {} did not build", source.name, source.version));
    }
    // Into the bin directory.
    let target_dir = engine.target.as_ref().map(|t| t.dir.clone()).ok_or("the package has no target directory")?;
    let profile_dir = match r.profile.as_str() {
        "dev" | "test" => "debug",
        "release" | "bench" => "release",
        p => p,
    };
    let mut built = target_dir;
    if let Some(t) = &r.target {
        built = built.join(t);
    }
    built = built.join(profile_dir);
    let bin_dir = root.join("bin");
    util::create_private_dir(&bin_dir).map_err(|e| format!("cannot create {}: {}", bin_dir.display(), e))?;
    let mut record = load_record();
    let owners: Vec<(String, String)> = record["roots"][util::path_str(root)].as_object().into_iter().flatten().flat_map(|(pkg, p)| p["bins"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|b| (b.to_string(), pkg.clone())).collect::<Vec<_>>()).collect();
    let mut placed = vec![];
    for (kind, n) in &wanted {
        let file = exe(n, r.target.as_deref());
        if reserved(n) {
            return Err(format!("{} would replace Haul's own `{}` in {}", source.name, n, bin_dir.display()));
        }
        let dest = bin_dir.join(&file);
        if fs::symlink_metadata(&dest).is_ok() && !r.force {
            match owners.iter().find(|(b, _)| b == n) {
                Some((_, owner)) if *owner == source.name => {}
                Some((_, owner)) => return Err(format!("{} is {}'s; --force replaces it", dest.display(), owner)),
                None => return Err(format!("{} exists and was not installed by Haul; --force replaces it", dest.display())),
            }
        }
        let from = if kind == "bin" { built.join(&file) } else { built.join("examples").join(&file) };
        let staged = bin_dir.join(format!(".{}.{}", file, std::process::id()));
        fs::copy(&from, &staged).map_err(|e| format!("cannot copy {}: {}", from.display(), e))?;
        if cfg!(windows) && dest.exists() {
            let _ = fs::rename(&dest, bin_dir.join(format!(".{}.old.{}", file, std::process::id())));
        }
        fs::rename(&staged, &dest).map_err(|e| {
            let _ = fs::remove_file(&staged);
            format!("cannot install {}: {}", dest.display(), e)
        })?;
        placed.push(n.clone());
    }
    if !r.no_track {
        let roots = record["roots"].as_object_mut().ok_or("the install record has no roots")?;
        let entry = roots.entry(util::path_str(root)).or_insert_with(|| json!({}));
        // A binary another package had (with --force) is this one's now.
        for (_, p) in entry.as_object_mut().into_iter().flatten() {
            if let Some(bins) = p["bins"].as_array_mut() {
                bins.retain(|b| !placed.iter().any(|n| b == n));
            }
        }
        entry[&source.name] = json!({ "version": source.version, "source": source.record, "features": features, "profile": r.profile, "target": r.target, "bins": placed, "toolchain": engine.toolchain.id, "installed_at": util::iso8601(util::now_wall_ns()) });
        save_record(&record)?;
    }
    let home = util::home_dir();
    let tilde = |p: &Path| home.as_ref().and_then(|h| p.strip_prefix(h).ok().map(|r| format!("~/{}", r.display()))).unwrap_or_else(|| p.display().to_string());
    phase.finish(true, &format!("Installed {} {} · {} → {}", source.name, source.version, placed.join(", "), tilde(&bin_dir)));
    Ok(())
}

fn exe(name: &str, target: Option<&str>) -> String {
    let windows = target.map(|t| t.contains("windows")).unwrap_or(cfg!(windows));
    if windows {
        format!("{}.exe", name)
    } else {
        name.to_string()
    }
}

fn feature_record(r: &Request) -> Value {
    let f: BTreeSet<&String> = r.features.iter().collect();
    json!({ "features": f, "all": r.all_features, "no_default": r.no_default_features })
}

/// The binaries (`("bin", name)`) and examples (`("example", name)`) to install.
fn select_targets(pkg: &Value, r: &Request) -> Result<Vec<(String, String)>, String> {
    let targets: Vec<&Value> = pkg["targets"].as_array().into_iter().flatten().collect();
    let kinds = |t: &Value, k: &str| t["kind"].as_array().is_some_and(|ks| ks.iter().any(|x| x == k));
    let enabled: BTreeSet<String> = {
        let declared: Vec<String> = pkg["features"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        let mut on: BTreeSet<String> = r.features.iter().cloned().collect();
        if r.all_features {
            on.extend(declared.iter().cloned());
        } else if !r.no_default_features {
            on.extend(pkg["features"]["default"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(str::to_string)));
        }
        on
    };
    let available = |t: &Value| t["required-features"].as_array().map_or(true, |req| req.iter().filter_map(Value::as_str).all(|f| enabled.contains(f)));
    let mut out = vec![];
    let name_of = |t: &Value| t["name"].as_str().unwrap_or("").to_string();
    for (kind, asked, all) in [("bin", &r.bins, r.all_bins), ("example", &r.examples, r.all_examples)] {
        for n in asked {
            let t = targets.iter().find(|t| kinds(t, kind) && t["name"] == json!(n)).ok_or_else(|| format!("{} has no {} `{}`", util::vstr(pkg, "name"), kind, n))?;
            if !available(t) {
                return Err(format!("{} `{}` needs features {}", kind, n, t["required-features"]));
            }
            out.push((kind.to_string(), n.clone()));
        }
        if all {
            out.extend(targets.iter().filter(|t| kinds(t, kind) && available(t)).map(|t| (kind.to_string(), name_of(t))));
        }
    }
    if out.is_empty() && r.bins.is_empty() && r.examples.is_empty() && !r.all_examples {
        out.extend(targets.iter().filter(|t| kinds(t, "bin") && available(t)).map(|t| ("bin".to_string(), name_of(t))));
    }
    out.dedup();
    if out.is_empty() {
        return Err(format!("there is nothing to install in `{}`: it has no binaries{}", util::vstr(pkg, "name"), if targets.iter().any(|t| kinds(t, "bin")) { " the selected features enable" } else { "" }));
    }
    Ok(out)
}

/// `cargo metadata` of the manifest at `manifest`.
fn metadata(engine: &crate::engine::Engine, manifest: &Path, no_deps: bool) -> Result<Value, String> {
    let mut args: Vec<OsString> = ["metadata", "--format-version", "1", "--manifest-path"].map(OsString::from).to_vec();
    args.push(manifest.as_os_str().to_os_string());
    if no_deps {
        args.push("--no-deps".into());
    }
    let mut c = engine.command(&args, &resolve::classify(&[]));
    c.stdin(std::process::Stdio::null()).env(crate::ENV_EXEC_GUARD, format!("{}:install", std::process::id()));
    let out = c.output().map_err(|e| format!("cannot run {}: {}", engine.cargo().display(), e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().filter(|l| !l.trim().is_empty()).map(str::trim).collect::<Vec<_>>().join("\n"));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {}", e))
}

/// A crate from a registry: the release Cargo's resolver picks, its source copied into Haul's
/// cache.
fn registry(name: &str, req: &Option<String>, r: &Request, inv: &Invocation, cfg: &Config) -> Result<Source, String> {
    let probe = util::cache_dir().join("tmp").join(format!("install-{}", std::process::id()));
    let _ = fs::remove_dir_all(&probe);
    fs::create_dir_all(probe.join("src")).map_err(|e| format!("cannot create {}: {}", probe.display(), e))?;
    let out = (|| {
        let registry = r.registry.as_ref().map(|reg| format!(", registry = {:?}", reg)).unwrap_or_default();
        let manifest = format!("[package]\nname = \"haul-install-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[dependencies]\n{:?} = {{ version = {:?}, artifact = \"bin\", default-features = false{} }}\n\n[workspace]\n", name, requirement(req), registry);
        fs::write(probe.join("Cargo.toml"), manifest).map_err(|e| e.to_string())?;
        fs::write(probe.join("src/lib.rs"), "").map_err(|e| e.to_string())?;
        let mut probe_inv = resolve::classify(&inv.toolchain.iter().map(|t| OsString::from(format!("+{}", t))).collect::<Vec<_>>());
        probe_inv.targets.clear();
        let engine = crate::engine::for_command(&probe_inv, &probe, cfg)?;
        // Releases for this toolchain's Rust, as `cargo install` picks them.
        let release = crate::rustc_info::version_verbose(&util::memo_dir("rustc"), &util::path_str(&engine.rustc()))?;
        let rust = release.lines().find_map(|l| l.strip_prefix("release: ")).unwrap_or("").split('-').next().unwrap_or("").to_string();
        let text = fs::read_to_string(probe.join("Cargo.toml")).map_err(|e| e.to_string())?;
        fs::write(probe.join("Cargo.toml"), text.replacen("publish = false\n", &format!("publish = false\nrust-version = {:?}\n", rust), 1)).map_err(|e| e.to_string())?;
        let mut args: Vec<OsString> = ["-Zbindeps", "--config", "resolver.incompatible-rust-versions=\"fallback\"", "metadata", "--format-version", "1", "--manifest-path"].map(OsString::from).to_vec();
        args.push(probe.join("Cargo.toml").into_os_string());
        let mut c = engine.command(&args, &resolve::classify(&[]));
        c.stdin(std::process::Stdio::null()).env("RUSTC_BOOTSTRAP", "1").env(crate::ENV_EXEC_GUARD, format!("{}:install", std::process::id()));
        let o = c.output().map_err(|e| format!("cannot run {}: {}", engine.cargo().display(), e))?;
        if !o.status.success() {
            let err = String::from_utf8_lossy(&o.stderr);
            if err.contains("requires a `bin` artifact") {
                return Err(format!("there is nothing to install in `{}`: it has no binaries", name));
            }
            return Err(err.lines().filter(|l| l.trim_start().starts_with("error") || l.contains("candidate versions") || l.contains("required by") || l.contains("location searched")).map(str::trim).collect::<Vec<_>>().join("\n"));
        }
        let meta: Value = serde_json::from_slice(&o.stdout).map_err(|e| format!("cargo metadata: {}", e))?;
        let root = meta["resolve"]["root"].as_str().ok_or("cargo metadata named no root")?.to_string();
        let node = meta["resolve"]["nodes"].as_array().into_iter().flatten().find(|n| n["id"] == json!(root)).ok_or("cargo metadata has no root node")?;
        // `dependencies` (not `deps`, which lists only those with a library: a crate of binaries
        // has none).
        let ids: Vec<&str> = node["dependencies"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let pkg = meta["packages"].as_array().into_iter().flatten().find(|p| ids.contains(&util::vstr(p, "id").as_str()) && util::vstr(p, "name").replace('-', "_") == name.replace('-', "_")).ok_or_else(|| format!("Cargo resolved no {}", name))?;
        let version = util::vstr(pkg, "version");
        let manifest = PathBuf::from(util::vstr(pkg, "manifest_path"));
        let from = manifest.parent().ok_or("no package directory")?;
        let src = util::cache_dir().join("src").join(format!("{}-{}-{}", name, version, &util::digest_parts([util::vstr(pkg, "source")])[..8]));
        let lock = util::ExclusiveLock::acquire(&copy_lock(&src)).map_err(|e| format!("cannot lock {}: {}", src.display(), e))?;
        copy_package(from, &src)?;
        let display = match &r.registry {
            Some(reg) => format!("registry `{}`", reg),
            None => "crates.io".to_string(),
        };
        Ok(Source { name: util::vstr(pkg, "name"), version, dir: src.clone(), record: json!({ "kind": "registry", "id": util::vstr(pkg, "source"), "display": display }), copy_root: Some(src), _lock: Some(lock) })
    })();
    let _ = fs::remove_dir_all(&probe);
    out
}

/// A copy of a published package's source (complete or not there at all), its lockfile kept
/// aside; marked used now.
fn copy_package(from: &Path, to: &Path) -> Result<(), String> {
    if to.join(COMPLETE).is_file() {
        return fs::write(to.join(COMPLETE), b"").map_err(|e| format!("cannot mark {} used: {}", to.display(), e));
    }
    let staging = to.with_file_name(format!(".{}.{}.part", util::lossy(to.file_name().unwrap_or_default()), std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let out = (|| -> std::io::Result<()> {
        let mut stack = vec![PathBuf::new()];
        while let Some(rel) = stack.pop() {
            fs::create_dir_all(staging.join(&rel))?;
            for e in fs::read_dir(from.join(&rel))?.flatten() {
                let r = rel.join(e.file_name());
                let md = fs::symlink_metadata(e.path())?;
                if md.is_dir() {
                    stack.push(r);
                } else if md.is_file() && r != Path::new(".cargo-ok") {
                    let into = if r == Path::new("Cargo.lock") { staging.join(PUBLISHED_LOCK) } else { staging.join(&r) };
                    fs::copy(e.path(), into)?;
                }
            }
        }
        fs::write(staging.join(COMPLETE), b"")?;
        if to.exists() {
            fs::remove_dir_all(to)?;
        }
        fs::rename(&staging, to)
    })();
    out.map_err(|e| {
        let _ = fs::remove_dir_all(&staging);
        format!("cannot copy {} to {}: {}", from.display(), to.display(), e)
    })
}

/// A package from a git repository, cloned with the system `git` into Haul's cache.
fn git(url: &str, git_ref: Option<&(String, String)>, name: &str, inv: &Invocation, cwd: &Path, cfg: &Config) -> Result<Source, String> {
    let git = util::which("git").ok_or("--git needs git on PATH")?;
    let dir = util::cache_dir().join("src").join(format!("git-{}", &util::digest_parts([url.to_string(), format!("{:?}", git_ref)])[..16]));
    let lock = util::ExclusiveLock::acquire(&copy_lock(&dir)).map_err(|e| format!("cannot lock {}: {}", dir.display(), e))?;
    match fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove the previous clone {}: {}", dir.display(), e)),
    }
    let mut clone = std::process::Command::new(&git);
    clone.args(["clone", "--quiet", "--recurse-submodules"]);
    if let Some((kind, r)) = git_ref.filter(|(k, _)| k != "rev") {
        let _ = kind;
        clone.args(["--branch", r]);
    }
    clone.arg(url).arg(&dir).stdin(std::process::Stdio::null());
    let out = clone.output().map_err(|e| format!("cannot run git: {}", e))?;
    if !out.status.success() {
        return Err(format!("git could not clone {}: {}", url, String::from_utf8_lossy(&out.stderr).trim()));
    }
    if let Some((_, rev)) = git_ref.filter(|(k, _)| k == "rev") {
        let out = std::process::Command::new(&git).arg("-C").arg(&dir).args(["checkout", "--quiet", rev]).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("git has no revision {} in {}: {}", rev, url, String::from_utf8_lossy(&out.stderr).trim()));
        }
    }
    let head = std::process::Command::new(&git).arg("-C").arg(&dir).args(["rev-parse", "HEAD"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let (mut s, root) = local(&dir, name, inv, cwd, cfg)?;
    if root.join("Cargo.lock").is_file() {
        fs::rename(root.join("Cargo.lock"), root.join(PUBLISHED_LOCK)).map_err(|e| format!("cannot keep {}'s lockfile aside: {}", url, e))?;
    }
    fs::write(dir.join(COMPLETE), b"").map_err(|e| format!("cannot mark {} complete: {}", dir.display(), e))?;
    s.record = json!({ "kind": "git", "url": url, "rev": head, "display": format!("{}#{}", url, &head[..head.len().min(8)]) });
    s.copy_root = Some(root);
    s._lock = Some(lock);
    Ok(s)
}

/// A package in a local directory (a workspace, or one of its members by `name`), and the root
/// of its workspace.
fn local(dir: &Path, name: &str, inv: &Invocation, _cwd: &Path, cfg: &Config) -> Result<(Source, PathBuf), String> {
    let _ = inv;
    let engine = crate::engine::for_command(&resolve::classify(&[]), dir, cfg)?;
    let meta = metadata(&engine, &dir.join("Cargo.toml"), true)?;
    let members: Vec<&Value> = meta["packages"].as_array().into_iter().flatten().collect();
    let with_bins = |p: &&&Value| p["targets"].as_array().is_some_and(|t| t.iter().any(|t| t["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "bin"))));
    let pkg = if name.is_empty() {
        let candidates: Vec<&&Value> = members.iter().filter(with_bins).collect();
        match candidates.as_slice() {
            [one] => **one,
            [] => return Err(format!("there is nothing to install in {}: no package has binaries", dir.display())),
            many => return Err(format!("{} has several packages with binaries; name one: {}", dir.display(), many.iter().map(|p| util::vstr(p, "name")).collect::<Vec<_>>().join(", "))),
        }
    } else {
        *members.iter().find(|p| p["name"] == json!(name)).ok_or_else(|| format!("{} has no package `{}`", dir.display(), name))?
    };
    let manifest = PathBuf::from(util::vstr(pkg, "manifest_path"));
    let root = meta["workspace_root"].as_str().map(PathBuf::from).ok_or("cargo metadata named no workspace root")?;
    Ok((Source { name: util::vstr(pkg, "name"), version: util::vstr(pkg, "version"), dir: manifest.parent().unwrap_or(dir).to_path_buf(), record: json!({ "kind": "path", "path": util::path_str(&util::canon(dir)), "display": util::path_str(&util::canon(dir)) }), copy_root: None, _lock: None }, root))
}

/// `haul uninstall <package>… [--bin NAME]… [--root DIR]`.
fn uninstall(args: &[String], cwd: &Path) -> Result<i32, String> {
    let mut pkgs = vec![];
    let mut bins = vec![];
    let mut r = Request::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bin" => bins.push(need(args, &mut i, "--bin")?),
            "--root" => r.root = Some(PathBuf::from(need(args, &mut i, "--root")?)),
            "-q" | "--quiet" | "-v" | "--verbose" => {}
            f if f.starts_with('-') => return Err(format!("`haul uninstall` has no option `{}`", f)),
            p => pkgs.push(p.split('@').next().unwrap_or(p).to_string()),
        }
        i += 1;
    }
    if pkgs.is_empty() {
        return Err("`haul uninstall` needs a package".into());
    }
    let root = root_of(&r, cwd)?;
    let mut record = load_record();
    let key = util::path_str(&root);
    for pkg in &pkgs {
        let entry = record["roots"][&key][pkg].clone();
        let had: Vec<String> = entry["bins"].as_array().ok_or_else(|| format!("{} is not installed in {}", pkg, root.display()))?.iter().filter_map(|b| b.as_str().map(str::to_string)).collect();
        let gone: Vec<String> = if bins.is_empty() { had.clone() } else { bins.iter().filter(|b| had.contains(b)).cloned().collect() };
        if let Some(b) = bins.iter().find(|b| !had.contains(b)) {
            return Err(format!("{} has no binary `{}`", pkg, b));
        }
        for b in &gone {
            let f = root.join("bin").join(exe(b, entry["target"].as_str()));
            match fs::remove_file(&f) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot remove {}: {}", f.display(), e)),
            }
        }
        let left: Vec<String> = had.into_iter().filter(|b| !gone.contains(b)).collect();
        let pkgs_of_root = record["roots"][&key].as_object_mut().ok_or("the install record has no such root")?;
        if left.is_empty() {
            pkgs_of_root.remove(pkg);
        } else {
            pkgs_of_root[pkg]["bins"] = json!(left);
        }
        crate::ui::Phase::start().finish(true, &format!("Removed {} ({})", pkg, gone.join(", ")));
    }
    save_record(&record)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Request, String> {
        parse_install(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn reads_cargo_installs_arguments() {
        let r = p(&["ripgrep@14.1", "--features", "pcre2,simd", "-F", "x", "--locked", "-j", "4", "--root=/opt/x"]).unwrap();
        assert_eq!(r.crates, vec![("ripgrep".to_string(), Some("14.1".to_string()))]);
        assert_eq!(r.features, ["pcre2", "simd", "x"]);
        assert!(r.locked);
        assert_eq!(r.build_args, ["-j", "4"]);
        assert_eq!(r.root, Some(PathBuf::from("/opt/x")));
        assert_eq!(r.profile, "release");
        assert_eq!(p(&["x", "--debug"]).unwrap().profile, "dev");
        assert_eq!(p(&["x", "--version", "1.2.3"]).unwrap().crates[0].1.as_deref(), Some("1.2.3"));
        assert!(p(&["--git", "https://g/x", "--rev", "abc"]).unwrap().git_ref.is_some());
        for (bad, why) in [(&["x", "--frobnicate"][..], "no option"), (&[][..], "needs a crate"), (&["a", "b", "--version", "1"], "exactly one crate"), (&["x@1", "--version", "2"], "twice"), (&["--path", ".", "--git", "u"], "exclude"), (&["x", "--tag", "v1"], "go with --git"), (&["x/y"], "not a crate name")] {
            let e = p(bad).unwrap_err();
            assert!(e.contains(why), "{:?}: {}", bad, e);
        }
    }

    #[test]
    fn a_bare_version_is_exactly_that_one() {
        assert_eq!(requirement(&None), "*");
        assert_eq!(requirement(&Some("1.2.3".into())), "=1.2.3");
        assert_eq!(requirement(&Some("1.2".into())), "1.2");
        assert_eq!(requirement(&Some("^1".into())), "^1");
        assert!(reserved("cargo") && reserved("rustfmt") && reserved("haul") && !reserved("rg"));
    }
}
