// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `history_bundle::open_bundle` (the receiver-side history-bundle
//! open path, I-6/I-7). The fuzz input is the attacker-controlled OUTER
//! `sealed_bytes`: a malicious / corrupt bundle reaches this path before any
//! authentication. The harness drives the OUTER `SealedBundle` prost-decode +
//! the pinned-`algorithm` discriminator gate (I-7) + the nonce-length check +
//! the AEAD-open (I-6: a forged ciphertext surfaces as an auth failure, not a
//! parse failure) under a FIXED all-zero secret (so a valid AEAD tag cannot be
//! forged and the harness is reproducible). Any panic / overflow here is a
//! vulnerability — the bytes are unauthenticated at decode time.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::open_history_bundle(data);
});
