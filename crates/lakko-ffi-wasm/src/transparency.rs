// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Key-transparency inclusion verifier (KT-log S3b) for the web client.
//!
//! After fetching `GET /transparency/root` (the operator-signed Merkle
//! root), `GET /transparency/latest/:account_key` (the latest entry's
//! `{idx, version, identity_pubkey}`) and `GET /transparency/proof/:idx`
//! (the audit path), a client calls [`verify_log_inclusion`] to prove
//! the identity it is about to trust is committed by the
//! operator-signed log — NOT just whatever the (zero-trust) server
//! happened to return.
//!
//! Two gates, BOTH must pass:
//!
//! 1. [`SignedRoot::verify`](lakko_transparency::SignedRoot::verify)
//!    against the **client-pinned** operator key — a forged root (signed
//!    by anyone but the pinned operator) fails here.
//! 2. [`verify_inclusion`](lakko_transparency::verify_inclusion) — proves
//!    THIS exact `(account_key, identity_pubkey, version)` leaf is at
//!    `leaf_index` of the tree that root commits to. A different pubkey,
//!    version, index, or a tampered audit path fails here.
//!
//! The verdict is bound to the exact tuple the caller cares about, so a
//! malicious server cannot substitute a different identity and pass.
//!
//! ## No-rollback stays client-side
//!
//! This verifier is STATELESS — it does NOT compare `log_size` to a
//! last-seen value. The append-only (no-rollback) check
//! (`new.log_size >= last_seen.log_size`) requires per-account persisted
//! state the client owns; baking it in here would either need the caller
//! to thread the last-seen size through every call (and we would still
//! not persist it) or silently accept any size. The client compares the
//! fetched `log_size` against its own monotone last-seen marker BEFORE
//! (or after) calling this, exactly as the supersession pin store does.

use lakko_crypto::signing;
use lakko_transparency::{InclusionProof, LogEntry, SignedRoot, TreeHash};
use wasm_bindgen::prelude::*;

/// Verify that `entryIdentityPubkey` (at `entryVersion`) for the
/// account-key `entryUsernameHash` is committed by the operator-signed
/// transparency log (KT-log S3b). The decomposed fields mirror the JSON
/// a client decodes from `/transparency/{root,latest,proof}`.
///
/// Runs two gates and returns successfully only if BOTH pass:
///
/// 1. `signedRoot.verify(operator)` — the root is signed by the
///    client-pinned operator key (a forged root fails here).
/// 2. `verify_inclusion(entry, proof, signedRoot)` — this exact
///    `(account_key, pubkey, version)` leaf is at `leafIndex` of that
///    root's tree (a substituted identity / tampered proof fails here).
///
/// The no-rollback (append-only) check is the CALLER's job: compare the
/// fetched `logSize` against the last-seen monotone value before
/// trusting the result. This function is stateless and does not persist
/// or compare any last-seen size.
///
/// # Errors
///
/// Throws a `JsError` if:
/// - any fixed-length field is the wrong length (`treeRoot`/`entryUsernameHash`
///   != 32, `rootSignature` != 64, `auditPath` length not a multiple of 32),
/// - `operatorPubkey` is not 1..=64 bytes or not a valid Ed25519 key,
/// - the root signature does not verify under `operatorPubkey`,
/// - the inclusion proof does not bind the entry to the root.
#[wasm_bindgen(js_name = verifyLogInclusion)]
#[allow(clippy::too_many_arguments)]
pub fn verify_log_inclusion(
    operator_pubkey: &[u8],
    tree_root: &[u8],
    log_size: u64,
    timestamp_unix: u64,
    root_signature: &[u8],
    leaf_index: u64,
    audit_path: &[u8],
    entry_username_hash: &[u8],
    entry_identity_pubkey: &[u8],
    entry_version: u64,
) -> Result<(), JsError> {
    // The whole verdict lives in `verify_log_inclusion_inner` (which
    // returns a plain `String` error so it is host-testable —
    // `JsError::new` panics on a non-wasm target per wasm-bindgen-0.2's
    // "non-wasm targets" rule). This thin wrapper only maps that error to
    // a `JsError` for the JS boundary.
    verify_log_inclusion_inner(
        operator_pubkey,
        tree_root,
        log_size,
        timestamp_unix,
        root_signature,
        leaf_index,
        audit_path,
        entry_username_hash,
        entry_identity_pubkey,
        entry_version,
    )
    .map_err(|msg| JsError::new(&msg))
}

/// The `JsError`-free core of [`verify_log_inclusion`]. Returns
/// `Err(String)` on any failure so the negative paths are exercisable on
/// the host target (the `#[wasm_bindgen]` wrapper maps `String` →
/// `JsError`). See [`verify_log_inclusion`] for the two-gate semantics.
#[allow(clippy::too_many_arguments)]
fn verify_log_inclusion_inner(
    operator_pubkey: &[u8],
    tree_root: &[u8],
    log_size: u64,
    timestamp_unix: u64,
    root_signature: &[u8],
    leaf_index: u64,
    audit_path: &[u8],
    entry_username_hash: &[u8],
    entry_identity_pubkey: &[u8],
    entry_version: u64,
) -> Result<(), String> {
    // The operator key is client-pinned out-of-band; accept the same
    // 1..=64 byte range the log column allows, but it must parse as a
    // valid Ed25519 verifying key (32 bytes canonical).
    if operator_pubkey.is_empty() || operator_pubkey.len() > 64 {
        return Err("operator pubkey must be 1..=64 bytes".to_string());
    }
    let operator = signing::VerifyingKey::from_bytes(operator_pubkey)
        .map_err(|_| "operator pubkey is not a valid Ed25519 key".to_string())?;

    let tree_root: TreeHash = tree_root
        .try_into()
        .map_err(|_| "tree_root must be 32 bytes".to_string())?;
    let signature = signing::Signature::from_bytes(root_signature)
        .map_err(|_| "root_signature must be 64 bytes".to_string())?;
    let username_hash: [u8; 32] = entry_username_hash
        .try_into()
        .map_err(|_| "entry_username_hash must be 32 bytes".to_string())?;

    if audit_path.len() % 32 != 0 {
        return Err("audit_path length must be a multiple of 32 bytes".to_string());
    }
    let audit_path: Vec<TreeHash> = audit_path
        .chunks_exact(32)
        .map(|c| {
            let mut h = [0u8; 32];
            h.copy_from_slice(c);
            h
        })
        .collect();

    let signed_root = SignedRoot {
        tree_root,
        log_size,
        timestamp_unix,
        signature,
    };
    let proof = InclusionProof {
        leaf_index,
        audit_path,
    };
    let entry = LogEntry {
        username_hash,
        identity_pubkey: entry_identity_pubkey.to_vec(),
        version: entry_version,
    };

    // Gate 1: the root is operator-signed. A forged root fails here.
    signed_root
        .verify(&operator)
        .map_err(|e| format!("signed root verify: {e}"))?;

    // Gate 2: this exact entry is committed by that root.
    lakko_transparency::verify_inclusion(&entry, &proof, &signed_root)
        .map_err(|e| format!("inclusion verify: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_transparency::InMemoryLog;

    /// Decomposed fields for one entry's inclusion call, mirroring the
    /// JSON a client decodes from `/transparency/{root,latest,proof}`.
    struct Decomposed {
        operator_pubkey: Vec<u8>,
        tree_root: Vec<u8>,
        log_size: u64,
        timestamp_unix: u64,
        root_signature: Vec<u8>,
        leaf_index: u64,
        audit_path: Vec<u8>,
        username_hash: Vec<u8>,
        identity_pubkey: Vec<u8>,
        version: u64,
    }

    /// Build a 4-entry log, sign the current root, and decompose the
    /// proof for `idx` into the FFI field shape.
    fn build_fixture(idx: u64) -> Decomposed {
        let operator = signing::SigningKey::generate();
        let mut log = InMemoryLog::new(operator);
        for i in 0..4u64 {
            log.append(LogEntry {
                username_hash: lakko_crypto::hash::blake3(format!("u{i}").as_bytes()),
                identity_pubkey: vec![0x10 + u8::try_from(i).unwrap(); 32],
                version: 1,
            });
        }
        let signed = log.sign_current_root(1_700_000_000);
        let proof = log.prove_inclusion(idx).expect("proof");
        let entry = log.entry(idx).expect("entry").clone();

        let mut audit_path = Vec::new();
        for h in &proof.audit_path {
            audit_path.extend_from_slice(h);
        }

        Decomposed {
            operator_pubkey: log.operator_key().to_bytes().to_vec(),
            tree_root: signed.tree_root.to_vec(),
            log_size: signed.log_size,
            timestamp_unix: signed.timestamp_unix,
            root_signature: signed.signature.to_bytes().to_vec(),
            leaf_index: proof.leaf_index,
            audit_path,
            username_hash: entry.username_hash.to_vec(),
            identity_pubkey: entry.identity_pubkey,
            version: entry.version,
        }
    }

    /// Drive the host-testable inner verifier from the decomposed fields.
    /// We call the inner (`String`-error) fn, not the `#[wasm_bindgen]`
    /// wrapper, because `JsError::new` panics on a non-wasm host. The
    /// wrapper is a one-line `.map_err(JsError::new)` over this, so the
    /// two-gate verdict logic is fully covered here.
    fn call(d: &Decomposed) -> Result<(), String> {
        verify_log_inclusion_inner(
            &d.operator_pubkey,
            &d.tree_root,
            d.log_size,
            d.timestamp_unix,
            &d.root_signature,
            d.leaf_index,
            &d.audit_path,
            &d.username_hash,
            &d.identity_pubkey,
            d.version,
        )
    }

    #[test]
    fn valid_inclusion_passes_both_gates() {
        for idx in 0..4 {
            let d = build_fixture(idx);
            call(&d).expect("valid inclusion verifies");
        }
    }

    #[test]
    fn flipped_root_signature_byte_fails() {
        let mut d = build_fixture(2);
        d.root_signature[0] ^= 0x01;
        assert!(
            call(&d).is_err(),
            "tampered root signature must fail gate 1"
        );
    }

    #[test]
    fn flipped_audit_path_byte_fails() {
        let mut d = build_fixture(2);
        assert!(
            !d.audit_path.is_empty(),
            "idx 2 of a 4-leaf tree has a path"
        );
        d.audit_path[0] ^= 0x01;
        assert!(call(&d).is_err(), "tampered audit path must fail gate 2");
    }

    #[test]
    fn wrong_operator_key_fails() {
        let mut d = build_fixture(1);
        d.operator_pubkey = signing::SigningKey::generate()
            .verifying_key()
            .to_bytes()
            .to_vec();
        assert!(call(&d).is_err(), "wrong operator key must fail gate 1");
    }

    #[test]
    fn wrong_entry_identity_pubkey_fails() {
        let mut d = build_fixture(1);
        d.identity_pubkey = vec![0xFF; 32];
        assert!(
            call(&d).is_err(),
            "a substituted identity must fail gate 2 (binding broken)"
        );
    }

    #[test]
    fn wrong_entry_version_fails() {
        let mut d = build_fixture(1);
        d.version = d.version.wrapping_add(1);
        assert!(
            call(&d).is_err(),
            "a different version must fail gate 2 (leaf bytes change)"
        );
    }

    #[test]
    fn malformed_lengths_fail_fast() {
        let base = build_fixture(0);

        // Wrong tree_root length.
        let mut d = build_fixture(0);
        d.tree_root.pop();
        assert!(call(&d).is_err(), "31-byte tree_root rejected");

        // Wrong root_signature length.
        let mut d = build_fixture(0);
        d.root_signature.push(0x00);
        assert!(call(&d).is_err(), "65-byte signature rejected");

        // audit_path not a multiple of 32.
        let mut d = build_fixture(2);
        d.audit_path.push(0x00);
        assert!(call(&d).is_err(), "non-multiple-of-32 audit path rejected");

        // Empty operator key.
        let mut d = build_fixture(0);
        d.operator_pubkey.clear();
        assert!(call(&d).is_err(), "empty operator key rejected");

        // Wrong username_hash length.
        let mut d = build_fixture(0);
        d.username_hash.pop();
        assert!(call(&d).is_err(), "31-byte username_hash rejected");

        // Sanity: the untouched base still passes.
        call(&base).expect("base fixture still valid");
    }
}
