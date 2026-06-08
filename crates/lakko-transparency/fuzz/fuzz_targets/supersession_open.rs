// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `SupersessionProof::open` bounded wire parser (G-4).
//!
//! The outermost attacker-touchable surface for the identity-supersession
//! proof: a peer hands these bytes in after a rotation, and the verify path
//! is now FFI-exposed (`verify_supersession` on UniFFI + wasm), so a
//! malicious peer reaches this parser with fully attacker-controlled bytes.
//! `open` slices a fixed 145-byte layout
//! (version | old_pubkey | new_pubkey | issued_at | expires_at | sig),
//! length- and version-gates it, then Ed25519-verifies the signature against
//! the EMBEDDED old key. Goal: prove the parser never panics / overflows /
//! abuses the stack on any input.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_transparency::SupersessionProof;

fuzz_target!(|data: &[u8]| {
    // The proof is a binary blob (not a string), so there is no UTF-8 gate —
    // feed the raw bytes straight into the parser.
    let _ = SupersessionProof::open(data);
});
