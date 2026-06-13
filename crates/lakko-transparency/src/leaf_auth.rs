// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Genesis / fresh-start leaf self-authorisation (G-30 #2, ADR-0019 Sprint 2).
//!
//! A KT-log leaf binds `(username_hash, version, identity_pubkey)`. For a normal
//! ROTATION (a prior entry exists for the account) the authorisation is the
//! [`SupersessionProof`](crate::SupersessionProof) signed by the PREVIOUS key —
//! verified client-side by [`verify_supersession_chain`](crate::verify_supersession_chain).
//! But two cases have no predecessor key to sign them:
//!
//! - the **genesis** entry (the first time an account appears), and
//! - a **lost-everything fresh-start** mint (ADR-0017: a new identity with NO
//!   prior key, published without a supersession proof, `is_reset = true`).
//!
//! For those, this module provides a *self-signature*: the holder of the leaf's
//! OWN key signs the account-to-key binding, proving the entry was authorised by
//! whoever controls the pubkey IN the leaf — not fabricated by a keyless server.
//!
//! ## Version-independent by design
//!
//! The self-signature covers `(username_hash, identity_pubkey)` and **NOT** the
//! leaf version. The version is assigned by the operator (`MAX(version) + 1`
//! under the append lock), so a client minting a fresh-start reset cannot know
//! it in advance. Omitting it is sound: the signature proves the account-to-key
//! authorisation; monotonicity is enforced elsewhere (the operator's `MAX+1`,
//! the peer's `pinnedLogVersion` floor, and the rotation chain). An attacker
//! who replays a valid self-sig can only re-assert the SAME `(account_key,
//! pubkey)` binding — idempotent in the log — and can never substitute a
//! different pubkey (the signature is over that exact key).
//!
//! ## TOFU, not anti-squatting
//!
//! This is TOFU at the log level. It does NOT prevent first-entry squatting (an
//! attacker able to append could self-sign a leaf for a victim's account_key
//! with the attacker's OWN key — a self-consistent self-signature). What it buys
//! is that every leaf is self-certifying, so the operator can REJECT a
//! fabricated (keyless) leaf, and the squatting residual is DETECTED via CONIKS
//! self-monitoring + the ADR-0016 fail-closed peer gate (see ADR-0019).
//!
//! The signing input is domain-separated from the supersession proof, so a
//! signature of one kind can never verify as the other.

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};

use crate::error::{Error, Result};

/// Domain separator for the genesis self-signature. Distinct from the
/// supersession proof's `lakko/identity-supersession/v1` domain so the two
/// signature kinds are cryptographically non-interchangeable.
const LEAF_GENESIS_DOMAIN: &[u8] = b"lakko/transparency/leaf-genesis/v1";

/// The signing pre-image for a leaf's genesis self-signature:
/// `DOMAIN ‖ username_hash(32) ‖ pubkey_len(4 LE) ‖ identity_pubkey`.
///
/// Deliberately version-independent (see the module docs): the version is
/// operator-assigned and unknown to a fresh-start minter, so it is not signed.
fn genesis_signing_input(username_hash: &[u8; 32], identity_pubkey: &[u8]) -> Vec<u8> {
    let pk_len = u32::try_from(identity_pubkey.len()).unwrap_or(u32::MAX);
    let mut payload =
        Vec::with_capacity(LEAF_GENESIS_DOMAIN.len() + 32 + 4 + identity_pubkey.len());
    payload.extend_from_slice(LEAF_GENESIS_DOMAIN);
    payload.extend_from_slice(username_hash);
    payload.extend_from_slice(&pk_len.to_le_bytes());
    payload.extend_from_slice(identity_pubkey);
    payload
}

/// Sign a genesis / fresh-start leaf's account-to-key binding with the leaf's
/// OWN identity key.
///
/// The caller MUST pass the signing key whose public half is `identity_pubkey`;
/// [`verify_leaf_genesis`] checks the signature against that embedded key, so a
/// mismatch yields an unverifiable signature. Returns the 64-byte Ed25519
/// signature to carry as the leaf's `leaf_auth`.
#[must_use]
pub fn sign_leaf_genesis(
    signing_key: &SigningKey,
    username_hash: &[u8; 32],
    identity_pubkey: &[u8],
) -> [u8; SIGNATURE_LEN] {
    signing_key
        .sign(&genesis_signing_input(username_hash, identity_pubkey))
        .to_bytes()
}

/// Verify a genesis self-signature against the embedded `identity_pubkey`.
///
/// Self-certifying: the signature over `DOMAIN ‖ username_hash ‖ pubkey` must
/// verify against `identity_pubkey`. A pass proves the holder of that key
/// authorised this `(account_key, pubkey)` binding.
///
/// # Errors
///
/// - [`Error::LeafGenesisBadKey`] if `identity_pubkey` is not a valid Ed25519
///   verifying key.
/// - [`Error::LeafGenesisBadLength`] if `sig` is not a 64-byte signature.
/// - [`Error::LeafGenesisInvalid`] if the signature does not verify against the
///   embedded key.
pub fn verify_leaf_genesis(
    sig: &[u8],
    username_hash: &[u8; 32],
    identity_pubkey: &[u8],
) -> Result<()> {
    let vk = VerifyingKey::from_bytes(identity_pubkey).map_err(|_| Error::LeafGenesisBadKey)?;
    let signature = Signature::from_bytes(sig).map_err(|_| Error::LeafGenesisBadLength)?;
    vk.verify(
        &genesis_signing_input(username_hash, identity_pubkey),
        &signature,
    )
    .map_err(|_| Error::LeafGenesisInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account_key(name: &str) -> [u8; 32] {
        lakko_crypto::hash::blake3(name.as_bytes())
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let pk = sk.verifying_key().to_bytes();
        let ak = account_key("alice");
        let sig = sign_leaf_genesis(&sk, &ak, &pk);
        verify_leaf_genesis(&sig, &ak, &pk).expect("genesis self-sig verifies");
    }

    #[test]
    fn version_is_not_bound() {
        // The signature is version-independent by design: the SAME
        // (account_key, pubkey) binding verifies regardless of the leaf
        // version the operator later assigns. (Re-presenting the same key is
        // idempotent in the log; an attacker cannot substitute a new pubkey.)
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let pk = sk.verifying_key().to_bytes();
        let ak = account_key("alice");
        let sig = sign_leaf_genesis(&sk, &ak, &pk);
        // No version anywhere — the single sig is the whole authorisation.
        verify_leaf_genesis(&sig, &ak, &pk).expect("verifies independent of version");
    }

    #[test]
    fn wrong_signer_key_fails() {
        // Sign with key A but embed key B — the verify is against the embedded
        // key, so it must fail.
        let signer = SigningKey::from_seed(&[0x11; 32]);
        let other = SigningKey::from_seed(&[0x99; 32]);
        let other_pk = other.verifying_key().to_bytes();
        let ak = account_key("alice");
        let sig = sign_leaf_genesis(&signer, &ak, &other_pk);
        assert!(matches!(
            verify_leaf_genesis(&sig, &ak, &other_pk),
            Err(Error::LeafGenesisInvalid)
        ));
    }

    #[test]
    fn tampered_account_key_fails() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let pk = sk.verifying_key().to_bytes();
        let sig = sign_leaf_genesis(&sk, &account_key("alice"), &pk);
        assert!(matches!(
            verify_leaf_genesis(&sig, &account_key("bob"), &pk),
            Err(Error::LeafGenesisInvalid)
        ));
    }

    #[test]
    fn tampered_signature_byte_fails() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let pk = sk.verifying_key().to_bytes();
        let ak = account_key("alice");
        let mut sig = sign_leaf_genesis(&sk, &ak, &pk);
        sig[0] ^= 0x01;
        assert!(matches!(
            verify_leaf_genesis(&sig, &ak, &pk),
            Err(Error::LeafGenesisInvalid)
        ));
    }

    #[test]
    fn wrong_length_signature_rejected() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let pk = sk.verifying_key().to_bytes();
        let ak = account_key("alice");
        let sig = sign_leaf_genesis(&sk, &ak, &pk);
        assert!(matches!(
            verify_leaf_genesis(&sig[..63], &ak, &pk),
            Err(Error::LeafGenesisBadLength)
        ));
    }

    #[test]
    fn non_ed25519_leaf_key_rejected() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let pk = sk.verifying_key().to_bytes();
        let ak = account_key("alice");
        let sig = sign_leaf_genesis(&sk, &ak, &pk);
        // A 31-byte pubkey is not a valid Ed25519 key.
        assert!(matches!(
            verify_leaf_genesis(&sig, &ak, &[0u8; 31]),
            Err(Error::LeafGenesisBadKey)
        ));
    }

    #[test]
    fn substituted_pubkey_fails() {
        // A self-sig for key A's binding must not verify for a different key B
        // (the signature is over the exact pubkey).
        let a = SigningKey::from_seed(&[0x11; 32]);
        let b = SigningKey::from_seed(&[0x22; 32]);
        let ak = account_key("alice");
        let sig = sign_leaf_genesis(&a, &ak, &a.verifying_key().to_bytes());
        assert!(verify_leaf_genesis(&sig, &ak, &b.verifying_key().to_bytes()).is_err());
    }
}
