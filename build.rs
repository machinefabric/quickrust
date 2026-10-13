//! Bakes where this QuickRust's releases are published and how they are trusted.
//!
//! A released QuickRust knows four things about its own release line, all given by the release
//! build and none read at run time:
//!
//! - `MFR_RELEASES_BASE_URL`: the release registry its manifest is published in;
//! - `MFR_RELEASE_CHANNEL`: the channel it was published on (`release` or `nightly`);
//! - `MFR_SIGNING_ENVIRONMENT`: the environment the registry's release key is certified for
//!   (`prod` or `staging`);
//! - `MFR_CARTRIDGE_ROOT_PUBKEYS`: the root public keys a release-key certificate must be signed
//!   by, comma-separated (the workspace's one root set; at least three, of which two must sign).
//!
//! They come together or not at all. A build given none of them is a build from source: it has no
//! release line, and `qrust self update` says so. A build given some of them stops here, because a
//! QuickRust that knew where to look for releases and not how to trust them would have to either
//! refuse every update or install what it cannot check.

use std::env;

const INPUTS: [&str; 4] = ["MFR_RELEASES_BASE_URL", "MFR_RELEASE_CHANNEL", "MFR_SIGNING_ENVIRONMENT", "MFR_CARTRIDGE_ROOT_PUBKEYS"];
/// The roots a certificate is checked against: two of them must have signed it, so fewer than
/// three leaves no root that can be lost or withheld.
const MIN_ROOTS: usize = 3;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let mut given = Vec::new();
    for name in INPUTS {
        println!("cargo:rerun-if-env-changed={}", name);
        let value = env::var(name).unwrap_or_default().trim().to_string();
        given.push((name, value));
    }
    let set: Vec<&str> = given.iter().filter(|(_, v)| !v.is_empty()).map(|(n, _)| *n).collect();
    if set.is_empty() {
        return;
    }
    if set.len() != INPUTS.len() {
        let missing: Vec<&str> = given.iter().filter(|(_, v)| v.is_empty()).map(|(n, _)| *n).collect();
        panic!("a release build of QuickRust is given {} together; this one has {} and not {}", INPUTS.join(", "), set.join(", "), missing.join(", "));
    }
    let value = |name: &str| given.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone()).unwrap();

    let url = value("MFR_RELEASES_BASE_URL");
    if !url.starts_with("https://") || url.ends_with('/') {
        panic!("MFR_RELEASES_BASE_URL must be an https URL with no trailing slash, got {:?}", url);
    }
    let channel = value("MFR_RELEASE_CHANNEL");
    if !matches!(channel.as_str(), "release" | "nightly") {
        panic!("MFR_RELEASE_CHANNEL must be `release` or `nightly`, got {:?}", channel);
    }
    let environment = value("MFR_SIGNING_ENVIRONMENT");
    if !matches!(environment.as_str(), "prod" | "staging") {
        panic!("MFR_SIGNING_ENVIRONMENT must be `prod` or `staging` for a release build, got {:?}", environment);
    }
    let roots: Vec<String> = value("MFR_CARTRIDGE_ROOT_PUBKEYS").split(',').map(|k| k.trim().to_string()).collect();
    if roots.iter().any(String::is_empty) {
        panic!("MFR_CARTRIDGE_ROOT_PUBKEYS has an empty entry: {:?}", roots);
    }
    let mut distinct = roots.clone();
    distinct.sort();
    distinct.dedup();
    if distinct.len() != roots.len() {
        panic!("MFR_CARTRIDGE_ROOT_PUBKEYS names a root twice: {:?}", roots);
    }
    if roots.len() < MIN_ROOTS {
        panic!("MFR_CARTRIDGE_ROOT_PUBKEYS has {} root(s); a certificate needs two of at least {}", roots.len(), MIN_ROOTS);
    }
    for root in &roots {
        if let Err(why) = minisign_public_key(root) {
            panic!("MFR_CARTRIDGE_ROOT_PUBKEYS entry {:?} is not a minisign public key: {}", root, why);
        }
    }

    println!("cargo:rustc-env=QRUST_BAKED_RELEASES_URL={}", url);
    println!("cargo:rustc-env=QRUST_BAKED_CHANNEL={}", channel);
    println!("cargo:rustc-env=QRUST_BAKED_SIGNING_ENVIRONMENT={}", environment);
    println!("cargo:rustc-env=QRUST_BAKED_ROOT_PUBKEYS={}", roots.join(","));
}

/// A minisign public key is 42 bytes in base64: `Ed`, an 8-byte key id, a 32-byte ed25519 key.
/// Checked here so that a mistyped root stops the build, not every update of what was built.
fn minisign_public_key(text: &str) -> Result<(), String> {
    let bytes = base64(text).ok_or("not base64")?;
    if bytes.len() != 42 {
        return Err(format!("{} bytes, not 42", bytes.len()));
    }
    if &bytes[..2] != b"Ed" {
        return Err("it does not start with the ed25519 tag `Ed`".into());
    }
    Ok(())
}

fn base64(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = ALPHABET.iter().position(|a| *a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}
