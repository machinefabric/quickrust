//! Installing QuickRust on a machine (`qrust setup`) and removing it (`qrust self uninstall`).
//!
//! Setup puts QuickRust into `~/.qrust/bin` (the binary, its rustc wrapper, and the proxies: QuickRust as
//! `cargo`, `rustc`, `rustfmt`, …), puts that directory first on `PATH` in the shell's startup
//! files, imports what is the user's own from Cargo's home (`~/.cargo`, or `CARGO_HOME`) once —
//! registry credentials and the `[registries] [registry] [source] [net] [http]` tables of its
//! configuration — and installs the default toolchain. Running it again repairs what is missing
//! and never imports again. `setup.json` in QuickRust's home records what it did, for `doctor` and
//! for uninstalling.

use crate::engine;
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
}

/// An import from Cargo's home: from where, what was imported, what was not (and why).
pub type Imported = (PathBuf, Vec<String>, Vec<String>);

pub fn setup(opts: &Options) -> Result<Done, String> {
    let bin = engine::bin_dir();
    util::create_private_dir(&bin).map_err(|e| format!("cannot create {}: {}", bin.display(), e))?;
    let qrust = install_binaries(&bin)?;
    let proxies = link_proxies(&bin, &qrust)?;
    let path_edits = if opts.modify_path { put_on_path(&bin)? } else { vec![] };
    let previous = util::read_json(&record_path());
    let import = match previous.as_ref().and_then(|r| r.get("imported_from")).filter(|v| !v.is_null()) {
        Some(_) => None,
        None => import_cargo_home()?,
    };
    let mut record = previous.unwrap_or_else(|| json!({ "format": 1 }));
    record["version"] = json!(env!("CARGO_PKG_VERSION"));
    record["bin"] = json!(util::path_str(&bin));
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
    Ok(Done { version: env!("CARGO_PKG_VERSION").to_string(), bin, proxies, path_edits, import })
}

/// Copies the running QuickRust and its rustc wrapper into `bin` (unless they run from there);
/// returns `bin/qrust`.
fn install_binaries(bin: &Path) -> Result<PathBuf, String> {
    let (me, _) = crate::resolve::self_identity();
    install_from(bin, &me)
}

/// Copies the QuickRust binary `qrust` and the rustc wrapper next to it into `bin` (unless they are
/// there already); returns `bin/qrust`.
fn install_from(bin: &Path, me: &Path) -> Result<PathBuf, String> {
    let me = me.to_path_buf();
    let wrapper = util::exe_candidates(me.parent().unwrap_or(Path::new(".")), "qrust-rustc-wrapper").into_iter().find(|p| util::is_executable(p)).ok_or_else(|| format!("qrust-rustc-wrapper is not next to {}; QuickRust is installed with it", me.display()))?;
    let target = util::exe_candidates(bin, "qrust").remove(0);
    for (from, to) in [(&me, target.clone()), (&wrapper, util::exe_candidates(bin, "qrust-rustc-wrapper").remove(0))] {
        if util::canon(from) == util::canon(&to) {
            continue;
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
    Ok(target)
}

/// Makes `cargo` and every proxy in `bin` QuickRust (`bin/qrust`). An entry there that is not QuickRust's
/// is an error, never overwritten.
fn link_proxies(bin: &Path, qrust: &Path) -> Result<usize, String> {
    let names: Vec<&str> = std::iter::once("cargo").chain(engine::PROXIES.iter().copied()).collect();
    for name in &names {
        let link = util::exe_candidates(bin, name).remove(0);
        if let Ok(md) = fs::symlink_metadata(&link) {
            if !is_qrust(&link, &md, qrust) {
                return Err(format!("{} exists and is not QuickRust; move it away and run `qrust setup` again", link.display()));
            }
        }
        let staged = bin.join(format!(".{}.{}", name, std::process::id()));
        let _ = fs::remove_file(&staged);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(qrust.file_name().unwrap_or_default(), &staged);
        #[cfg(not(unix))]
        let made = fs::hard_link(qrust, &staged).or_else(|_| fs::copy(qrust, &staged).map(|_| ()));
        made.and_then(|_| fs::rename(&staged, &link)).map_err(|e| {
            let _ = fs::remove_file(&staged);
            format!("cannot create {}: {}", link.display(), e)
        })?;
    }
    Ok(names.len())
}

/// Whether the entry at `link` is QuickRust: a link to a QuickRust binary (QuickRust's `bin` holds only QuickRust's
/// links), or on Windows a copy of `qrust`.
fn is_qrust(link: &Path, md: &fs::Metadata, qrust: &Path) -> bool {
    if md.file_type().is_symlink() {
        return true;
    }
    cfg!(windows) && md.len() == fs::metadata(qrust).map(|m| m.len()).unwrap_or(u64::MAX) && util::blake3_file(link).ok() == util::blake3_file(qrust).ok()
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
/// it was published in (or `mirror`, a copy of that registry: `self.update-url`).
///
/// The registry's manifest is trusted only through its signature (`crate::release`), the archive
/// only as the file the manifest describes, and the binary in it only once it runs and says it is
/// the version it was published as. Then it replaces the running one in QuickRust's `bin`.
pub fn self_update(mirror: &str) -> Result<Updated, String> {
    let identity = crate::release::identity().ok_or(
        "this QuickRust was built from source, so it has no release line to update from: build the newer source, or install a release",
    )?;
    let registry = if mirror.is_empty() { identity.registry.as_str() } else { mirror };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "the clock is before 1970, so no certificate's validity can be checked".to_string())?.as_secs();
    self_update_from(registry, &identity.channel, &identity.trust, now)
}

/// [`self_update`] with everything a build bakes in passed explicitly.
pub fn self_update_from(registry: &str, channel: &str, trust: &crate::release::Trust, now: u64) -> Result<Updated, String> {
    let bin = engine::bin_dir();
    let installed = util::exe_candidates(&bin, "qrust").remove(0);
    if !installed.is_file() {
        return Err(format!("QuickRust is not set up ({} does not exist): `qrust setup` installs it, then `qrust self update` keeps it current", installed.display()));
    }
    let platform = crate::release::platform().ok_or("QuickRust publishes no release for this platform; build it from source")?;
    let client = crate::net::Client::new();
    let current = env!("CARGO_PKG_VERSION").to_string();
    let Some(release) = crate::release::fetch_latest(&client, registry, trust, channel, platform, now)? else {
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
        let qrust = install_from(&bin, &new)?;
        link_proxies(&bin, &qrust)?;
        let mut record = util::read_json(&record_path()).unwrap_or_else(|| json!({ "format": 1 }));
        record["version"] = json!(release.version);
        util::write_json(&record_path(), &record).map_err(|e| e.to_string())?;
        Ok(Updated { from: current.clone(), to: release.version.clone() })
    })();
    let _ = fs::remove_dir_all(&work);
    out
}

/// What `doctor` reports: whether QuickRust is set up, and whether its `bin` comes first on `PATH`.
pub fn status() -> (Option<Value>, bool) {
    let record = util::read_json(&record_path());
    let first = util::which("cargo").map(|c| c.parent() == Some(engine::bin_dir().as_path())).unwrap_or(false);
    (record, first)
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
}
