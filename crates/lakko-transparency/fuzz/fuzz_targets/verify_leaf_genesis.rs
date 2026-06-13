// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the genesis leaf self-signature verifier (G-30 #2, ADR-0019).
//!
//! A poisoned serving tier controls the leaf `leaf_auth` (the signature bytes)
//! AND the served `(username_hash, version, identity_pubkey)` it is checked
//! against. This harness carves the fuzz bytes into that layout, builds the
//! `LogEntry`, and drives `verify_leaf_genesis`. The property: the parse +
//! verify NEVER panics on arbitrary bytes (the false-accept property — a forged
//! self-sig is rejected — is pinned by the unit tests). Most inputs bail at the
//! embedded-key parse or the signature check; a valid-length carve lets some
//! reach the Ed25519 verify.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_transparency::verify_leaf_genesis;

/// Minimum bytes: signature(64) + username_hash(32).
const MIN_LEN: usize = 64 + 32;

fuzz_target!(|data: &[u8]| {
    if data.len() < MIN_LEN {
        return;
    }
    let mut o = 0usize;
    let sig = &data[o..o + 64];
    o += 64;
    let username_hash: [u8; 32] = data[o..o + 32].try_into().expect("32-byte slice");
    o += 32;

    // The remainder is the (variable) embedded identity pubkey, capped at the
    // 64-byte range the verifier's key parser tolerates so the fuzzer can push a
    // 32-byte canonical key all the way into the Ed25519 verify.
    let rest = &data[o..];
    let pk_len = rest.len().min(64);
    let identity_pubkey = &rest[..pk_len];

    let _ = verify_leaf_genesis(sig, &username_hash, identity_pubkey);
});
