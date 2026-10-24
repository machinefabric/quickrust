//! A release line a test signs itself: three roots, a release key they certify, and manifests
//! signed with it, in the form the release registry publishes (`src/release.rs` describes the
//! protocol). Used by the unit tests of the verifier and by the tests that update a built
//! QuickRust from a registry they serve.
//!
//! The formats are written out here, not taken from the code under test: a test that asked the
//! verifier what a certificate looks like would agree with it whatever it said.
#![allow(dead_code)]

use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;

pub const ISSUED_AT: u64 = 1_700_000_000;
pub const NOT_AFTER: u64 = ISSUED_AT + 365 * 86_400;
/// A time inside the validity of [`Line::sign`]'s certificate.
pub const NOW: u64 = ISSUED_AT + 86_400;

const CERTIFICATE_FORMAT: &str = "machinefabric-release-key-cert/1";
const ENVELOPE_FORMAT: &str = "machinefabric-manifest-sig/1";
const CHARTER_FORMAT: &str = "machinefabric-registry-charter/1";
const CHARTER_ENVELOPE_FORMAT: &str = "machinefabric-charter-sig/1";

/// An ed25519 key with a minisign key id.
pub struct Key(SigningKey, [u8; 8]);

impl Key {
    /// A key made from `seed`, with `seed` repeated as its key id.
    pub fn new(seed: u8) -> Key {
        Key(SigningKey::from_bytes(&[seed; 32]), [seed; 8])
    }

    /// The key id, lower-case hexadecimal.
    pub fn id(&self) -> String {
        self.1.iter().map(|b| format!("{:02x}", b)).collect()
    }

    /// The public key in minisign's form: `Ed`, the key id, the ed25519 key, in base64.
    pub fn public(&self) -> String {
        let mut bytes = b"Ed".to_vec();
        bytes.extend_from_slice(&self.1);
        bytes.extend_from_slice(self.0.verifying_key().as_bytes());
        base64(&bytes)
    }

    /// A raw ed25519 signature over `bytes`, in base64.
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

    /// The roots' public keys, as a build is given them.
    pub fn root_keys(&self) -> Vec<String> {
        self.roots.iter().map(Key::public).collect()
    }

    /// The certificate of `key` for `environment`, as the roots sign it.
    pub fn certificate(key: &Key, environment: &str, issued_at: u64, not_after: u64) -> String {
        json!({ "format": CERTIFICATE_FORMAT, "release_pubkey": key.public(), "key_id": key.id(), "environment": environment, "issued_at": issued_at, "not_after": not_after }).to_string()
    }

    /// The signature file of `manifest`: `certificate` signed by `signers`, the manifest signed
    /// by `by`.
    pub fn envelope(manifest: &[u8], certificate: &str, signers: &[&Key], by: &Key) -> Vec<u8> {
        let root_signatures: Vec<_> = signers.iter().map(|root| json!({ "root_key_id": root.id(), "signature": root.sign(certificate.as_bytes()) })).collect();
        json!({
            "format": ENVELOPE_FORMAT,
            "certificates": [{ "certificate": certificate, "root_signatures": root_signatures }],
            "manifest_signature": { "key_id": by.id(), "signature": by.sign(manifest) },
        })
        .to_string()
        .into_bytes()
    }

    /// `manifest` signed as a publish does it: the release key certified for `environment` by
    /// roots A and B, valid from [`ISSUED_AT`] to [`NOT_AFTER`].
    pub fn sign_for(&self, manifest: &[u8], environment: &str) -> Vec<u8> {
        let certificate = Line::certificate(&self.release, environment, ISSUED_AT, NOT_AFTER);
        Line::envelope(manifest, &certificate, &[&self.roots[0], &self.roots[1]], &self.release)
    }

    /// [`Line::sign_for`] the `prod` environment.
    pub fn sign(&self, manifest: &[u8]) -> Vec<u8> {
        self.sign_for(manifest, "prod")
    }

    /// The charter of the `kind` registry at `anchor`, whose contents are signed under
    /// `environment`: its manifest at `<anchor>/{app_id}/manifest`, valid from [`ISSUED_AT`] with no
    /// end of its own.
    pub fn charter(&self, anchor: &str, kind: &str, environment: &str) -> Vec<u8> {
        json!({
            "format": CHARTER_FORMAT,
            "anchor": anchor,
            "kind": kind,
            "slug": "test-releases",
            "environment": environment,
            "locations": { "manifest": format!("{}/{{app_id}}/manifest", anchor) },
            "issued_at": ISSUED_AT,
            "not_after": null,
        })
        .to_string()
        .into_bytes()
    }

    /// The signature file of `charter`: `certificate` signed by `signers`, the charter by `by`.
    pub fn charter_envelope(charter: &[u8], certificate: &str, signers: &[&Key], by: &Key) -> Vec<u8> {
        let root_signatures: Vec<_> = signers.iter().map(|root| json!({ "root_key_id": root.id(), "signature": root.sign(certificate.as_bytes()) })).collect();
        json!({
            "format": CHARTER_ENVELOPE_FORMAT,
            "certificates": [{ "certificate": certificate, "root_signatures": root_signatures }],
            "charter_signature": { "key_id": by.id(), "signature": by.sign(charter) },
        })
        .to_string()
        .into_bytes()
    }

    /// `charter` signed as MachineFabric signs charters: the release key certified for `prod` by
    /// roots A and B.
    pub fn sign_charter(&self, charter: &[u8]) -> Vec<u8> {
        let certificate = Line::certificate(&self.release, "prod", ISSUED_AT, NOT_AFTER);
        Line::charter_envelope(charter, &certificate, &[&self.roots[0], &self.roots[1]], &self.release)
    }
}

/// QuickRust's manifest with one version on `channel` for `platform`, and the other channel
/// empty.
pub fn manifest(channel: &str, version: &str, platform: &str, name: &str, url: &str, sha256: &str, size: u64) -> Vec<u8> {
    let other = if channel == "release" { "nightly" } else { "release" };
    json!({
        "schemaVersion": "5.0",
        "appId": "quickrust",
        "name": "QuickRust",
        "lastUpdated": "2026-10-13T00:00:00Z",
        "channels": {
            channel: { "latestVersion": version, "versions": { version: { "publishedAt": "2026-10-13T00:00:00Z", "platforms": { platform: { "label": platform, "installers": [
                { "type": if name.ends_with(".zip") { "zip" } else { "targz" }, "label": "archive", "name": name, "path": url, "sha256": sha256, "size": size }
            ] } } } } },
            other: { "latestVersion": "", "versions": {} },
        },
    })
    .to_string()
    .into_bytes()
}
