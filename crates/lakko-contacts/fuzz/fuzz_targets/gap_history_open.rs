// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `open_gap_history` (ADR-0091 section 7, security review
//! 2026-11-21).
//!
//! A sealed gap history is posted to a rendezvous anyone who saw the request
//! can write to, and the returning device opens every row there: the version
//! byte (checked first), the length bounds and the sealed-box refusal see
//! attacker-chosen bytes before any key is trusted; the signed-plaintext
//! parser behind the box (signer key, signature, bundle) is driven directly
//! on the same bytes.
//!
//! Asserted: no panic; nothing opens under a fixed gap secret the fuzzer
//! cannot sign and seal for; the signed-plaintext parser accepts nothing a
//! fuzzer writes; a bundle sealed to that key and signed by a fixed leaf
//! opens back to itself and that leaf, with the leaf key nowhere in the
//! clear.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_contacts::fuzz_api::gap_history_open(data),
        "a gap history opened without its key, or a sealed bundle did not round-trip"
    );
});
