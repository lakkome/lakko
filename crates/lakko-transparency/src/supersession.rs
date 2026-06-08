// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Identity supersession proof (G-4, security invariant #6, ADR-0007 §6).
//!
//! When an account's deterministic AccountIdentity rotates — e.g. a password
//! change rotates the OPAQUE export key, which rotates the identity key — the
//! holder of the **old** identity key signs a *supersession proof* that
//! authorises the **new** key as its successor. A peer that already trusts
//! the old key (from a prior TOFU pin) verifies the proof and bridges trust
//! to the new key WITHOUT a fresh TOFU bounce.
//!
//! The proof is published to the key-transparency log (a later sprint) so the
//! operator cannot equivocate — show one successor to one peer and a
//! different successor to another. This module is the proof primitive only.
//!
//! Invariant #6 ("identity rotation only via a KT-log supersession proof; old
//! keys do not come back") is enforced two ways here:
//!
//! - **one-directional** — the proof is signed by the *old* key over the
//!   *new* key, so only the legitimate prior key-holder can authorise a
//!   successor; a stranger cannot mint a transition.
//! - **time-bounded** — the validity window is capped at
//!   [`MAX_VALIDITY_SECS`] (30 days), so a leaked old key cannot
//!   indefinitely re-authorise arbitrary successors.
//!
//! Wire layout (all integers big-endian):
//!
//! ```text
//!   version(1) = 1
//!   old_pubkey(32)
//!   new_pubkey(32)
//!   issued_at_unix_secs(8, i64)
//!   expires_at_unix_secs(8, i64)
//!   signature(64)        // old key over: DOMAIN ‖ version ‖ old ‖ new ‖ issued ‖ expires
//! ```
//!
//! The signature covers every field, so tampering with any byte invalidates
//! it. [`verify_supersession`] is the strict entry point: signature (against
//! the *embedded* old key) → the embedded old key matches the caller's
//! trusted key → a sane validity window → freshness.
//!
//! **Note for the KT-log integration sprint (security review, G-4).** The
//! signature verifies via `lakko_crypto::signing` Ed25519, whose default
//! `verify` enforces scalar-malleability but NOT the group-element /
//! weak-key checks that only `verify_strict` does. That is harmless for the
//! standalone primitive — a malleated signature still authenticates the
//! *same* claims — but it means the signature **bytes are not unique** for a
//! given transition. When the proof is carried in a KT-log entry, any
//! dedup / identity / inclusion key MUST be derived from the **claim tuple**
//! `(old_pubkey, new_pubkey, issued_at, expires_at)`, never from the raw
//! signature bytes (otherwise an attacker holding one valid proof could mint
//! a second distinct-bytes proof for the same transition). Alternatively the
//! integration can switch to a strict verify. The KT entry must also tie
//! `old_pubkey` to the account's `username_hash` so the per-account pin this
//! module delegates to is itself non-equivocable.

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};

use crate::error::{Error, Result};

/// Domain separator mixed into the signing input.
const DOMAIN: &[u8] = b"lakko/identity-supersession/v1";

/// Wire-format version. The verifier refuses any other value.
const VERSION: u8 = 1;

/// Length of a big-endian `i64` timestamp.
const TS_LEN: usize = 8;

/// Total serialized supersession-proof length.
pub const SUPERSESSION_PROOF_LEN: usize =
    1 + PUBLIC_KEY_LEN + PUBLIC_KEY_LEN + TS_LEN + TS_LEN + SIGNATURE_LEN;

/// Maximum validity window for a supersession proof — 30 days. Bounds how
/// long a leaked old key can re-authorise a successor (invariant #6).
pub const MAX_VALIDITY_SECS: i64 = 30 * 24 * 60 * 60;

/// The authenticated claims carried by a supersession proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupersessionProof {
    /// The retiring identity public key (the one peers already trust).
    pub old_pubkey: [u8; PUBLIC_KEY_LEN],
    /// The successor identity public key being authorised.
    pub new_pubkey: [u8; PUBLIC_KEY_LEN],
    /// When the proof was issued, unix seconds.
    pub issued_at_unix_secs: i64,
    /// Absolute expiry, unix seconds (`<= issued + 30 days`).
    pub expires_at_unix_secs: i64,
}

impl SupersessionProof {
    /// Construct proof claims to be signed by the old key.
    #[must_use]
    pub fn new(
        old_pubkey: [u8; PUBLIC_KEY_LEN],
        new_pubkey: [u8; PUBLIC_KEY_LEN],
        issued_at_unix_secs: i64,
        expires_at_unix_secs: i64,
    ) -> Self {
        Self {
            old_pubkey,
            new_pubkey,
            issued_at_unix_secs,
            expires_at_unix_secs,
        }
    }

    /// The exact bytes the signature covers (domain-separated).
    fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            DOMAIN.len() + 1 + PUBLIC_KEY_LEN + PUBLIC_KEY_LEN + TS_LEN + TS_LEN,
        );
        buf.extend_from_slice(DOMAIN);
        buf.push(VERSION);
        buf.extend_from_slice(&self.old_pubkey);
        buf.extend_from_slice(&self.new_pubkey);
        buf.extend_from_slice(&self.issued_at_unix_secs.to_be_bytes());
        buf.extend_from_slice(&self.expires_at_unix_secs.to_be_bytes());
        buf
    }

    /// Serialize + sign these claims with the **old** identity signing key.
    ///
    /// The caller is responsible for `old_signing_key`'s public half equalling
    /// [`Self::old_pubkey`] (otherwise the proof will not verify) and for the
    /// validity window being sane — [`verify_supersession`] re-checks both.
    #[must_use]
    pub fn sign(&self, old_signing_key: &SigningKey) -> [u8; SUPERSESSION_PROOF_LEN] {
        let sig = old_signing_key.sign(&self.signing_input());
        let mut out = [0u8; SUPERSESSION_PROOF_LEN];
        out[0] = VERSION;
        let mut off = 1;
        out[off..off + PUBLIC_KEY_LEN].copy_from_slice(&self.old_pubkey);
        off += PUBLIC_KEY_LEN;
        out[off..off + PUBLIC_KEY_LEN].copy_from_slice(&self.new_pubkey);
        off += PUBLIC_KEY_LEN;
        out[off..off + TS_LEN].copy_from_slice(&self.issued_at_unix_secs.to_be_bytes());
        off += TS_LEN;
        out[off..off + TS_LEN].copy_from_slice(&self.expires_at_unix_secs.to_be_bytes());
        off += TS_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the signature against the **embedded**
    /// old public key. Returns the authenticated claims.
    ///
    /// This proves "the holder of `old_pubkey` signed this transition", but
    /// does NOT check that `old_pubkey` is the key the caller trusts, nor the
    /// validity window — use [`verify_supersession`] for the strict path.
    ///
    /// # Errors
    ///
    /// [`Error::SupersessionBadLength`] / [`Error::SupersessionBadVersion`] on
    /// a malformed envelope; [`Error::SupersessionBadSignature`] if the
    /// embedded key or the signature is malformed, or the signature does not
    /// verify.
    pub fn open(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != SUPERSESSION_PROOF_LEN {
            return Err(Error::SupersessionBadLength { got: bytes.len() });
        }
        if bytes[0] != VERSION {
            return Err(Error::SupersessionBadVersion(bytes[0]));
        }
        let mut off = 1;
        let mut old_pubkey = [0u8; PUBLIC_KEY_LEN];
        old_pubkey.copy_from_slice(&bytes[off..off + PUBLIC_KEY_LEN]);
        off += PUBLIC_KEY_LEN;
        let mut new_pubkey = [0u8; PUBLIC_KEY_LEN];
        new_pubkey.copy_from_slice(&bytes[off..off + PUBLIC_KEY_LEN]);
        off += PUBLIC_KEY_LEN;
        let mut iss = [0u8; TS_LEN];
        iss.copy_from_slice(&bytes[off..off + TS_LEN]);
        off += TS_LEN;
        let mut exp = [0u8; TS_LEN];
        exp.copy_from_slice(&bytes[off..off + TS_LEN]);
        off += TS_LEN;

        let claims = Self {
            old_pubkey,
            new_pubkey,
            issued_at_unix_secs: i64::from_be_bytes(iss),
            expires_at_unix_secs: i64::from_be_bytes(exp),
        };

        let sig = Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN])
            .map_err(|_| Error::SupersessionBadSignature)?;
        let vk =
            VerifyingKey::from_bytes(&old_pubkey).map_err(|_| Error::SupersessionBadSignature)?;
        vk.verify(&claims.signing_input(), &sig)
            .map_err(|_| Error::SupersessionBadSignature)?;
        Ok(claims)
    }

    /// `true` when `now_unix_secs` is strictly past the expiry.
    #[must_use]
    pub fn is_expired(&self, now_unix_secs: i64) -> bool {
        now_unix_secs > self.expires_at_unix_secs
    }

    /// `true` when the validity window is sane: `issued <= expires <=
    /// issued + 30 days` (using saturating arithmetic so a crafted
    /// near-`i64::MAX` issued time cannot overflow the bound check).
    #[must_use]
    pub fn validity_window_ok(&self) -> bool {
        self.expires_at_unix_secs >= self.issued_at_unix_secs
            && self.expires_at_unix_secs
                <= self.issued_at_unix_secs.saturating_add(MAX_VALIDITY_SECS)
    }
}

/// Strictly verify a supersession proof and return the authorised **new**
/// public key. The order is: signature (against the embedded old key) → the
/// embedded old key matches `trusted_old_pubkey` → a sane validity window →
/// freshness.
///
/// `trusted_old_pubkey` is the key the caller already pins for this account
/// (from a prior TOFU / KT-log inclusion). On success the caller may rotate
/// its pin to the returned new key.
///
/// # Errors
///
/// Propagates [`SupersessionProof::open`] errors, then
/// [`Error::SupersessionOldKeyMismatch`] /
/// [`Error::SupersessionInvalidValidityWindow`] /
/// [`Error::SupersessionExpired`] if the (now-authenticated) claims do not
/// bind to the trusted key, have an out-of-range window, or have lapsed.
pub fn verify_supersession(
    bytes: &[u8],
    trusted_old_pubkey: &[u8; PUBLIC_KEY_LEN],
    now_unix_secs: i64,
) -> Result<[u8; PUBLIC_KEY_LEN]> {
    let proof = SupersessionProof::open(bytes)?;
    if &proof.old_pubkey != trusted_old_pubkey {
        return Err(Error::SupersessionOldKeyMismatch);
    }
    if !proof.validity_window_ok() {
        return Err(Error::SupersessionInvalidValidityWindow);
    }
    if proof.is_expired(now_unix_secs) {
        return Err(Error::SupersessionExpired);
    }
    Ok(proof.new_pubkey)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_key() -> SigningKey {
        SigningKey::from_seed(&[0x11; 32])
    }

    fn sample(old: &SigningKey) -> SupersessionProof {
        SupersessionProof::new(
            old.verifying_key().to_bytes(),
            [0xBB; PUBLIC_KEY_LEN],
            1_000,
            1_000 + MAX_VALIDITY_SECS,
        )
    }

    #[test]
    fn sign_then_verify_returns_new_key() {
        let old = old_key();
        let proof = sample(&old);
        let wire = proof.sign(&old);
        assert_eq!(wire.len(), SUPERSESSION_PROOF_LEN);
        let new = verify_supersession(&wire, &old.verifying_key().to_bytes(), 1_500)
            .expect("valid proof verifies");
        assert_eq!(new, [0xBB; PUBLIC_KEY_LEN]);
    }

    #[test]
    fn tampering_any_field_breaks_the_signature() {
        let old = old_key();
        let wire = sample(&old).sign(&old);
        for idx in [1usize, 40, 70, SUPERSESSION_PROOF_LEN - 1] {
            let mut bad = wire;
            bad[idx] ^= 0x01;
            assert!(
                matches!(
                    SupersessionProof::open(&bad),
                    Err(Error::SupersessionBadSignature)
                ),
                "tampering byte {idx} must invalidate the signature"
            );
        }
    }

    #[test]
    fn old_key_must_match_the_trusted_pin() {
        let old = old_key();
        let wire = sample(&old).sign(&old);
        // A peer that trusts a DIFFERENT old key must reject it.
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key().to_bytes();
        assert!(matches!(
            verify_supersession(&wire, &other, 1_500),
            Err(Error::SupersessionOldKeyMismatch)
        ));
    }

    #[test]
    fn a_proof_signed_by_a_stranger_does_not_verify() {
        // An attacker forges old_pubkey = the victim's key, but cannot sign
        // for it. The signature check (against the embedded key) fails.
        let victim = old_key();
        let attacker = SigningKey::from_seed(&[0x99; 32]);
        let claims = SupersessionProof::new(
            victim.verifying_key().to_bytes(), // claim the victim's old key
            [0xCC; PUBLIC_KEY_LEN],
            1_000,
            1_000 + MAX_VALIDITY_SECS,
        );
        // ...but sign with the attacker's key.
        let wire = claims.sign(&attacker);
        assert!(matches!(
            SupersessionProof::open(&wire),
            Err(Error::SupersessionBadSignature)
        ));
    }

    #[test]
    fn expiry_is_enforced() {
        let old = old_key();
        let pk = old.verifying_key().to_bytes();
        let wire = sample(&old).sign(&old); // expires at 1000 + 30d
        assert!(verify_supersession(&wire, &pk, 1_000 + MAX_VALIDITY_SECS).is_ok());
        assert!(matches!(
            verify_supersession(&wire, &pk, 1_000 + MAX_VALIDITY_SECS + 1),
            Err(Error::SupersessionExpired)
        ));
    }

    #[test]
    fn validity_window_must_be_within_thirty_days() {
        let old = old_key();
        let pk = old.verifying_key().to_bytes();
        // expires more than 30 days after issued -> rejected.
        let too_long = SupersessionProof::new(pk, [0xBB; 32], 1_000, 1_000 + MAX_VALIDITY_SECS + 1)
            .sign(&old);
        assert!(matches!(
            verify_supersession(&too_long, &pk, 1_500),
            Err(Error::SupersessionInvalidValidityWindow)
        ));
        // expires before issued -> rejected.
        let inverted = SupersessionProof::new(pk, [0xBB; 32], 5_000, 4_000).sign(&old);
        assert!(matches!(
            verify_supersession(&inverted, &pk, 4_500),
            Err(Error::SupersessionInvalidValidityWindow)
        ));
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let old = old_key();
        assert!(matches!(
            SupersessionProof::open(&[0u8; 10]),
            Err(Error::SupersessionBadLength { got: 10 })
        ));
        let mut wire = sample(&old).sign(&old);
        wire[0] = 9;
        assert!(matches!(
            SupersessionProof::open(&wire),
            Err(Error::SupersessionBadVersion(9))
        ));
    }
}
