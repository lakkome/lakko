// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the `verify_supersession_chain` rotation verifier (G-30 #2,
//! ADR-0019).
//!
//! A poisoned serving tier controls EVERY field a client feeds the chain: the
//! `/root` bytes, the `/latest` entry, the `/proof` audit path, and the
//! identity bundle's `supersession_proof`. The operator key + the pin are the
//! client-held inputs (here also carved, to exercise the parser). This harness
//! carves the fuzz bytes into the chain's field layout, assembles the
//! `PinnedIdentity` + `RotationEvidence`, and drives the six-gate verifier. The
//! property: the decode + verify NEVER panics on arbitrary bytes (the
//! false-accept property is pinned by the FFI/core unit tests). Most inputs bail
//! at the operator-root signature gate; the fixed-length carve lets some inputs
//! reach the inner Merkle walk and the supersession-proof parse.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_crypto::signing::{Signature, VerifyingKey};
use lakko_transparency::{
    verify_supersession_chain, InclusionProof, LogEntry, PinnedIdentity, RotationEvidence,
    SignedRoot, TreeHash,
};

/// 145-byte fixed `SupersessionProof` wire length.
const SUPERSESSION_LEN: usize = 145;

/// Minimum bytes to carve the fixed-layout prefix:
/// operator(32) + pinned_old(32) + tree_root(32) + signature(64) +
/// 6×u64(48) + username_hash(32) + supersession(145) = 385.
const MIN_LEN: usize = 32 + 32 + 32 + 64 + 48 + 32 + SUPERSESSION_LEN;

/// Fixed verification instant so the harness is reproducible.
const NOW_UNIX_SECS: i64 = 1_700_000_500;

fn le_u64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    u64::from_le_bytes(a)
}

fuzz_target!(|data: &[u8]| {
    if data.len() < MIN_LEN {
        return;
    }
    let mut o = 0usize;
    let operator_bytes = &data[o..o + 32];
    o += 32;
    let pinned_old_bytes = &data[o..o + 32];
    o += 32;
    let tree_root_bytes = &data[o..o + 32];
    o += 32;
    let signature_bytes = &data[o..o + 64];
    o += 64;
    let new_log_size = le_u64(&data[o..o + 8]);
    o += 8;
    let new_timestamp_unix = le_u64(&data[o..o + 8]);
    o += 8;
    let leaf_index = le_u64(&data[o..o + 8]);
    o += 8;
    let entry_version = le_u64(&data[o..o + 8]);
    o += 8;
    let pinned_version = le_u64(&data[o..o + 8]);
    o += 8;
    let last_seen_log_size = le_u64(&data[o..o + 8]);
    o += 8;
    let username_hash: [u8; 32] = data[o..o + 32].try_into().expect("32-byte slice");
    o += 32;
    let supersession_proof = &data[o..o + SUPERSESSION_LEN];
    o += SUPERSESSION_LEN;

    // The remainder splits into a (variable) entry pubkey + the audit path
    // (truncated to a multiple of 32 so some inputs reach the Merkle walk).
    let rest = &data[o..];
    let pk_len = rest
        .first()
        .map_or(0, |b| (*b as usize % 64) + 1)
        .min(rest.len().saturating_sub(1));
    let identity_pubkey = rest.get(1..1 + pk_len).unwrap_or(&[]).to_vec();
    let audit_src = rest.get(1 + pk_len..).unwrap_or(&[]);
    let audit_len = (audit_src.len() / 32) * 32;
    let audit_path: Vec<TreeHash> = audit_src[..audit_len]
        .chunks_exact(32)
        .map(|c| {
            let mut h = [0u8; 32];
            h.copy_from_slice(c);
            h
        })
        .collect();

    // Parse failures on the operator key / pinned key / root / signature are the
    // common early bail and must not panic.
    let Ok(operator) = VerifyingKey::from_bytes(operator_bytes) else {
        return;
    };
    let pinned_old: [u8; 32] = match pinned_old_bytes.try_into() {
        Ok(p) => p,
        Err(_) => return,
    };
    let tree_root: TreeHash = match tree_root_bytes.try_into() {
        Ok(t) => t,
        Err(_) => return,
    };
    let Ok(signature) = Signature::from_bytes(signature_bytes) else {
        return;
    };
    let Ok(zero_sig) = Signature::from_bytes(&[0u8; 64]) else {
        return;
    };

    let new_root = SignedRoot {
        tree_root,
        log_size: new_log_size,
        timestamp_unix: new_timestamp_unix,
        signature,
    };
    let new_entry = LogEntry {
        username_hash,
        identity_pubkey,
        version: entry_version,
    };
    let inclusion = InclusionProof {
        leaf_index,
        audit_path,
    };
    let last_seen_root = SignedRoot {
        tree_root: [0u8; 32],
        log_size: last_seen_log_size,
        timestamp_unix: 0,
        signature: zero_sig,
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

    let _ = verify_supersession_chain(&pinned, &evidence, NOW_UNIX_SECS);
});
