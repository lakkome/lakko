// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The Delivery Service request proof ([ADR-0053]).
//!
//! A claimed journal on `services/ds` answers only a request signed with a
//! key the journal was claimed under: for a group a per-epoch Ed25519 key
//! every member derives from the MLS exporter (`ACCESS_KEY_LABEL`), for a
//! mailbox the identity key its pseudonym is the hash of. This crate is the
//! one definition of what is signed and how the proof travels, shared by the
//! server and both FFI surfaces so no client re-derives the bytes.
//!
//! # Wire format
//!
//! One request header:
//!
//! ```text
//! X-Lakko-DS-Proof: v1.<epoch>.<unix_ms>.<nonce_b64url>.<sig_b64url>
//! ```
//!
//! `epoch` and `unix_ms` are ASCII decimals, the nonce is 16 bytes and the
//! signature 64, both base64url without padding.
//!
//! # Canonical bytes
//!
//! ```text
//! "lakko/ds/proof/v1" || 0x00 || method || 0x00 || canonical_path || 0x00
//!   || query || 0x00 || BLAKE3(body) || epoch(8 BE) || unix_ms(8 BE) || nonce(16)
//! ```
//!
//! `canonical_path` is the service path (`/ds/group/<hex>/messages`), never
//! an nginx prefix, so web and native sign the same bytes. The NUL separators
//! keep `method || path` unambiguous, and the epoch is bound so a proof under
//! one epoch's key cannot be presented under another label. The body enters
//! as its BLAKE3 digest, so a signature is the same size for a 1 MB publish
//! as for an empty long-poll.
//!
//! # What the verifier checks, in order
//!
//! 1. the header parses (bounded, fail-closed);
//! 2. `unix_ms` lies within [`PROOF_WINDOW_MS`] of the server's clock;
//! 3. the signature verifies under the key the caller resolved for `epoch`.
//!
//! Key resolution and the nonce replay cache are the server's, since both
//! are state; this crate is pure.
//!
//! [ADR-0053]: ../../../docs/adr/0053-ds-journal-authorisation.md

#![forbid(unsafe_code)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_contacts::IdentityPseudonym;
use lakko_crypto::hash;

pub use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};

/// The request header the proof travels in (lowercase, as HTTP/2 carries it).
pub const PROOF_HEADER_NAME: &str = "x-lakko-ds-proof";
/// Domain separator that opens the canonical bytes.
pub const CANONICAL_DOMAIN: &[u8] = b"lakko/ds/proof/v1";
/// The MLS exporter label whose 32-byte output seeds a group's access key.
pub const ACCESS_KEY_LABEL: &str = "lakko/ds/access/v1";
/// Nonce length in bytes.
pub const NONCE_LEN: usize = 16;
/// Ed25519 signature length in bytes.
pub const SIGNATURE_LEN: usize = 64;
/// How far from the server's clock a proof's `unix_ms` may lie, either way.
pub const PROOF_WINDOW_MS: u64 = 120_000;
/// The epoch label a mailbox proof carries (a mailbox has no epochs).
pub const MAILBOX_EPOCH: u64 = 0;
/// Upper bound on the header's length; anything longer is refused unparsed.
pub const MAX_HEADER_LEN: usize = 256;

const VERSION: &str = "v1";
const SEP: u8 = 0;

/// The parts of a request the proof binds.
#[derive(Clone, Copy, Debug)]
pub struct RequestParts<'a> {
    /// HTTP method, upper case (`GET`, `POST`, `PUT`).
    pub method: &'a str,
    /// The service path, without any reverse-proxy prefix.
    pub canonical_path: &'a str,
    /// The raw query string without the `?`, or empty.
    pub query: &'a str,
    /// The request body, or empty.
    pub body: &'a [u8],
}

/// A parsed proof header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// The epoch whose key signed this (`MAILBOX_EPOCH` for a mailbox).
    pub epoch: u64,
    /// The signer's clock at signing, unix milliseconds.
    pub unix_ms: u64,
    /// Fresh per request; the server refuses a repeat within the window.
    pub nonce: [u8; NONCE_LEN],
    /// Ed25519 over the canonical bytes.
    pub signature: [u8; SIGNATURE_LEN],
}

/// Why a proof was refused. Every variant is a refusal; the server maps all of
/// them to one response so a caller learns nothing from which one it hit.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProofError {
    /// The header did not parse (wrong shape, wrong lengths, bad base64).
    #[error("proof header malformed")]
    Malformed,
    /// A version this crate does not speak.
    #[error("proof header version unknown")]
    UnknownVersion,
    /// `unix_ms` is outside the window around the verifier's clock.
    #[error("proof outside the time window")]
    OutsideWindow,
    /// The signature does not verify under the resolved key.
    #[error("proof signature invalid")]
    BadSignature,
}

/// The bytes a proof signs. Total and allocation-bounded by the inputs.
#[must_use]
pub fn canonical_bytes(
    req: &RequestParts<'_>,
    epoch: u64,
    unix_ms: u64,
    nonce: &[u8; NONCE_LEN],
) -> Vec<u8> {
    let digest = hash::blake3(req.body);
    let mut out = Vec::with_capacity(
        CANONICAL_DOMAIN.len()
            + req.method.len()
            + req.canonical_path.len()
            + req.query.len()
            + digest.len()
            + 4
            + 8
            + 8
            + NONCE_LEN,
    );
    out.extend_from_slice(CANONICAL_DOMAIN);
    out.push(SEP);
    out.extend_from_slice(req.method.as_bytes());
    out.push(SEP);
    out.extend_from_slice(req.canonical_path.as_bytes());
    out.push(SEP);
    out.extend_from_slice(req.query.as_bytes());
    out.push(SEP);
    out.extend_from_slice(&digest);
    out.extend_from_slice(&epoch.to_be_bytes());
    out.extend_from_slice(&unix_ms.to_be_bytes());
    out.extend_from_slice(nonce);
    out
}

/// Sign a request under `key` for `epoch`.
#[must_use]
pub fn sign(
    key: &SigningKey,
    req: &RequestParts<'_>,
    epoch: u64,
    unix_ms: u64,
    nonce: [u8; NONCE_LEN],
) -> Proof {
    let bytes = canonical_bytes(req, epoch, unix_ms, &nonce);
    Proof {
        epoch,
        unix_ms,
        nonce,
        signature: key.sign(&bytes).to_bytes(),
    }
}

/// Verify `proof` over `req` under `key`, with `now_ms` the verifier's clock.
///
/// Checks the window before the signature so a stale proof costs no
/// signature verification. Resolving `key` from `proof.epoch` and refusing a
/// replayed nonce are the caller's.
///
/// # Errors
///
/// [`ProofError::OutsideWindow`] or [`ProofError::BadSignature`].
pub fn verify(
    proof: &Proof,
    req: &RequestParts<'_>,
    key: &VerifyingKey,
    now_ms: u64,
) -> Result<(), ProofError> {
    if now_ms.abs_diff(proof.unix_ms) > PROOF_WINDOW_MS {
        return Err(ProofError::OutsideWindow);
    }
    let bytes = canonical_bytes(req, proof.epoch, proof.unix_ms, &proof.nonce);
    let signature =
        Signature::from_bytes(&proof.signature).map_err(|_| ProofError::BadSignature)?;
    key.verify(&bytes, &signature)
        .map_err(|_| ProofError::BadSignature)
}

impl Proof {
    /// The header value.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{VERSION}.{}.{}.{}.{}",
            self.epoch,
            self.unix_ms,
            URL_SAFE_NO_PAD.encode(self.nonce),
            URL_SAFE_NO_PAD.encode(self.signature),
        )
    }

    /// Parse a header value. Bounded (`MAX_HEADER_LEN`) and fail-closed:
    /// five dot-separated fields, decimal epoch and time, a 16-byte nonce and
    /// a 64-byte signature in base64url without padding, nothing else.
    ///
    /// # Errors
    ///
    /// [`ProofError::UnknownVersion`] on a version this crate does not speak,
    /// [`ProofError::Malformed`] on anything else that does not parse.
    pub fn parse(value: &str) -> Result<Self, ProofError> {
        if value.len() > MAX_HEADER_LEN {
            return Err(ProofError::Malformed);
        }
        let mut parts = value.split('.');
        let version = parts.next().ok_or(ProofError::Malformed)?;
        let epoch = parts.next().ok_or(ProofError::Malformed)?;
        let unix_ms = parts.next().ok_or(ProofError::Malformed)?;
        let nonce = parts.next().ok_or(ProofError::Malformed)?;
        let signature = parts.next().ok_or(ProofError::Malformed)?;
        if parts.next().is_some() {
            return Err(ProofError::Malformed);
        }
        if version != VERSION {
            return Err(ProofError::UnknownVersion);
        }
        let epoch = parse_decimal(epoch)?;
        let unix_ms = parse_decimal(unix_ms)?;
        let nonce_bytes = URL_SAFE_NO_PAD
            .decode(nonce)
            .map_err(|_| ProofError::Malformed)?;
        let signature_bytes = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| ProofError::Malformed)?;
        let nonce: [u8; NONCE_LEN] = nonce_bytes
            .as_slice()
            .try_into()
            .map_err(|_| ProofError::Malformed)?;
        let signature: [u8; SIGNATURE_LEN] = signature_bytes
            .as_slice()
            .try_into()
            .map_err(|_| ProofError::Malformed)?;
        Ok(Self {
            epoch,
            unix_ms,
            nonce,
            signature,
        })
    }
}

/// A strict decimal: 1..=20 ASCII digits, no sign, no leading `+`, fits a u64.
fn parse_decimal(field: &str) -> Result<u64, ProofError> {
    if field.is_empty() || field.len() > 20 || !field.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProofError::Malformed);
    }
    field.parse::<u64>().map_err(|_| ProofError::Malformed)
}

/// The group access signing key for one epoch, from the 32 exporter bytes of
/// `Group::export_secret(ACCESS_KEY_LABEL, group_id, 32)`.
#[must_use]
pub fn access_signing_key(exporter_seed: &[u8; 32]) -> SigningKey {
    SigningKey::from_seed(exporter_seed)
}

/// Whether `identity_pk` is the key a mailbox pseudonym is named after
/// (`IdentityPseudonym::from_identity`, the ADR-0005 derivation). The server
/// runs this on a mailbox claim; the derivation is defined once in
/// `lakko-contacts` and used here rather than copied.
#[must_use]
pub fn mailbox_owner_matches(pseudonym: &[u8; 32], identity_pk: &VerifyingKey) -> bool {
    mailbox_pseudonym(identity_pk) == *pseudonym
}

/// The pseudonym of the mailbox `identity_pk` owns - the same derivation
/// [`mailbox_owner_matches`] checks, for a client naming its own mailbox.
#[must_use]
pub fn mailbox_pseudonym(identity_pk: &VerifyingKey) -> [u8; 32] {
    *IdentityPseudonym::from_identity(identity_pk).as_bytes()
}

/// Harness entry points for the fuzz crate. Not part of the supported API.
#[doc(hidden)]
pub mod fuzz_api {
    use super::{Proof, RequestParts, SigningKey};

    /// Parse the bytes as a header and, when they parse, verify the result
    /// against a fixed key and request. `true` when the parse succeeds.
    #[must_use]
    pub fn parse_and_verify(data: &[u8]) -> bool {
        let Ok(text) = core::str::from_utf8(data) else {
            return false;
        };
        let Ok(proof) = Proof::parse(text) else {
            return false;
        };
        let key = SigningKey::from_seed(&[0x5Au8; 32]).verifying_key();
        let req = RequestParts {
            method: "GET",
            canonical_path: "/ds/group/00/messages",
            query: "after=0",
            body: &[],
        };
        let _ = super::verify(&proof, &req, &key, proof.unix_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x11u8; 32])
    }

    fn req(body: &[u8]) -> RequestParts<'_> {
        RequestParts {
            method: "POST",
            canonical_path: "/ds/group/ab/messages",
            query: "",
            body,
        }
    }

    #[test]
    fn round_trips_through_the_header() {
        let k = key();
        let r = req(b"hello");
        let proof = sign(&k, &r, 7, 1_000_000, [9u8; NONCE_LEN]);
        let header = proof.encode();
        assert!(header.starts_with("v1.7.1000000."));
        let parsed = Proof::parse(&header).expect("parse");
        assert_eq!(parsed, proof);
        verify(&parsed, &r, &k.verifying_key(), 1_000_500).expect("verify");
    }

    #[test]
    fn every_bound_field_is_bound() {
        let k = key();
        let r = req(b"hello");
        let proof = sign(&k, &r, 7, 1_000_000, [9u8; NONCE_LEN]);
        let vk = k.verifying_key();
        let now = 1_000_000;
        let variants: [RequestParts<'_>; 4] = [
            RequestParts { method: "GET", ..r },
            RequestParts {
                canonical_path: "/ds/group/ab/knocks",
                ..r
            },
            RequestParts {
                query: "after=1",
                ..r
            },
            RequestParts {
                body: b"hellp",
                ..r
            },
        ];
        for v in &variants {
            assert_eq!(verify(&proof, v, &vk, now), Err(ProofError::BadSignature));
        }
        let mut other_epoch = proof.clone();
        other_epoch.epoch = 8;
        assert_eq!(
            verify(&other_epoch, &r, &vk, now),
            Err(ProofError::BadSignature)
        );
        let mut other_nonce = proof.clone();
        other_nonce.nonce[0] ^= 1;
        assert_eq!(
            verify(&other_nonce, &r, &vk, now),
            Err(ProofError::BadSignature)
        );
        let other_key = SigningKey::from_seed(&[0x22u8; 32]).verifying_key();
        assert_eq!(
            verify(&proof, &r, &other_key, now),
            Err(ProofError::BadSignature)
        );
    }

    #[test]
    fn the_window_is_checked_before_the_signature() {
        let k = key();
        let r = req(b"");
        let proof = sign(&k, &r, 0, 1_000_000, [0u8; NONCE_LEN]);
        let vk = k.verifying_key();
        verify(&proof, &r, &vk, 1_000_000 + PROOF_WINDOW_MS).expect("edge inside");
        verify(&proof, &r, &vk, 1_000_000 - PROOF_WINDOW_MS).expect("edge inside");
        assert_eq!(
            verify(&proof, &r, &vk, 1_000_000 + PROOF_WINDOW_MS + 1),
            Err(ProofError::OutsideWindow)
        );
        assert_eq!(
            verify(&proof, &r, &vk, 1_000_000 - PROOF_WINDOW_MS - 1),
            Err(ProofError::OutsideWindow)
        );
    }

    #[test]
    fn parse_is_bounded_and_fail_closed() {
        let good = sign(&key(), &req(b""), 1, 2, [3u8; NONCE_LEN]).encode();
        assert!(Proof::parse(&good).is_ok());
        assert_eq!(Proof::parse(""), Err(ProofError::Malformed));
        assert_eq!(
            Proof::parse("v2.1.2.AAAA.AAAA"),
            Err(ProofError::UnknownVersion)
        );
        assert_eq!(
            Proof::parse(&format!("{good}.extra")),
            Err(ProofError::Malformed)
        );
        assert_eq!(
            Proof::parse(&good.replacen("v1.1.", "v1.+1.", 1)),
            Err(ProofError::Malformed)
        );
        assert_eq!(
            Proof::parse(&good.replacen("v1.1.2.", "v1.1.99999999999999999999999.", 1)),
            Err(ProofError::Malformed)
        );
        let long = "v1.1.2.".to_string() + &"A".repeat(MAX_HEADER_LEN);
        assert_eq!(Proof::parse(&long), Err(ProofError::Malformed));
        // A nonce of the wrong length, a signature of the wrong length.
        assert_eq!(Proof::parse("v1.1.2.AAAA.AAAA"), Err(ProofError::Malformed));
    }

    const GOLDEN_CANONICAL_BLAKE3: &str =
        "881c504b2a652f3f768cc78f2904f780cb9a3409f76afc6d354daba91e32ab48";

    #[test]
    fn canonical_bytes_are_pinned() {
        let r = RequestParts {
            method: "GET",
            canonical_path: "/ds/group/00/messages",
            query: "after=0&wait=25",
            body: b"",
        };
        let bytes = canonical_bytes(&r, 3, 1_700_000_000_000, &[0xABu8; NONCE_LEN]);
        // domain, NUL, method, NUL, path, NUL, query, NUL, blake3(""), epoch, ms, nonce
        let expected_len = CANONICAL_DOMAIN.len()
            + 1
            + 3
            + 1
            + r.canonical_path.len()
            + 1
            + r.query.len()
            + 1
            + 32
            + 8
            + 8
            + NONCE_LEN;
        assert_eq!(bytes.len(), expected_len);
        assert!(
            bytes.starts_with(b"lakko/ds/proof/v1\0GET\0/ds/group/00/messages\0after=0&wait=25\0")
        );
        assert_eq!(&bytes[bytes.len() - NONCE_LEN..], &[0xABu8; NONCE_LEN]);
        // The pinned golden: a change to the canonical layout must fail here,
        // not on a live server refusing every client.
        assert_eq!(hex(&hash::blake3(&bytes)), GOLDEN_CANONICAL_BLAKE3);
    }

    #[test]
    fn a_mailbox_is_named_after_its_identity_key() {
        let k = key();
        let vk = k.verifying_key();
        let pseudonym = *IdentityPseudonym::from_identity(&vk).as_bytes();
        assert!(mailbox_owner_matches(&pseudonym, &vk));
        let other = SigningKey::from_seed(&[0x33u8; 32]).verifying_key();
        assert!(!mailbox_owner_matches(&pseudonym, &other));
    }

    #[test]
    fn the_access_key_is_a_pure_function_of_the_exporter_seed() {
        let a = access_signing_key(&[0x44u8; 32]);
        let b = access_signing_key(&[0x44u8; 32]);
        assert_eq!(a.verifying_key().to_bytes(), b.verifying_key().to_bytes());
        let c = access_signing_key(&[0x45u8; 32]);
        assert_ne!(a.verifying_key().to_bytes(), c.verifying_key().to_bytes());
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }
}
