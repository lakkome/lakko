// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — The seal announcement parser and verifier (ADR-0101 section 10, `fuzz_api::open_reset_seals`): discovery serves the announcement beside the identity bundle, so every byte is the operator's choice. Asserts that whatever decodes re-encodes to its own bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_reset_seals(data);
});
