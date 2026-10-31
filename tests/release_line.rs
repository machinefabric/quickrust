//! QuickRust's own release line, end to end: what a build is given, and what a QuickRust built
//! with a release line will and will not update itself to.
//!
//! The QuickRust these tests update is built here, as a release build is: with a registry's anchor,
//! a channel, a signing environment, three root keys and three charter root keys baked in
//! (`build.rs`). The roots are the test's own (`common::release_line`), so the test can publish —
//! and charter — what a release operator would, and what one never should.
#![cfg(unix)]

mod common;

use common::release_line::{manifest, Key, Line};
use common::{ok, run, stderr, stdout, Sandbox};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const REGISTRY: &str = "https://releases.invalid";
const CHANNEL: &str = "nightly";
const ENVIRONMENT: &str = "staging";

/// The registry's name for this platform, written out: the code under test has its own table.
fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        other => panic!("these tests have no platform name for {:?}", other),
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Builds QuickRust with `inputs` as its release line, in a target directory of this suite's own.
/// The directory is shared by every build here, so only QuickRust itself is compiled again when
/// the inputs change, and one build at a time uses it.
fn build(inputs: &[(&str, &str)]) -> Output {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut cargo = Command::new(std::env::var_os("CARGO").expect("cargo runs the tests"));
    cargo.args(["build", "--locked", "--bin", "qrust", "--bin", "qrust-rustc-wrapper", "--manifest-path"]);
    cargo.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    cargo.arg("--target-dir").arg(target_dir());
    for name in ["MFR_RELEASES_REGISTRY_URL", "MFR_RELEASE_CHANNEL", "MFR_SIGNING_ENVIRONMENT", "MFR_CARTRIDGE_ROOT_PUBKEYS", "MFR_CHARTER_ROOT_PUBKEYS"] {
        cargo.env_remove(name);
    }
    cargo.envs(inputs.iter().copied());
    cargo.output().expect("cargo starts")
}

fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("release-line")
}

/// What a release build is given: variable names and their values.
type Inputs = Vec<(&'static str, String)>;

fn line_inputs(line: &Line) -> Inputs {
    vec![
        ("MFR_RELEASES_REGISTRY_URL", REGISTRY.to_string()),
        ("MFR_RELEASE_CHANNEL", CHANNEL.to_string()),
        ("MFR_SIGNING_ENVIRONMENT", ENVIRONMENT.to_string()),
        ("MFR_CARTRIDGE_ROOT_PUBKEYS", line.root_keys().join(",")),
        // MachineFabric's roots, which the registry's charter chains to: here the test line's
        // own, as prod's and staging's content roots are MachineFabric's.
        ("MFR_CHARTER_ROOT_PUBKEYS", line.root_keys().join(",")),
    ]
}

/// Builds QuickRust with `inputs` and, if it built, copies the two programs out of the shared
/// target directory (which the next build overwrites) into a directory named for `name`; returns
/// the build's output and the copied `qrust`.
fn build_and_keep(inputs: &[(&str, &str)], name: &str) -> (Output, PathBuf) {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let kept = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{}-{}", name, std::process::id()));
    let out = build(inputs);
    if out.status.success() {
        let _ = fs::remove_dir_all(&kept);
        fs::create_dir_all(&kept).unwrap();
        for program in ["qrust", "qrust-rustc-wrapper"] {
            fs::copy(target_dir().join("debug").join(program), kept.join(program)).unwrap();
        }
    }
    (out, kept.join("qrust"))
}

/// A QuickRust built with the test line baked in.
fn released_quickrust() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let inputs = line_inputs(&Line::new());
        let inputs: Vec<(&str, &str)> = inputs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let (out, qrust) = build_and_keep(&inputs, "released");
        assert!(out.status.success(), "a release build with a whole release line builds:\n{}", stderr(&out));
        qrust
    })
}

// TEST0277: A build from source has no release line and says so
#[test]
fn test0277_a_build_from_source_has_no_release_line_and_says_so() {
    let sb = Sandbox::new("no-release-line");
    let home = sb.dir.join("user");
    fs::create_dir_all(&home).unwrap();
    ok(sb.qrust(&sb.dir).args(["setup", "--toolchain", "none", "--no-modify-path"]).env("HOME", &home));
    let doctor = run(sb.qrust(&sb.dir).arg("doctor").env("HOME", &home));
    assert!(stdout(&doctor).contains("release line       none: built from source"), "{}", stdout(&doctor));
    // Not even from a registry it is pointed at: it has no roots to check one against.
    let update = run(sb.qrust(&sb.dir).args(["self", "update"]).env("HOME", &home).env("QRUST_UPDATE_URL", "http://127.0.0.1:9"));
    assert_eq!(update.status.code(), Some(2));
    assert!(stderr(&update).contains("was built from source, so it has no release line to update from"), "{}", stderr(&update));
    let check = run(sb.qrust(&sb.dir).args(["self", "check"]).env("HOME", &home).env("QRUST_UPDATE_URL", "http://127.0.0.1:9"));
    assert_eq!(check.status.code(), Some(2));
    assert!(stderr(&check).contains("was built from source, so it has no release line to look for a newer release on"), "{}", stderr(&check));
}

/// A registry, its signing environment and its roots are what any build in a release environment
/// is surrounded by. Without a channel they publish nothing: the build is one from source.
#[test]
fn test0278_a_registry_without_a_channel_does_not_make_a_release_build() {
    let line = Line::new();
    let inputs = line_inputs(&line);
    let inputs: Vec<(&str, &str)> = inputs.iter().filter(|(k, _)| *k != "MFR_RELEASE_CHANNEL").map(|(k, v)| (*k, v.as_str())).collect();
    let (out, qrust) = build_and_keep(&inputs, "no-channel");
    assert!(out.status.success(), "a build given a registry and no channel builds:\n{}", stderr(&out));
    let sb = Sandbox::new("no-channel");
    let home = sb.dir.join("user");
    fs::create_dir_all(&home).unwrap();
    let doctor = run(sb.with_shim(qrust.to_str().unwrap(), &sb.dir).arg("doctor").env("HOME", &home));
    assert!(stdout(&doctor).contains("release line       none: built from source"), "{}", stdout(&doctor));
}

// TEST0279: A release build is given its release line whole or does not build
#[test]
fn test0279_a_release_build_is_given_its_release_line_whole_or_does_not_build() {
    let line = Line::new();
    let whole = line_inputs(&line);
    let refused = |change: &dyn Fn(&mut Inputs)| -> String {
        let mut inputs = whole.clone();
        change(&mut inputs);
        let inputs: Vec<(&str, &str)> = inputs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let out = build(&inputs);
        assert!(!out.status.success(), "this must not build: {:?}", inputs);
        stderr(&out)
    };
    let set = |inputs: &mut Inputs, name: &str, value: String| inputs.iter_mut().find(|(k, _)| *k == name).unwrap().1 = value;

    // Where to look for releases, and no way to trust them.
    let said = refused(&|i| i.retain(|(k, _)| *k != "MFR_CARTRIDGE_ROOT_PUBKEYS"));
    assert!(said.contains("(MFR_RELEASE_CHANNEL is set) is given MFR_RELEASES_REGISTRY_URL, MFR_SIGNING_ENVIRONMENT, MFR_CARTRIDGE_ROOT_PUBKEYS, MFR_CHARTER_ROOT_PUBKEYS with it; this one is not given MFR_CARTRIDGE_ROOT_PUBKEYS"), "{}", said);
    // A registry it could not tell was chartered.
    let said = refused(&|i| i.retain(|(k, _)| *k != "MFR_CHARTER_ROOT_PUBKEYS"));
    assert!(said.contains("this one is not given MFR_CHARTER_ROOT_PUBKEYS"), "{}", said);
    // Trust, and nowhere to look.
    let said = refused(&|i| i.retain(|(k, _)| matches!(*k, "MFR_RELEASE_CHANNEL" | "MFR_CARTRIDGE_ROOT_PUBKEYS" | "MFR_CHARTER_ROOT_PUBKEYS")));
    assert!(said.contains("this one is not given MFR_RELEASES_REGISTRY_URL, MFR_SIGNING_ENVIRONMENT"), "{}", said);

    let said = refused(&|i| set(i, "MFR_RELEASES_REGISTRY_URL", "http://releases.invalid".into()));
    assert!(said.contains("MFR_RELEASES_REGISTRY_URL must be an https URL with no trailing slash"), "{}", said);
    let said = refused(&|i| set(i, "MFR_RELEASE_CHANNEL", "stable".into()));
    assert!(said.contains("MFR_RELEASE_CHANNEL must be `release` or `nightly`"), "{}", said);
    let said = refused(&|i| set(i, "MFR_SIGNING_ENVIRONMENT", "dev".into()));
    assert!(said.contains("MFR_SIGNING_ENVIRONMENT must be `prod`, `staging` or `lab` for a release build"), "{}", said);
    let said = refused(&|i| set(i, "MFR_CHARTER_ROOT_PUBKEYS", line.root_keys()[..2].join(",")));
    assert!(said.contains("MFR_CHARTER_ROOT_PUBKEYS has 2 root(s); a certificate needs two of at least 3"), "{}", said);
    // Two of at least three roots sign a certificate: with two known, neither may be lost.
    let said = refused(&|i| set(i, "MFR_CARTRIDGE_ROOT_PUBKEYS", line.root_keys()[..2].join(",")));
    assert!(said.contains("has 2 root(s); a certificate needs two of at least 3"), "{}", said);
    let twice = [line.roots[0].public(), line.roots[1].public(), line.roots[0].public()].join(",");
    let said = refused(&|i| set(i, "MFR_CARTRIDGE_ROOT_PUBKEYS", twice.clone()));
    assert!(said.contains("names a root twice"), "{}", said);
    let mangled = format!("{},{},not-a-key", line.roots[0].public(), line.roots[1].public());
    let said = refused(&|i| set(i, "MFR_CARTRIDGE_ROOT_PUBKEYS", mangled.clone()));
    assert!(said.contains("is not a minisign public key"), "{}", said);
}

/// What the test registry serves, for the release line the test QuickRust was built with.
struct Published<'a> {
    server: &'a common::registry::Registry,
    line: &'a Line,
}

impl Published<'_> {
    /// An archive as a release has it: the directory `quickrust`, holding `qrust` (here a script
    /// that says `says`) and the rustc wrapper.
    fn archive(says: &str) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, body) in [("qrust", format!("#!/bin/sh\necho '{}'\n", says)), ("qrust-rustc-wrapper", "#!/bin/sh\n".to_string())] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o755);
            b.append_data(&mut h, format!("quickrust/{}", name), body.as_bytes()).unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&b.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    }

    /// Charters the registry at its anchor for this build's environment, signed as MachineFabric
    /// signs charters.
    fn charter(&self) {
        let charter = self.line.charter(&self.server.url, "releases", ENVIRONMENT);
        let certificate = Line::certificate(&self.line.release, "prod", now() - 3600, now() + 86_400);
        self.server.put("/charter.json.sig", Line::charter_envelope(&charter, &certificate, &[&self.line.roots[0], &self.line.roots[1]], &self.line.release));
        self.server.put("/charter.json", charter);
    }

    /// Puts `version` on `channel`: its archive, and a manifest that describes it (`sha256` and
    /// `size` of the archive unless given otherwise). Returns the manifest; nothing signs it yet.
    fn release(&self, channel: &str, version: &str, says: &str, sha256: Option<&str>, size: Option<u64>) -> Vec<u8> {
        let name = format!("qrust-{}-{}.tar.gz", version, platform());
        let archive = Published::archive(says);
        let path = format!("/quickrust/{}/{}/{}", channel, version, name);
        let digest = sha256_hex(&archive);
        let manifest = manifest(channel, version, platform(), &name, &format!("{}{}", self.server.url, path), sha256.unwrap_or(&digest), size.unwrap_or(archive.len() as u64));
        self.server.put(&path, archive);
        self.server.put("/quickrust/manifest", manifest.clone());
        manifest
    }

    /// Signs `manifest` as a publish does: the release key, certified for this build's
    /// environment by two of its roots, valid now.
    fn sign(&self, manifest: &[u8]) {
        let certificate = Line::certificate(&self.line.release, ENVIRONMENT, now() - 3600, now() + 86_400);
        self.put_signature(Line::envelope(manifest, &certificate, &[&self.line.roots[0], &self.line.roots[2]], &self.line.release));
    }

    fn put_signature(&self, envelope: Vec<u8>) {
        self.server.put("/quickrust/manifest.sig", envelope);
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{:02x}", b)).collect()
}

// TEST0280: A released quickrust updates itself only to what its registry signed
#[test]
fn test0280_a_released_quickrust_updates_itself_only_to_what_its_registry_signed() {
    let quickrust = released_quickrust();
    let sb = Sandbox::new("release-line");
    let home = sb.dir.join("user");
    fs::create_dir_all(&home).unwrap();
    let released = |args: &[&str]| -> Command {
        let mut c = sb.with_shim(quickrust.to_str().unwrap(), &sb.dir);
        c.args(args).env("HOME", &home);
        c
    };
    ok(&mut released(&["setup", "--toolchain", "none", "--no-modify-path"]));
    let installed = sb.home.join("bin/qrust");
    let doctor = stdout(&run(&mut released(&["doctor"])));
    assert!(doctor.contains(&format!("release line       the {} channel of {} (3 signing roots, {}; 3 charter roots)", CHANNEL, REGISTRY, ENVIRONMENT)), "{}", doctor);

    let server = common::registry::Registry::start();
    let line = Line::new();
    let published = Published { server: &server, line: &line };
    // The registry it was built for does not exist; it is pointed at another, which it reads only
    // once MachineFabric's roots charter it, and trusts exactly as far as its roots go.
    let update = || run(released(&["self", "update"]).env("QRUST_UPDATE_URL", &server.url));
    let refused = |why: &str| {
        let out = update();
        assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
        assert!(stderr(&out).contains(why), "expected {:?} in:\n{}", why, stderr(&out));
    };
    let current = env!("CARGO_PKG_VERSION");
    let before = fs::read(&installed).unwrap();

    refused(&format!("{} has no charter ({}/charter.json is missing), so it is not a release registry MachineFabric chartered", server.url, server.url));
    published.charter();
    refused(&format!("no QuickRust release is published at {} yet", server.url));

    // A manifest nobody signed says nothing, whatever it offers.
    let unsigned = published.release(CHANNEL, "999.0.0", "qrust 999.0.0", None, None);
    refused("is published without a signature");

    // Signed, and then replaced: the signature is of the manifest it was made for.
    published.sign(&unsigned);
    published.release(CHANNEL, "999.0.1", "qrust 999.0.1", None, None);
    refused("the signature does not verify over the document's bytes");

    // Signed by a release key that roots this build has never heard of certified.
    let strangers = Line { roots: [Key::new(21), Key::new(22), Key::new(23)], release: Key::new(29) };
    let offered = published.release(CHANNEL, "999.0.0", "qrust 999.0.0", None, None);
    let certificate = Line::certificate(&strangers.release, ENVIRONMENT, now() - 3600, now() + 86_400);
    published.put_signature(Line::envelope(&offered, &certificate, &[&strangers.roots[0], &strangers.roots[1]], &strangers.release));
    refused("is signed by 0 trusted root(s); 2 are required");

    // Certified by this build's roots, for the other environment.
    let certificate = Line::certificate(&line.release, "prod", now() - 3600, now() + 86_400);
    published.put_signature(Line::envelope(&offered, &certificate, &[&line.roots[0], &line.roots[1]], &line.release));
    refused("is for 'prod', and this client trusts 'staging'");

    // A certificate that has run out.
    let certificate = Line::certificate(&line.release, ENVIRONMENT, now() - 7200, now() - 3600);
    published.put_signature(Line::envelope(&offered, &certificate, &[&line.roots[0], &line.roots[1]], &line.release));
    refused("expired at");

    // From here on the manifest is trusted, and what it describes is held to it.
    let other = "0".repeat(64);
    let wrong_digest = published.release(CHANNEL, "999.0.0", "qrust 999.0.0", Some(&other), None);
    published.sign(&wrong_digest);
    refused("is not the published file");
    let wrong_size = published.release(CHANNEL, "999.0.0", "qrust 999.0.0", None, Some(7));
    published.sign(&wrong_size);
    refused("bytes; the release manifest says 7");
    let impostor = published.release(CHANNEL, "999.0.0", "qrust 998.0.0", None, None);
    published.sign(&impostor);
    refused("says \"qrust 998.0.0\", not qrust 999.0.0");
    assert_eq!(fs::read(&installed).unwrap(), before, "nothing that was refused replaced anything");

    // Its own version is the latest: nothing to do.
    let same = published.release(CHANNEL, current, "qrust old", None, None);
    published.sign(&same);
    let out = update();
    assert!(out.status.success() && stderr(&out).contains(&format!("QuickRust {} is the latest", current)), "{}", stderr(&out));

    // A genuine, signed manifest of an older release (a registry rolled back, or one that serves
    // an earlier manifest): QuickRust is never replaced by something older than it is.
    let older = published.release(CHANNEL, "0.0.1", "qrust 0.0.1", None, None);
    published.sign(&older);
    let out = update();
    assert!(out.status.success() && stderr(&out).contains(&format!("QuickRust {} is the latest", current)), "{}", stderr(&out));
    assert_eq!(fs::read(&installed).unwrap(), before);

    // A newer release on the OTHER channel is not this QuickRust's: its own channel has nothing.
    let elsewhere = published.release("release", "999.0.0", "qrust 999.0.0", None, None);
    published.sign(&elsewhere);
    refused(&format!("no QuickRust release is published on the {} channel at {} yet", CHANNEL, server.url));
    assert_eq!(fs::read(&installed).unwrap(), before);

    // A newer release on its channel: installed, and `cargo` is it.
    let newer = published.release(CHANNEL, "999.0.0", "qrust 999.0.0", None, None);
    published.sign(&newer);
    let out = update();
    assert!(out.status.success() && stderr(&out).contains(&format!("QuickRust 999.0.0 installed (was {})", current)), "{}", stderr(&out));
    assert_eq!(stdout(&ok(Command::new(&installed).arg("--version"))), "qrust 999.0.0\n");
    assert_eq!(stdout(&ok(Command::new(sb.home.join("bin/cargo")).arg("--version"))), "qrust 999.0.0\n");
}

/// The variables any of which makes a run count as CI, where QuickRust never looks for a release.
const CI_VARIABLES: [&str; 7] = ["CI", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE", "JENKINS_URL", "TF_BUILD", "TEAMCITY_VERSION"];

// TEST0326: A released quickrust finds a newer signed release after a build and names the command
#[test]
fn test0326_a_released_quickrust_finds_a_newer_signed_release_after_a_build_and_names_the_command() {
    let quickrust = released_quickrust();
    let server = common::registry::Registry::start();
    let line = Line::new();
    let published = Published { server: &server, line: &line };
    published.charter();
    let newer = published.release(CHANNEL, "999.0.0", "qrust 999.0.0", None, None);
    published.sign(&newer);
    let current = env!("CARGO_PKG_VERSION");
    let released = |sb: &Sandbox, args: &[&str]| -> Command {
        let home = sb.dir.join("user");
        fs::create_dir_all(&home).unwrap();
        let mut c = sb.with_shim(quickrust.to_str().unwrap(), &sb.dir);
        c.args(args).env("HOME", &home).env("QRUST_UPDATE_URL", &server.url);
        for name in CI_VARIABLES {
            c.env_remove(name);
        }
        c
    };
    let state = |sb: &Sandbox| fs::read_to_string(sb.home.join("self-update.json")).ok().map(|s| serde_json::from_str::<serde_json::Value>(&s).unwrap());

    // Asked: the newer release, and what installs it.
    let sb = Sandbox::new("self-check");
    let checked = ok(&mut released(&sb, &["self", "check"]));
    assert!(stderr(&checked).contains(&format!("QuickRust 999.0.0 is available (this is {}): run `qrust self update` to update", current)), "{}", stderr(&checked));
    let doctor = stdout(&run(&mut released(&sb, &["doctor"])));
    assert!(doctor.contains(&format!("newest release     999.0.0 on {}, checked", server.url)), "{}", doctor);

    // A registry that cannot be reached: `qrust self check` says why, and doctor says the check
    // did not finish.
    let unreachable = run(released(&sb, &["self", "check"]).env("QRUST_UPDATE_URL", "http://127.0.0.1:9"));
    assert_eq!(unreachable.status.code(), Some(2), "{}", stderr(&unreachable));
    let doctor = stdout(&run(released(&sb, &["doctor"]).env("QRUST_UPDATE_URL", "http://127.0.0.1:9")));
    assert!(doctor.contains("newest release     the check") && doctor.contains("did not finish"), "{}", doctor);

    // Not asked: a build starts the check in the background, and returns without waiting for it.
    let sb = Sandbox::new("self-check-build");
    sb.package("p", "p");
    let mut build = released(&sb, &["build"]);
    build.current_dir(sb.dir.join("p"));
    ok(&mut build);
    let started = state(&sb).expect("the build marked a check started before it returned");
    assert!(started["checked_ns"].as_u64().unwrap() > 0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while state(&sb).and_then(|s| s["latest"].as_str().map(str::to_string)).as_deref() != Some("999.0.0") {
        assert!(std::time::Instant::now() < deadline, "the background check recorded {:?}; its log:\n{}", state(&sb), fs::read_to_string(sb.home.join("self-update.log")).unwrap_or_default());
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // Turned off, or in CI: a build starts no check.
    for (name, value) in [("QRUST_SELF_UPDATE_CHECK", "off"), ("CI", "true"), ("TF_BUILD", "True")] {
        let sb = Sandbox::new("self-check-off");
        sb.package("p", "p");
        let mut build = released(&sb, &["build"]);
        build.current_dir(sb.dir.join("p")).env(name, value);
        ok(&mut build);
        assert_eq!(state(&sb), None, "{}={}", name, value);
    }
}
