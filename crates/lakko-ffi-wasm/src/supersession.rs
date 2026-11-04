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

/// Sign a two-way conversation link (ADR-0074 section 3.7): both keys state
/// that the account's conversations are bound to the first and the directory
/// publishes the second. Pass each identity's `ed25519Seed`. Returns the
/// 201-byte link to upload with the identity bundle.
///
/// Throws if a seed is not 32 bytes, or the two seeds are one key.
#[wasm_bindgen(js_name = signConversationLink)]
pub fn sign_conversation_link(
    conversation_ed25519_seed: &[u8],
    current_ed25519_seed: &[u8],
    issued_at_unix_secs: i64,
) -> Result<Vec<u8>, JsError> {
    let conversation = signing_key_from_seed(conversation_ed25519_seed)?;
    let current = signing_key_from_seed(current_ed25519_seed)?;
    lakko_transparency::ConversationLink::sign(&conversation, &current, issued_at_unix_secs)
        .map(|link| link.to_vec())
        .map_err(|e| JsError::new(&format!("conversation link: {e}")))
}

/// Verify a conversation link against the key the caller left pinned for the
/// account and return the 32-byte conversation key it names, which the caller
/// records as a proven predecessor of that pin.
///
/// Throws if `trustedCurrentKey` is not 32 bytes, or the link is malformed,
/// names another current key, or either signature does not verify. A caller
/// ignores a refused link: it changes nothing.
#[wasm_bindgen(js_name = verifyConversationLink)]
pub fn verify_conversation_link(
    link: &[u8],
    trusted_current_key: &[u8],
) -> Result<Vec<u8>, JsError> {
    let current: [u8; 32] = trusted_current_key
        .try_into()
        .map_err(|_| JsError::new("trusted current key must be 32 bytes"))?;
    lakko_transparency::verify_conversation_link(link, &current)
        .map(|key| key.to_vec())
        .map_err(|e| JsError::new(&format!("conversation link: {e}")))
}

/// Sign a genesis / fresh-start KT-log leaf's account-to-key binding with the
/// leaf's OWN identity key (G-30 #2, ADR-0019). Used for the first-ever publish
/// and a lost-everything fresh-start mint (a new key with NO supersession
/// proof). Pass the identity's `ed25519Seed`; the leaf's pubkey is derived from
/// it internally so the self-signature always matches the embedded key. The
/// signature is version-INDEPENDENT (the version is operator-assigned and
/// unknown to a fresh-start minter). Returns the 64-byte signature to carry as
/// the leaf's `leaf_auth`.
///
/// Throws if the seed is not 32 bytes or `usernameHash` is not 32 bytes.
#[wasm_bindgen(js_name = signLeafGenesis)]
pub fn sign_leaf_genesis(ed25519_seed: &[u8], username_hash: &[u8]) -> Result<Vec<u8>, JsError> {
    let sk = signing_key_from_seed(ed25519_seed)?;
    let username_hash: [u8; 32] = username_hash
        .try_into()
        .map_err(|_| JsError::new("username_hash must be 32 bytes"))?;
    let pubkey = sk.verifying_key().to_bytes();
    Ok(lakko_transparency::sign_leaf_genesis(&sk, &username_hash, &pubkey).to_vec())
}

/// Verify a genesis self-signature against the embedded `identityPubkey`
/// (G-30 #2, ADR-0019). A pass proves the holder of `identityPubkey` authorised
/// this `(usernameHash, pubkey)` account-to-key binding.
///
/// Throws if `usernameHash` is not 32 bytes, the embedded `identityPubkey` is
/// not a valid Ed25519 key, `sig` is not 64 bytes, or the signature does not
/// verify.
#[wasm_bindgen(js_name = verifyLeafGenesis)]
pub fn verify_leaf_genesis(
    sig: &[u8],
    username_hash: &[u8],
    identity_pubkey: &[u8],
) -> Result<(), JsError> {
    let username_hash: [u8; 32] = username_hash
        .try_into()
        .map_err(|_| JsError::new("username_hash must be 32 bytes"))?;
    lakko_transparency::verify_leaf_genesis(sig, &username_hash, identity_pubkey)
        .map_err(|e| JsError::new(&format!("leaf genesis verify: {e}")))
}

/// Verify a witness co-signature (ADR-0020) over a transparency root's
/// `(treeRoot, logSize)` against the CLIENT-PINNED `witnessKey` — the defence
/// against an operator split-view (G-30 #1). A client requires this AND the
/// operator's own root signature before trusting a root. `cosig` is the
/// self-describing wire blob served alongside the root.
///
/// Throws if `expectedTreeRoot` is not 32 bytes, the co-sig is the wrong length
/// / version, its embedded root differs from `expectedTreeRoot` /
/// `expectedLogSize`, or the signature does not verify under `witnessKey`.
#[wasm_bindgen(js_name = verifyWitnessCosig)]
pub fn verify_witness_cosig(
    cosig: &[u8],
    witness_key: &[u8],
    expected_tree_root: &[u8],
    expected_log_size: u64,
) -> Result<(), JsError> {
    let expected_tree_root: [u8; 32] = expected_tree_root
        .try_into()
        .map_err(|_| JsError::new("expected_tree_root must be 32 bytes"))?;
    lakko_transparency::verify_witness_cosig(
        cosig,
        witness_key,
        &expected_tree_root,
        expected_log_size,
    )
    .map_err(|e| JsError::new(&format!("witness co-sig verify: {e}")))
}

/// Verify a K-of-N witness QUORUM (ADR-0039, G-30 finding-1) over a transparency
/// root's `(treeRoot, logSize)`: at least `threshold` DISTINCT client-pinned
/// witnesses must each have co-signed the SAME root. The K-of-N generalisation of
/// `verifyWitnessCosig` — the client pins `N` witness keys and requires `K` of
/// them, so an operator can no longer fork the log by colluding with a single
/// witness (it must corrupt `K`).
///
/// The two lists cross the boundary FLAT-CONCATENATED (the role-table FFI
/// convention): `cosigsFlat` is `N` co-signature blobs each exactly
/// `WITNESS_COSIG_LEN` (105) bytes, `pinnedKeysFlat` is `M` witness verifying keys
/// each 32 bytes; both are sliced into fixed-length chunks on the Rust side. The
/// quorum counts DISTINCT pinned keys, never co-signatures, so a replayed
/// co-signature cannot inflate the tally (`cosigsFlat` is attacker-controlled).
///
/// Throws if `expectedTreeRoot` is not 32 bytes, `cosigsFlat` is not a multiple of
/// 105 bytes, `pinnedKeysFlat` is not a multiple of 32 bytes, `threshold` is `0`
/// (rejected fail-closed — invariant #4), or fewer than `threshold` distinct
/// pinned keys produced a valid co-signature over the expected root.
#[wasm_bindgen(js_name = verifyWitnessQuorum)]
pub fn verify_witness_quorum(
    cosigs_flat: &[u8],
    pinned_keys_flat: &[u8],
    threshold: u32,
    expected_tree_root: &[u8],
    expected_log_size: u64,
) -> Result<(), JsError> {
    // The verdict lives in the `JsError`-free `_inner` (a plain `String` error so
    // the negative paths are host-testable — `JsError::new` panics on a non-wasm
    // target). This thin wrapper only maps that error to a `JsError`.
    verify_witness_quorum_inner(
        cosigs_flat,
        pinned_keys_flat,
        threshold,
        expected_tree_root,
        expected_log_size,
    )
    .map_err(|msg| JsError::new(&msg))
}

/// The `JsError`-free core of [`verify_witness_quorum`]. Returns `Err(String)` on
/// any failure so the negative paths run on the host target (the
/// `#[wasm_bindgen]` wrapper maps `String` → `JsError`).
fn verify_witness_quorum_inner(
    cosigs_flat: &[u8],
    pinned_keys_flat: &[u8],
    threshold: u32,
    expected_tree_root: &[u8],
    expected_log_size: u64,
) -> Result<(), String> {
    let expected_tree_root: [u8; 32] = expected_tree_root
        .try_into()
        .map_err(|_| "expected_tree_root must be 32 bytes".to_string())?;
    if cosigs_flat.len() % lakko_transparency::WITNESS_COSIG_LEN != 0 {
        return Err(format!(
            "cosigs must be a multiple of {} bytes",
            lakko_transparency::WITNESS_COSIG_LEN
        ));
    }
    if pinned_keys_flat.len() % 32 != 0 {
        return Err("pinned keys must be a multiple of 32 bytes".to_string());
    }
    let cosig_refs: Vec<&[u8]> = cosigs_flat
        .chunks_exact(lakko_transparency::WITNESS_COSIG_LEN)
        .collect();
    let key_refs: Vec<&[u8]> = pinned_keys_flat.chunks_exact(32).collect();
    lakko_transparency::verify_witness_quorum(
        &cosig_refs,
        &key_refs,
        threshold as usize,
        &expected_tree_root,
        expected_log_size,
    )
    .map_err(|e| format!("witness quorum verify: {e}"))
}

/// Verify an auth-attested genesis attestation's full account / usernameHash /
/// pubkey binding against the CLIENT-PINNED auth attest verifying key (ADR-0036,
/// closes the G-30 finding-2 squatting residual). A pass proves the party that
/// OPAQUE-authenticated as `expectedAccountId` presented `expectedPubkey` as its
/// genesis identity key (bridged to the KT-log leaf by `expectedUsernameHash`) —
/// so only the account holder could have minted it.
///
/// This is the CLIENT first-observation path: it checks the signature plus all
/// four bindings but deliberately does NOT enforce freshness (the attestation is
/// a permanent historical artifact). Verify beside the ADR-0019 self-signature.
///
/// Throws if `attestVerifyingKey` is not a valid Ed25519 key, `expectedAccountId`
/// is not 16 bytes, `expectedUsernameHash` / `expectedPubkey` are not 32 bytes,
/// the attestation is malformed / the wrong length or version, the signature does
/// not verify, or any of the account / usernameHash / pubkey bindings mismatch.
#[wasm_bindgen(js_name = verifyGenesisAttestation)]
pub fn verify_genesis_attestation(
    attestation: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_username_hash: &[u8],
    expected_pubkey: &[u8],
) -> Result<(), JsError> {
    // The verdict lives in the `JsError`-free `_inner` (a plain `String` error so
    // the negative paths are host-testable — `JsError::new` panics on a non-wasm
    // target per wasm-bindgen-0.2's "non-wasm targets" rule). This thin wrapper
    // only maps that error to a `JsError` for the JS boundary.
    verify_genesis_attestation_inner(
        attestation,
        attest_verifying_key,
        expected_account_id,
        expected_username_hash,
        expected_pubkey,
    )
    .map_err(|msg| JsError::new(&msg))
}

/// The `JsError`-free core of [`verify_genesis_attestation`]. Returns
/// `Err(String)` on any failure so the negative paths are exercisable on the host
/// target (the `#[wasm_bindgen]` wrapper maps `String` → `JsError`).
fn verify_genesis_attestation_inner(
    attestation: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_username_hash: &[u8],
    expected_pubkey: &[u8],
) -> Result<(), String> {
    let verifying = lakko_crypto::signing::VerifyingKey::from_bytes(attest_verifying_key)
        .map_err(|_| "attest verifying key is not a valid Ed25519 key".to_string())?;
    let expected_account: [u8; 16] = expected_account_id
        .try_into()
        .map_err(|_| "expected_account_id must be 16 bytes".to_string())?;
    let expected_username_hash: [u8; 32] = expected_username_hash
        .try_into()
        .map_err(|_| "expected_username_hash must be 32 bytes".to_string())?;
    let expected_pubkey: [u8; 32] = expected_pubkey
        .try_into()
        .map_err(|_| "expected_pubkey must be 32 bytes".to_string())?;
    lakko_attest::verify_genesis_binding(
        attestation,
        &verifying,
        &expected_account,
        &expected_username_hash,
        &expected_pubkey,
    )
    .map(|_| ())
    .map_err(|e| format!("genesis attestation verify: {e}"))
}

/// Derive the transparency `usernameHash` for an `accountId`
/// (`BLAKE3("lakko/transparency/account/v1" ‖ accountId)`) — the same derivation
/// discovery uses on append. A client computes the `expectedUsernameHash` it
/// feeds to `verifyGenesisAttestation` from the target's `accountId` (obtained
/// via the `username → accountId` discovery lookup) without re-implementing
/// BLAKE3. Returns the 32-byte hash.
#[wasm_bindgen(js_name = accountUsernameHash)]
#[must_use]
pub fn account_username_hash(account_id: &[u8]) -> Vec<u8> {
    lakko_transparency::account_username_hash(account_id).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    fn signed_genesis(
        signing: &SigningKey,
        account: &[u8; 16],
        username_hash: &[u8; 32],
        pubkey: &[u8; 32],
    ) -> Vec<u8> {
        lakko_attest::GenesisAttestation::new(*account, *username_hash, *pubkey, i64::MAX)
            .sign(signing)
            .to_vec()
    }

    // Exercise the `JsError`-free core so the negative paths run on the host
    // (`JsError::new` panics on a non-wasm target).
    #[test]
    fn genesis_attestation_verify_roundtrips() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let verifying = signing.verifying_key().to_bytes();
        let account = [0xAA; 16];
        let username_hash = [0xBB; 32];
        let pubkey = [0xCC; 32];
        let wire = signed_genesis(&signing, &account, &username_hash, &pubkey);
        verify_genesis_attestation_inner(&wire, &verifying, &account, &username_hash, &pubkey)
            .expect("genesis attestation verifies");
        // Wrong expected pubkey → binding fails.
        assert!(verify_genesis_attestation_inner(
            &wire,
            &verifying,
            &account,
            &username_hash,
            &[0x00; 32]
        )
        .is_err());
        // Signed by another key → signature does not verify under the pinned key.
        let other = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes();
        assert!(
            verify_genesis_attestation_inner(&wire, &other, &account, &username_hash, &pubkey)
                .is_err()
        );
        // Malformed / bad-length blob.
        assert!(verify_genesis_attestation_inner(
            &[0u8; 10],
            &verifying,
            &account,
            &username_hash,
            &pubkey
        )
        .is_err());
    }

    #[test]
    fn account_username_hash_matches_transparency() {
        let account_id = [0x07u8; 16];
        assert_eq!(
            account_username_hash(&account_id),
            lakko_transparency::account_username_hash(&account_id).to_vec()
        );
    }

    // Exercise the `JsError`-free quorum core on the host (ADR-0039, G-30
    // finding-1). Signs real witness co-sigs via `lakko_transparency` and drives
    // them through the flat-concatenated FFI split.
    #[test]
    fn witness_quorum_k_of_n_flat_concat() {
        let tree_root = [0xABu8; 32];
        let log_size = 7u64;
        let sks: Vec<SigningKey> = [1u8, 2, 3]
            .iter()
            .map(|s| SigningKey::from_seed(&[*s; 32]))
            .collect();
        let pinned_flat: Vec<u8> = sks
            .iter()
            .flat_map(|sk| sk.verifying_key().to_bytes())
            .collect();
        let cosigs_flat: Vec<u8> = sks
            .iter()
            .flat_map(|sk| lakko_transparency::sign_witness_cosig(sk, &tree_root, log_size))
            .collect();

        // 3-of-3 and 1-of-3 both met by three distinct valid co-sigs.
        verify_witness_quorum_inner(&cosigs_flat, &pinned_flat, 3, &tree_root, log_size)
            .expect("3-of-3 met");
        verify_witness_quorum_inner(&cosigs_flat, &pinned_flat, 1, &tree_root, log_size)
            .expect("1-of-3 met");

        // Only two witnesses actually co-signed (K-1) → threshold 3 not met.
        let two = &cosigs_flat[..2 * lakko_transparency::WITNESS_COSIG_LEN];
        assert!(verify_witness_quorum_inner(two, &pinned_flat, 3, &tree_root, log_size).is_err());

        // Zero threshold is rejected fail-closed even with valid co-sigs present.
        assert!(
            verify_witness_quorum_inner(&cosigs_flat, &pinned_flat, 0, &tree_root, log_size)
                .is_err()
        );

        // A non-multiple-of-105 cosig buffer and a non-multiple-of-32 key buffer
        // are both rejected before any signature check.
        assert!(verify_witness_quorum_inner(
            &cosigs_flat[..cosigs_flat.len() - 1],
            &pinned_flat,
            1,
            &tree_root,
            log_size
        )
        .is_err());
        assert!(verify_witness_quorum_inner(
            &cosigs_flat,
            &pinned_flat[..pinned_flat.len() - 1],
            1,
            &tree_root,
            log_size
        )
        .is_err());

        // Wrong expected tree_root → no co-sig binds, quorum not met.
        assert!(verify_witness_quorum_inner(
            &cosigs_flat,
            &pinned_flat,
            1,
            &[0xCDu8; 32],
            log_size
        )
        .is_err());
    }
}
