//! Whether a newer QuickRust is published, and saying so.
//!
//! Once a day, after a build, a detached, low-priority `qrust self check --background` reads
//! the release manifest `qrust self update` installs from, through the same charter and
//! signature, and records the newest release of this QuickRust's channel for this platform in
//! `~/.qrust/self-update.json`. A build never waits for it: it is a process of its own, each of its
//! requests is one attempt bounded end to end by [`REQUEST_TIMEOUT`], and what it finds is told
//! after a later build. A check that fails, for want of a network or for any other reason, is
//! recorded and tried again a day later, and nothing is said about it.
//!
//! What a build tells: after a build in a terminal, when the newest release recorded is newer than
//! the running QuickRust, one note naming it and the command that updates it (`qrust self update`,
//! or the package manager's), at most once a day.
//!
//! Nothing is checked in CI, with `self.update-check = "off"` (`QRUST_SELF_UPDATE_CHECK=off`), by a
//! QuickRust built from source (it has no release line), or where QuickRust publishes no release.

use crate::config::Config;
use crate::release::Trust;
use crate::util;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Between two checks, and between two notes of a newer release.
pub const INTERVAL_NS: u64 = 24 * 3_600 * 1_000_000_000;
/// How long one request of a check may take, from connecting to the last byte of the answer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const FORMAT: u32 = 1;

/// Where the newest release is read from: the release line this QuickRust was built with, or
/// the registry `self.update-url` names instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub registry: String,
    pub channel: String,
    pub trust: Trust,
    pub charter_roots: Vec<String>,
    pub platform: String,
}

impl Source {
    /// The source `qrust self update` reads, or why there is none.
    pub fn of(cfg: &Config) -> Result<Source, String> {
        let identity = crate::release::identity().ok_or("this QuickRust was built from source, so it has no release line to look for a newer release on")?;
        let platform = crate::release::platform().ok_or("QuickRust publishes no release for this platform")?;
        let other = cfg.str("self.update-url");
        Ok(Source {
            registry: if other.is_empty() { identity.registry } else { other.to_string() },
            channel: identity.channel,
            trust: identity.trust,
            charter_roots: identity.charter_roots,
            platform: platform.to_string(),
        })
    }

    /// The source a background check and a build's note use: none in CI, with
    /// `self.update-check = "off"`, or without a source at all.
    fn background(cfg: &Config) -> Option<Source> {
        if cfg.str("self.update-check") != "background" || util::is_ci() {
            return None;
        }
        Source::of(cfg).ok()
    }
}

/// What the checks have found, in `~/.qrust/self-update.json`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub format: u32,
    /// When the last check started, finished or not.
    pub checked_ns: u64,
    /// The registry and channel `latest` was read from.
    pub registry: String,
    pub channel: String,
    /// The newest release for this platform, as the last check that finished found it.
    pub latest: Option<String>,
    /// Why the last check did not finish, when it did not.
    pub failed: Option<String>,
    /// When a newer release was last told.
    pub notified_ns: u64,
}

pub fn state_path() -> PathBuf {
    util::qrust_home().join("self-update.json")
}

fn log_path() -> PathBuf {
    util::qrust_home().join("self-update.log")
}

/// The state at `path`. A file that is missing, unreadable or of another shape is a state with
/// nothing found yet: a check then runs, and writes it again.
pub fn load(path: &Path) -> State {
    util::read_json(path).and_then(|v| serde_json::from_value::<State>(v).ok()).filter(|s| s.format == FORMAT).unwrap_or_default()
}

fn save(path: &Path, state: &State) -> std::io::Result<()> {
    let value = serde_json::to_value(State { format: FORMAT, ..state.clone() }).map_err(std::io::Error::other)?;
    util::write_json(path, &value)
}

/// A check is due a day after the last one started, and at once when the clock is before it
/// (it was set back).
fn due(state: &State, now_ns: u64) -> bool {
    now_ns >= state.checked_ns.saturating_add(INTERVAL_NS) || now_ns < state.checked_ns
}

/// After a build: starts a background check when one is due. It is marked started first, so the
/// builds until it ends start no other, and a check that cannot be marked is not started (it
/// would start after every build). Nothing that goes wrong here is said: a build is not the place
/// for it, and `qrust self check` says it.
pub fn start(cfg: &Config, qrust: &Path) {
    if Source::background(cfg).is_none() {
        return;
    }
    let path = state_path();
    let mut state = load(&path);
    let now = util::now_wall_ns();
    if !due(&state, now) {
        return;
    }
    state.checked_ns = now;
    if save(&path, &state).is_err() {
        return;
    }
    let Ok(log) = std::fs::File::create(log_path()) else { return };
    let mut c = crate::resolve::qrust_command(qrust);
    c.args(["self", "check", "--background"]);
    let _ = crate::proc::spawn_detached(&mut c, log);
}

/// Reads the newest release of `source` (`None` when its channel has published nothing yet) and
/// records what was found, or why nothing was, in the state at `path`. A failed check keeps the
/// release an earlier one found on the same registry and channel.
pub fn check(path: &Path, source: &Source, client: &crate::net::Client, now_ns: u64) -> Result<Option<String>, String> {
    let found = crate::release::fetch_latest(client, &source.registry, &source.trust, &source.charter_roots, &source.channel, &source.platform, now_ns / 1_000_000_000).map(|r| r.map(|r| r.version));
    let mut state = load(path);
    let same_line = state.registry == source.registry && state.channel == source.channel;
    state.checked_ns = now_ns;
    state.registry = source.registry.clone();
    state.channel = source.channel.clone();
    match &found {
        Ok(latest) => {
            state.latest = latest.clone();
            state.failed = None;
        }
        Err(e) => {
            if !same_line {
                state.latest = None;
            }
            state.failed = Some(e.clone());
        }
    }
    save(path, &state).map_err(|e| format!("cannot record the check in {}: {}", path.display(), e))?;
    found
}

/// The release recorded in `state` for `source` when it is newer than `current`.
pub fn newer(state: &State, source: &Source, current: &str) -> Option<String> {
    if state.registry != source.registry || state.channel != source.channel {
        return None;
    }
    let latest = state.latest.as_deref()?;
    let (Ok(l), Ok(c)) = (semver::Version::parse(latest), semver::Version::parse(current)) else { return None };
    (l > c).then(|| latest.to_string())
}

/// What tells a newer release, and how to install it.
pub fn message(latest: &str, current: &str, update: &str) -> String {
    format!("QuickRust {} is available (this is {}): run `{}` to update", latest, current, update)
}

/// After a build in a terminal: the note of a newer release, when one is recorded and none was
/// told in the last day. It is marked told before it is returned; one that cannot be marked is
/// not told, or it would be told after every build.
pub fn notice(cfg: &Config) -> Option<String> {
    let source = Source::background(cfg)?;
    notice_in(&state_path(), &source, env!("CARGO_PKG_VERSION"), util::now_wall_ns(), &|| crate::setup::update_command().ok())
}

/// [`notice`] with its state, source, version, clock and update command given.
pub fn notice_in(path: &Path, source: &Source, current: &str, now_ns: u64, update: &dyn Fn() -> Option<String>) -> Option<String> {
    let mut state = load(path);
    let latest = newer(&state, source, current)?;
    if now_ns < state.notified_ns.saturating_add(INTERVAL_NS) && now_ns >= state.notified_ns {
        return None;
    }
    let update = update()?;
    state.notified_ns = now_ns;
    save(path, &state).ok()?;
    Some(message(&latest, current, &update))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testserver::{Route, Server};
    use crate::release::testing::*;

    const PLATFORM: &str = "linux-x86_64";
    const SHA: &str = "b74a72297e9404fc35470af7fc77f157e2899577d2907bcdffe90397f89c14a5";
    const NOW_NS: u64 = NOW * 1_000_000_000;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("qrust-update-check-{}", util::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("self-update.json")
    }

    /// A chartered release registry publishing `version` as its latest release.
    fn registry(line: &Line, version: &str) -> (Server, Source) {
        let server = Server::start();
        let anchor = server.url("").trim_end_matches('/').to_string();
        let serve = |body: Vec<u8>| Route { body, ..Route::default() };
        let charter = line.charter(&anchor, "releases", "prod");
        server.route("/charter.json", serve(charter.clone()));
        server.route("/charter.json.sig", serve(line.sign_charter(&charter)));
        publish(&server, line, version);
        let source = Source { registry: anchor, channel: "release".into(), trust: Trust { roots: line.root_keys(), environment: "prod".into() }, charter_roots: line.root_keys(), platform: PLATFORM.into() };
        (server, source)
    }

    fn publish(server: &Server, line: &Line, version: &str) {
        let name = format!("qrust-{}-{}.tar.gz", version, PLATFORM);
        let bytes = manifest("release", version, PLATFORM, &name, &format!("https://r.example/{}", name), SHA, 4096);
        server.route("/quickrust/manifest.sig", Route { body: line.sign(&bytes), ..Route::default() });
        server.route("/quickrust/manifest", Route { body: bytes, ..Route::default() });
    }

    fn client() -> crate::net::Client {
        crate::net::Client::bounded(REQUEST_TIMEOUT)
    }

    fn self_update() -> Option<String> {
        Some("qrust self update".into())
    }

    // TEST0321: A newer release in the signed manifest is told with the command that updates
    #[test]
    fn test0321_a_newer_release_in_the_signed_manifest_is_told_with_the_command_that_updates() {
        let line = Line::new();
        let (server, source) = registry(&line, "1.170.0");
        let path = scratch();
        assert_eq!(check(&path, &source, &client(), NOW_NS), Ok(Some("1.170.0".into())));
        assert_eq!(notice_in(&path, &source, "1.164.1495", NOW_NS, &self_update).as_deref(), Some("QuickRust 1.170.0 is available (this is 1.164.1495): run `qrust self update` to update"));
        // A package manager's installation names its manager's command instead.
        let path = scratch();
        check(&path, &source, &client(), NOW_NS).unwrap();
        let brew = || Some("brew upgrade quickrust".to_string());
        assert_eq!(notice_in(&path, &source, "1.164.1495", NOW_NS, &brew).as_deref(), Some("QuickRust 1.170.0 is available (this is 1.164.1495): run `brew upgrade quickrust` to update"));
        // The running release, or one newer than the manifest's, is told nothing.
        for current in ["1.170.0", "1.170.1", "2.0.0"] {
            let path = scratch();
            check(&path, &source, &client(), NOW_NS).unwrap();
            assert_eq!(notice_in(&path, &source, current, NOW_NS, &self_update), None, "{}", current);
        }
        // A manifest whose signature does not hold is no news of a release.
        let path = scratch();
        let name = format!("qrust-1.180.0-{}.tar.gz", PLATFORM);
        server.route("/quickrust/manifest", Route { body: manifest("release", "1.180.0", PLATFORM, &name, "https://r.example/x", SHA, 4096), ..Route::default() });
        assert!(check(&path, &source, &client(), NOW_NS).is_err());
        assert_eq!(notice_in(&path, &source, "1.164.1495", NOW_NS, &self_update), None);
    }

    // TEST0322: A newer release is told at most once a day and only for the registry it came from
    #[test]
    fn test0322_a_newer_release_is_told_at_most_once_a_day_and_only_for_the_registry_it_came_from() {
        let line = Line::new();
        let (_registry, source) = registry(&line, "1.170.0");
        let path = scratch();
        check(&path, &source, &client(), NOW_NS).unwrap();
        let tell = |now| notice_in(&path, &source, "1.164.1495", now, &self_update).is_some();
        assert!(tell(NOW_NS));
        assert!(!tell(NOW_NS + 1), "told a moment ago");
        assert!(!tell(NOW_NS + INTERVAL_NS - 1), "told less than a day ago");
        assert!(tell(NOW_NS + INTERVAL_NS), "a day later");
        assert!(tell(NOW_NS - 1), "the clock set back before the last note");
        // Read from another registry or channel (`self.update-url` changed since), it is not this
        // QuickRust's news.
        let elsewhere = Source { registry: "https://release.example".into(), ..source.clone() };
        assert_eq!(notice_in(&path, &elsewhere, "1.164.1495", NOW_NS + 3 * INTERVAL_NS, &self_update), None);
        let nightly = Source { channel: "nightly".into(), ..source.clone() };
        assert_eq!(notice_in(&path, &nightly, "1.164.1495", NOW_NS + 3 * INTERVAL_NS, &self_update), None);
        // No command to name (the package marker cannot be read): nothing is told, and nothing
        // is marked told.
        assert_eq!(notice_in(&path, &source, "1.164.1495", NOW_NS + 3 * INTERVAL_NS, &|| None), None);
        assert!(tell(NOW_NS + 3 * INTERVAL_NS));
    }

    // TEST0323: A check that fails is recorded silently and keeps what an earlier one found
    #[test]
    fn test0323_a_check_that_fails_is_recorded_silently_and_keeps_what_an_earlier_one_found() {
        let line = Line::new();
        let (server, source) = registry(&line, "1.170.0");
        let path = scratch();
        check(&path, &source, &client(), NOW_NS).unwrap();
        // The registry fails: asked once, not retried (the next check is tomorrow's), and the
        // release found before is still the newest known.
        server.route("/quickrust/manifest", Route { status: Some(503), ..Route::default() });
        let before = server.requests("/quickrust/manifest").len();
        let failed = check(&path, &source, &client(), NOW_NS + INTERVAL_NS).unwrap_err();
        assert_eq!(server.requests("/quickrust/manifest").len(), before + 1, "one attempt");
        let state = load(&path);
        assert_eq!((state.checked_ns, state.latest.as_deref(), state.failed.as_deref()), (NOW_NS + INTERVAL_NS, Some("1.170.0"), Some(failed.as_str())));
        assert!(notice_in(&path, &source, "1.164.1495", NOW_NS + INTERVAL_NS, &self_update).is_some());
        // Failing on another registry, it knows nothing of a newer release there.
        publish(&server, &line, "1.170.0");
        let path = scratch();
        check(&path, &source, &client(), NOW_NS).unwrap();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let unreachable = Source { registry: format!("http://{}", closed.local_addr().unwrap()), ..source.clone() };
        drop(closed);
        assert!(check(&path, &unreachable, &client(), NOW_NS).is_err());
        assert_eq!(load(&path).latest, None);
        assert_eq!(notice_in(&path, &unreachable, "1.164.1495", NOW_NS, &self_update), None);
        // A state file that is not one is a state with nothing found, and a check is due.
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load(&path), State::default());
        assert!(due(&load(&path), NOW_NS));
    }

    // TEST0324: A registry that does not answer holds a check no longer than its bound
    #[test]
    fn test0324_a_registry_that_does_not_answer_holds_a_check_no_longer_than_its_bound() {
        // A server that takes the connection, reads the request and never answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                held.push(stream);
            }
        });
        let line = Line::new();
        let source = Source { registry: format!("http://{}", addr), channel: "release".into(), trust: Trust { roots: line.root_keys(), environment: "prod".into() }, charter_roots: line.root_keys(), platform: PLATFORM.into() };
        let bound = Duration::from_secs(1);
        let started = std::time::Instant::now();
        assert!(check(&scratch(), &source, &crate::net::Client::bounded(bound), NOW_NS).is_err());
        assert!(started.elapsed() < bound * 3, "a check that waits on a silent registry took {:?}", started.elapsed());
    }

    // TEST0325: A check is due a day after the last one started
    #[test]
    fn test0325_a_check_is_due_a_day_after_the_last_one_started() {
        let at = |checked_ns| State { checked_ns, ..State::default() };
        assert!(due(&State::default(), NOW_NS), "never checked");
        assert!(!due(&at(NOW_NS), NOW_NS));
        assert!(!due(&at(NOW_NS), NOW_NS + INTERVAL_NS - 1));
        assert!(due(&at(NOW_NS), NOW_NS + INTERVAL_NS));
        assert!(due(&at(NOW_NS), NOW_NS - 1), "the clock set back");
    }
}
