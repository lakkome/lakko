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
use lakko_transparency::{
    InclusionProof, LogEntry, PinnedIdentity, RotationEvidence, SignedRoot, TreeHash,
};
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

/// Domain-separation string for the transparency-log account-key
/// derivation. MUST stay byte-identical to discovery's
/// `services/discovery/src/transparency_log.rs::ACCOUNT_KEY_DOMAIN`
/// — both sides derive the same `username_hash` from it, so a change
/// here is a wire break with the server's append path.
const ACCOUNT_KEY_DOMAIN: &str = "lakko/transparency/account/v1";

/// Derive the transparency-log account-key (the leaf's opaque
/// `username_hash`) for an `account_id`:
/// `BLAKE3("lakko/transparency/account/v1" || account_id)`.
///
/// The web client needs this to (a) query
/// `GET /transparency/latest/:account_key` and (b) pass the exact
/// `entry_username_hash` into [`verify_log_inclusion`]. We expose it
/// from the audited Rust core rather than bundle a JS BLAKE3 — the
/// derivation must match discovery's `account_key(...)` byte-for-byte,
/// and re-implementing it in TS would be a second, drift-prone copy of
/// a security-relevant primitive.
///
/// Mirrors the un-keyed `lakko_crypto::hash::blake3` over the
/// domain-prefixed `account_id`. `account_id` is public, so the result
/// reveals nothing new; the domain separator only stops a collision
/// with any other BLAKE3 use of the same bytes.
#[wasm_bindgen(js_name = transparencyAccountKey)]
#[must_use]
pub fn transparency_account_key(account_id: &[u8]) -> Vec<u8> {
    let mut input = Vec::with_capacity(ACCOUNT_KEY_DOMAIN.len() + account_id.len());
    input.extend_from_slice(ACCOUNT_KEY_DOMAIN.as_bytes());
    input.extend_from_slice(account_id);
    lakko_crypto::hash::blake3(&input).to_vec()
}

/// Verify a complete identity-rotation chain against the KT log (G-30 #2,
/// ADR-0019). Unlike `verifySupersession` — which checks only the bundle's
/// standalone proof against the local pin — this anchors the rotation to the
/// operator-signed, append-only log: a rotation is bound to a SINGLE committed
/// version under a no-rollback root, which makes a leaked-old-key equivocation
/// DETECTABLE (two distinct successors cannot both be log-confirmed at one
/// version; the second peer to fetch sees a version-floor or inclusion failure).
///
/// The decomposed fields mirror the JSON a client gathers from
/// `/transparency/{root,latest,proof}` plus the `supersession_proof` carried in
/// the peer's identity bundle and what the peer currently pins. On success the
/// caller re-pins to the returned NEW public key (32 bytes) at `entryVersion`.
///
/// `lastSeenLogSize` is the client's persisted monotone high-water mark; the
/// chain's no-rollback check reads only the size, so the stale root's other
/// fields are reconstructed as zero (never inspected).
///
/// # Errors
///
/// Throws a `JsError` if any fixed-length field is the wrong length, the
/// operator key is not a valid Ed25519 key, or any chain gate fails (forged
/// root, rollback, inclusion mismatch, wrong account, non-newer version, or a
/// supersession proof that is absent / not signed by the pinned old key /
/// out-of-window / expired / not authorising exactly the entry's new key).
#[wasm_bindgen(js_name = verifySupersessionChain)]
#[allow(clippy::too_many_arguments)]
pub fn verify_supersession_chain(
    operator_pubkey: &[u8],
    new_tree_root: &[u8],
    new_log_size: u64,
    new_timestamp_unix: u64,
    new_root_signature: &[u8],
    entry_username_hash: &[u8],
    entry_identity_pubkey: &[u8],
    entry_version: u64,
    inclusion_leaf_index: u64,
    inclusion_audit_path: &[u8],
    supersession_proof: &[u8],
    pinned_old_pubkey: &[u8],
    pinned_version: u64,
    last_seen_log_size: u64,
    now_unix_secs: i64,
) -> Result<Vec<u8>, JsError> {
    verify_supersession_chain_inner(
        operator_pubkey,
        new_tree_root,
        new_log_size,
        new_timestamp_unix,
        new_root_signature,
        entry_username_hash,
        entry_identity_pubkey,
        entry_version,
        inclusion_leaf_index,
        inclusion_audit_path,
        supersession_proof,
        pinned_old_pubkey,
        pinned_version,
        last_seen_log_size,
        now_unix_secs,
    )
    .map_err(|msg| JsError::new(&msg))
}

/// The `JsError`-free core of [`verify_supersession_chain`]. Returns
/// `Err(String)` on any failure so the negative paths are exercisable on the
/// host target (the `#[wasm_bindgen]` wrapper maps `String` -> `JsError`).
#[allow(clippy::too_many_arguments)]
fn verify_supersession_chain_inner(
    operator_pubkey: &[u8],
    new_tree_root: &[u8],
    new_log_size: u64,
    new_timestamp_unix: u64,
    new_root_signature: &[u8],
    entry_username_hash: &[u8],
    entry_identity_pubkey: &[u8],
    entry_version: u64,
    inclusion_leaf_index: u64,
    inclusion_audit_path: &[u8],
    supersession_proof: &[u8],
    pinned_old_pubkey: &[u8],
    pinned_version: u64,
    last_seen_log_size: u64,
    now_unix_secs: i64,
) -> Result<Vec<u8>, String> {
    let operator = signing::VerifyingKey::from_bytes(operator_pubkey)
        .map_err(|_| "operator pubkey is not a valid Ed25519 key".to_string())?;
    let new_tree_root: TreeHash = new_tree_root
        .try_into()
        .map_err(|_| "new_tree_root must be 32 bytes".to_string())?;
    let new_root_signature = signing::Signature::from_bytes(new_root_signature)
        .map_err(|_| "root_signature must be 64 bytes".to_string())?;
    let username_hash: [u8; 32] = entry_username_hash
        .try_into()
        .map_err(|_| "entry_username_hash must be 32 bytes".to_string())?;
    let pinned_old: [u8; 32] = pinned_old_pubkey
        .try_into()
        .map_err(|_| "pinned_old_pubkey must be 32 bytes".to_string())?;

    if inclusion_audit_path.len() % 32 != 0 {
        return Err("audit_path length must be a multiple of 32 bytes".to_string());
    }
    let audit_path: Vec<TreeHash> = inclusion_audit_path
        .chunks_exact(32)
        .map(|c| {
            let mut h = [0u8; 32];
            h.copy_from_slice(c);
            h
        })
        .collect();

    let new_root = SignedRoot {
        tree_root: new_tree_root,
        log_size: new_log_size,
        timestamp_unix: new_timestamp_unix,
        signature: new_root_signature,
    };
    let new_entry = LogEntry {
        username_hash,
        identity_pubkey: entry_identity_pubkey.to_vec(),
        version: entry_version,
    };
    let inclusion = InclusionProof {
        leaf_index: inclusion_leaf_index,
        audit_path,
    };
    // `check_no_rollback` reads only `log_size`; the other fields of the stale
    // root are zeroed and never inspected. A zero signature parses fine.
    let last_seen_root = SignedRoot {
        tree_root: [0u8; 32],
        log_size: last_seen_log_size,
        timestamp_unix: 0,
        signature: signing::Signature::from_bytes(&[0u8; 64])
            .map_err(|_| "zero signature parse".to_string())?,
    };

    let pinned = PinnedIdentity {
        username_hash: &username_hash,
        old_pubkey: &pinned_old,
        version: pinned_version,
        last_seen_root: &last_seen_root,
    };
    let evidence = RotationEvidence {
        operator: &operator,
        new_root: &new_root,
        new_entry: &new_entry,
        inclusion: &inclusion,
        supersession_proof,
    };

    let rotated = lakko_transparency::verify_supersession_chain(&pinned, &evidence, now_unix_secs)
        .map_err(|e| format!("supersession chain verify: {e}"))?;
    Ok(rotated.new_pubkey.to_vec())
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
    fn account_key_matches_domain_separated_blake3() {
        // Pin the exact derivation discovery uses
        // (`services/discovery/src/transparency_log.rs::account_key`):
        // BLAKE3("lakko/transparency/account/v1" || account_id).
        let id = [0xABu8; 16];
        let got = transparency_account_key(&id);

        let mut expected_input = Vec::new();
        expected_input.extend_from_slice(b"lakko/transparency/account/v1");
        expected_input.extend_from_slice(&id);
        let expected = lakko_crypto::hash::blake3(&expected_input);

        assert_eq!(got.as_slice(), expected.as_slice());
        assert_eq!(got.len(), 32);

        // Domain-separated: the bare account_id hash must NOT collide.
        assert_ne!(got.as_slice(), lakko_crypto::hash::blake3(&id).as_slice());

        // Deterministic + distinct per account.
        assert_eq!(transparency_account_key(&id), got);
        assert_ne!(
            transparency_account_key(&[0u8; 16]),
            transparency_account_key(&[1u8; 16])
        );

        // The pinned domain string itself — a change is a wire break.
        assert_eq!(ACCOUNT_KEY_DOMAIN, "lakko/transparency/account/v1");
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

    // -- G-30 #2 / ADR-0019: verifySupersessionChain --

    /// Decomposed fields for a rotation-chain call: a `(v1 old key) ->
    /// (v2 new key)` log plus the supersession proof + the pin.
    struct DecomposedChain {
        operator_pubkey: Vec<u8>,
        new_tree_root: Vec<u8>,
        new_log_size: u64,
        new_timestamp_unix: u64,
        new_root_signature: Vec<u8>,
        entry_username_hash: Vec<u8>,
        entry_identity_pubkey: Vec<u8>,
        entry_version: u64,
        inclusion_leaf_index: u64,
        inclusion_audit_path: Vec<u8>,
        supersession_proof: Vec<u8>,
        pinned_old_pubkey: Vec<u8>,
        pinned_version: u64,
        last_seen_log_size: u64,
        now_unix_secs: i64,
        new_pubkey: Vec<u8>,
    }

    fn build_chain_fixture() -> DecomposedChain {
        use lakko_transparency::SupersessionProof;
        let operator = signing::SigningKey::generate();
        let old_sk = signing::SigningKey::generate();
        let new_sk = signing::SigningKey::generate();
        let account_key = lakko_crypto::hash::blake3(b"alice");

        let mut log = InMemoryLog::new(operator);
        log.append(LogEntry {
            username_hash: account_key,
            identity_pubkey: old_sk.verifying_key().to_bytes().to_vec(),
            version: 1,
        });
        let v2_idx = log.append(LogEntry {
            username_hash: account_key,
            identity_pubkey: new_sk.verifying_key().to_bytes().to_vec(),
            version: 2,
        });
        let signed = log.sign_current_root(1_700_000_000);
        let proof = log.prove_inclusion(v2_idx).expect("proof");
        let entry = log.entry(v2_idx).expect("entry").clone();

        let issued = 1_700_000_000_i64;
        let expires = issued + 1000;
        let supersession_proof = SupersessionProof::new(
            old_sk.verifying_key().to_bytes(),
            new_sk.verifying_key().to_bytes(),
            issued,
            expires,
        )
        .sign(&old_sk)
        .to_vec();

        let mut audit_path = Vec::new();
        for h in &proof.audit_path {
            audit_path.extend_from_slice(h);
        }

        DecomposedChain {
            operator_pubkey: log.operator_key().to_bytes().to_vec(),
            new_tree_root: signed.tree_root.to_vec(),
            new_log_size: signed.log_size,
            new_timestamp_unix: signed.timestamp_unix,
            new_root_signature: signed.signature.to_bytes().to_vec(),
            entry_username_hash: entry.username_hash.to_vec(),
            entry_identity_pubkey: entry.identity_pubkey,
            entry_version: entry.version,
            inclusion_leaf_index: proof.leaf_index,
            inclusion_audit_path: audit_path,
            supersession_proof,
            pinned_old_pubkey: old_sk.verifying_key().to_bytes().to_vec(),
            pinned_version: 1,
            last_seen_log_size: 1,
            now_unix_secs: issued + 10,
            new_pubkey: new_sk.verifying_key().to_bytes().to_vec(),
        }
    }

    fn call_chain(d: &DecomposedChain) -> Result<Vec<u8>, String> {
        verify_supersession_chain_inner(
            &d.operator_pubkey,
            &d.new_tree_root,
            d.new_log_size,
            d.new_timestamp_unix,
            &d.new_root_signature,
            &d.entry_username_hash,
            &d.entry_identity_pubkey,
            d.entry_version,
            d.inclusion_leaf_index,
            &d.inclusion_audit_path,
            &d.supersession_proof,
            &d.pinned_old_pubkey,
            d.pinned_version,
            d.last_seen_log_size,
            d.now_unix_secs,
        )
    }

    #[test]
    fn chain_valid_rotation_returns_authorised_new_key() {
        let d = build_chain_fixture();
        let got = call_chain(&d).expect("valid rotation chain verifies");
        assert_eq!(got, d.new_pubkey, "re-pin to the authorised new key");
    }

    #[test]
    fn chain_forged_root_signature_fails() {
        let mut d = build_chain_fixture();
        d.new_root_signature[0] ^= 0x01;
        assert!(call_chain(&d).is_err());
    }

    #[test]
    fn chain_wrong_pinned_old_key_fails() {
        let mut d = build_chain_fixture();
        d.pinned_old_pubkey = signing::SigningKey::generate()
            .verifying_key()
            .to_bytes()
            .to_vec();
        assert!(call_chain(&d).is_err());
    }

    #[test]
    fn chain_non_newer_version_fails() {
        let mut d = build_chain_fixture();
        d.pinned_version = 2;
        assert!(call_chain(&d).is_err());
    }

    #[test]
    fn chain_rollback_fails() {
        let mut d = build_chain_fixture();
        d.last_seen_log_size = d.new_log_size + 1;
        assert!(call_chain(&d).is_err());
    }

    #[test]
    fn chain_expired_proof_fails() {
        let mut d = build_chain_fixture();
        d.now_unix_secs = 1_700_000_000 + 5000;
        assert!(call_chain(&d).is_err());
    }

    #[test]
    fn chain_malformed_lengths_fail_fast() {
        let mut d = build_chain_fixture();
        d.new_tree_root.pop();
        assert!(call_chain(&d).is_err());

        let mut d = build_chain_fixture();
        d.pinned_old_pubkey.pop();
        assert!(call_chain(&d).is_err());

        let mut d = build_chain_fixture();
        d.inclusion_audit_path.push(0x00);
        assert!(call_chain(&d).is_err());
    }
}
