//! Installing QuickRust on a machine (`qrust setup`) and removing it (`qrust self uninstall`).
//!
//! Setup puts QuickRust into `~/.qrust/bin` (the binary, its rustc wrapper, and the proxies: QuickRust as
//! `cargo`, `rustc`, `rustfmt`, …), puts that directory first on `PATH` in the shell's startup
//! files, imports what is the user's own from Cargo's home (`~/.cargo`, or `CARGO_HOME`) once —
//! registry credentials and the `[registries] [registry] [source] [net] [http]` tables of its
//! configuration — and installs the default toolchain. Running it again repairs what is missing
//! and never imports again. `setup.json` in QuickRust's home records what it did, for `doctor` and
//! for uninstalling.
//!
//! There is one `qrust` per machine. Started from a package manager's install (see `package`), setup
//! copies nothing: `~/.qrust/bin`'s `qrust` and rustc wrapper are links to the package's, by the path
//! that stays across its versions, so every `qrust` on `PATH` is that one program and the package
//! manager's upgrade upgrades what runs; a copy an earlier setup made there is replaced by the link.
//! Started from anything else (an archive, a source build), setup copies the binary into
//! `~/.qrust/bin`, and that copy is the install: `qrust self update` keeps it current and `qrust self
//! uninstall` removes it. A copy does not set up over a package's install, and a package's install
//! is updated and removed by its manager, never by QuickRust.

use crate::engine;
use crate::package::Package;
use crate::util;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Marks the lines setup adds to shell startup files.
const MARK: &str = "# Added by `qrust setup`: QuickRust's bin first on PATH";
/// The configuration tables imported from Cargo's home: where crates come from and how.
const IMPORTED_TABLES: &[&str] = &["registries", "registry", "source", "net", "http"];

pub struct Options {
    pub modify_path: bool,
    /// The toolchain to install now; `None`: none.
    pub toolchain: Option<String>,
}

fn record_path() -> PathBuf {
    util::qrust_home().join("setup.json")
}

fn home() -> Result<PathBuf, String> {
    util::home_dir().ok_or_else(|| "HOME is not set".to_string())
}

/// A path as shell startup files and pasted commands name it: through `$HOME` when it is under it.
pub fn shell_path(p: &Path) -> String {
    match home().ok().and_then(|h| p.strip_prefix(&h).ok().map(Path::to_path_buf)) {
        Some(rel) => format!("$HOME/{}", util::path_str(&rel)),
        None => util::path_str(p),
    }
}

/// What setup did, for the summary.
pub struct Done {
    pub version: String,
    pub bin: PathBuf,
    pub proxies: usize,
    pub path_edits: Vec<PathBuf>,
    /// When this run imported from Cargo's home.
    pub import: Option<Imported>,
    /// The package manager's install `bin` links to; `None`: `bin` holds QuickRust's own copy.
    pub package: Option<Package>,
    /// A copy of QuickRust in `bin` that this run replaced with the link to the package's.
    pub replaced_copy: Option<PathBuf>,
}

/// An import from Cargo's home: from where, what was imported, what was not (and why).
pub type Imported = (PathBuf, Vec<String>, Vec<String>);

pub fn setup(opts: &Options) -> Result<Done, String> {
    let bin = engine::bin_dir();
    util::create_private_dir(&bin).map_err(|e| format!("cannot create {}: {}", bin.display(), e))?;
    let previous = util::read_json(&record_path());
    let (me, _) = crate::resolve::self_identity();
    let package = crate::package::of(&me)?;
    let (installed, replaced_copy) = match &package {
        Some(p) => link_package(&bin, p)?,
        None => {
            refuse_over_package(&bin)?;
            (install_from(&bin, &me)?, None)
        }
    };
    let proxies = link_proxies(&bin, &installed.qrust, &known_binaries(previous.as_ref(), &installed), PROXIES)?;
    let path_edits = if opts.modify_path { put_on_path(&bin)? } else { vec![] };
    let import = match previous.as_ref().and_then(|r| r.get("imported_from")).filter(|v| !v.is_null()) {
        Some(_) => None,
        None => import_cargo_home()?,
    };
    let mut record = previous.unwrap_or_else(|| json!({ "format": 1 }));
    remember_binary(&mut record, &installed.qrust)?;
    record["version"] = json!(env!("CARGO_PKG_VERSION"));
    record["bin"] = json!(util::path_str(&bin));
    record["installed_by"] = match &package {
        Some(p) => json!({ "manager": p.manager, "program": util::path_str(&p.program) }),
        None => Value::Null,
    };
    record["set_up_at"] = json!(util::iso8601(util::now_wall_ns()));
    let mut edits: Vec<Value> = record["path_edits"].as_array().cloned().unwrap_or_default();
    for e in &path_edits {
        if !edits.contains(&json!(util::path_str(e))) {
            edits.push(json!(util::path_str(e)));
        }
    }
    record["path_edits"] = json!(edits);
    if let Some((from, imported, skipped)) = &import {
        record["imported_from"] = json!(util::path_str(from));
        record["imported"] = json!(imported);
        record["not_imported"] = json!(skipped);
    } else if record.get("imported_from").is_none() {
        record["imported_from"] = Value::Null;
    }
    util::write_json(&record_path(), &record).map_err(|e| format!("cannot write {}: {}", record_path().display(), e))?;
    Ok(Done { version: env!("CARGO_PKG_VERSION").to_string(), bin, proxies, path_edits, import, package, replaced_copy })
}

/// What an install put in `bin`: `bin/qrust`, and the digest of the `qrust` it replaced, if it
/// replaced one.
#[derive(Debug)]
struct Installed {
    qrust: PathBuf,
    replaced: Option<String>,
}

/// Makes `bin`'s `qrust` and rustc wrapper links to the package's, by the path that stays across
/// the package's versions. A copy of QuickRust there (an earlier setup's, from an archive or a
/// package before setup linked) is replaced, and returned; anything else is refused.
#[cfg(unix)]
fn link_package(bin: &Path, package: &Package) -> Result<(Installed, Option<PathBuf>), String> {
    let wrapper = package.wrapper();
    if !util::is_executable(&wrapper) {
        return Err(format!("qrust-rustc-wrapper is not at {}, where {} installs it beside qrust; reinstall the package", wrapper.display(), package.manager));
    }
    let qrust = util::exe_candidates(bin, "qrust").remove(0);
    let mut replaced = None;
    for (link, to) in [(qrust.clone(), package.program.clone()), (util::exe_candidates(bin, "qrust-rustc-wrapper").remove(0), wrapper)] {
        match fs::symlink_metadata(&link) {
            Ok(md) if md.file_type().is_symlink() => {}
            Ok(md) if md.is_file() => {
                if link == qrust {
                    replaced = Some(link.clone());
                }
            }
            Ok(_) => return Err(format!("{} exists and is not QuickRust; move it away and run `qrust setup` again", link.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot read {}: {}", link.display(), e)),
        }
        let staged = bin.join(format!(".{}.{}", link.file_name().map(util::lossy).unwrap_or_default(), std::process::id()));
        let _ = fs::remove_file(&staged);
        std::os::unix::fs::symlink(&to, &staged).and_then(|_| fs::rename(&staged, &link)).map_err(|e| {
            let _ = fs::remove_file(&staged);
            format!("cannot link {} to {}: {}", link.display(), to.display(), e)
        })?;
    }
    Ok((Installed { qrust, replaced: None }, replaced))
}

/// No package manager installs QuickRust where proxies are copies (`package::of` refuses a marker
/// there).
#[cfg(not(unix))]
fn link_package(_bin: &Path, package: &Package) -> Result<(Installed, Option<PathBuf>), String> {
    Err(format!("{} cannot install QuickRust on this platform; install it from its archive", package.manager))
}

/// A QuickRust that installs itself does not set up over a package manager's: `bin` links to the
/// package's `qrust` while that package is installed. A link the package left behind (it was
/// removed) is replaced.
fn refuse_over_package(bin: &Path) -> Result<(), String> {
    let qrust = util::exe_candidates(bin, "qrust").remove(0);
    if !fs::symlink_metadata(&qrust).is_ok_and(|m| m.file_type().is_symlink()) || !qrust.exists() {
        return Ok(());
    }
    match crate::package::of(&qrust)? {
        Some(p) => Err(format!(
            "{} links to the QuickRust {} installed ({}), and `{}` keeps it current; to set up this QuickRust instead, remove that one first (`{}`)",
            qrust.display(),
            p.manager,
            p.program.display(),
            p.update,
            p.uninstall
        )),
        None => Ok(()),
    }
}

/// What only a QuickRust that installed itself does to itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnAction {
    Update,
    Uninstall,
}

/// The package manager's install this QuickRust is, if it is one: the running binary's, or the one
/// `bin` links to.
fn package_install(bin: &Path, me: &Path) -> Result<Option<Package>, String> {
    if let Some(p) = crate::package::of(me)? {
        return Ok(Some(p));
    }
    let qrust = util::exe_candidates(bin, "qrust").remove(0);
    if qrust.exists() {
        crate::package::of(&qrust)
    } else {
        Ok(None)
    }
}

/// Refuses `action` when a package manager installed QuickRust here, naming the manager's command:
/// the package is its manager's to update and remove.
pub fn refuse_for_package(action: OwnAction) -> Result<(), String> {
    let (me, _) = crate::resolve::self_identity();
    refuse_for_package_in(&engine::bin_dir(), &me, action)
}

/// The command that updates this QuickRust: the package manager's when one installed it (the
/// running binary, or the one `~/.qrust/bin` links to), as `qrust self update` names it, and
/// otherwise `qrust self update` itself.
pub fn update_command() -> Result<String, String> {
    let (me, _) = crate::resolve::self_identity();
    Ok(match package_install(&engine::bin_dir(), &me)? {
        Some(p) => p.update,
        None => "qrust self update".to_string(),
    })
}

fn refuse_for_package_in(bin: &Path, me: &Path, action: OwnAction) -> Result<(), String> {
    let Some(p) = package_install(bin, me)? else { return Ok(()) };
    Err(match action {
        OwnAction::Update => format!("QuickRust was installed with {}, which updates it: `{}`", p.manager, p.update),
        OwnAction::Uninstall => format!(
            "QuickRust was installed with {}, which removes it: `{}`; QuickRust's home ({}: toolchains, installed programs, caches, records) and the lines marked `{}` in your shell startup files stay, for you to delete",
            p.manager,
            p.uninstall,
            util::qrust_home().display(),
            MARK
        ),
    })
}

/// Copies the QuickRust binary `qrust` and the rustc wrapper next to it into `bin` (unless they are
/// there already).
///
/// The digest of the `qrust` this replaces is kept: on Windows the proxies are copies of it, and an
/// update that forgot what it replaced took its own proxies for a stranger's.
fn install_from(bin: &Path, me: &Path) -> Result<Installed, String> {
    let me = me.to_path_buf();
    let wrapper = util::exe_candidates(me.parent().unwrap_or(Path::new(".")), "qrust-rustc-wrapper").into_iter().find(|p| util::is_executable(p)).ok_or_else(|| format!("qrust-rustc-wrapper is not next to {}; QuickRust is installed with it", me.display()))?;
    let target = util::exe_candidates(bin, "qrust").remove(0);
    let mut replaced = None;
    for (from, to) in [(&me, target.clone()), (&wrapper, util::exe_candidates(bin, "qrust-rustc-wrapper").remove(0))] {
        if util::canon(from) == util::canon(&to) {
            continue;
        }
        if to == target && to.is_file() {
            replaced = Some(util::blake3_file(&to).map_err(|e| format!("cannot read {}: {}", to.display(), e))?);
        }
        let staged = bin.join(format!(".{}.{}", to.file_name().map(util::lossy).unwrap_or_default(), std::process::id()));
        fs::copy(from, &staged).map_err(|e| format!("cannot copy {} to {}: {}", from.display(), staged.display(), e))?;
        // Windows cannot replace a running executable: the old one is moved aside first.
        if cfg!(windows) && to.exists() {
            let aside = bin.join(format!(".{}.old.{}", to.file_name().map(util::lossy).unwrap_or_default(), std::process::id()));
            fs::rename(&to, &aside).map_err(|e| format!("cannot replace {}: {}", to.display(), e))?;
        }
        fs::rename(&staged, &to).map_err(|e| {
            let _ = fs::remove_file(&staged);
            format!("cannot install {}: {}", to.display(), e)
        })?;
    }
    Ok(Installed { qrust: target, replaced })
}

/// How `bin` holds the proxies: symbolic links to `qrust` (Unix), or copies of it (Windows, as hard
/// links where the filesystem allows).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Proxies {
    Links,
    Copies,
}

const PROXIES: Proxies = if cfg!(windows) { Proxies::Copies } else { Proxies::Links };

/// The digests of every QuickRust binary `bin` may hold copies of besides the one being installed:
/// the `qrust` this install replaced, the ones earlier installs moved aside in `bin`
/// (`.qrust.exe.old.<pid>` on Windows), and every one setup recorded installing (`binaries` in
/// `setup.json`).
///
/// A copy of an earlier QuickRust is QuickRust's own. Recognised only as a copy of the binary
/// installing now, every proxy a previous release left on Windows was refused, so a machine set up
/// by one release could not update to the next, by `qrust self update` or by `qrust setup` (22 Oct
/// 2026). Such a refused update had already replaced `qrust` and moved the earlier one aside, so the
/// proxies are copies of neither the binary installing now nor the one it replaces. The binary moved
/// aside is what they are copies of, and the record predates `binaries`.
fn known_binaries(record: Option<&Value>, installed: &Installed) -> Vec<String> {
    let mut known: Vec<String> = record
        .and_then(|r| r.get("binaries"))
        .and_then(Value::as_array)
        .map(|digests| digests.iter().filter_map(|d| d.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    known.extend(installed.replaced.clone());
    known.extend(moved_aside(&installed.qrust));
    known
}

/// The digests of the binaries an install moved aside beside `qrust`, named
/// `.<qrust's file name>.old.<pid>` by `install_from`.
fn moved_aside(qrust: &Path) -> Vec<String> {
    let (Some(bin), Some(name)) = (qrust.parent(), qrust.file_name().map(util::lossy)) else {
        return Vec::new();
    };
    let prefix = format!(".{name}.old.");
    let Ok(entries) = fs::read_dir(bin) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| util::lossy(&entry.file_name()).starts_with(&prefix))
        .filter_map(|entry| util::blake3_file(&entry.path()).ok())
        .collect()
}

/// Adds the digest of `qrust` to the record's `binaries`, once.
fn remember_binary(record: &mut Value, qrust: &Path) -> Result<(), String> {
    let digest = util::blake3_file(qrust).map_err(|e| format!("cannot read {}: {}", qrust.display(), e))?;
    let mut binaries: Vec<Value> = record["binaries"].as_array().cloned().unwrap_or_default();
    if !binaries.contains(&json!(digest)) {
        binaries.push(json!(digest));
    }
    record["binaries"] = json!(binaries);
    Ok(())
}

/// Makes `cargo` and every proxy in `bin` QuickRust (`bin/qrust`). An entry there that is not QuickRust's
/// is an error, never overwritten. `known` names the earlier QuickRust binaries a copy may be of.
fn link_proxies(bin: &Path, qrust: &Path, known: &[String], mode: Proxies) -> Result<usize, String> {
    let names: Vec<&str> = std::iter::once("cargo").chain(engine::PROXIES.iter().copied()).collect();
    let current = util::blake3_file(qrust).map_err(|e| format!("cannot read {}: {}", qrust.display(), e))?;
    for name in &names {
        let link = util::exe_candidates(bin, name).remove(0);
        if let Ok(md) = fs::symlink_metadata(&link) {
            if !is_qrust(&link, &md, &current, known, mode) {
                return Err(format!("{} exists and is not QuickRust; move it away and run `qrust setup` again", link.display()));
            }
        }
        let staged = bin.join(format!(".{}.{}", name, std::process::id()));
        let _ = fs::remove_file(&staged);
        make_proxy(qrust, &staged, mode).and_then(|_| fs::rename(&staged, &link)).map_err(|e| {
            let _ = fs::remove_file(&staged);
            format!("cannot create {}: {}", link.display(), e)
        })?;
    }
    Ok(names.len())
}

fn make_proxy(qrust: &Path, staged: &Path, mode: Proxies) -> std::io::Result<()> {
    match mode {
        Proxies::Copies => fs::hard_link(qrust, staged).or_else(|_| fs::copy(qrust, staged).map(|_| ())),
        #[cfg(unix)]
        Proxies::Links => std::os::unix::fs::symlink(qrust.file_name().unwrap_or_default(), staged),
        #[cfg(not(unix))]
        Proxies::Links => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "this platform's proxies are copies")),
    }
}

/// Whether the entry at `link` is QuickRust: a link (QuickRust's `bin` holds only QuickRust's
/// links), or, where proxies are copies, a copy of the binary installing now (`current`) or of one
/// QuickRust installed before (`known`).
fn is_qrust(link: &Path, md: &fs::Metadata, current: &str, known: &[String], mode: Proxies) -> bool {
    if md.file_type().is_symlink() {
        return true;
    }
    mode == Proxies::Copies
        && md.is_file()
        && util::blake3_file(link).is_ok_and(|digest| digest == current || known.contains(&digest))
}

/// The shell code that puts `bin` first on `PATH` (sh and fish).
#[cfg(not(windows))]
fn env_scripts(bin: &Path) -> (String, String) {
    let p = shell_path(bin);
    let sh = format!("#!/bin/sh\n# QuickRust's bin first on PATH: QuickRust as cargo, rustc and the other toolchain programs.\ncase \"$PATH\" in\n    \"{p}\" | \"{p}\":*) ;;\n    *) export PATH=\"{p}:$PATH\" ;;\nesac\n");
    let fish = format!("# QuickRust's bin first on PATH: QuickRust as cargo, rustc and the other toolchain programs.\nif test \"$PATH[1]\" != \"{p}\"\n    set -gx PATH \"{p}\" $PATH\nend\n");
    (sh, fish)
}

/// Writes QuickRust's environment files and sources them from the shell startup files that exist
/// (`.profile` always); returns the files changed.
#[cfg(not(windows))]
fn put_on_path(bin: &Path) -> Result<Vec<PathBuf>, String> {
    let home = home()?;
    let qrust_home = util::qrust_home();
    let (sh, fish) = env_scripts(bin);
    let write = |p: &Path, text: &str| util::write_atomic(p, text.as_bytes()).map_err(|e| format!("cannot write {}: {}", p.display(), e));
    write(&qrust_home.join("env"), &sh)?;
    write(&qrust_home.join("env.fish"), &fish)?;
    let line = format!(". \"{}\"", shell_path(&qrust_home.join("env")));
    let mut changed = vec![];
    let zsh = util::which("zsh").is_some();
    for (name, wanted) in [(".profile", true), (".bash_profile", false), (".bashrc", false), (".zshenv", zsh)] {
        let file = home.join(name);
        if !wanted && !file.exists() {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap_or_default();
        if text.lines().any(|l| l.trim() == line) {
            continue;
        }
        let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
        write(&file, &format!("{}{}\n{}\n{}\n", text, sep, MARK, line))?;
        changed.push(file);
    }
    let fish_dir = home.join(".config/fish");
    if fish_dir.is_dir() {
        let conf = fish_dir.join("conf.d/qrust.fish");
        let wanted = format!("{}\nsource \"{}\"\n", MARK, shell_path(&qrust_home.join("env.fish")));
        if fs::read_to_string(&conf).ok().as_deref() != Some(wanted.as_str()) {
            fs::create_dir_all(conf.parent().unwrap_or(&fish_dir)).map_err(|e| e.to_string())?;
            write(&conf, &wanted)?;
            changed.push(conf);
        }
    }
    Ok(changed)
}

/// Puts `bin` first on the user's `PATH` (the registry value Windows keeps for new processes).
#[cfg(windows)]
fn put_on_path(bin: &Path) -> Result<Vec<PathBuf>, String> {
    let script = format!("$b = '{}'; $p = [Environment]::GetEnvironmentVariable('Path', 'User'); $rest = @(($p -split ';') | Where-Object {{ $_ -and $_ -ne $b }}); [Environment]::SetEnvironmentVariable('Path', ((@($b) + $rest) -join ';'), 'User')", util::path_str(bin).replace('\'', "''"));
    let out = std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output().map_err(|e| format!("cannot run powershell to set PATH: {}", e))?;
    if !out.status.success() {
        return Err(format!("could not set the user PATH: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(vec![PathBuf::from("HKCU\\Environment\\Path")])
}

/// Imports the user's own settings from Cargo's home into QuickRust's, once: credentials, and the
/// tables of its configuration that say where crates come from. `None` when there is nothing.
fn import_cargo_home() -> Result<Option<Imported>, String> {
    let from = std::env::var_os("CARGO_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| *p != util::cargo_home()).or_else(|| home().ok().map(|h| h.join(".cargo")));
    let Some(from) = from.filter(|f| f.is_dir()) else { return Ok(None) };
    let to = util::cargo_home();
    util::create_private_dir(&to).map_err(|e| format!("cannot create {}: {}", to.display(), e))?;
    let mut imported = vec![];
    let mut skipped = vec![];
    let first = |names: [&str; 2]| names.iter().map(|n| from.join(n)).find(|p| p.is_file());
    if let Some(cred) = first(["credentials.toml", "credentials"]) {
        let dest = to.join("credentials.toml");
        if dest.exists() {
            skipped.push(format!("{} (QuickRust's Cargo home has credentials already)", cred.display()));
        } else {
            let text = fs::read(&cred).map_err(|e| format!("cannot read {}: {}", cred.display(), e))?;
            write_private(&dest, &text)?;
            imported.push("credentials".to_string());
        }
    }
    if let Some(config) = first(["config.toml", "config"]) {
        let text = fs::read_to_string(&config).map_err(|e| format!("cannot read {}: {}", config.display(), e))?;
        let table: toml::Table = toml::from_str(&text).map_err(|e| format!("{} is not valid TOML: {}", config.display(), e.message()))?;
        let mut kept = toml::Table::new();
        for (k, v) in table {
            if IMPORTED_TABLES.contains(&k.as_str()) {
                kept.insert(k.clone(), v);
                imported.push(format!("[{}]", k));
            } else {
                skipped.push(format!("[{}]", k));
            }
        }
        let dest = to.join("config.toml");
        if !kept.is_empty() {
            if dest.exists() {
                skipped.extend(imported.iter().filter(|i| i.starts_with('[')).cloned());
                imported.retain(|i| !i.starts_with('['));
            } else {
                let body = toml::to_string(&kept).map_err(|e| e.to_string())?;
                let header = format!("# Imported by `qrust setup` from {} (where crates come from).\n# This is the Cargo configuration of every build QuickRust runs.\n\n", config.display());
                write_private(&dest, format!("{}{}", header, body).as_bytes())?;
            }
        }
    }
    Ok(Some((from, imported, skipped)))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    util::write_atomic(path, bytes).map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| format!("cannot restrict {}: {}", path.display(), e))?;
    }
    Ok(())
}

/// Removes what setup added to shell startup files and QuickRust's home with everything in it:
/// installed toolchains and programs, caches, records. Returns what was removed.
pub fn uninstall() -> Result<Vec<String>, String> {
    refuse_for_package(OwnAction::Uninstall)?;
    let mut removed = vec![];
    let record = util::read_json(&record_path());
    for f in record.iter().flat_map(|r| r["path_edits"].as_array().cloned().unwrap_or_default()).filter_map(|v| v.as_str().map(PathBuf::from)) {
        if f.extension().is_some_and(|e| e == "fish") {
            if fs::remove_file(&f).is_ok() {
                removed.push(util::path_str(&f));
            }
            continue;
        }
        if cfg!(windows) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&f) else { continue };
        let source_line = format!(". \"{}\"", shell_path(&util::qrust_home().join("env")));
        if let Some(restored) = without_setup_lines(&text, &source_line) {
            util::write_atomic(&f, restored.as_bytes()).map_err(|e| format!("cannot write {}: {}", f.display(), e))?;
            removed.push(format!("the PATH line in {}", f.display()));
        }
    }
    #[cfg(windows)]
    {
        let bin = engine::bin_dir();
        let script = format!("$b = '{}'; $p = [Environment]::GetEnvironmentVariable('Path', 'User'); [Environment]::SetEnvironmentVariable('Path', ((($p -split ';') | Where-Object {{ $_ -and $_ -ne $b }}) -join ';'), 'User')", util::path_str(&bin).replace('\'', "''"));
        let _ = std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output();
    }
    let home = util::qrust_home();
    if home.exists() {
        fs::remove_dir_all(&home).map_err(|e| format!("cannot remove {}: {}", home.display(), e))?;
        removed.push(util::path_str(&home));
    }
    Ok(removed)
}

/// `text` without what setup appended (a blank line, `MARK`, the source line), or `None` when
/// it holds none of it.
fn without_setup_lines(text: &str, source_line: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![true; lines.len()];
    let mut found = false;
    for i in 0..lines.len() {
        if lines[i] == MARK && lines.get(i + 1).is_some_and(|l| l.trim() == source_line) {
            found = true;
            keep[i] = false;
            keep[i + 1] = false;
            if i > 0 && lines[i - 1].is_empty() {
                keep[i - 1] = false;
            }
        }
    }
    found.then(|| lines.iter().zip(&keep).filter(|(_, k)| **k).map(|(l, _)| format!("{}\n", l)).collect())
}

/// A self-update's outcome: the version before and the one now (equal when QuickRust was current).
pub struct Updated {
    pub from: String,
    pub to: String,
}

/// `qrust self update`: the latest release of this QuickRust's channel, from the release registry
/// it was published in (or `other`, the anchor of another release registry MachineFabric
/// chartered: `self.update-url`).
///
/// The registry is read only through its charter, and its manifest is trusted only through its
/// signature (`crate::release`), the archive
/// only as the file the manifest describes, and the binary in it only once it runs and says it is
/// the version it was published as. Then it replaces the running one in QuickRust's `bin`.
pub fn self_update(other: &str) -> Result<Updated, String> {
    let identity = crate::release::identity().ok_or(
        "this QuickRust was built from source, so it has no release line to update from: build the newer source, or install a release",
    )?;
    let registry = if other.is_empty() { identity.registry.as_str() } else { other };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "the clock is before 1970, so no certificate's validity can be checked".to_string())?.as_secs();
    self_update_from(registry, &identity.channel, &identity.trust, &identity.charter_roots, now)
}

/// [`self_update`] with everything a build bakes in passed explicitly.
pub fn self_update_from(registry: &str, channel: &str, trust: &crate::release::Trust, charter_roots: &[String], now: u64) -> Result<Updated, String> {
    refuse_for_package(OwnAction::Update)?;
    let bin = engine::bin_dir();
    let installed = util::exe_candidates(&bin, "qrust").remove(0);
    if !installed.is_file() {
        return Err(format!("QuickRust is not set up ({} does not exist): `qrust setup` installs it, then `qrust self update` keeps it current", installed.display()));
    }
    let platform = crate::release::platform().ok_or("QuickRust publishes no release for this platform; build it from source")?;
    let client = crate::net::Client::new();
    let current = env!("CARGO_PKG_VERSION").to_string();
    let Some(release) = crate::release::fetch_latest(&client, registry, trust, charter_roots, channel, platform, now)? else {
        return Err(format!("no QuickRust release is published on the {} channel at {} yet", channel, registry));
    };
    let newer = match (semver::Version::parse(&release.version), semver::Version::parse(&current)) {
        (Ok(l), Ok(c)) => l > c,
        (Err(e), _) => return Err(format!("the release manifest's latest version {:?} is not a version: {}", release.version, e)),
        (_, Err(e)) => return Err(format!("this QuickRust's version {:?} is not a version: {}", current, e)),
    };
    if !newer {
        return Ok(Updated { from: current.clone(), to: current });
    }
    let work = util::cache_dir().join("tmp").join(format!("self-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let out = (|| {
        let archive = work.join(&release.name);
        client.download(&release.url, &archive, &release.sha256, &mut |_, _| {}).map_err(|e| e.to_string())?;
        let size = fs::metadata(&archive).map_err(|e| format!("cannot read {}: {}", archive.display(), e))?.len();
        if size != release.size {
            return Err(format!("{} is {} bytes; the release manifest says {}", release.url, size, release.size));
        }
        let unpacked = work.join("qrust");
        match release.archive {
            crate::release::Archive::TarGz => crate::archive::unpack_file(&archive, crate::archive::Compression::Gz, crate::release::ARCHIVE_ROOT, &unpacked, 1 << 30),
            crate::release::Archive::Zip => crate::archive::unpack_zip_file(&archive, crate::release::ARCHIVE_ROOT, &unpacked, 1 << 30),
        }
        .map_err(|e| e.to_string())?;
        let new = util::exe_candidates(&unpacked, "qrust").remove(0);
        // It must run, and be the release it was published as.
        let says = std::process::Command::new(&new).arg("--version").output().map_err(|e| format!("the downloaded QuickRust does not run: {}", e))?;
        let says = String::from_utf8_lossy(&says.stdout).trim().to_string();
        if says != format!("qrust {}", release.version) {
            return Err(format!("the downloaded QuickRust says {:?}, not qrust {}", says, release.version));
        }
        let previous = util::read_json(&record_path());
        let installed = install_from(&bin, &new)?;
        link_proxies(&bin, &installed.qrust, &known_binaries(previous.as_ref(), &installed), PROXIES)?;
        let mut record = previous.unwrap_or_else(|| json!({ "format": 1 }));
        remember_binary(&mut record, &installed.qrust)?;
        record["version"] = json!(release.version);
        util::write_json(&record_path(), &record).map_err(|e| e.to_string())?;
        Ok(Updated { from: current.clone(), to: release.version.clone() })
    })();
    let _ = fs::remove_dir_all(&work);
    out
}

/// What setup recorded (`setup.json`), if QuickRust is set up.
pub fn record() -> Option<Value> {
    util::read_json(&record_path())
}

/// A `qrust` or `cargo` that comes before QuickRust's on `PATH` and is another program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shadow {
    /// The entry on `PATH`.
    pub program: PathBuf,
    /// The file it is.
    pub is: PathBuf,
    /// The package manager that installed it, when it is a package's QuickRust.
    pub package: Option<String>,
    /// What puts its directory on `PATH` (see `origin`).
    pub origin: Option<Origin>,
}

/// The line of a shell startup file that names a directory on `PATH`, and the startup files that
/// source the file it is in, outermost first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub file: PathBuf,
    pub line: usize,
    pub sourced_from: Vec<(PathBuf, usize)>,
    /// How many other lines name it too, earlier in the order the shell reads them.
    pub others: usize,
}

/// What `PATH` makes of QuickRust: whether a `cargo` on it is QuickRust's, and what comes before
/// QuickRust's `qrust` and `cargo` under those names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnPath {
    pub cargo: bool,
    pub shadows: Vec<Shadow>,
}

/// Looks along `path` for `qrust` and `cargo`, which are QuickRust's when they are the program
/// `qrust` (the one setup installed) is: the same file once links are followed or, where proxies are
/// copies, the same content. `startup` are the shell's startup files, in the order it reads them,
/// for what puts each directory on `PATH` (`home`: how they name the home directory).
pub fn on_path(path: &std::ffi::OsStr, qrust: &Path, startup: &[PathBuf], home: Option<&Path>) -> OnPath {
    let ours = util::canon(qrust);
    let digest = (PROXIES == Proxies::Copies).then(|| util::blake3_file(qrust).ok()).flatten();
    let same = |p: &Path| util::canon(p) == ours || digest.as_ref().is_some_and(|d| util::blake3_file(p).ok().as_ref() == Some(d));
    let mut found = [false, false];
    let mut shadows = vec![];
    for dir in std::env::split_paths(path).filter(|d| !d.as_os_str().is_empty()) {
        for (i, name) in ["qrust", "cargo"].iter().enumerate() {
            if found[i] {
                continue;
            }
            let Some(entry) = util::exe_candidates(&dir, name).into_iter().find(|c| util::is_executable(c)) else { continue };
            if same(&entry) {
                found[i] = true;
            } else {
                let package = crate::package::of(&entry).ok().flatten().map(|p| p.manager);
                shadows.push(Shadow { is: util::canon(&entry), program: entry, package, origin: origin(&dir, startup, home) });
            }
        }
        if found == [true, true] {
            break;
        }
    }
    OnPath { cargo: found[1], shadows }
}

/// The shell's startup files, in the order it reads them at login: those of `shell` (zsh, bash,
/// fish, or a POSIX shell's) in `home` and in `etc`, after the directories macOS's `path_helper`
/// reads (`/etc/paths`, `/etc/paths.d`).
pub fn startup_files(shell: &str, home: &Path, etc: &Path) -> Vec<PathBuf> {
    let mut files = vec![etc.join("paths")];
    let mut extra: Vec<PathBuf> = fs::read_dir(etc.join("paths.d")).into_iter().flatten().flatten().map(|e| e.path()).collect();
    extra.sort();
    files.extend(extra);
    let name = Path::new(shell).file_name().map(util::lossy).unwrap_or_default();
    match name.as_str() {
        "zsh" => files.extend([etc.join("zshenv"), home.join(".zshenv"), etc.join("zprofile"), home.join(".zprofile"), etc.join("zshrc"), home.join(".zshrc"), etc.join("zlogin"), home.join(".zlogin")]),
        "bash" => {
            files.push(etc.join("profile"));
            // bash reads the first of these that exists.
            files.extend([".bash_profile", ".bash_login", ".profile"].iter().map(|f| home.join(f)).find(|f| f.is_file()));
            files.extend([etc.join("bash.bashrc"), home.join(".bashrc")]);
        }
        "fish" => {
            let mut conf: Vec<PathBuf> = fs::read_dir(home.join(".config/fish/conf.d")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "fish")).collect();
            conf.sort();
            files.extend(conf);
            files.push(home.join(".config/fish/config.fish"));
        }
        _ => files.extend([etc.join("profile"), home.join(".profile")]),
    }
    files
}

/// The last line, in the order the shell reads `startup` (and what they source), that names `dir`:
/// literally, through `$HOME`, `${HOME}` or `~` (`home`), as a line of `/etc/paths`, or as the
/// Homebrew prefix `brew shellenv` adds. The last is the one that put it where it is.
fn origin(dir: &Path, startup: &[PathBuf], home: Option<&Path>) -> Option<Origin> {
    let mut forms = vec![util::path_str(dir)];
    if let Some(rel) = home.and_then(|h| dir.strip_prefix(h).ok()) {
        let rel = util::path_str(rel);
        forms.extend([format!("$HOME/{rel}"), format!("${{HOME}}/{rel}"), format!("~/{rel}")]);
    }
    let homebrew = util::is_executable(&dir.join("brew"));
    let names = |line: &str, whole_lines: bool| {
        if whole_lines {
            return forms.iter().any(|f| line == f);
        }
        (homebrew && line.contains("brew shellenv"))
            || forms.iter().any(|f| line.match_indices(f.as_str()).any(|(i, _)| line[i + f.len()..].chars().next().map_or(true, |c| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.'))))
    };
    let mut hits: Vec<Origin> = vec![];
    let mut seen = std::collections::HashSet::new();
    for file in startup {
        scan(file, &[], &names, home, &mut seen, &mut hits);
    }
    let others = hits.len().saturating_sub(1);
    hits.pop().map(|o| Origin { others, ..o })
}

/// Collects the lines of `file` that `names` (and of the files it sources, in order) into `hits`.
fn scan(file: &Path, chain: &[(PathBuf, usize)], names: &dyn Fn(&str, bool) -> bool, home: Option<&Path>, seen: &mut std::collections::HashSet<PathBuf>, hits: &mut Vec<Origin>) {
    if chain.len() > 8 || !seen.insert(util::canon(file)) {
        return;
    }
    let Ok(text) = fs::read_to_string(file) else { return };
    // `/etc/paths` and `/etc/paths.d/*` list directories, one per line.
    let whole_lines = file.parent().is_some_and(|d| d.file_name().is_some_and(|n| n == "paths.d")) || file.file_name().is_some_and(|n| n == "paths");
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if names(line, whole_lines) {
            hits.push(Origin { file: file.to_path_buf(), line: i + 1, sourced_from: chain.to_vec(), others: 0 });
        }
        if let Some(sourced) = sourced_file(line, file, home) {
            let mut deeper = chain.to_vec();
            deeper.push((file.to_path_buf(), i + 1));
            scan(&sourced, &deeper, names, home, seen, hits);
        }
    }
}

/// The file a `source FILE` or `. FILE` line reads, when it names one plainly (a path, through
/// `$HOME`, `${HOME}` or `~`, relative to the sourcing file's directory otherwise).
fn sourced_file(line: &str, from: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let rest = line.strip_prefix("source ").or_else(|| line.strip_prefix(". "))?;
    let word = rest.split_whitespace().next()?.trim_matches(|c| c == '"' || c == '\'');
    let expanded = match (word.strip_prefix("$HOME/").or_else(|| word.strip_prefix("${HOME}/")).or_else(|| word.strip_prefix("~/")), home) {
        (Some(rel), Some(h)) => h.join(rel),
        (Some(_), None) => return None,
        (None, _) if word.contains('$') => return None,
        (None, _) => from.parent().unwrap_or(Path::new("/")).join(word),
    };
    expanded.is_file().then_some(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    // TEST0113: The environment script puts the directory first once
    #[test]
    #[cfg(not(windows))]
    fn test0113_the_environment_script_puts_the_directory_first_once() {
        let (sh, fish) = env_scripts(Path::new("/opt/h/bin"));
        let run = |path: &str| -> String {
            let out = std::process::Command::new("/bin/sh").arg("-c").arg(format!("{}\nprintf %s \"$PATH\"", sh)).env("PATH", path).output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        assert_eq!(run("/usr/bin:/bin"), "/opt/h/bin:/usr/bin:/bin");
        assert_eq!(run("/opt/h/bin:/usr/bin"), "/opt/h/bin:/usr/bin", "already first: unchanged");
        assert_eq!(run("/root/.cargo/bin:/opt/h/bin"), "/opt/h/bin:/root/.cargo/bin:/opt/h/bin", "later on PATH is not enough");
        assert!(fish.contains("set -gx PATH \"/opt/h/bin\" $PATH"));
    }

    // TEST0114: Uninstalling restores a startup file exactly
    #[test]
    fn test0114_uninstalling_restores_a_startup_file_exactly() {
        let line = ". \"$HOME/.qrust/env\"";
        for original in ["export A=1\n", "", "export A=1\n\nexport B=2\n"] {
            let sep = if original.is_empty() || original.ends_with('\n') { "" } else { "\n" };
            let edited = format!("{}{}\n{}\n{}\n", original, sep, MARK, line);
            assert_eq!(without_setup_lines(&edited, line).as_deref(), Some(original));
        }
        assert_eq!(without_setup_lines("export A=1\n", line), None);
        // A source line someone wrote themselves stays.
        assert_eq!(without_setup_lines(&format!("{}\n", line), line), None);
    }

    /// A stand-in QuickRust: `qrust` with `contents`, and the rustc wrapper beside it.
    fn release(dir: &Path, contents: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let qrust = util::exe_candidates(dir, "qrust").remove(0);
        fs::write(&qrust, contents).unwrap();
        fs::write(util::exe_candidates(dir, "qrust-rustc-wrapper").remove(0), "wrapper").unwrap();
        #[cfg(unix)]
        for p in [&qrust, &util::exe_candidates(dir, "qrust-rustc-wrapper").remove(0)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        qrust
    }

    // TEST0302: where proxies are copies, a machine set up by one QuickRust updates to the next,
    // and a stranger's program in `bin` is still refused
    //
    // On Windows the proxies are copies of `qrust`, and only a copy of the binary installing now
    // was taken for QuickRust's: the next release refused every proxy the previous one left, so
    // neither `qrust self update` nor `qrust setup` could update a machine. Run with copies on every
    // platform, so it is held here and not only on Windows.
    #[test]
    fn test0302_where_proxies_are_copies_one_release_updates_to_the_next() {
        let dir = std::env::temp_dir().join(format!("qrust-setup-{}", util::new_id()));
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let cargo = util::exe_candidates(&bin, "cargo").remove(0);

        // The first release sets the machine up.
        let first = install_from(&bin, &release(&dir.join("first"), "QuickRust 1")).unwrap();
        link_proxies(&bin, &first.qrust, &known_binaries(None, &first), Proxies::Copies).unwrap();
        let mut record = json!({ "format": 1 });
        remember_binary(&mut record, &first.qrust).unwrap();
        assert_eq!(fs::read_to_string(&cargo).unwrap(), "QuickRust 1");

        // The next one replaces it and takes the first one's proxies for its own: by the binary it
        // replaced, and by the record.
        let second = install_from(&bin, &release(&dir.join("second"), "QuickRust 2")).unwrap();
        assert!(second.replaced.is_some(), "the install did not keep what it replaced");
        link_proxies(&bin, &second.qrust, &known_binaries(Some(&record), &second), Proxies::Copies)
            .expect("a copy of the previous QuickRust is QuickRust's own");
        assert_eq!(fs::read_to_string(&cargo).unwrap(), "QuickRust 2");

        // A machine whose record names no binaries still updates, by the one replaced.
        let third = install_from(&bin, &release(&dir.join("third"), "QuickRust 3")).unwrap();
        link_proxies(&bin, &third.qrust, &known_binaries(Some(&json!({ "format": 1 })), &third), Proxies::Copies)
            .expect("an install whose record predates `binaries` still recognises its proxies");

        // A machine an earlier QuickRust left half-updated: `qrust` already replaced, the one before
        // it moved aside (as Windows installs do), the proxies still copies of that one, and a record
        // that names no binaries. The update finishes.
        let half = dir.join("half");
        fs::create_dir_all(&half).unwrap();
        let old = release(&dir.join("old"), "QuickRust old");
        let half_qrust = util::exe_candidates(&half, "qrust").remove(0);
        let old_name = half_qrust.file_name().map(util::lossy).unwrap();
        fs::copy(&old, half.join(format!(".{old_name}.old.4242"))).unwrap();
        fs::write(&half_qrust, "QuickRust newer").unwrap();
        let half_cargo = util::exe_candidates(&half, "cargo").remove(0);
        fs::copy(&old, &half_cargo).unwrap();
        let next = install_from(&half, &release(&dir.join("next"), "QuickRust next")).unwrap();
        link_proxies(&half, &next.qrust, &known_binaries(Some(&json!({ "format": 1 })), &next), Proxies::Copies)
            .expect("the proxies of the binary moved aside are QuickRust's");
        assert_eq!(fs::read_to_string(&half_cargo).unwrap(), "QuickRust next");

        // Something that is no QuickRust is never overwritten. A new file, not a write through the
        // proxy: a proxy may be a hard link to `qrust` itself.
        fs::remove_file(&cargo).unwrap();
        fs::write(&cargo, "somebody else's cargo").unwrap();
        let refused = link_proxies(&bin, &third.qrust, &known_binaries(Some(&record), &third), Proxies::Copies).unwrap_err();
        assert!(refused.contains("is not QuickRust"), "{refused}");
        assert_eq!(fs::read_to_string(&cargo).unwrap(), "somebody else's cargo");
        let _ = fs::remove_dir_all(&dir);
    }
    /// A package's QuickRust as Homebrew lays it out: `qrust` and its wrapper in the versioned
    /// `Cellar/quickrust/<version>/libexec` of `dir`, reached by `opt/quickrust` (which the upgrade
    /// moves), with the marker. Returns the `qrust` by its stable path.
    #[cfg(unix)]
    fn packaged(dir: &Path, version: &str) -> PathBuf {
        let libexec = dir.join("Cellar/quickrust").join(version).join("libexec");
        release(&libexec, &format!("QuickRust {version}"));
        let opt = dir.join("opt/quickrust");
        fs::create_dir_all(opt.parent().unwrap()).unwrap();
        let _ = fs::remove_file(&opt);
        std::os::unix::fs::symlink(libexec.parent().unwrap(), &opt).unwrap();
        let program = opt.join("libexec/qrust");
        let marker = json!({ "format": 1, "manager": "Homebrew", "program": util::path_str(&program), "update": "brew upgrade quickrust", "uninstall": "brew uninstall quickrust" });
        fs::write(libexec.join(crate::package::MARKER), marker.to_string()).unwrap();
        program
    }

    // TEST0307: Setup from a package links to it by its stable path and replaces a copy
    #[cfg(unix)]
    #[test]
    fn test0307_setup_from_a_package_links_to_it_by_its_stable_path_and_replaces_a_copy() {
        let dir = std::env::temp_dir().join(format!("qrust-setup-{}", util::new_id()));
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        // A machine an archive set up first: QuickRust's own copy in `bin`, proxies linking to it.
        let copy = install_from(&bin, &release(&dir.join("archive"), "QuickRust archive")).unwrap();
        link_proxies(&bin, &copy.qrust, &[], Proxies::Links).unwrap();
        assert!(fs::symlink_metadata(bin.join("qrust")).unwrap().is_file());
        // Then a package's setup: the copy goes, a link to the package by its stable path stays.
        let program = packaged(&dir, "1.0");
        let package = crate::package::of(&program).unwrap().expect("the package's marker");
        let (linked, replaced) = link_package(&bin, &package).unwrap();
        link_proxies(&bin, &linked.qrust, &[], Proxies::Links).unwrap();
        assert_eq!(replaced.as_deref(), Some(bin.join("qrust").as_path()), "a copy was replaced without saying so");
        assert_eq!(fs::read_link(bin.join("qrust")).unwrap(), program, "not linked by the stable path");
        assert_eq!(fs::read_link(bin.join("qrust-rustc-wrapper")).unwrap(), dir.join("opt/quickrust/libexec/qrust-rustc-wrapper"));
        // `cargo` and every proxy are the package's program; nothing in `bin` is a copy.
        for name in std::iter::once("cargo").chain(engine::PROXIES.iter().copied()) {
            assert_eq!(fs::read_to_string(bin.join(name)).unwrap(), "QuickRust 1.0", "{name}");
        }
        assert!(fs::read_dir(&bin).unwrap().flatten().all(|e| e.file_type().unwrap().is_symlink()), "a copy is left in bin");
        // The package manager's upgrade moves `opt`: what runs is the new version, untouched by setup.
        packaged(&dir, "2.0");
        assert_eq!(fs::read_to_string(bin.join("cargo")).unwrap(), "QuickRust 2.0");
        // Setup again from the package changes nothing, and replaces no copy.
        let package = crate::package::of(&bin.join("qrust")).unwrap().unwrap();
        assert_eq!(link_package(&bin, &package).unwrap().1, None);
        // A package without its rustc wrapper, or something else at `qrust`, is refused.
        fs::remove_file(bin.join("qrust")).unwrap();
        fs::create_dir(bin.join("qrust")).unwrap();
        assert!(link_package(&bin, &package).unwrap_err().contains("is not QuickRust"));
        fs::remove_dir(bin.join("qrust")).unwrap();
        fs::remove_file(dir.join("Cellar/quickrust/2.0/libexec/qrust-rustc-wrapper")).unwrap();
        assert!(link_package(&bin, &package).unwrap_err().contains("qrust-rustc-wrapper is not at"));
        let _ = fs::remove_dir_all(&dir);
    }

    // TEST0308: A package's QuickRust is updated and removed by its manager, never by itself
    #[cfg(unix)]
    #[test]
    fn test0308_a_packages_quickrust_is_updated_and_removed_by_its_manager_never_by_itself() {
        let dir = std::env::temp_dir().join(format!("qrust-setup-{}", util::new_id()));
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let standalone = release(&dir.join("archive"), "QuickRust archive");
        // A QuickRust that installed itself does both.
        let copy = install_from(&bin, &standalone).unwrap();
        assert!(refuse_for_package_in(&bin, &copy.qrust, OwnAction::Update).is_ok());
        assert!(refuse_for_package_in(&bin, &copy.qrust, OwnAction::Uninstall).is_ok());
        // Once `bin` links to a package, whichever QuickRust runs: the package's manager.
        let program = packaged(&dir, "1.0");
        link_package(&bin, &crate::package::of(&program).unwrap().unwrap()).unwrap();
        for me in [&program, &bin.join("qrust"), &standalone] {
            let update = refuse_for_package_in(&bin, me, OwnAction::Update).unwrap_err();
            assert!(update.contains("installed with Homebrew") && update.contains("`brew upgrade quickrust`"), "{update}");
            let uninstall = refuse_for_package_in(&bin, me, OwnAction::Uninstall).unwrap_err();
            assert!(uninstall.contains("`brew uninstall quickrust`") && uninstall.contains(MARK), "{uninstall}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    // TEST0309: A copy does not set up over a package install but over one that was removed
    #[cfg(unix)]
    #[test]
    fn test0309_a_copy_does_not_set_up_over_a_package_install_but_over_one_that_was_removed() {
        let dir = std::env::temp_dir().join(format!("qrust-setup-{}", util::new_id()));
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        assert!(refuse_over_package(&bin).is_ok(), "nothing set up");
        let program = packaged(&dir, "1.0");
        link_package(&bin, &crate::package::of(&program).unwrap().unwrap()).unwrap();
        let refused = refuse_over_package(&bin).unwrap_err();
        assert!(refused.contains("Homebrew") && refused.contains("`brew uninstall quickrust`"), "{refused}");
        // `brew uninstall` took the package; the link it left leads nowhere, and a copy may go there.
        fs::remove_dir_all(dir.join("Cellar")).unwrap();
        assert!(refuse_over_package(&bin).is_ok());
        let copy = install_from(&bin, &release(&dir.join("archive"), "QuickRust archive")).unwrap();
        assert!(fs::symlink_metadata(&copy.qrust).unwrap().is_file());
        assert_eq!(fs::read_to_string(&copy.qrust).unwrap(), "QuickRust archive");
        let _ = fs::remove_dir_all(&dir);
    }

    // TEST0310: Doctor names what comes before QuickRust on PATH and the startup line that put it there
    #[cfg(unix)]
    #[test]
    fn test0310_doctor_names_what_comes_before_quickrust_on_path_and_the_line_that_put_it_there() {
        let dir = std::env::temp_dir().join(format!("qrust-setup-{}", util::new_id()));
        let (home, etc) = (dir.join("home"), dir.join("etc"));
        let bin = home.join(".qrust/bin");
        fs::create_dir_all(&bin).unwrap();
        let program = packaged(&dir.join("homebrew"), "1.0");
        // Homebrew's bin: its `brew`, its link to the package's `qrust`, and Homebrew's own Rust `cargo`.
        let brew_bin = dir.join("homebrew/bin");
        fs::create_dir_all(&brew_bin).unwrap();
        release(&dir.join("homebrew/rust"), "cargo from Homebrew's rust");
        fs::rename(dir.join("homebrew/rust/qrust"), brew_bin.join("cargo")).unwrap();
        fs::write(brew_bin.join("brew"), "brew").unwrap();
        fs::set_permissions(brew_bin.join("brew"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&program, brew_bin.join("qrust")).unwrap();
        // ~/.zprofile runs `brew shellenv` and puts QuickRust's bin first; then ~/.zshrc sources a
        // toolset's profile, which runs `brew shellenv` again. A stray directory with another
        // `qrust` comes from ~/.zprofile.
        let stray = home.join("tools/bin");
        release(&stray, "some other qrust");
        fs::create_dir_all(home.join("toolset")).unwrap();
        fs::write(home.join(".zprofile"), "eval \"$(brew shellenv)\"\nexport PATH=\"$HOME/tools/bin:$PATH\"\n. \"$HOME/.qrust/env\"\n").unwrap();
        fs::write(home.join(".zshrc"), "# toolset\nsource ~/toolset/profile.zsh\n").unwrap();
        fs::write(home.join("toolset/profile.zsh"), format!("export A=1\neval \"$({}/brew shellenv)\"\n", brew_bin.display())).unwrap();
        let startup = startup_files("/bin/zsh", &home, &etc);
        let path = std::env::join_paths([&stray, &brew_bin, &bin]).unwrap();
        let qrust = bin.join("qrust");
        // `bin` still holds an archive's copy: Homebrew's `qrust` is another QuickRust, and says so.
        let copy = install_from(&bin, &release(&dir.join("archive"), "QuickRust archive")).unwrap();
        link_proxies(&bin, &copy.qrust, &[], Proxies::Links).unwrap();
        let on = on_path(&path, &qrust, &startup, Some(&home));
        let shadow = |p: &Path| on.shadows.iter().find(|s| s.program == p).cloned().unwrap_or_else(|| panic!("{} not named: {:?}", p.display(), on.shadows));
        assert_eq!(on.shadows.len(), 3, "{:?}", on.shadows);
        let stray_qrust = shadow(&stray.join("qrust"));
        let o = stray_qrust.origin.expect("the line that puts it there");
        assert_eq!((o.file, o.line, o.sourced_from.len(), o.others), (home.join(".zprofile"), 2, 0, 0));
        let brew_qrust = shadow(&brew_bin.join("qrust"));
        assert_eq!(brew_qrust.package.as_deref(), Some("Homebrew"));
        // The line that put it ahead is the last of the two that name it.
        let o = brew_qrust.origin.expect("brew shellenv puts it there");
        assert_eq!((o.file, o.line, o.sourced_from, o.others), (home.join("toolset/profile.zsh"), 2, vec![(home.join(".zshrc"), 2)], 1));
        assert!(shadow(&brew_bin.join("cargo")).package.is_none());
        assert!(on.cargo, "QuickRust's cargo is on PATH, after the others");
        // Once setup links `bin` to the package, Homebrew's `qrust` is QuickRust's: only the stray
        // `qrust` and Homebrew's Rust `cargo` are left.
        link_package(&bin, &crate::package::of(&program).unwrap().unwrap()).unwrap();
        let on = on_path(&path, &qrust, &startup, Some(&home));
        let named: Vec<&Path> = on.shadows.iter().map(|s| s.program.as_path()).collect();
        assert_eq!(named, [stray.join("qrust").as_path(), brew_bin.join("cargo").as_path()]);
        // Without QuickRust's bin on PATH there is no QuickRust `cargo`, and a directory no startup
        // file names has no origin.
        let elsewhere = dir.join("elsewhere");
        release(&elsewhere, "yet another");
        let on = on_path(&std::env::join_paths([&elsewhere]).unwrap(), &qrust, &startup, Some(&home));
        assert!(!on.cargo && on.shadows.len() == 1 && on.shadows[0].origin.is_none(), "{:?}", on);
        let _ = fs::remove_dir_all(&dir);
    }

    // TEST0311: Every package's marker is one QuickRust reads, naming its manager's commands
    #[cfg(unix)]
    #[test]
    fn test0311_every_packages_marker_is_one_quickrust_reads_naming_its_managers_commands() {
        let packaging = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging");
        // The marker each package writes, between the lines that open and close its here-document.
        let marker = |file: &str| -> String {
            let text = fs::read_to_string(packaging.join(file)).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            let open = lines.iter().position(|l| l.contains("<<") && l.contains("QRUST_INSTALLED_BY")).unwrap_or_else(|| panic!("{file} writes no marker"));
            let close = lines[open + 1..].iter().position(|l| l.trim() == "QRUST_INSTALLED_BY").unwrap_or_else(|| panic!("{file}'s marker is not closed")) + open + 1;
            lines[open + 1..close].iter().map(|l| l.trim()).collect::<Vec<_>>().join("\n")
        };
        for (file, manager, prefix_var, update, uninstall) in [
            ("quickrust.rb.in", "Homebrew", "#{opt_libexec}", "brew upgrade quickrust", "brew uninstall quickrust"),
            ("quickrust.spec.in", "dnf", "/opt/quickrust", "sudo dnf upgrade quickrust", "sudo dnf remove quickrust"),
            ("deb/postinst", "apt", "@{prefix}", "sudo apt install --only-upgrade quickrust", "sudo apt remove quickrust"),
        ] {
            let dir = std::env::temp_dir().join(format!("qrust-marker-{}", util::new_id()));
            let qrust = release(&dir, "QuickRust");
            let text = marker(file).replace(prefix_var, &util::path_str(&dir));
            fs::write(dir.join(crate::package::MARKER), text).unwrap();
            let p = crate::package::of(&qrust).unwrap_or_else(|e| panic!("{file}: {e}")).unwrap_or_else(|| panic!("{file}: no package"));
            assert_eq!((p.manager.as_str(), p.update.as_str(), p.uninstall.as_str(), p.program.as_path()), (manager, update, uninstall, qrust.as_path()), "{file}");
            let _ = fs::remove_dir_all(&dir);
        }
        // What the formula substitutes is where it installs the programs.
        let formula = fs::read_to_string(packaging.join("quickrust.rb.in")).unwrap();
        assert!(formula.contains("libexec.install Dir[\"*\"]") && formula.contains("(libexec/\"qrust-installed-by.json\").write"));
        // The rpm owns its marker; the deb removes the one its postinst wrote.
        assert!(fs::read_to_string(packaging.join("quickrust.spec.in")).unwrap().contains("\n/opt/quickrust/qrust-installed-by.json\n"));
        assert!(fs::read_to_string(packaging.join("deb/postrm")).unwrap().contains("rm -f @{prefix}/qrust-installed-by.json"));
    }
}
