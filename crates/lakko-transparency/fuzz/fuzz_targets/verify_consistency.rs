// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the RFC 6962 §2.1.2 `verify_consistency` proof verifier
//! (ADR-0020, witness co-signing).
//!
//! The witness receives `(old_size, old_root, new_size, new_root, proof)` from
//! the operator (an untrusted party in the split-view threat model) and runs
//! `verify_consistency` before co-signing. This harness carves arbitrary bytes
//! into that field layout and drives the verifier; the property is that the
//! reconstruction NEVER panics / overflows on arbitrary input (the false-accept
//! property — a fork is rejected — is pinned by the crate's exhaustive
//! round-trip + negative unit tests).

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_transparency::{verify_consistency, TreeHash};

/// old_size(8) + new_size(8) + old_root(32) + new_root(32) = 80.
const MIN_LEN: usize = 8 + 8 + 32 + 32;

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
    let old_size = le_u64(&data[o..o + 8]);
    o += 8;
    let new_size = le_u64(&data[o..o + 8]);
    o += 8;
    let old_root: TreeHash = data[o..o + 32].try_into().expect("32-byte slice");
    o += 32;
    let new_root: TreeHash = data[o..o + 32].try_into().expect("32-byte slice");
    o += 32;

    // The remainder is the proof, truncated to a multiple of 32.
    let rest = &data[o..];
    let proof_len = (rest.len() / 32) * 32;
    let proof: Vec<TreeHash> = rest[..proof_len]
        .chunks_exact(32)
        .map(|c| {
            let mut h = [0u8; 32];
            h.copy_from_slice(c);
            h
        })
        .collect();

    // The sizes are attacker-controlled u64s; the verifier must not panic /
    // overflow on any value (e.g. huge sizes, old_size > new_size, zero).
    let _ = verify_consistency(old_size, &old_root, new_size, &new_root, &proof);
});
