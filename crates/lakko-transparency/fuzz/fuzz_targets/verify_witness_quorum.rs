// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the `verify_witness_quorum` K-of-N verifier (ADR-0039, G-30
//! finding-1).
//!
//! A malicious operator (the split-view adversary) serves the witness co-sig
//! blobs to a client, so their bytes are fully attacker-controlled. This
//! harness pins a FIXED, deterministic set of witness verifying keys plus a
//! fixed `(tree_root, log_size)`, carves the arbitrary input into several
//! candidate co-sig slices, and drives `verify_witness_quorum` across a range of
//! thresholds (including the fail-closed `0` and an unsatisfiable one). The
//! property: the bounded quorum tally + per-candidate verify NEVER panics /
//! overflows on arbitrary input (the dedup-by-key / false-accept properties are
//! pinned by the crate unit tests).

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_crypto::signing::SigningKey;
use lakko_transparency::{verify_witness_quorum, TreeHash};

/// Split the attacker-controlled input into at most this many candidate co-sig
/// slices, so a single run exercises the per-key `.any()` loop over several
/// blobs at once.
const MAX_CANDIDATES: usize = 4;

fuzz_target!(|data: &[u8]| {
    // Fixed, deterministic pinned witness set. In production these are trusted
    // config; fixing them here keeps the harness reproducible while the fuzzer
    // drives only the attacker-controlled co-sig bytes. The fuzzer cannot forge
    // a valid Ed25519 co-sig under any of these keys, so it exercises the
    // length / version / binding / signature-parse / verify-fail paths plus the
    // quorum tally arithmetic.
    let vk1 = SigningKey::from_seed(&[1u8; 32]).verifying_key().to_bytes();
    let vk2 = SigningKey::from_seed(&[2u8; 32]).verifying_key().to_bytes();
    let vk3 = SigningKey::from_seed(&[3u8; 32]).verifying_key().to_bytes();
    let pinned: [&[u8]; 3] = [&vk1, &vk2, &vk3];

    let expected_tree_root: TreeHash = [0u8; 32];
    let expected_log_size: u64 = 0;

    // Carve `data` into up to MAX_CANDIDATES contiguous chunks as candidate
    // co-sig blobs. `.max(1)` keeps the chunk size non-zero on tiny inputs.
    let chunk = (data.len() / MAX_CANDIDATES).max(1);
    let cosigs: Vec<&[u8]> = data.chunks(chunk).take(MAX_CANDIDATES).collect();

    // Cover the fail-closed zero threshold, the satisfiable range, and an
    // unsatisfiable threshold above the pinned-key count; none may panic.
    for threshold in [0usize, 1, 2, 3, 4] {
        let _ = verify_witness_quorum(
            &cosigs,
            &pinned,
            threshold,
            &expected_tree_root,
            expected_log_size,
        );
    }
});
