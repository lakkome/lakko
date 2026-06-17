// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Witness co-signing of KT-log roots (ADR-0020).
//!
//! The KT log is operator-signed + append-only. The G-30 #2 chain verifier
//! (ADR-0019) makes a *leaked-old-key* equivocation detectable, but a malicious
//! **operator** — it holds the root signing key — can still sign two divergent
//! valid roots and show each to a different victim (a **split-view** / fork).
//! Each client's local no-rollback check passes, so the fork is invisible.
//!
//! ADR-0020's defence is an independent **witness** that co-signs every root,
//! but only after verifying (via a Merkle consistency proof, see
//! [`verify_consistency`](crate::verify_consistency)) that the new root
//! append-only-extends its last-co-signed root. A client requires BOTH the
//! operator signature AND a valid witness co-signature before trusting a root.
//! To fork, the operator would need the witness to co-sign two mutually-
//! inconsistent roots — which the honest witness refuses, since two forks
//! cannot both extend its single last-co-signed root.
//!
//! This module is the co-signature primitive: a compact, self-describing wire
//! blob the witness signs and a client verifies. It is domain-separated from
//! the operator's root signature and from every other Lakko Ed25519 use, so a
//! signature of one kind can never verify as another.

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};

use crate::error::{Error, Result};
use crate::merkle::TreeHash;

/// Domain separator for the witness co-signature. Distinct from the operator's
/// root signing payload (which is un-prefixed `tree_root ‖ log_size ‖
/// timestamp`) and from the supersession / leaf-genesis domains.
const WITNESS_DOMAIN: &[u8] = b"lakko/transparency/witness/v1";

/// Co-signature wire-format version.
const VERSION: u8 = 1;

/// Wire length of a witness co-signature:
/// `version(1) ‖ tree_root(32) ‖ log_size(8 BE) ‖ signature(64)`.
pub const WITNESS_COSIG_LEN: usize = 1 + 32 + 8 + SIGNATURE_LEN;

/// The signing pre-image a witness co-signs:
/// `DOMAIN ‖ version ‖ tree_root(32) ‖ log_size(8 BE)`.
///
/// It binds the `(tree_root, log_size)` that uniquely identifies a tree state
/// (two forks differ in `tree_root` at the same `log_size`). The operator's own
/// timestamp is NOT covered — the witness attests the tree state, not the
/// operator's clock.
fn cosig_signing_input(tree_root: &TreeHash, log_size: u64) -> Vec<u8> {
    let mut payload = Vec::with_capacity(WITNESS_DOMAIN.len() + 1 + 32 + 8);
    payload.extend_from_slice(WITNESS_DOMAIN);
    payload.push(VERSION);
    payload.extend_from_slice(tree_root);
    payload.extend_from_slice(&log_size.to_be_bytes());
    payload
}

/// Produce a witness co-signature over a root's `(tree_root, log_size)`.
///
/// The witness MUST have verified the root's consistency with its last-co-signed
/// root BEFORE calling this (that is the split-view defence — see the module
/// docs); this primitive only mints the signature. Returns the self-describing
/// [`WITNESS_COSIG_LEN`]-byte wire blob the operator serves to clients.
#[must_use]
pub fn sign_witness_cosig(
    witness_key: &SigningKey,
    tree_root: &TreeHash,
    log_size: u64,
) -> [u8; WITNESS_COSIG_LEN] {
    let sig = witness_key.sign(&cosig_signing_input(tree_root, log_size));
    let mut out = [0u8; WITNESS_COSIG_LEN];
    out[0] = VERSION;
    let mut off = 1;
    out[off..off + 32].copy_from_slice(tree_root);
    off += 32;
    out[off..off + 8].copy_from_slice(&log_size.to_be_bytes());
    off += 8;
    out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
    out
}

/// Verify a witness co-signature against the pinned `witness_key`, binding it to
/// the `(expected_tree_root, expected_log_size)` the caller is verifying.
///
/// The blob is self-describing; this checks (in order) the length, the version,
/// that its embedded `(tree_root, log_size)` matches what the caller expects (so
/// a co-sig minted for a different root is rejected before the signature check),
/// and finally the Ed25519 signature against `witness_key`.
///
/// # Errors
///
/// - [`Error::WitnessCosigBadLength`] if `bytes` is not [`WITNESS_COSIG_LEN`].
/// - [`Error::WitnessCosigBadVersion`] on an unknown version byte.
/// - [`Error::WitnessCosigBindingMismatch`] if the embedded root differs from
///   the expected one.
/// - [`Error::WitnessCosigInvalid`] if `witness_key` is not a valid Ed25519 key
///   or the signature does not verify.
pub fn verify_witness_cosig(
    bytes: &[u8],
    witness_key: &[u8],
    expected_tree_root: &TreeHash,
    expected_log_size: u64,
) -> Result<()> {
    if bytes.len() != WITNESS_COSIG_LEN {
        return Err(Error::WitnessCosigBadLength { got: bytes.len() });
    }
    if bytes[0] != VERSION {
        return Err(Error::WitnessCosigBadVersion(bytes[0]));
    }

    // Fixed offsets; the length check above guarantees these slices exist. The
    // `map_err` keeps the crate `expect`-free (the conversion cannot actually
    // fail on a length-checked 32-byte slice).
    let embedded_root: TreeHash = bytes[1..33]
        .try_into()
        .map_err(|_| Error::WitnessCosigBadLength { got: bytes.len() })?;
    let size_bytes: [u8; 8] = bytes[33..41]
        .try_into()
        .map_err(|_| Error::WitnessCosigBadLength { got: bytes.len() })?;
    let embedded_size = u64::from_be_bytes(size_bytes);
    let sig_bytes = &bytes[41..WITNESS_COSIG_LEN];

    if &embedded_root != expected_tree_root || embedded_size != expected_log_size {
        return Err(Error::WitnessCosigBindingMismatch);
    }

    let vk = VerifyingKey::from_bytes(witness_key).map_err(|_| Error::WitnessCosigInvalid)?;
    let signature = Signature::from_bytes(sig_bytes).map_err(|_| Error::WitnessCosigInvalid)?;
    vk.verify(
        &cosig_signing_input(&embedded_root, embedded_size),
        &signature,
    )
    .map_err(|_| Error::WitnessCosigInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_at(seed: u8) -> TreeHash {
        lakko_crypto::hash::blake3(&[seed; 16])
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let cosig = sign_witness_cosig(&wk, &tr, 42);
        assert_eq!(cosig.len(), WITNESS_COSIG_LEN);
        verify_witness_cosig(&cosig, &vk, &tr, 42).expect("witness co-sig verifies");
    }

    #[test]
    fn wrong_witness_key_fails() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let other = SigningKey::from_seed(&[0x44; 32])
            .verifying_key()
            .to_bytes();
        let tr = root_at(0xAB);
        let cosig = sign_witness_cosig(&wk, &tr, 42);
        assert!(matches!(
            verify_witness_cosig(&cosig, &other, &tr, 42),
            Err(Error::WitnessCosigInvalid)
        ));
    }

    #[test]
    fn binding_mismatch_wrong_root_or_size_fails() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let cosig = sign_witness_cosig(&wk, &tr, 42);
        // A co-sig spliced onto a DIFFERENT root.
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, &root_at(0xCD), 42),
            Err(Error::WitnessCosigBindingMismatch)
        ));
        // Same root, DIFFERENT size — a fork at a different point.
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, &tr, 43),
            Err(Error::WitnessCosigBindingMismatch)
        ));
    }

    #[test]
    fn tampered_signature_byte_fails() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let mut cosig = sign_witness_cosig(&wk, &tr, 42);
        cosig[WITNESS_COSIG_LEN - 1] ^= 0x01;
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, &tr, 42),
            Err(Error::WitnessCosigInvalid)
        ));
    }

    #[test]
    fn tampered_embedded_root_fails_binding() {
        // Flipping a byte in the embedded root changes both the binding (so it
        // no longer matches the expected root) — caught at the binding gate.
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let mut cosig = sign_witness_cosig(&wk, &tr, 42);
        cosig[1] ^= 0x01; // first byte of the embedded tree_root
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, &tr, 42),
            Err(Error::WitnessCosigBindingMismatch)
        ));
    }

    #[test]
    fn wrong_length_rejected() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let cosig = sign_witness_cosig(&wk, &tr, 42);
        assert!(matches!(
            verify_witness_cosig(&cosig[..WITNESS_COSIG_LEN - 1], &vk, &tr, 42),
            Err(Error::WitnessCosigBadLength { .. })
        ));
    }

    #[test]
    fn unknown_version_rejected() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let mut cosig = sign_witness_cosig(&wk, &tr, 42);
        cosig[0] = 0xFF;
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, &tr, 42),
            Err(Error::WitnessCosigBadVersion(0xFF))
        ));
    }

    #[test]
    fn distinct_domain_from_operator_root_signature() {
        // A witness co-sig's signing pre-image is domain-prefixed, so the same
        // (tree_root, log_size) bytes the OPERATOR signs un-prefixed produce a
        // different message — the two signature kinds never cross-validate.
        let tr = root_at(0xAB);
        let input = cosig_signing_input(&tr, 42);
        assert!(input.starts_with(WITNESS_DOMAIN));
        assert_ne!(input.len(), 32 + 8); // not the operator's bare payload
    }
}
