// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Action attestation (G-13, [ADR-0015]).
//!
//! An *action attestation* is a compact, domain-separated, Ed25519-signed
//! proof that a given account completed a passkey ceremony authorising a
//! specific action (identified by an opaque 32-byte `action_hash`), valid
//! until an expiry. It is the foundational primitive for **peer-verified**
//! device-removal authorisation: `services/auth` signs it on
//! `/auth/action/finish`, the removing device carries it alongside its MLS
//! `Remove` commit, and the receiving devices verify it before applying the
//! commit — so on-device malware that bypasses the app UI cannot evict a
//! device without a real passkey gesture.
//!
//! The format is deliberately a small fixed-layout binary blob rather than a
//! JWT: the verifier lives in the audited `lakko-mls` apply path, so it must
//! be trivial to read, bounded, and fuzzable, and it must not pull a JWS /
//! JWKS-fetch dependency into the core. The trusted verifying key is supplied
//! by the caller (the client holds it from pinned / bootstrap trust config).
//!
//! ```text
//! wire           = version(1) ‖ account_id(16) ‖ action_hash(32)
//!                  ‖ expires_at_unix_secs(8, big-endian i64) ‖ Ed25519_sig(64)
//! signing input  = DOMAIN ‖ version ‖ account_id ‖ action_hash ‖ expires_at
//! ```
//!
//! The signature covers every field (via `signing input`), so tampering with
//! any byte invalidates it. `verify_bound` is the strict entry point: it
//! checks the signature, then the account / action binding, then freshness.
//!
//! [ADR-0015]: ../../../docs/adr/0015-device-removal-authorization.md

#![forbid(unsafe_code)]

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

/// Domain separator mixed into the signing input. Bumping this string (and
/// [`VERSION`]) invalidates every previously-issued attestation and prevents
/// the signature from being confused with any other use of the same key.
const DOMAIN: &[u8] = b"lakko/device-attest/v1";

/// Wire-format version. The verifier refuses any other value.
const VERSION: u8 = 1;

/// Length of the account id (raw UUIDv7 bytes).
const ACCOUNT_LEN: usize = 16;

/// Length of the opaque action hash.
const ACTION_HASH_LEN: usize = 32;

/// Length of the big-endian `i64` expiry.
const EXPIRES_LEN: usize = 8;

/// Total serialized attestation length:
/// `version + account_id + action_hash + expires_at + signature`.
pub const ATTESTATION_LEN: usize = 1 + ACCOUNT_LEN + ACTION_HASH_LEN + EXPIRES_LEN + SIGNATURE_LEN;

/// Why an attestation was rejected. Returned by [`ActionAttestation::open`]
/// and [`verify_bound`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AttestError {
    /// The byte slice is not exactly [`ATTESTATION_LEN`] long.
    #[error("attestation has wrong length: {got} bytes")]
    BadLength {
        /// The actual length received.
        got: usize,
    },
    /// The version byte is not [`VERSION`].
    #[error("unsupported attestation version: {0}")]
    BadVersion(u8),
    /// The Ed25519 signature did not verify (tampered field, wrong key, or
    /// malformed signature bytes).
    #[error("attestation signature is invalid")]
    BadSignature,
    /// The attested account id does not match the expected one.
    #[error("attestation account id does not match the expected account")]
    AccountMismatch,
    /// The attested action hash does not match the expected one.
    #[error("attestation action hash does not match the expected action")]
    ActionMismatch,
    /// The attestation's expiry is in the past relative to `now`.
    #[error("attestation has expired")]
    Expired,
}

/// The authenticated claims carried by an attestation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionAttestation {
    /// The account the passkey ceremony authenticated (raw 16-byte id).
    pub account_id: [u8; ACCOUNT_LEN],
    /// The opaque 32-byte action hash the ceremony bound to.
    pub action_hash: [u8; ACTION_HASH_LEN],
    /// Absolute expiry, unix seconds.
    pub expires_at_unix_secs: i64,
}

impl ActionAttestation {
    /// Construct claims to be signed.
    #[must_use]
    pub fn new(
        account_id: [u8; ACCOUNT_LEN],
        action_hash: [u8; ACTION_HASH_LEN],
        expires_at_unix_secs: i64,
    ) -> Self {
        Self {
            account_id,
            action_hash,
            expires_at_unix_secs,
        }
    }

    /// The exact bytes the signature covers (domain-separated).
    fn signing_input(&self) -> Vec<u8> {
        let mut buf =
            Vec::with_capacity(DOMAIN.len() + 1 + ACCOUNT_LEN + ACTION_HASH_LEN + EXPIRES_LEN);
        buf.extend_from_slice(DOMAIN);
        buf.push(VERSION);
        buf.extend_from_slice(&self.account_id);
        buf.extend_from_slice(&self.action_hash);
        buf.extend_from_slice(&self.expires_at_unix_secs.to_be_bytes());
        buf
    }

    /// Serialize + sign these claims into the [`ATTESTATION_LEN`]-byte wire
    /// form. Called by `services/auth` on `/auth/action/finish`.
    #[must_use]
    pub fn sign(&self, signing: &SigningKey) -> [u8; ATTESTATION_LEN] {
        let sig = signing.sign(&self.signing_input());
        let mut out = [0u8; ATTESTATION_LEN];
        out[0] = VERSION;
        let mut off = 1;
        out[off..off + ACCOUNT_LEN].copy_from_slice(&self.account_id);
        off += ACCOUNT_LEN;
        out[off..off + ACTION_HASH_LEN].copy_from_slice(&self.action_hash);
        off += ACTION_HASH_LEN;
        out[off..off + EXPIRES_LEN].copy_from_slice(&self.expires_at_unix_secs.to_be_bytes());
        off += EXPIRES_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the Ed25519 signature against
    /// `verifying`. Returns the authenticated claims.
    ///
    /// This does **not** check the account / action binding or freshness —
    /// callers that need the full check should use [`verify_bound`], which is
    /// the strict path the audited apply code uses.
    ///
    /// # Errors
    ///
    /// [`AttestError::BadLength`] / [`AttestError::BadVersion`] on a malformed
    /// envelope; [`AttestError::BadSignature`] if the signature is malformed
    /// or does not verify.
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, AttestError> {
        if bytes.len() != ATTESTATION_LEN {
            return Err(AttestError::BadLength { got: bytes.len() });
        }
        if bytes[0] != VERSION {
            return Err(AttestError::BadVersion(bytes[0]));
        }
        let mut off = 1;
        let mut account_id = [0u8; ACCOUNT_LEN];
        account_id.copy_from_slice(&bytes[off..off + ACCOUNT_LEN]);
        off += ACCOUNT_LEN;
        let mut action_hash = [0u8; ACTION_HASH_LEN];
        action_hash.copy_from_slice(&bytes[off..off + ACTION_HASH_LEN]);
        off += ACTION_HASH_LEN;
        let mut exp = [0u8; EXPIRES_LEN];
        exp.copy_from_slice(&bytes[off..off + EXPIRES_LEN]);
        off += EXPIRES_LEN;
        let expires_at_unix_secs = i64::from_be_bytes(exp);

        let sig =
            Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN]).map_err(|_| AttestError::BadSignature)?;
        let claims = Self {
            account_id,
            action_hash,
            expires_at_unix_secs,
        };
        verifying
            .verify(&claims.signing_input(), &sig)
            .map_err(|_| AttestError::BadSignature)?;
        Ok(claims)
    }

    /// `true` when `now_unix_secs` is strictly past the expiry.
    #[must_use]
    pub fn is_expired(&self, now_unix_secs: i64) -> bool {
        now_unix_secs > self.expires_at_unix_secs
    }
}

/// Strictly verify an attestation: signature, then account + action binding,
/// then freshness. This is the entry point peer devices use before applying
/// an attested operation.
///
/// # Errors
///
/// Propagates [`ActionAttestation::open`] errors, then
/// [`AttestError::AccountMismatch`] / [`AttestError::ActionMismatch`] /
/// [`AttestError::Expired`] if the (now-authenticated) claims do not match
/// what the caller expects or have lapsed.
pub fn verify_bound(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_account: &[u8; ACCOUNT_LEN],
    expected_action_hash: &[u8; ACTION_HASH_LEN],
    now_unix_secs: i64,
) -> Result<ActionAttestation, AttestError> {
    let att = ActionAttestation::open(bytes, verifying)?;
    if &att.account_id != expected_account {
        return Err(AttestError::AccountMismatch);
    }
    if &att.action_hash != expected_action_hash {
        return Err(AttestError::ActionMismatch);
    }
    if att.is_expired(now_unix_secs) {
        return Err(AttestError::Expired);
    }
    Ok(att)
}

/// Fuzz-only harness entry points (`#[doc(hidden)]`, not part of the supported
/// public API) — the lakko-mls `fuzz_api` posture, letting the out-of-tree
/// `fuzz/` crate drive the attestation parser without a lakko-crypto dependency
/// of its own.
#[doc(hidden)]
pub mod fuzz_api {
    use super::{ActionAttestation, SigningKey};

    /// Try to [`ActionAttestation::open`] the bytes against a FIXED verifying
    /// key. The bytes are attacker-controlled — an attestation rides alongside a
    /// peer's DeviceGroup `Remove` commit (ADR-0015), so a malformed envelope
    /// reaches this bounded parser before any state is consulted — and the
    /// parser must stay panic-free on any input. The fuzzer cannot forge a valid
    /// signature against the fixed key, so it exercises the length / version /
    /// field-slicing / signature-parse / verify-fail path. Returns `true` on a
    /// (vanishingly rare) success.
    #[must_use]
    pub fn open_attestation(bytes: &[u8]) -> bool {
        let vk = SigningKey::from_seed(&[0x11; 32]).verifying_key();
        ActionAttestation::open(bytes, &vk).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x11; 32])
    }

    fn sample() -> ActionAttestation {
        ActionAttestation::new([0xAA; 16], [0xBB; 32], 1_000)
    }

    #[test]
    fn sign_then_verify_bound_roundtrips() {
        let sk = key();
        let att = sample();
        let wire = att.sign(&sk);
        assert_eq!(wire.len(), ATTESTATION_LEN);
        let got = verify_bound(&wire, &sk.verifying_key(), &[0xAA; 16], &[0xBB; 32], 999)
            .expect("valid attestation verifies");
        assert_eq!(got, att);
    }

    #[test]
    fn tampering_any_field_breaks_the_signature() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        // Flip one byte in each region (account, action, expiry, sig) and
        // confirm the signature no longer verifies.
        for idx in [1usize, 20, 50, ATTESTATION_LEN - 1] {
            let mut bad = wire;
            bad[idx] ^= 0x01;
            assert_eq!(
                ActionAttestation::open(&bad, &vk),
                Err(AttestError::BadSignature),
                "tampering byte {idx} must invalidate the signature"
            );
        }
    }

    #[test]
    fn wrong_key_is_rejected() {
        let wire = sample().sign(&key());
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key();
        assert_eq!(
            ActionAttestation::open(&wire, &other),
            Err(AttestError::BadSignature)
        );
    }

    #[test]
    fn account_and_action_binding_is_enforced() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        assert_eq!(
            verify_bound(&wire, &vk, &[0xCC; 16], &[0xBB; 32], 999),
            Err(AttestError::AccountMismatch)
        );
        assert_eq!(
            verify_bound(&wire, &vk, &[0xAA; 16], &[0xCC; 32], 999),
            Err(AttestError::ActionMismatch)
        );
    }

    #[test]
    fn expiry_is_enforced() {
        let sk = key();
        let wire = sample().sign(&sk); // expires_at = 1000
        let vk = sk.verifying_key();
        // exactly at expiry is still valid; one second past is not.
        assert!(verify_bound(&wire, &vk, &[0xAA; 16], &[0xBB; 32], 1_000).is_ok());
        assert_eq!(
            verify_bound(&wire, &vk, &[0xAA; 16], &[0xBB; 32], 1_001),
            Err(AttestError::Expired)
        );
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let sk = key();
        let vk = sk.verifying_key();
        assert_eq!(
            ActionAttestation::open(&[0u8; 10], &vk),
            Err(AttestError::BadLength { got: 10 })
        );
        let mut wire = sample().sign(&sk);
        wire[0] = 9; // bad version
        assert_eq!(ActionAttestation::open(&wire, &vk), Err(AttestError::BadVersion(9)));
    }

    #[test]
    fn distinct_actions_do_not_cross_verify() {
        // An attestation minted for action X must not satisfy a check for
        // action Y — the core property that binds a removal to its leaf.
        let sk = key();
        let vk = sk.verifying_key();
        let wire_x = ActionAttestation::new([0xAA; 16], [0x01; 32], 1_000).sign(&sk);
        assert_eq!(
            verify_bound(&wire_x, &vk, &[0xAA; 16], &[0x02; 32], 999),
            Err(AttestError::ActionMismatch)
        );
    }
}
