// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the `verify_witness_cosig` parser/verifier (ADR-0020).
//!
//! A malicious operator (the split-view adversary) serves the witness co-sig
//! blob to a client, so its bytes are fully attacker-controlled. This harness
//! carves arbitrary bytes into `(witness_key, expected_tree_root,
//! expected_log_size, cosig)` and drives `verify_witness_cosig`. The property:
//! the bounded parser + verify NEVER panics / overflows on arbitrary input (the
//! false-accept property is pinned by the crate unit tests).

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_transparency::{verify_witness_cosig, LogId, TreeHash};

/// witness_key(32) + expected_tree_root(32) + expected_log_size(8) = 72; the
/// remainder is the attacker-controlled co-sig blob.
const MIN_LEN: usize = 32 + 32 + 8;

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
    let witness_key = &data[o..o + 32];
    o += 32;
    let expected_tree_root: TreeHash = data[o..o + 32].try_into().expect("32-byte slice");
    o += 32;
    let expected_log_size = le_u64(&data[o..o + 8]);
    o += 8;
    let cosig = &data[o..];

    // Both logs: the log name enters the signature check (witness v2).
    for log in LogId::ALL {
        let _ = verify_witness_cosig(
            cosig,
            witness_key,
            log,
            &expected_tree_root,
            expected_log_size,
        );
    }
});
