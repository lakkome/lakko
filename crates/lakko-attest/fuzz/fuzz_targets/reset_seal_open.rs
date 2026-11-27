// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — The reset seal parser and verifier (ADR-0101 section 10, `fuzz_api::open_reset_seal`): the seal rides the reset's attested publish and the KT reset leaf, both served by the operator. Asserts that whatever decodes re-encodes to its own body.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_reset_seal(data);
});
