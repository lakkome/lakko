// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the client-side `verify_log_inclusion` decode + two-gate
//! verify path (KT-log S3b).
//!
//! A poisoned serving tier controls EVERY field a client feeds the verifier:
//! the `/root` bytes (tree_root, log_size, timestamp, signature), the
//! `/proof` bytes (leaf_index, audit_path), and the `/latest` bytes
//! (identity_pubkey, version). The operator key is the only client-pinned
//! input. This harness carves the fuzz bytes into that exact field layout —
//! mirroring the `lakko-ffi-{wasm,uniffi}` `verify_log_inclusion` decode —
//! and drives the two gates (`SignedRoot::verify` then `verify_inclusion`).
//! The property is: the decode + verify NEVER panics on arbitrary bytes (the
//! false-accept property — a tampered tuple is rejected — is pinned by the
//! FFI unit tests). Most inputs bail at an early length / key / signature
//! gate; the fixed-length carve lets the fuzzer push some inputs all the way
//! into the Merkle `verify_inclusion` walk.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_crypto::signing::{Signature, VerifyingKey};
use lakko_transparency::{verify_inclusion, InclusionProof, LogEntry, SignedRoot, TreeHash};

/// Minimum bytes to carve the fixed-layout prefix:
/// operator(32) + tree_root(32) + signature(64) + log_size(8) +
/// timestamp(8) + leaf_index(8) + version(8) + username_hash(32) = 192.
const MIN_LEN: usize = 32 + 32 + 64 + 8 + 8 + 8 + 8 + 32;

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
    let tree_root_bytes = &data[o..o + 32];
    o += 32;
    let signature_bytes = &data[o..o + 64];
    o += 64;
    let log_size = le_u64(&data[o..o + 8]);
    o += 8;
    let timestamp_unix = le_u64(&data[o..o + 8]);
    o += 8;
    let leaf_index = le_u64(&data[o..o + 8]);
    o += 8;
    let version = le_u64(&data[o..o + 8]);
    o += 8;
    let username_hash: [u8; 32] = data[o..o + 32].try_into().expect("32-byte slice");
    o += 32;

    // The remainder splits into a (variable) identity pubkey + the audit path
    // (truncated to a multiple of 32 so some inputs reach the Merkle walk
    // rather than always tripping the length gate). Cap the pubkey at the
    // 1..=64 byte range the verifier accepts.
    let rest = &data[o..];
    let pk_len = rest.first().map_or(0, |b| (*b as usize % 64) + 1).min(rest.len().saturating_sub(1));
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

    // Gate-1 inputs: a parse failure on the operator key / signature / root
    // is the common early bail and must not panic.
    let Ok(operator) = VerifyingKey::from_bytes(operator_bytes) else {
        return;
    };
    let Ok(signature) = Signature::from_bytes(signature_bytes) else {
        return;
    };
    let tree_root: TreeHash = match tree_root_bytes.try_into() {
        Ok(r) => r,
        Err(_) => return,
    };

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
        identity_pubkey,
        version,
    };

    // Two gates, in the same order as the FFI: a forged root fails gate 1
    // before the Merkle walk ever sees the attacker-chosen tree_root.
    if signed_root.verify(&operator).is_ok() {
        let _ = verify_inclusion(&entry, &proof, &signed_root);
    }
});
