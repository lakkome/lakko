// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - the donation statement parsers (`docs/ux/donations.md`
//! section 3). The input is a method statement or a phone statement as
//! discovery serves it; a hostile operator chooses every byte, and the client
//! parses it before anything is shown. Both parsers run on the same bytes
//! against a FIXED verifying key (the fuzzer cannot forge a signature), so the
//! length prefixes, the bounds checked before any copy, UTF-8, the character
//! and per-kind rules and the verify failure are exercised; whatever decodes
//! must re-sign to a statement of the same length and the same content.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_donation(data);
});
