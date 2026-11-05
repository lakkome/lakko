// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_group_contact_request` (ADR-0086 section 6).
//!
//! A contact request carried inside a group rides an MLS application
//! message any member (or the zero-trust DS replaying a corrupt row) can
//! craft, and reaches this bounded, fail-closed decoder before any state is
//! consulted: a domain, a version byte checked before any length, and a
//! length-prefixed sealed box bounded before it is copied. Every member then
//! tries to open the box with its own key.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder, the dispatcher or the receiver's rule, and
//!   the rule under a fixed key refuses a box the fuzzer could not have
//!   sealed to it;
//! * a decoded envelope lies within the advertised lengths;
//! * round-trip stability: whatever decodes re-composes to exactly the input
//!   (the encoding is canonical), and the dispatcher classifies the same
//!   bytes as the same envelope.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::group_contact_request_decode(data) {
        assert!(ok, "decode, compose, the length bounds and dispatch must agree on an envelope");
    }
});
