//! Haul's toolchains: Rust releases Haul downloads, installs and keeps under
//! `$HAUL_HOME/toolchains`, so that nothing besides Haul needs installing.
//!
//! Which toolchain a command uses, first match wins:
//! 1. `+<toolchain>` as the first argument;
//! 2. `RUSTUP_TOOLCHAIN` in the environment (the name tools and editors set);
//! 3. the toolchain file (`rust-toolchain.toml` / `rust-toolchain`) of the directory or the
//!    nearest one above it;
//! 4. the default (`haul toolchain default`; `stable` until set).
//!
//! A toolchain, and the components and targets a toolchain file or the command asks for, are
//! installed on first use. A channel (`stable`, `1.98`, `nightly`) names the release it was
//! installed as until it is updated; releases and dated nightlies never change.

pub mod dist;
pub mod file;
pub mod install;
pub mod spec;
pub mod state;

use crate::{net, util};
use file::{Profile, ToolchainFile};
use install::Report;
use spec::{Channel, Spec};
use std::path::{Path, PathBuf};

/// Haul's toolchain directories under its home.
#[derive(Clone, Debug)]
pub struct Layout {
    home: PathBuf,
}

impl Layout {
    pub fn new(home: PathBuf) -> Layout {
        Layout { home }
    }

    pub fn current() -> Layout {
        Layout::new(util::haul_home())
    }

    pub fn toolchains(&self) -> PathBuf {
        self.home.join("toolchains")
    }

    /// Archives being downloaded (disposable).
    pub fn downloads(&self) -> PathBuf {
        self.home.join("cache").join("downloads")
    }
}

/// An installed (or linked) toolchain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    /// `1.99.0-x86_64-unknown-linux-gnu`, `nightly-2026-10-11-<host>`, a linked name, or a path.
    pub id: String,
    pub dir: PathBuf,
    /// Linked or given by path: Haul uses it as it is and never changes it.
    pub linked: bool,
}

impl Toolchain {
    pub fn bin(&self, tool: &str) -> PathBuf {
        let name = if cfg!(windows) { format!("{}.exe", tool) } else { tool.to_string() };
        self.dir.join("bin").join(name)
    }

    pub fn rustc(&self) -> PathBuf {
        self.bin("rustc")
    }

    pub fn cargo(&self) -> PathBuf {
        self.bin("cargo")
    }

    /// Haul's record of it (`None` for a linked toolchain).
    pub fn record(&self) -> Option<install::Record> {
        if self.linked {
            return None;
        }
        install::read_record(&self.dir).ok()
    }
}

/// Why a toolchain was chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Plus,
    Env,
    File(PathBuf),
    Default,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Plus => write!(f, "`+toolchain` argument"),
            Source::Env => write!(f, "RUSTUP_TOOLCHAIN"),
            Source::File(p) => write!(f, "{}", p.display()),
            Source::Default => write!(f, "default toolchain"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub spec: Spec,
    pub source: Source,
    /// The toolchain file, when it chose the toolchain: its components, targets and profile apply.
    pub file: Option<ToolchainFile>,
}

/// The toolchain for a command run in `cwd` with `+<name>` = `plus` (see the module notes).
pub fn select(layout: &Layout, cwd: &Path, plus: Option<&str>) -> Result<Selection, String> {
    if let Some(p) = plus {
        return Ok(Selection { spec: Spec::parse(p)?, source: Source::Plus, file: None });
    }
    if let Some(v) = std::env::var_os("RUSTUP_TOOLCHAIN").filter(|v| !v.is_empty()) {
        let v = v.into_string().map_err(|v| format!("RUSTUP_TOOLCHAIN is not UTF-8: {:?}", v))?;
        return Ok(Selection { spec: Spec::parse(&v).map_err(|e| format!("RUSTUP_TOOLCHAIN: {}", e))?, source: Source::Env, file: None });
    }
    if let Some(f) = file::find(cwd)? {
        return Ok(Selection { spec: f.spec.clone(), source: Source::File(f.path.clone()), file: Some(f) });
    }
    let default = state::load(&layout.toolchains())?.default;
    Ok(Selection { spec: Spec::parse(&default).map_err(|e| format!("the default toolchain: {}", e))?, source: Source::Default, file: None })
}

/// The host toolchains are installed for: this machine's architecture and system (on Linux, the
/// C library the system has, whatever Haul itself was built against).
pub fn host() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "linux" => {
            let glibc = ["/lib64/ld-linux-x86-64.so.2", "/lib/ld-linux-aarch64.so.1", "/lib/ld-linux-x86-64.so.2", "/lib64/ld-linux-aarch64.so.1", "/lib/ld-linux-armhf.so.3", "/lib/ld-linux-riscv64-lp64d.so.1", "/lib64/ld64.so.2"].iter().any(|p| Path::new(p).exists());
            format!("{}-unknown-linux-{}", arch, if glibc { "gnu" } else { "musl" })
        }
        "macos" => format!("{}-apple-darwin", arch),
        "windows" => format!("{}-pc-windows-msvc", arch),
        "freebsd" => format!("{}-unknown-freebsd", arch),
        os => format!("{}-unknown-{}", arch, os),
    }
}

/// What a command needs beyond the toolchain file: components (`rustc-dev`), targets
/// (`--target wasm32-unknown-unknown`), and the profile a new install gets (over the file's).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    pub components: Vec<String>,
    pub targets: Vec<String>,
    pub profile: Option<Profile>,
}

/// The toolchain `sel` names, installed with what it and `needs` ask for, from the dist server
/// `server` (`toolchain.dist-server`).
pub fn ensure(layout: &Layout, sel: &Selection, needs: &Needs, server: &str, report: &mut dyn Report) -> Result<Toolchain, String> {
    let mut components: Vec<String> = sel.file.iter().flat_map(|f| f.components.iter().cloned()).collect();
    let mut targets: Vec<String> = sel.file.iter().flat_map(|f| f.targets.iter().cloned()).collect();
    components.extend(needs.components.iter().cloned());
    targets.extend(needs.targets.iter().cloned());
    let profile = needs.profile.or(sel.file.as_ref().and_then(|f| f.profile)).unwrap_or(Profile::Default);
    match &sel.spec {
        Spec::Path(dir) => linked(dir.display().to_string(), dir.clone(), &components, &targets),
        Spec::Named(name) => {
            let st = state::load(&layout.toolchains())?;
            let dir = st.linked.get(name).ok_or_else(|| format!("no toolchain is named `{}` (`haul toolchain link {} <dir>` names one)", name, name))?;
            linked(name.clone(), dir.clone(), &components, &targets)
        }
        Spec::Dist { channel, date, host: for_host } => {
            let host = for_host.clone().unwrap_or_else(host);
            let client = net::Client::new();
            let key = channel_key(channel, date.as_deref(), &host);
            let known = match (channel, date) {
                (Channel::Version(v), None) if v.split('.').count() == 3 => Some(format!("{}-{}", v, host)),
                (Channel::Nightly | Channel::Beta, Some(d)) => Some(format!("{}-{}-{}", channel.dist_name(), d, host)),
                _ => state::load(&layout.toolchains())?.channels.get(&key).map(|c| c.toolchain.clone()),
            };
            if let Some(id) = known.filter(|id| layout.toolchains().join(id).join(install::RECORD).exists()) {
                return complete(layout, &id, &host, &components, &targets, server, &client, report);
            }
            let (manifest, sha) = resolve(&client, server, channel, date.as_deref(), &host, profile, &components, &targets)?;
            let id = match channel {
                Channel::Stable | Channel::Version(_) => format!("{}-{}", manifest.version(), host),
                Channel::Beta | Channel::Nightly => format!("{}-{}-{}", channel.dist_name(), manifest.date(), host),
            };
            let dir = layout.toolchains().join(&id);
            {
                let locked = install::lock(layout, &id, report)?;
                if !dir.join(install::RECORD).exists() {
                    let plan = manifest.plan(&host, profile, &components, &targets).map_err(|e| format!("Rust {}: {}", manifest.version(), e))?;
                    let origin = channel_tracked(channel, date.as_deref()).then_some(key.as_str());
                    install::install(layout, &locked, &install::Release { id: &id, host: &host, profile, manifest: &manifest, origin }, &plan, server, &client, report)?;
                }
            }
            if channel_tracked(channel, date.as_deref()) {
                state::update(&layout.toolchains(), |s| {
                    s.channels.insert(key, state::ChannelState { toolchain: id.clone(), manifest_sha256: sha, checked_ns: util::now_wall_ns() });
                    Ok(())
                })?;
            }
            complete(layout, &id, &host, &components, &targets, server, &client, report)
        }
    }
}

/// `channels.json`'s key for a channel: `stable-<host>`, `1.98-<host>`, `stable-2026-10-01-<host>`.
fn channel_key(channel: &Channel, date: Option<&str>, host: &str) -> String {
    match date {
        Some(d) => format!("{}-{}-{}", channel.dist_name(), d, host),
        None => format!("{}-{}", channel.dist_name(), host),
    }
}

/// Whether the release a channel names is remembered: everything but full releases and dated
/// nightlies/betas, whose names are their ids.
fn channel_tracked(channel: &Channel, date: Option<&str>) -> bool {
    match (channel, date) {
        (Channel::Version(v), _) => v.split('.').count() == 2,
        (Channel::Nightly | Channel::Beta, Some(_)) => false,
        _ => true,
    }
}

/// The manifest of the release `channel` names. An undated nightly is the latest one, within a
/// week, that has everything asked for.
#[allow(clippy::too_many_arguments)]
fn resolve(client: &net::Client, server: &str, channel: &Channel, date: Option<&str>, host: &str, profile: Profile, components: &[String], targets: &[String]) -> Result<(dist::Manifest, String), String> {
    let name = channel.dist_name();
    let url = dist::manifest_url(server, name, date);
    let (manifest, sha) = dist::fetch(client, &url)?.ok_or_else(|| match date {
        Some(d) => format!("there is no Rust {} release of {}", name, d),
        None => format!("there is no Rust release `{}`", name),
    })?;
    if !(matches!(channel, Channel::Nightly) && date.is_none()) {
        return Ok((manifest, sha));
    }
    let mut day = manifest.date().to_string();
    let mut candidate = (manifest, sha);
    for _ in 0..7 {
        match candidate.0.plan(host, profile, components, targets) {
            Ok(_) => return Ok(candidate),
            Err(dist::Missing::Unavailable { .. }) => {}
            Err(e) => return Err(format!("Rust nightly: {}", e)),
        }
        day = previous_day(&day).ok_or_else(|| format!("the nightly manifest has an unreadable date {:?}", day))?;
        if let Some(found) = dist::fetch(client, &dist::manifest_url(server, "nightly", Some(&day)))? {
            candidate = found;
        }
    }
    let missing = candidate.0.plan(host, profile, components, targets).err().map(|e| e.to_string()).unwrap_or_default();
    Err(format!("no nightly of the last week has everything asked for ({})", missing))
}

/// `YYYY-MM-DD` of the day before.
fn previous_day(date: &str) -> Option<String> {
    let mut it = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    // Days from civil (Howard Hinnant's algorithm), minus one, and back.
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468 - 1;
    let wall = (days as u64).checked_mul(86_400_000_000_000)?;
    Some(util::iso8601(wall)[..10].to_string())
}

/// A linked toolchain or toolchain directory, which must already have what is asked for.
fn linked(id: String, dir: PathBuf, components: &[String], targets: &[String]) -> Result<Toolchain, String> {
    let tc = Toolchain { id, dir, linked: true };
    if !tc.rustc().is_file() {
        return Err(format!("{} is not a toolchain: it has no {}", tc.dir.display(), tc.rustc().display()));
    }
    for t in targets {
        if !tc.dir.join("lib/rustlib").join(t).join("lib").is_dir() {
            return Err(format!("toolchain {} has no standard library for {}, and Haul does not change a linked toolchain", tc.id, t));
        }
    }
    if let Some(c) = components.first() {
        let listed = std::fs::read_to_string(tc.dir.join("lib/rustlib/components")).unwrap_or_default();
        if !components.iter().all(|c| listed.lines().any(|l| l == c || l == format!("{}-preview", c))) {
            return Err(format!("toolchain {} may lack component {}, and Haul does not change a linked toolchain", tc.id, c));
        }
    }
    Ok(tc)
}

/// The installed toolchain `id`, with the components and targets asked for added from its own
/// release. Checking needs only the toolchain's record; its manifest is read when something is
/// missing.
#[allow(clippy::too_many_arguments)]
fn complete(layout: &Layout, id: &str, host: &str, components: &[String], targets: &[String], server: &str, client: &net::Client, report: &mut dyn Report) -> Result<Toolchain, String> {
    let dir = layout.toolchains().join(id);
    let tc = Toolchain { id: id.to_string(), dir: dir.clone(), linked: false };
    let wanted: Vec<(&str, &str)> = components.iter().map(|c| (c.as_str(), host)).chain(targets.iter().map(|t| ("rust-std", t.as_str()))).collect();
    // A component is asked for by its name (`clippy`), recorded by its package (`clippy-preview`).
    let lacking = |record: &install::Record| -> Vec<(&str, &str)> { wanted.iter().copied().filter(|(c, t)| !record.has(c, t) && !record.has(&format!("{}-preview", c), t)).collect() };
    if lacking(&install::read_record(&dir)?).is_empty() {
        return Ok(tc);
    }
    let locked = install::lock(layout, id, report)?;
    let record = install::read_record(&dir)?;
    let lacking = lacking(&record);
    if lacking.is_empty() {
        return Ok(tc);
    }
    let manifest_path = dir.join("lib/rustlib/channel-manifest.toml");
    let manifest = dist::Manifest::parse(std::fs::read(&manifest_path).map_err(|e| format!("cannot read {}: {}", manifest_path.display(), e))?).map_err(|e| format!("{}: {}", manifest_path.display(), e))?;
    let mut add: Vec<dist::Component> = vec![];
    for (c, t) in lacking {
        let c = manifest.component(&manifest.package(c), t).map_err(|e| format!("Rust {}: {}", record.version, e))?;
        if !add.contains(&c) {
            add.push(c);
        }
    }
    install::add(layout, &locked, &dir, &add, server, client, report)?;
    Ok(tc)
}

/// Adds `components` to the installed toolchain `tc` from its own release (a linked toolchain
/// must have them already).
pub fn ensure_components(layout: &Layout, tc: &Toolchain, components: &[String], server: &str, report: &mut dyn Report) -> Result<(), String> {
    if tc.linked {
        return linked(tc.id.clone(), tc.dir.clone(), components, &[]).map(|_| ());
    }
    let host = install::read_record(&tc.dir)?.host;
    complete(layout, &tc.id, &host, components, &[], server, &net::Client::new(), report).map(|_| ())
}

/// A floating channel moved on: the toolchain it named and the one it names now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    pub channel: String,
    pub from: String,
    pub to: String,
}

/// Brings installed floating channels (`stable`, `1.98`, `nightly`; all of them, or `only`) to
/// their latest release: the new release is installed with the components and targets the old
/// one had, and `prepare`d (Haul's compiler driver), then the channel names it. A channel whose
/// manifest's published SHA-256 is the one it was installed from is up to date without
/// downloading the manifest. The toolchain a channel leaves is retired later (`retire`).
pub fn update(layout: &Layout, server: &str, only: Option<&str>, prepare: &mut dyn FnMut(&Toolchain), report: &mut dyn Report) -> Result<Vec<Moved>, String> {
    let client = net::Client::new();
    let mut moved = vec![];
    for (key, ch) in state::load(&layout.toolchains())?.channels {
        if only.is_some_and(|o| o != key) {
            continue;
        }
        let Ok(spec @ Spec::Dist { .. }) = Spec::parse(&key) else { continue };
        let Spec::Dist { channel, date: None, host: Some(host) } = &spec else { continue };
        if !spec.floating() {
            continue;
        }
        let published = client.get_bytes(&format!("{}.sha256", dist::manifest_url(server, channel.dist_name(), None)), &[]).map_err(|e| e.to_string())?;
        let published = String::from_utf8_lossy(&published).split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if !published.is_empty() && published == ch.manifest_sha256 && layout.toolchains().join(&ch.toolchain).join(install::RECORD).exists() {
            state::update(&layout.toolchains(), |s| {
                if let Some(c) = s.channels.get_mut(&key) {
                    c.checked_ns = util::now_wall_ns();
                }
                Ok(())
            })?;
            continue;
        }
        let old = install::read_record(&layout.toolchains().join(&ch.toolchain)).ok();
        let components: Vec<String> = old.iter().flat_map(|r| r.components.iter().filter(|(p, t, _)| p != "rust-std" && (t == host || t == "*")).map(|(p, _, _)| p.clone())).collect();
        let targets: Vec<String> = old.iter().flat_map(|r| r.components.iter().filter(|(p, t, _)| p == "rust-std" && t != host).map(|(_, t, _)| t.clone())).collect();
        let profile = old.as_ref().and_then(|r| Profile::parse(&r.profile).ok()).unwrap_or(Profile::Default);
        let (manifest, sha) = resolve(&client, server, channel, None, host, profile, &components, &targets)?;
        let id = match channel {
            Channel::Stable | Channel::Version(_) => format!("{}-{}", manifest.version(), host),
            Channel::Beta | Channel::Nightly => format!("{}-{}-{}", channel.dist_name(), manifest.date(), host),
        };
        if !layout.toolchains().join(&id).join(install::RECORD).exists() {
            let locked = install::lock(layout, &id, report)?;
            if !layout.toolchains().join(&id).join(install::RECORD).exists() {
                let plan = manifest.plan(host, profile, &components, &targets).map_err(|e| format!("Rust {}: {}", manifest.version(), e))?;
                install::install(layout, &locked, &install::Release { id: &id, host, profile, manifest: &manifest, origin: Some(&key) }, &plan, server, &client, report)?;
            }
        }
        if id != ch.toolchain {
            prepare(&Toolchain { id: id.clone(), dir: layout.toolchains().join(&id), linked: false });
            moved.push(Moved { channel: key.clone(), from: ch.toolchain.clone(), to: id.clone() });
        }
        state::update(&layout.toolchains(), |s| {
            s.channels.insert(key.clone(), state::ChannelState { toolchain: id.clone(), manifest_sha256: sha.clone(), checked_ns: util::now_wall_ns() });
            Ok(())
        })?;
    }
    Ok(moved)
}

/// How long a toolchain a channel left stays after its last use.
pub const RETIRE_AFTER_NS: u64 = 24 * 3_600 * 1_000_000_000;
/// The file whose modification time is a toolchain's last use (refreshed at most hourly).
const USED: &str = ".haul-used";

/// Notes that `tc` is in use (at most one write an hour).
pub fn mark_used(tc: &Toolchain) {
    if tc.linked {
        return;
    }
    let marker = tc.dir.join(USED);
    let fresh = std::fs::metadata(&marker).map(|m| util::mtime_ns(&m) + 3_600 * 1_000_000_000 > util::now_wall_ns()).unwrap_or(false);
    if !fresh {
        let _ = std::fs::write(&marker, b"");
    }
}

/// Deletes the toolchains that channels left behind: installed for a channel that now names
/// another release, named by no channel, unused for `RETIRE_AFTER_NS`. Returns their ids.
pub fn retire(layout: &Layout, report: &mut dyn Report) -> Result<Vec<String>, String> {
    let st = state::load(&layout.toolchains())?;
    let mut retired = vec![];
    for tc in installed(layout) {
        let Ok(rec) = install::read_record(&tc.dir) else { continue };
        let Some(origin) = &rec.origin else { continue };
        let named = st.channels.values().any(|c| c.toolchain == tc.id);
        let left = st.channels.get(origin).is_some_and(|c| c.toolchain != tc.id);
        let last_used = std::fs::metadata(tc.dir.join(USED)).map(|m| util::mtime_ns(&m)).unwrap_or(rec.installed_ns);
        if named || !left || last_used + RETIRE_AFTER_NS > util::now_wall_ns() {
            continue;
        }
        remove(layout, &tc.id, report)?;
        retired.push(tc.id);
    }
    Ok(retired)
}

/// Once a day per channel, after a build that used it: checks in the background (a detached,
/// low-priority `haul toolchain update --channel <key> --background`) whether the channel has a
/// newer release, installs it for the next build, and retires what channels left behind. Never
/// in CI, and not with `toolchain.auto-update = "off"`. Returns whether a check started.
pub fn start_update(layout: &Layout, key: &str, haul: &Path, auto_update: &str) -> Result<bool, String> {
    if auto_update != "background" || util::is_ci() {
        return Ok(false);
    }
    if !update_due(&state::load(&layout.toolchains())?, key, util::now_wall_ns()) {
        return Ok(false);
    }
    // Marked checked now: the builds until the check ends do not start another.
    state::update(&layout.toolchains(), |s| {
        if let Some(c) = s.channels.get_mut(key) {
            c.checked_ns = util::now_wall_ns();
        }
        Ok(())
    })?;
    let log = std::fs::File::create(layout.toolchains().join(".update.log")).map_err(|e| e.to_string())?;
    let mut c = std::process::Command::new(haul);
    c.args(["toolchain", "update", "--channel", key, "--background"]);
    crate::proc::spawn_detached(&mut c, log).map_err(|e| format!("cannot start {}: {}", haul.display(), e))?;
    Ok(true)
}

/// A channel is checked for a newer release a day after its last check.
fn update_due(st: &state::State, key: &str, now_ns: u64) -> bool {
    st.channels.get(key).is_some_and(|c| c.checked_ns + RETIRE_AFTER_NS <= now_ns)
}

/// The `channels.json` key of a toolchain selection that can move (`stable-<host>`), if it can.
pub fn floating_key(spec: &Spec) -> Option<String> {
    match spec {
        Spec::Dist { channel, date: None, host } if spec.floating() => Some(channel_key(channel, None, &host.clone().unwrap_or_else(self::host))),
        _ => None,
    }
}

/// Deletes an installed toolchain and forgets the channels naming it.
pub fn remove(layout: &Layout, id: &str, report: &mut dyn Report) -> Result<(), String> {
    let dir = layout.toolchains().join(id);
    if id.starts_with('.') || id.contains('/') || id.contains('\\') || !dir.is_dir() {
        return Err(format!("no toolchain {} is installed (`haul toolchain list` shows them)", id));
    }
    let _locked = install::lock(layout, id, report)?;
    // Renamed away first: a toolchain is never seen half deleted.
    let doomed = layout.toolchains().join(".staging").join(format!("{}.removing.{}", id, std::process::id()));
    util::create_private_dir(doomed.parent().unwrap_or(&doomed)).map_err(|e| e.to_string())?;
    std::fs::rename(&dir, &doomed).map_err(|e| format!("cannot remove {}: {}", dir.display(), e))?;
    state::update(&layout.toolchains(), |s| {
        s.channels.retain(|_, c| c.toolchain != id);
        Ok(())
    })?;
    std::fs::remove_dir_all(&doomed).map_err(|e| format!("cannot remove {}: {}", doomed.display(), e))
}

/// The installed toolchain a `rustc` belongs to, if it is one of Haul's.
pub fn owning(layout: &Layout, rustc: &Path) -> Option<Toolchain> {
    let dir = rustc.parent()?.parent()?;
    if dir.parent()? != layout.toolchains() || !dir.join(install::RECORD).is_file() {
        return None;
    }
    Some(Toolchain { id: util::lossy(dir.file_name()?), dir: dir.to_path_buf(), linked: false })
}

/// Every installed toolchain, by id.
pub fn installed(layout: &Layout) -> Vec<Toolchain> {
    let mut out: Vec<Toolchain> = std::fs::read_dir(layout.toolchains())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().join(install::RECORD).is_file())
        .map(|e| Toolchain { id: e.file_name().to_string_lossy().into_owned(), dir: e.path(), linked: false })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

#[cfg(test)]
pub mod testdist {
    //! A dist server for tests: releases of fake components in the real archive format.

    use super::*;
    use crate::archive::testing::{tar, xz, Item};
    use crate::net::testserver::{Route, Server};

    pub struct Dist {
        pub srv: Server,
    }

    pub struct NoReport {
        pub waited: usize,
    }

    impl install::Report for NoReport {
        fn waiting(&mut self, _id: &str) {
            self.waited += 1;
        }
        fn progress(&mut self, _done: usize, _total: usize, _bytes: u64, _current: &str) {}
    }

    /// The files a fake component installs.
    fn files(pkg: &str, target: &str) -> Vec<(String, Vec<u8>, u32)> {
        let f = |p: &str, body: &str, mode: u32| (p.to_string(), body.as_bytes().to_vec(), mode);
        match pkg {
            "rustc" => vec![f("bin/rustc", "#!/bin/sh\necho rustc-fake\n", 0o755), f("lib/librustc_driver-fake.so", "so", 0o644)],
            "cargo" => vec![f("bin/cargo", "#!/bin/sh\necho cargo-fake\n", 0o755)],
            "rust-std" => vec![f(&format!("lib/rustlib/{}/lib/libstd-fake.rlib", target), "std", 0o644)],
            "clippy-preview" => vec![f("bin/cargo-clippy", "#!/bin/sh\n", 0o755), f("bin/clippy-driver", "#!/bin/sh\n", 0o755)],
            "rustfmt-preview" => vec![f("bin/rustfmt", "#!/bin/sh\n", 0o755)],
            "rust-src" => vec![f("lib/rustlib/src/rust/library/core/src/lib.rs", "//! core", 0o644)],
            "rustc-dev" => vec![f(&format!("lib/rustlib/{}/lib/librustc_driver-fake.rlib", target), "dev", 0o644)],
            other => vec![f(&format!("share/{}/README", other), other, 0o644)],
        }
    }

    /// The archive of a fake component, as the dist server publishes it.
    pub fn component_archive(pkg: &str, target: &str, version: &str) -> (String, Vec<u8>) {
        let root = if target == "*" { format!("{}-{}", pkg, version) } else { format!("{}-{}-{}", pkg, version, target) };
        let inner = if pkg == "rust-std" { format!("rust-std-{}", target) } else { pkg.to_string() };
        let fs = files(pkg, target);
        let manifest: String = fs.iter().map(|(p, _, _)| format!("file:{}\n", p)).collect();
        let mut names: Vec<(String, Vec<u8>, u32)> = vec![(format!("{}/components", root), format!("{}\n", inner).into_bytes(), 0o644), (format!("{}/rust-installer-version", root), b"3\n".to_vec(), 0o644), (format!("{}/{}/manifest.in", root, inner), manifest.into_bytes(), 0o644)];
        names.extend(fs.into_iter().map(|(p, b, m)| (format!("{}/{}/{}", root, inner, p), b, m)));
        let items: Vec<Item> = names.iter().map(|(n, b, m)| Item::File(n, b, *m)).collect();
        (root, xz(&tar(&items)))
    }

    impl Dist {
        pub fn start() -> Dist {
            Dist { srv: Server::start() }
        }

        /// Publishes a release: its archives and its manifest at `channel_path` (e.g.
        /// `/dist/channel-rust-stable.toml`).
        pub fn publish(&self, channel_path: &str, date: &str, version: &str, pkgs: &[(&str, &str, bool)]) {
            let mut hashes = std::collections::HashMap::new();
            for (pkg, target, available) in pkgs {
                if !available {
                    continue;
                }
                let (root, bytes) = component_archive(pkg, target, version);
                hashes.insert(format!("{}{}", pkg, target), crate::util::sha256_hex(&bytes));
                self.srv.route(&format!("/dist/{}/{}.tar.xz", date, root), Route { body: bytes, ..Default::default() });
            }
            let manifest = dist::testing::manifest(&self.srv.base, date, version, pkgs, &|p, t| hashes.get(&format!("{}{}", p, t)).cloned().unwrap_or_default());
            self.srv.route(channel_path, Route { body: manifest.clone().into_bytes(), ..Default::default() });
            self.srv.route(&format!("{}.sha256", channel_path), Route { body: format!("{}  x.toml\n", crate::util::sha256_hex(manifest.as_bytes())).into_bytes(), ..Default::default() });
        }

        pub fn served(&self) -> usize {
            self.srv.state.lock().unwrap().log.len()
        }
    }

    pub fn release(host: &str) -> Vec<(&'static str, String, bool)> {
        vec![("rustc", host.into(), true), ("cargo", host.into(), true), ("rust-std", host.into(), true), ("rust-std", "wasm32-unknown-unknown".into(), true), ("clippy-preview", host.into(), true), ("rustfmt-preview", host.into(), true), ("rust-src", "*".into(), true), ("rustc-dev", host.into(), true)]
    }

    pub fn publish_release(d: &Dist, path: &str, date: &str, version: &str, host: &str) {
        let r = release(host);
        let pkgs: Vec<(&str, &str, bool)> = r.iter().map(|(p, t, a)| (*p, t.as_str(), *a)).collect();
        d.publish(path, date, version, &pkgs);
    }
}

#[cfg(test)]
mod tests {
    use super::testdist::*;
    use super::*;

    fn layout() -> Layout {
        Layout::new(std::env::temp_dir().join(format!("haul-tc-{}", util::new_id())))
    }

    fn sel(s: &str) -> Selection {
        Selection { spec: Spec::parse(s).unwrap(), source: Source::Plus, file: None }
    }

    #[test]
    fn a_channel_is_installed_once_and_then_used_without_the_network() {
        let (d, l, h) = (Dist::start(), layout(), host());
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-10-01", "1.99.0", &h);
        let tc = ensure(&l, &sel("stable"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        assert_eq!(tc.id, format!("1.99.0-{}", h));
        assert!(!tc.linked);
        assert_eq!(std::process::Command::new(tc.rustc()).output().unwrap().stdout, b"rustc-fake\n");
        assert!(tc.bin("cargo-clippy").is_file() && tc.bin("rustfmt").is_file());
        assert!(tc.dir.join("lib/rustlib/src/rust/library/core/src/lib.rs").is_file());
        assert!(!tc.dir.join("lib/rustlib/wasm32-unknown-unknown").exists(), "targets come when asked for");
        assert!(!tc.dir.join(format!("lib/rustlib/{}/lib/librustc_driver-fake.rlib", h)).exists(), "rustc-dev is not in the default profile");
        let listed = std::fs::read_to_string(tc.dir.join("lib/rustlib/components")).unwrap();
        assert_eq!(listed.lines().collect::<Vec<_>>(), ["rustc", "cargo", &format!("rust-std-{}", h), "clippy-preview", "rustfmt-preview", "rust-src"]);
        assert_eq!(std::fs::read_to_string(tc.dir.join("lib/rustlib/manifest-cargo")).unwrap(), "file:bin/cargo\n");
        assert_eq!(state::load(&l.toolchains()).unwrap().channels[&format!("stable-{}", h)].toolchain, tc.id);
        assert!(std::fs::read_dir(l.downloads()).unwrap().next().is_none(), "archives are dropped once installed");
        let before = d.served();
        assert_eq!(ensure(&l, &sel("stable"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap(), tc);
        assert_eq!(ensure(&l, &sel("1.99.0"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap(), tc);
        assert_eq!(d.served(), before, "an installed toolchain needs no download");
        assert_eq!(owning(&l, &tc.rustc()), Some(tc.clone()));
        assert_eq!(installed(&l), vec![tc]);
    }

    #[test]
    fn targets_and_components_come_from_the_installed_release() {
        let (d, l, h) = (Dist::start(), layout(), host());
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-10-01", "1.99.0", &h);
        let tc = ensure(&l, &sel("stable"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        // Stable moves on; the toolchain installed as stable still gets its own release's parts.
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-11-12", "1.100.0", &h);
        let needs = Needs { components: vec!["rustc-dev".into()], targets: vec!["wasm32-unknown-unknown".into()], ..Default::default() };
        let again = ensure(&l, &sel("stable"), &needs, &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        assert_eq!(again, tc);
        assert!(tc.dir.join("lib/rustlib/wasm32-unknown-unknown/lib/libstd-fake.rlib").is_file());
        assert!(tc.dir.join(format!("lib/rustlib/{}/lib/librustc_driver-fake.rlib", h)).is_file());
        assert!(!tc.dir.join("lib/rustlib/.haul-adding").exists());
        let rec = tc.record().unwrap();
        assert!(rec.has("rustc-dev", &h) && rec.has("rust-std", "wasm32-unknown-unknown"));
        assert!(d.srv.requests("/dist/2026-11-12/rustc-dev-1.100.0-".to_string().as_str()).is_empty());
        let e = ensure(&l, &sel("stable"), &Needs { components: vec!["miri".into()], ..Default::default() }, &d.srv.base, &mut NoReport { waited: 0 }).unwrap_err();
        assert!(e.contains("no component `miri-preview`") || e.contains("no component `miri`"), "{}", e);
    }

    #[test]
    fn an_update_moves_channels_to_the_new_release_with_what_they_had() {
        let (d, l, h) = (Dist::start(), layout(), host());
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-10-01", "1.99.0", &h);
        publish_release(&d, "/dist/channel-rust-1.99.0.toml", "2026-10-01", "1.99.0", &h);
        let needs = Needs { targets: vec!["wasm32-unknown-unknown".into()], ..Default::default() };
        let old = ensure(&l, &sel("stable"), &needs, &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        let pinned = ensure(&l, &sel("1.99.0"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        assert_eq!(old, pinned);
        // What the channel moves to is prepared (Haul's compiler driver) installed, before the
        // channel names it.
        let mut prepared = vec![];
        let mut prepare = |tc: &Toolchain| prepared.push((tc.id.clone(), tc.record().is_some(), state::load(&l.toolchains()).unwrap().channels[&format!("stable-{}", h)].toolchain.clone()));
        assert!(update(&l, &d.srv.base, None, &mut prepare, &mut NoReport { waited: 0 }).unwrap().is_empty(), "nothing newer yet");
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-11-12", "1.100.0", &h);
        let moved = update(&l, &d.srv.base, None, &mut prepare, &mut NoReport { waited: 0 }).unwrap();
        assert_eq!(moved, vec![Moved { channel: format!("stable-{}", h), from: old.id.clone(), to: format!("1.100.0-{}", h) }]);
        assert_eq!(prepared, vec![(format!("1.100.0-{}", h), true, old.id.clone())]);
        let new = ensure(&l, &sel("stable"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        assert_eq!(new.id, format!("1.100.0-{}", h));
        assert!(new.record().unwrap().has("rust-std", "wasm32-unknown-unknown"), "the new release gets the old one's targets");
        assert_eq!(ensure(&l, &sel("1.99.0"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap(), old, "a release is not moved");
        remove(&l, &old.id, &mut NoReport { waited: 0 }).unwrap();
        assert!(!old.dir.exists());
        assert_eq!(installed(&l), vec![new.clone()]);
        assert!(remove(&l, "../x", &mut NoReport { waited: 0 }).is_err());
        assert!(remove(&l, &old.id, &mut NoReport { waited: 0 }).unwrap_err().contains("no toolchain"));
    }

    #[test]
    fn an_unchanged_channel_is_confirmed_by_its_checksum_alone() {
        let (d, l, h) = (Dist::start(), layout(), host());
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-10-01", "1.99.0", &h);
        ensure(&l, &sel("stable"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        let before = state::load(&l.toolchains()).unwrap().channels[&format!("stable-{}", h)].checked_ns;
        let manifests = || d.srv.requests("/dist/channel-rust-stable.toml").len();
        let fetched = manifests();
        assert!(update(&l, &d.srv.base, Some(&format!("stable-{}", h)), &mut |_: &Toolchain| {}, &mut NoReport { waited: 0 }).unwrap().is_empty());
        assert_eq!(manifests(), fetched, "the manifest is not downloaded again");
        assert_eq!(d.srv.requests("/dist/channel-rust-stable.toml.sha256").len(), 2);
        assert!(state::load(&l.toolchains()).unwrap().channels[&format!("stable-{}", h)].checked_ns > before);
        // Another channel is not touched by an update of this one.
        assert!(update(&l, &d.srv.base, Some("nightly-x"), &mut |_: &Toolchain| {}, &mut NoReport { waited: 0 }).unwrap().is_empty());
        assert_eq!(d.srv.requests("/dist/channel-rust-stable.toml.sha256").len(), 2);
    }

    #[test]
    fn what_a_channel_left_is_retired_after_a_day_unused_and_nothing_else_is() {
        let (d, l, h) = (Dist::start(), layout(), host());
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-10-01", "1.99.0", &h);
        publish_release(&d, "/dist/channel-rust-1.98.0.toml", "2026-08-01", "1.98.0", &h);
        let old = ensure(&l, &sel("stable"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        let pinned = ensure(&l, &sel("1.98.0"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        mark_used(&old);
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-11-12", "1.100.0", &h);
        update(&l, &d.srv.base, None, &mut |_: &Toolchain| {}, &mut NoReport { waited: 0 }).unwrap();
        assert!(retire(&l, &mut NoReport { waited: 0 }).unwrap().is_empty(), "used within the day");
        let day_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600);
        for tc in [&old, &pinned] {
            std::fs::File::create(tc.dir.join(USED)).unwrap().set_modified(day_ago).unwrap();
        }
        assert_eq!(retire(&l, &mut NoReport { waited: 0 }).unwrap(), vec![old.id.clone()]);
        assert!(!old.dir.exists());
        assert!(pinned.dir.exists(), "a release asked for by its name is never retired");
        let new = ensure(&l, &sel("stable"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        std::fs::File::create(new.dir.join(USED)).unwrap().set_modified(day_ago).unwrap();
        assert!(retire(&l, &mut NoReport { waited: 0 }).unwrap().is_empty(), "the release a channel names stays, used or not");
    }

    #[test]
    fn a_channel_is_checked_a_day_after_its_last_check_and_never_when_off() {
        let mut st = state::State::default();
        st.channels.insert("stable-h".into(), state::ChannelState { toolchain: "1.99.0-h".into(), manifest_sha256: String::new(), checked_ns: 1_000 });
        assert!(!update_due(&st, "stable-h", 1_000 + RETIRE_AFTER_NS - 1));
        assert!(update_due(&st, "stable-h", 1_000 + RETIRE_AFTER_NS));
        assert!(!update_due(&st, "nightly-h", u64::MAX), "a channel never installed is not checked");
        assert!(!start_update(&layout(), "stable-h", Path::new("/nonexistent"), "off").unwrap());
        assert_eq!(floating_key(&Spec::parse("stable").unwrap()), Some(format!("stable-{}", host())));
        assert_eq!(floating_key(&Spec::parse("1.98").unwrap()), Some(format!("1.98-{}", host())));
        assert_eq!(floating_key(&Spec::parse("1.98.1").unwrap()), None);
        assert_eq!(floating_key(&Spec::parse("nightly-2026-10-11").unwrap()), None);
    }

    #[test]
    fn a_file_that_is_not_the_published_one_installs_nothing() {
        let (d, l, h) = (Dist::start(), layout(), host());
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-10-01", "1.99.0", &h);
        // Replace one archive after publishing: its hash no longer matches the manifest.
        let (root, _) = component_archive("cargo", &h, "1.99.0");
        d.srv.route(&format!("/dist/2026-10-01/{}.tar.xz", root), crate::net::testserver::Route { body: b"not an archive".to_vec(), ..Default::default() });
        let e = ensure(&l, &sel("stable"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap_err();
        assert!(e.contains("is not the published file"), "{}", e);
        assert!(installed(&l).is_empty());
        let staging: Vec<_> = std::fs::read_dir(l.toolchains().join(".staging")).unwrap().flatten().collect();
        assert!(staging.is_empty(), "{:?}", staging);
        assert!(state::load(&l.toolchains()).unwrap().channels.is_empty());
    }

    #[test]
    fn concurrent_first_uses_install_once() {
        let (d, l, h) = (Dist::start(), layout(), host());
        publish_release(&d, "/dist/channel-rust-1.99.0.toml", "2026-10-01", "1.99.0", &h);
        let ids: Vec<String> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..4).map(|_| s.spawn(|| ensure(&l, &sel("1.99.0"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap().id)).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(ids.iter().all(|i| *i == ids[0]));
        let (root, _) = component_archive("rustc", &h, "1.99.0");
        assert_eq!(d.srv.requests(&format!("/dist/2026-10-01/{}.tar.xz", root)).len(), 1, "one download of each archive");
    }

    #[test]
    fn an_undated_nightly_is_the_latest_with_everything_asked_for() {
        let (d, l, h) = (Dist::start(), layout(), host());
        let mut today: Vec<(&str, String, bool)> = release(&h);
        today.iter_mut().filter(|p| p.0 == "rustfmt-preview").for_each(|p| p.2 = false);
        let pkgs: Vec<(&str, &str, bool)> = today.iter().map(|(p, t, a)| (*p, t.as_str(), *a)).collect();
        d.publish("/dist/channel-rust-nightly.toml", "2026-10-12", "1.101.0-nightly", &pkgs);
        d.publish("/dist/2026-10-12/channel-rust-nightly.toml", "2026-10-12", "1.101.0-nightly", &pkgs);
        publish_release(&d, "/dist/2026-10-11/channel-rust-nightly.toml", "2026-10-11", "1.101.0-nightly", &h);
        let tc = ensure(&l, &sel("nightly"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        assert_eq!(tc.id, format!("nightly-2026-10-11-{}", h));
        assert!(tc.bin("rustfmt").is_file());
        // A minimal profile does not need rustfmt: today's nightly does.
        let minimal = Selection { file: Some(file::ToolchainFile { path: "/x/rust-toolchain.toml".into(), spec: Spec::parse("nightly").unwrap(), components: vec![], targets: vec![], profile: Some(Profile::Minimal) }), ..sel("nightly") };
        let l2 = layout();
        assert_eq!(ensure(&l2, &minimal, &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap().id, format!("nightly-2026-10-12-{}", h));
        let e = ensure(&layout(), &sel("nightly-2026-10-13"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap_err();
        assert!(e.contains("no Rust nightly release of 2026-10-13"), "{}", e);
    }

    #[test]
    fn an_interrupted_add_is_undone_and_a_conflicting_one_changes_nothing() {
        let (d, l, h) = (Dist::start(), layout(), host());
        publish_release(&d, "/dist/channel-rust-stable.toml", "2026-10-01", "1.99.0", &h);
        let tc = ensure(&l, &sel("stable"), &Needs::default(), &d.srv.base, &mut NoReport { waited: 0 }).unwrap();
        // As if an add of rustc-dev stopped after moving one file in.
        let half = tc.dir.join(format!("lib/rustlib/{}/lib/librustc_driver-fake.rlib", h));
        std::fs::write(&half, "partial").unwrap();
        std::fs::write(tc.dir.join("lib/rustlib/.haul-adding"), format!("rustc-dev\t{}\trustc-dev\tlib/rustlib/{}/lib/librustc_driver-fake.rlib\n", h, h)).unwrap();
        drop(install::lock(&l, &tc.id, &mut NoReport { waited: 0 }).unwrap());
        assert!(!half.exists() && !tc.dir.join("lib/rustlib/.haul-adding").exists());
        assert!(tc.rustc().is_file(), "what the record holds stays");
        // A file in the way of a component: the add fails before moving anything.
        std::fs::write(&half, "in the way").unwrap();
        let e = ensure(&l, &sel("stable"), &Needs { components: vec!["rustc-dev".into()], ..Default::default() }, &d.srv.base, &mut NoReport { waited: 0 }).unwrap_err();
        assert!(e.contains("would replace"), "{}", e);
        assert_eq!(std::fs::read_to_string(&half).unwrap(), "in the way");
        assert!(!tc.record().unwrap().has("rustc-dev", &h));
    }

    #[test]
    fn linked_toolchains_are_used_as_they_are() {
        let l = layout();
        let dir = l.toolchains().join("..").join("mine");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join(if cfg!(windows) { "rustc.exe" } else { "rustc" }), "").unwrap();
        state::update(&l.toolchains(), |s| {
            s.linked.insert("mine".into(), dir.clone());
            Ok(())
        })
        .unwrap();
        let tc = ensure(&l, &sel("mine"), &Needs::default(), "http://127.0.0.1:9", &mut NoReport { waited: 0 }).unwrap();
        assert!(tc.linked && tc.dir == dir);
        let e = ensure(&l, &sel("mine"), &Needs { targets: vec!["wasm32-unknown-unknown".into()], ..Default::default() }, "http://127.0.0.1:9", &mut NoReport { waited: 0 }).unwrap_err();
        assert!(e.contains("does not change a linked toolchain"), "{}", e);
        assert!(ensure(&l, &sel("other"), &Needs::default(), "http://127.0.0.1:9", &mut NoReport { waited: 0 }).unwrap_err().contains("haul toolchain link other"));
    }

    #[test]
    fn days_go_back_across_months_and_years() {
        assert_eq!(previous_day("2026-10-12").as_deref(), Some("2026-10-11"));
        assert_eq!(previous_day("2026-03-01").as_deref(), Some("2026-02-28"));
        assert_eq!(previous_day("2024-03-01").as_deref(), Some("2024-02-29"));
        assert_eq!(previous_day("2027-01-01").as_deref(), Some("2026-12-31"));
        assert_eq!(previous_day("x"), None);
    }
}
