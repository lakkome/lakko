// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `open_admission_ticket` (ADR-0087 section 7).
//!
//! A subscriber row's ticket is written by whoever subscribes and served by
//! the untrusted channels service, and a publisher's device opens it before
//! deciding to seal a seed: the version byte, the fixed length, the
//! ephemeral key, the AEAD refusal under a fixed admission seed, and the
//! plaintext check (key, signature) fed directly since a fuzzer cannot pass
//! the AEAD.
//!
//! Asserted: no panic, and nothing is ever accepted (a fuzzer cannot forge
//! the invite key's signature).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        !lakko_channels::fuzz_api::admission_ticket_open(data),
        "an admission ticket the fuzzer made must never be accepted"
    );
});
