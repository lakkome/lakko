// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_open_items` (ADR-0090 section 3.9).
//!
//! An open-items hand-over is an MLS application message any member of a
//! group (or the zero-trust DS replaying a corrupt row) can craft, and it
//! reaches this bounded, fail-closed decoder before any state is consulted:
//! a domain, a version byte checked before any length, an item count, and
//! per item a header, a body bounded before it is copied, and a ring, vote
//! or checklist part whose counts are checked against what is left before
//! anything is allocated.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder, the dispatcher or the assembler;
//! * a decoded envelope lies within the advertised lengths;
//! * round-trip stability: whatever decodes re-composes to exactly the
//!   input, the dispatcher classifies it the same, and every item the
//!   assembler returns is one the envelope carried.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::open_items_decode(data) {
        assert!(
            ok,
            "decode, compose, the bounds, dispatch and assembly must agree"
        );
    }
});
