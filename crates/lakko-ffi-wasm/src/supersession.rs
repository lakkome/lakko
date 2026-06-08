// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Identity supersession proof surface (G-4, ADR-0007 §6) for the web client.
//!
//! On rotation (e.g. recover-from-words minting a fresh account identity) the
//! OLD identity signs a proof authorising the NEW identity as its successor.
//! Peers verify it against the old key they already pin and rotate their pin to
//! the new key — bridging trust WITHOUT a blind TOFU re-accept (security
//! invariant #6: old keys never silently come back).
//!
//! This is the proof primitive only. The KT-log chain verifier
//! (`verify_supersession_chain`) ties a proof to an operator-signed Merkle
//! inclusion; it consumes log types that have no wasm wire format yet and lands
//! with the transparency service.

use lakko_crypto::signing::SigningKey;
use wasm_bindgen::prelude::*;

/// Build the [`SigningKey`] for an identity from its 32-byte Ed25519 seed (the
/// bytes the `ed25519Seed` getter on an `Identity` returns).
fn signing_key_from_seed(seed: &[u8]) -> Result<SigningKey, JsError> {
    let arr: [u8; 32] = seed
        .try_into()
        .map_err(|_| JsError::new("ed25519 seed must be 32 bytes"))?;
    Ok(SigningKey::from_seed(&arr))
}

/// Sign an identity supersession proof (G-4): the OLD identity authorises the
/// NEW identity as its successor, valid in `[issuedAtUnixSecs, expiresAtUnixSecs]`
/// (the verifier additionally caps the window at 30 days). Pass each identity's
/// `ed25519Seed`; both public keys are derived internally so the proof's
/// embedded old key always matches the signing key. Returns the wire-form proof
/// to publish alongside the rotation.
#[wasm_bindgen(js_name = signSupersessionProof)]
pub fn sign_supersession_proof(
    old_ed25519_seed: &[u8],
    new_ed25519_seed: &[u8],
    issued_at_unix_secs: i64,
    expires_at_unix_secs: i64,
) -> Result<Vec<u8>, JsError> {
    let old_sk = signing_key_from_seed(old_ed25519_seed)?;
    let new_sk = signing_key_from_seed(new_ed25519_seed)?;
    let proof = lakko_transparency::SupersessionProof::new(
        old_sk.verifying_key().to_bytes(),
        new_sk.verifying_key().to_bytes(),
        issued_at_unix_secs,
        expires_at_unix_secs,
    );
    Ok(proof.sign(&old_sk).to_vec())
}

/// Verify an identity supersession proof (G-4) against the old key the caller
/// already pins for this account. Returns the authorised NEW public key
/// (32 bytes) the caller should re-pin on success.
///
/// Throws if `trustedOldPubkey` is not 32 bytes, or the proof is malformed /
/// not signed by the trusted old key / carries an out-of-range (>30-day)
/// window / has expired relative to `nowUnixSecs`.
#[wasm_bindgen(js_name = verifySupersession)]
pub fn verify_supersession(
    proof_bytes: &[u8],
    trusted_old_pubkey: &[u8],
    now_unix_secs: i64,
) -> Result<Vec<u8>, JsError> {
    let old: [u8; 32] = trusted_old_pubkey
        .try_into()
        .map_err(|_| JsError::new("trusted old pubkey must be 32 bytes"))?;
    let new = lakko_transparency::verify_supersession(proof_bytes, &old, now_unix_secs)
        .map_err(|e| JsError::new(&format!("supersession verify: {e}")))?;
    Ok(new.to_vec())
}
