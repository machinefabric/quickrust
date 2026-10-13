//! QuickRust's releases: where they are published, and what makes one trusted.
//!
//! QuickRust is published in a release registry, as one manifest per product at
//! `<registry>/quickrust/manifest`, with a signature beside it at `<registry>/quickrust/manifest.sig`.
//! The manifest lists, per channel (`release`, `nightly`), every version with the archive of each
//! platform, its SHA-256 and its size.
//!
//! Nothing in a manifest is believed until its signature chains to the root keys this QuickRust
//! was built with:
//!
//! 1. the signature file carries one or more **release-key certificates**. A certificate is
//!    trusted when two distinct roots of this build signed its exact bytes, it is for this build's
//!    signing environment, and the clock is inside its validity;
//! 2. the **manifest signature** must verify, under the release key of one of those certificates,
//!    over the exact bytes that were fetched.
//!
//! Every certificate in the file must be valid: one that is not means a publishing mistake or
//! tampering, and reading past it would hide either. Only then is the manifest parsed, and an
//! archive taken from it is still held to the SHA-256 and size the manifest gives.
//!
//! A QuickRust built from source has no roots, and so no release it could trust: it has no
//! release line ([`identity`] is `None`).

use ed25519_dalek::Verifier;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt;

/// The product's name in the release registry.
pub const APP_ID: &str = "quickrust";
const CERTIFICATE_FORMAT: &str = "machinefabric-release-key-cert/1";
const ENVELOPE_FORMAT: &str = "machinefabric-manifest-sig/1";
const MANIFEST_SCHEMA_MAJOR: &str = "5";
/// Distinct roots that must have signed a certificate.
const REQUIRED_ROOT_SIGNATURES: usize = 2;

/// What a released QuickRust knows about its own release line, baked in by its build (`build.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The release registry this QuickRust was published in.
    pub registry: String,
    /// The channel it was published on: `release` or `nightly`.
    pub channel: String,
    pub trust: Trust,
}

/// The roots a release-key certificate is checked against, and the environment it must be for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    /// Root public keys, in minisign's base64 form.
    pub roots: Vec<String>,
    /// `prod` or `staging`.
    pub environment: String,
}

/// This build's release line, or `None` for a build from source.
pub fn identity() -> Option<Identity> {
    let registry = option_env!("QRUST_BAKED_RELEASES_URL")?;
    // `build.rs` bakes all four or none.
    let channel = option_env!("QRUST_BAKED_CHANNEL").expect("baked with the registry");
    let environment = option_env!("QRUST_BAKED_SIGNING_ENVIRONMENT").expect("baked with the registry");
    let roots = option_env!("QRUST_BAKED_ROOT_PUBKEYS").expect("baked with the registry");
    Some(Identity {
        registry: registry.to_string(),
        channel: channel.to_string(),
        trust: Trust { roots: roots.split(',').map(str::to_string).collect(), environment: environment.to_string() },
    })
}

/// The registry's name for the platform this QuickRust runs on, or `None` where it is not
/// published.
pub fn platform() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => return None,
    })
}

/// Why a manifest's signature does not chain to this build's roots.
#[derive(Debug, PartialEq, Eq)]
pub enum ChainError {
    MalformedEnvelope(String),
    UnsupportedEnvelopeFormat(String),
    NoCertificates,
    MalformedCertificate(String),
    UnsupportedCertificateFormat(String),
    MalformedKey(String),
    KeyIdMismatch { stated: String, actual: String },
    TooFewRootSignatures { key_id: String, have: usize },
    WrongEnvironment { key_id: String, certificate: String, build: String },
    Expired { key_id: String, not_after: u64, now: u64 },
    NotYetValid { key_id: String, issued_at: u64, now: u64 },
    NoCertificateForSigner { key_id: String },
    ManifestSignatureInvalid(String),
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChainError::MalformedEnvelope(e) => write!(f, "the signature file is not a manifest signature: {}", e),
            ChainError::UnsupportedEnvelopeFormat(found) => write!(f, "the signature file is of format {:?}, not {:?}", found, ENVELOPE_FORMAT),
            ChainError::NoCertificates => write!(f, "the signature file carries no release-key certificate"),
            ChainError::MalformedCertificate(e) => write!(f, "a release-key certificate cannot be read: {}", e),
            ChainError::UnsupportedCertificateFormat(found) => write!(f, "a release-key certificate is of format {:?}, not {:?}", found, CERTIFICATE_FORMAT),
            ChainError::MalformedKey(e) => write!(f, "a key or signature cannot be read: {}", e),
            ChainError::KeyIdMismatch { stated, actual } => write!(f, "a release-key certificate says its key is {} and carries key {}", stated, actual),
            ChainError::TooFewRootSignatures { key_id, have } => write!(f, "the certificate of release key {} is signed by {} of this QuickRust's roots; {} must sign", key_id, have, REQUIRED_ROOT_SIGNATURES),
            ChainError::WrongEnvironment { key_id, certificate, build } => write!(f, "the certificate of release key {} is for the {} environment; this QuickRust was built for {}", key_id, certificate, build),
            ChainError::Expired { key_id, not_after, now } => write!(f, "the certificate of release key {} expired at {} (it is {})", key_id, not_after, now),
            ChainError::NotYetValid { key_id, issued_at, now } => write!(f, "the certificate of release key {} is valid from {} (it is {})", key_id, issued_at, now),
            ChainError::NoCertificateForSigner { key_id } => write!(f, "the manifest is signed by key {}, which no certificate in the signature file authorizes", key_id),
            ChainError::ManifestSignatureInvalid(e) => write!(f, "the manifest's signature does not match the manifest: {}", e),
        }
    }
}

#[derive(Deserialize)]
struct Envelope {
    format: String,
    certificates: Vec<CertificateEntry>,
    manifest_signature: ManifestSignature,
}

#[derive(Deserialize)]
struct CertificateEntry {
    /// The certificate as the roots signed it: these exact bytes, never re-serialized.
    certificate: String,
    root_signatures: Vec<RootSignature>,
}

#[derive(Deserialize)]
struct RootSignature {
    signature: String,
}

#[derive(Deserialize)]
struct ManifestSignature {
    key_id: String,
    signature: String,
}

#[derive(Deserialize)]
struct Certificate {
    format: String,
    release_pubkey: String,
    key_id: String,
    environment: String,
    issued_at: u64,
    not_after: u64,
}

/// Checks that `envelope` (the bytes of `manifest.sig`) authorizes `manifest` (the exact bytes
/// of the manifest it was fetched beside) under `trust`, at the time `now` (seconds since 1970).
pub fn verify_manifest(manifest: &[u8], envelope: &[u8], trust: &Trust, now: u64) -> Result<(), ChainError> {
    let envelope: Envelope = serde_json::from_slice(envelope).map_err(|e| ChainError::MalformedEnvelope(e.to_string()))?;
    if envelope.format != ENVELOPE_FORMAT {
        return Err(ChainError::UnsupportedEnvelopeFormat(envelope.format));
    }
    if envelope.certificates.is_empty() {
        return Err(ChainError::NoCertificates);
    }
    let mut release_keys: Vec<(String, String)> = Vec::new();
    for entry in &envelope.certificates {
        let certificate = verify_certificate(entry, trust, now)?;
        release_keys.push((certificate.key_id, certificate.release_pubkey));
    }
    let signer = &envelope.manifest_signature.key_id;
    let Some((_, release_key)) = release_keys.iter().find(|(id, _)| id == signer) else {
        return Err(ChainError::NoCertificateForSigner { key_id: signer.clone() });
    };
    verify_signature(release_key, &envelope.manifest_signature.signature, manifest).map_err(|e| match e {
        SignatureError::Malformed(why) => ChainError::MalformedKey(why),
        SignatureError::Mismatch(why) => ChainError::ManifestSignatureInvalid(why),
    })
}

fn verify_certificate(entry: &CertificateEntry, trust: &Trust, now: u64) -> Result<Certificate, ChainError> {
    let certificate: Certificate = serde_json::from_str(&entry.certificate).map_err(|e| ChainError::MalformedCertificate(e.to_string()))?;
    if certificate.format != CERTIFICATE_FORMAT {
        return Err(ChainError::UnsupportedCertificateFormat(certificate.format));
    }
    // The stated key id must be the key's own: a mismatch is a certificate edited by hand.
    let key = PublicKey::parse(&certificate.release_pubkey).map_err(ChainError::MalformedKey)?;
    if key.id != certificate.key_id {
        return Err(ChainError::KeyIdMismatch { stated: certificate.key_id, actual: key.id });
    }
    // Distinct ROOTS, not distinct signatures: a signature by a key that is not a root counts for
    // nothing, and two by one root count once.
    let signers: BTreeSet<usize> = trust
        .roots
        .iter()
        .enumerate()
        .filter(|(_, root)| entry.root_signatures.iter().any(|s| verify_signature(root, &s.signature, entry.certificate.as_bytes()).is_ok()))
        .map(|(index, _)| index)
        .collect();
    if signers.len() < REQUIRED_ROOT_SIGNATURES {
        return Err(ChainError::TooFewRootSignatures { key_id: certificate.key_id, have: signers.len() });
    }
    if certificate.environment != trust.environment {
        return Err(ChainError::WrongEnvironment { key_id: certificate.key_id, certificate: certificate.environment, build: trust.environment.clone() });
    }
    if now > certificate.not_after {
        return Err(ChainError::Expired { key_id: certificate.key_id, not_after: certificate.not_after, now });
    }
    if certificate.issued_at > now {
        return Err(ChainError::NotYetValid { key_id: certificate.key_id, issued_at: certificate.issued_at, now });
    }
    Ok(certificate)
}

enum SignatureError {
    Malformed(String),
    Mismatch(String),
}

/// A raw ed25519 signature (base64) by a minisign public key (base64), over exact bytes.
fn verify_signature(public_key: &str, signature: &str, bytes: &[u8]) -> Result<(), SignatureError> {
    let key = PublicKey::parse(public_key).map_err(SignatureError::Malformed)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key.ed25519).map_err(|e| SignatureError::Malformed(e.to_string()))?;
    let signature = base64_decode(signature.trim()).ok_or_else(|| SignatureError::Malformed("a signature is not base64".to_string()))?;
    let signature: [u8; 64] = signature.try_into().map_err(|s: Vec<u8>| SignatureError::Malformed(format!("a signature is {} bytes, not 64", s.len())))?;
    key.verify(bytes, &ed25519_dalek::Signature::from_bytes(&signature)).map_err(|e| SignatureError::Mismatch(e.to_string()))
}

/// A minisign public key: `Ed`, an 8-byte key id, a 32-byte ed25519 key, in base64.
struct PublicKey {
    /// The key id, lower-case hexadecimal.
    id: String,
    ed25519: [u8; 32],
}

impl PublicKey {
    fn parse(text: &str) -> Result<PublicKey, String> {
        let bytes = base64_decode(text.trim()).ok_or("a public key is not base64")?;
        if bytes.len() != 42 {
            return Err(format!("a public key is {} bytes, not 42", bytes.len()));
        }
        if &bytes[..2] != b"Ed" {
            return Err("a public key does not start with the ed25519 tag `Ed`".to_string());
        }
        let mut ed25519 = [0u8; 32];
        ed25519.copy_from_slice(&bytes[10..]);
        Ok(PublicKey { id: bytes[2..10].iter().map(|b| format!("{:02x}", b)).collect(), ed25519 })
    }
}

/// Standard base64 with padding, as minisign and the signature files write it. Anything else
/// (a character outside the alphabet, a length that is not a multiple of four, data after the
/// padding, bits left over) is `None`.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return None;
    }
    let padding = bytes.iter().rev().take_while(|b| **b == b'=').count();
    if padding > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for byte in &bytes[..bytes.len() - padding] {
        let value = ALPHABET.iter().position(|a| a == byte)? as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // What is left are the bits the padding stands for, and they are zero in a canonical encoding.
    (acc == 0).then_some(out)
}

/// One published archive of QuickRust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The archive's file name, `qrust-<version>-<platform>.tar.gz`.
    pub name: String,
    pub url: String,
    /// Lower-case hexadecimal.
    pub sha256: String,
    pub size: u64,
}

impl Release {
    /// The directory the archive unpacks to: its name without `.tar.gz`.
    pub fn root(&self) -> &str {
        self.name.strip_suffix(".tar.gz").expect("checked when the release was read")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: String,
    app_id: String,
    channels: std::collections::BTreeMap<String, Channel>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Channel {
    latest_version: Option<String>,
    #[serde(default)]
    versions: std::collections::BTreeMap<String, Version>,
}

#[derive(Deserialize)]
struct Version {
    platforms: std::collections::BTreeMap<String, Platform>,
}

#[derive(Deserialize)]
struct Platform {
    installers: Vec<Installer>,
}

#[derive(Deserialize)]
struct Installer {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    path: String,
    sha256: String,
    size: u64,
}

/// The latest release of `channel` for `platform` in a manifest already verified, or `None` when
/// the channel has published nothing yet. A manifest that is not QuickRust's, or whose latest
/// version has no archive for the platform, is an error: the first is the wrong document, and the
/// second is a release this platform was left out of.
pub fn latest(manifest: &[u8], channel: &str, platform: &str) -> Result<Option<Release>, String> {
    let manifest: Manifest = serde_json::from_slice(manifest).map_err(|e| format!("the release manifest cannot be read: {}", e))?;
    if manifest.schema_version.split('.').next() != Some(MANIFEST_SCHEMA_MAJOR) {
        return Err(format!("the release manifest is of schema {}; this QuickRust reads schema {}", manifest.schema_version, MANIFEST_SCHEMA_MAJOR));
    }
    if manifest.app_id != APP_ID {
        return Err(format!("the release manifest is {}'s, not {}'s", manifest.app_id, APP_ID));
    }
    let Some(channel_releases) = manifest.channels.get(channel) else {
        return Err(format!("the release manifest has no {} channel", channel));
    };
    let Some(version) = channel_releases.latest_version.as_deref().filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let Some(release) = channel_releases.versions.get(version) else {
        return Err(format!("the release manifest names {} as the latest {} version and does not list it", version, channel));
    };
    let Some(for_platform) = release.platforms.get(platform) else {
        let mut published: Vec<&str> = release.platforms.keys().map(String::as_str).collect();
        published.sort_unstable();
        return Err(format!("QuickRust {} is not published for {} (it is for {})", version, platform, published.join(", ")));
    };
    let Some(archive) = for_platform.installers.iter().find(|i| i.kind == "targz") else {
        return Err(format!("QuickRust {} for {} is published without a tar.gz archive", version, platform));
    };
    if archive.name.strip_suffix(".tar.gz").map_or(true, |root| root.is_empty() || root.contains(['/', '\\'])) {
        return Err(format!("QuickRust {} for {} names its archive {:?}, which is not a tar.gz file name", version, platform, archive.name));
    }
    if archive.sha256.len() != 64 || !archive.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("QuickRust {} for {} carries {:?} as its SHA-256", version, platform, archive.sha256));
    }
    Ok(Some(Release { version: version.to_string(), name: archive.name.clone(), url: archive.path.clone(), sha256: archive.sha256.to_ascii_lowercase(), size: archive.size }))
}

/// Fetches QuickRust's manifest and its signature from `registry`, verifies the one against the
/// other under `trust`, and returns the latest release of `channel` for `platform`.
pub fn fetch_latest(client: &crate::net::Client, registry: &str, trust: &Trust, channel: &str, platform: &str, now: u64) -> Result<Option<Release>, String> {
    let url = format!("{}/{}/manifest", registry.trim_end_matches('/'), APP_ID);
    // Never from a cache: the manifest and its signature are replaced together, and a stale one
    // of either beside a fresh one of the other reads as a forgery.
    let fresh = [("Cache-Control", "no-cache")];
    let manifest = match client.get_bytes(&url, &fresh) {
        Ok(bytes) => bytes,
        Err(crate::net::Error::Status { status: 404, .. }) => return Err(format!("no QuickRust release is published at {} yet", registry)),
        Err(e) => return Err(e.to_string()),
    };
    let signature_url = format!("{}.sig", url);
    let signature = match client.get_bytes(&signature_url, &fresh) {
        Ok(bytes) => bytes,
        Err(crate::net::Error::Status { status: 404, .. }) => return Err(format!("{} is published without a signature ({} is missing), so nothing in it can be trusted", url, signature_url)),
        Err(e) => return Err(e.to_string()),
    };
    verify_manifest(&manifest, &signature, trust, now).map_err(|e| format!("{} is not a release manifest this QuickRust trusts: {}", url, e))?;
    latest(&manifest, channel, platform)
}

#[cfg(test)]
pub(crate) mod testing {
    //! A release line a test signs itself: three roots, a release key they certify, and manifests
    //! signed with it. The signing is here, behind `cfg(test)`, because QuickRust verifies and
    //! never signs.
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    pub const ISSUED_AT: u64 = 1_700_000_000;
    pub const NOT_AFTER: u64 = ISSUED_AT + 365 * 86_400;
    pub const NOW: u64 = ISSUED_AT + 86_400;

    pub struct Key(SigningKey, [u8; 8]);

    impl Key {
        /// A key made from `seed`, with `seed` repeated as its minisign key id.
        pub fn new(seed: u8) -> Key {
            Key(SigningKey::from_bytes(&[seed; 32]), [seed; 8])
        }
        pub fn id(&self) -> String {
            self.1.iter().map(|b| format!("{:02x}", b)).collect()
        }
        /// The public key in minisign's base64 form.
        pub fn public(&self) -> String {
            let mut bytes = b"Ed".to_vec();
            bytes.extend_from_slice(&self.1);
            bytes.extend_from_slice(self.0.verifying_key().as_bytes());
            base64(&bytes)
        }
        pub fn sign(&self, bytes: &[u8]) -> String {
            base64(&self.0.sign(bytes).to_bytes())
        }
    }

    pub fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
            for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> shift) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// A release line: roots A, B and C, and a release key.
    pub struct Line {
        pub roots: [Key; 3],
        pub release: Key,
    }

    impl Line {
        pub fn new() -> Line {
            Line { roots: [Key::new(1), Key::new(2), Key::new(3)], release: Key::new(9) }
        }
        pub fn trust(&self, environment: &str) -> super::Trust {
            super::Trust { roots: self.roots.iter().map(Key::public).collect(), environment: environment.to_string() }
        }
        /// The certificate of `key` for `environment`, as the roots sign it.
        pub fn certificate(key: &Key, environment: &str, issued_at: u64, not_after: u64) -> String {
            json!({ "format": super::CERTIFICATE_FORMAT, "release_pubkey": key.public(), "key_id": key.id(), "environment": environment, "issued_at": issued_at, "not_after": not_after }).to_string()
        }
        /// The signature file of `manifest`: `certificate` signed by `signers`, the manifest
        /// signed by `by`.
        pub fn envelope(manifest: &[u8], certificate: &str, signers: &[&Key], by: &Key) -> Vec<u8> {
            let root_signatures: Vec<_> = signers.iter().map(|root| json!({ "root_key_id": root.id(), "signature": root.sign(certificate.as_bytes()) })).collect();
            json!({
                "format": super::ENVELOPE_FORMAT,
                "certificates": [{ "certificate": certificate, "root_signatures": root_signatures }],
                "manifest_signature": { "key_id": by.id(), "signature": by.sign(manifest) },
            })
            .to_string()
            .into_bytes()
        }
        /// `manifest` signed as a publish does it: the release key certified for `prod` by roots
        /// A and B.
        pub fn sign(&self, manifest: &[u8]) -> Vec<u8> {
            let certificate = Line::certificate(&self.release, "prod", ISSUED_AT, NOT_AFTER);
            Line::envelope(manifest, &certificate, &[&self.roots[0], &self.roots[1]], &self.release)
        }
    }

    /// A manifest with one version on `channel` for `platform`.
    pub fn manifest(channel: &str, version: &str, platform: &str, name: &str, url: &str, sha256: &str, size: u64) -> Vec<u8> {
        let other = if channel == "release" { "nightly" } else { "release" };
        json!({
            "schemaVersion": "5.0",
            "appId": super::APP_ID,
            "name": "QuickRust",
            "lastUpdated": "2026-10-13T00:00:00Z",
            "channels": {
                channel: { "latestVersion": version, "versions": { version: { "publishedAt": "2026-10-13T00:00:00Z", "platforms": { platform: { "label": platform, "installers": [
                    { "type": "targz", "label": "tar.gz", "name": name, "path": url, "sha256": sha256, "size": size }
                ] } } } } },
                other: { "latestVersion": "", "versions": {} },
            },
        })
        .to_string()
        .into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    const SHA: &str = "b74a72297e9404fc35470af7fc77f157e2899577d2907bcdffe90397f89c14a5";

    fn a_manifest() -> Vec<u8> {
        manifest("release", "0.150.0", "linux-x86_64", "qrust-0.150.0-linux-x86_64.tar.gz", "https://r.example/quickrust/release/0.150.0/qrust-0.150.0-linux-x86_64.tar.gz", SHA, 4096)
    }

    #[test]
    fn a_manifest_signed_by_a_certified_release_key_is_trusted() {
        let line = Line::new();
        let manifest = a_manifest();
        assert_eq!(verify_manifest(&manifest, &line.sign(&manifest), &line.trust("prod"), NOW), Ok(()));
    }

    #[test]
    fn one_changed_byte_of_the_manifest_is_refused() {
        let line = Line::new();
        let manifest = a_manifest();
        let envelope = line.sign(&manifest);
        let mut other = manifest.clone();
        let at = other.iter().position(|b| *b == b'0').unwrap();
        other[at] = b'1';
        assert!(matches!(verify_manifest(&other, &envelope, &line.trust("prod"), NOW), Err(ChainError::ManifestSignatureInvalid(_))));
    }

    #[test]
    fn a_certificate_needs_two_distinct_roots_of_this_build() {
        let line = Line::new();
        let manifest = a_manifest();
        let certificate = Line::certificate(&line.release, "prod", ISSUED_AT, NOT_AFTER);
        let trust = line.trust("prod");
        let signed_by = |signers: &[&Key]| verify_manifest(&manifest, &Line::envelope(&manifest, &certificate, signers, &line.release), &trust, NOW);

        assert_eq!(signed_by(&[&line.roots[0], &line.roots[2]]), Ok(()), "any two of the three");
        assert_eq!(signed_by(&[&line.roots[0], &line.roots[1], &line.roots[2]]), Ok(()));
        let too_few = |have| Err(ChainError::TooFewRootSignatures { key_id: line.release.id(), have });
        assert_eq!(signed_by(&[&line.roots[1]]), too_few(1), "one root is not enough");
        assert_eq!(signed_by(&[&line.roots[1], &line.roots[1]]), too_few(1), "the same root twice is one root");
        let stranger = Key::new(7);
        assert_eq!(signed_by(&[&line.roots[1], &stranger]), too_few(1), "a key that is not a root signs nothing");
        assert_eq!(signed_by(&[]), too_few(0));
    }

    #[test]
    fn a_certificate_the_roots_did_not_sign_as_it_stands_is_refused() {
        // The release key swapped for another after signing: the roots' signatures are over the
        // certificate's exact bytes, so they no longer hold.
        let line = Line::new();
        let manifest = a_manifest();
        let attacker = Key::new(8);
        let signed = Line::certificate(&line.release, "prod", ISSUED_AT, NOT_AFTER);
        let forged = Line::certificate(&attacker, "prod", ISSUED_AT, NOT_AFTER);
        let mut envelope: serde_json::Value = serde_json::from_slice(&Line::envelope(&manifest, &signed, &[&line.roots[0], &line.roots[1]], &attacker)).unwrap();
        envelope["certificates"][0]["certificate"] = serde_json::json!(forged);
        let refused = verify_manifest(&manifest, envelope.to_string().as_bytes(), &line.trust("prod"), NOW);
        assert_eq!(refused, Err(ChainError::TooFewRootSignatures { key_id: attacker.id(), have: 0 }));
    }

    #[test]
    fn a_certificate_is_for_one_environment_and_one_span_of_time() {
        let line = Line::new();
        let manifest = a_manifest();
        let envelope = line.sign(&manifest);
        let id = line.release.id();
        assert_eq!(verify_manifest(&manifest, &envelope, &line.trust("staging"), NOW), Err(ChainError::WrongEnvironment { key_id: id.clone(), certificate: "prod".into(), build: "staging".into() }));
        assert_eq!(verify_manifest(&manifest, &envelope, &line.trust("prod"), NOT_AFTER), Ok(()), "valid through its last second");
        assert_eq!(verify_manifest(&manifest, &envelope, &line.trust("prod"), NOT_AFTER + 1), Err(ChainError::Expired { key_id: id.clone(), not_after: NOT_AFTER, now: NOT_AFTER + 1 }));
        assert_eq!(verify_manifest(&manifest, &envelope, &line.trust("prod"), ISSUED_AT), Ok(()), "valid from its first second");
        assert_eq!(verify_manifest(&manifest, &envelope, &line.trust("prod"), ISSUED_AT - 1), Err(ChainError::NotYetValid { key_id: id, issued_at: ISSUED_AT, now: ISSUED_AT - 1 }));
    }

    #[test]
    fn the_manifest_must_be_signed_by_a_key_a_certificate_names() {
        let line = Line::new();
        let manifest = a_manifest();
        let certificate = Line::certificate(&line.release, "prod", ISSUED_AT, NOT_AFTER);
        let stranger = Key::new(7);
        let envelope = Line::envelope(&manifest, &certificate, &[&line.roots[0], &line.roots[1]], &stranger);
        assert_eq!(verify_manifest(&manifest, &envelope, &line.trust("prod"), NOW), Err(ChainError::NoCertificateForSigner { key_id: stranger.id() }));
    }

    #[test]
    fn every_certificate_in_the_file_must_hold_not_only_the_one_that_signed() {
        let line = Line::new();
        let manifest = a_manifest();
        let good = Line::certificate(&line.release, "prod", ISSUED_AT, NOT_AFTER);
        let other = Key::new(6);
        let unsigned = Line::certificate(&other, "prod", ISSUED_AT, NOT_AFTER);
        let mut envelope: serde_json::Value = serde_json::from_slice(&Line::envelope(&manifest, &good, &[&line.roots[0], &line.roots[1]], &line.release)).unwrap();
        envelope["certificates"].as_array_mut().unwrap().push(serde_json::json!({ "certificate": unsigned, "root_signatures": [] }));
        assert_eq!(verify_manifest(&manifest, envelope.to_string().as_bytes(), &line.trust("prod"), NOW), Err(ChainError::TooFewRootSignatures { key_id: other.id(), have: 0 }));
    }

    #[test]
    fn a_signature_file_of_another_kind_is_refused_before_anything_in_it_is_used() {
        let line = Line::new();
        let manifest = a_manifest();
        let mut envelope: serde_json::Value = serde_json::from_slice(&line.sign(&manifest)).unwrap();
        envelope["format"] = serde_json::json!("machinefabric-manifest-sig/2");
        assert_eq!(verify_manifest(&manifest, envelope.to_string().as_bytes(), &line.trust("prod"), NOW), Err(ChainError::UnsupportedEnvelopeFormat("machinefabric-manifest-sig/2".into())));
        assert!(matches!(verify_manifest(&manifest, b"<html>not found</html>", &line.trust("prod"), NOW), Err(ChainError::MalformedEnvelope(_))));
        let mut none: serde_json::Value = serde_json::from_slice(&line.sign(&manifest)).unwrap();
        none["certificates"] = serde_json::json!([]);
        assert_eq!(verify_manifest(&manifest, none.to_string().as_bytes(), &line.trust("prod"), NOW), Err(ChainError::NoCertificates));
    }

    #[test]
    fn base64_is_read_strictly() {
        assert_eq!(base64_decode("TWFu"), Some(b"Man".to_vec()));
        assert_eq!(base64_decode("TWE="), Some(b"Ma".to_vec()));
        assert_eq!(base64_decode("TQ=="), Some(b"M".to_vec()));
        // Empty, not a multiple of four, data after the padding, too much padding, a character
        // outside the alphabet, and (the last two) bits set where the padding says there are none.
        for bad in ["", "TWF", "TWFu=", "T===", "TW=u", "TW!u", "TR==", "TWF="] {
            assert_eq!(base64_decode(bad), None, "{:?}", bad);
        }
        for bytes in [&b"a"[..], b"ab", b"abc", b"abcd", &[0u8, 255, 16, 32, 7]] {
            assert_eq!(base64_decode(&base64(bytes)).as_deref(), Some(bytes));
        }
    }

    #[test]
    fn the_latest_release_is_the_channels_own_for_this_platform() {
        let manifest = a_manifest();
        let release = latest(&manifest, "release", "linux-x86_64").unwrap().unwrap();
        assert_eq!(release, Release { version: "0.150.0".into(), name: "qrust-0.150.0-linux-x86_64.tar.gz".into(), url: "https://r.example/quickrust/release/0.150.0/qrust-0.150.0-linux-x86_64.tar.gz".into(), sha256: SHA.into(), size: 4096 });
        assert_eq!(release.root(), "qrust-0.150.0-linux-x86_64");
        // The other channel exists and has published nothing: not an error, and not the release
        // channel's version either.
        assert_eq!(latest(&manifest, "nightly", "linux-x86_64"), Ok(None));
        let missing = latest(&manifest, "release", "darwin-arm64").unwrap_err();
        assert_eq!(missing, "QuickRust 0.150.0 is not published for darwin-arm64 (it is for linux-x86_64)");
    }

    #[test]
    fn a_manifest_that_is_not_quickrusts_or_not_whole_is_an_error() {
        let edit = |change: &dyn Fn(&mut serde_json::Value)| {
            let mut value: serde_json::Value = serde_json::from_slice(&a_manifest()).unwrap();
            change(&mut value);
            latest(value.to_string().as_bytes(), "release", "linux-x86_64")
        };
        assert_eq!(edit(&|m| m["appId"] = "lungo-cli".into()).unwrap_err(), "the release manifest is lungo-cli's, not quickrust's");
        assert_eq!(edit(&|m| m["schemaVersion"] = "6.0".into()).unwrap_err(), "the release manifest is of schema 6.0; this QuickRust reads schema 5");
        assert_eq!(edit(&|m| m["channels"]["release"]["latestVersion"] = "0.151.0".into()).unwrap_err(), "the release manifest names 0.151.0 as the latest release version and does not list it");
        assert_eq!(edit(&|m| { m["channels"].as_object_mut().unwrap().remove("release"); }).unwrap_err(), "the release manifest has no release channel");
        let installer = |m: &mut serde_json::Value, key: &str, value: serde_json::Value| m["channels"]["release"]["versions"]["0.150.0"]["platforms"]["linux-x86_64"]["installers"][0][key] = value;
        assert!(edit(&|m| installer(m, "type", "zip".into())).unwrap_err().ends_with("is published without a tar.gz archive"));
        assert!(edit(&|m| installer(m, "sha256", "abc".into())).unwrap_err().ends_with("carries \"abc\" as its SHA-256"));
        assert!(edit(&|m| installer(m, "name", "../evil.tar.gz".into())).unwrap_err().ends_with("which is not a tar.gz file name"));
        assert!(latest(b"not json", "release", "linux-x86_64").unwrap_err().starts_with("the release manifest cannot be read"));
    }

    #[test]
    fn a_manifest_is_fetched_with_its_signature_and_trusted_only_together() {
        use crate::net::testserver::{Route, Server};
        let line = Line::new();
        let manifest = a_manifest();
        let server = Server::start();
        let client = crate::net::Client::new();
        let registry = server.url("");
        let registry = registry.trim_end_matches('/');
        let trust = line.trust("prod");
        let fetch = || fetch_latest(&client, registry, &trust, "release", "linux-x86_64", NOW);

        // Nothing published.
        assert_eq!(fetch().unwrap_err(), format!("no QuickRust release is published at {} yet", registry));
        // A manifest with no signature beside it is not read.
        let serve = |body: Vec<u8>| Route { body, ..Route::default() };
        server.route("/quickrust/manifest", serve(manifest.clone()));
        assert!(fetch().unwrap_err().contains("is published without a signature"), "{:?}", fetch());
        // With its signature, it is.
        server.route("/quickrust/manifest.sig", serve(line.sign(&manifest)));
        assert_eq!(fetch().unwrap().unwrap().version, "0.150.0");
        // The manifest replaced and the signature left: the pair no longer matches.
        let newer = manifest_bytes_for("0.151.0");
        server.route("/quickrust/manifest", serve(newer.clone()));
        let refused = fetch().unwrap_err();
        assert!(refused.contains("is not a release manifest this QuickRust trusts") && refused.contains("does not match the manifest"), "{}", refused);
        // Both replaced: trusted again, and the newer release is the one read.
        server.route("/quickrust/manifest.sig", serve(line.sign(&newer)));
        assert_eq!(fetch().unwrap().unwrap().version, "0.151.0");
    }

    fn manifest_bytes_for(version: &str) -> Vec<u8> {
        let name = format!("qrust-{}-linux-x86_64.tar.gz", version);
        manifest("release", version, "linux-x86_64", &name, &format!("https://r.example/quickrust/release/{}/{}", version, name), SHA, 4096)
    }
}
