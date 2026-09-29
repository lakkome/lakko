// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `Delegation::decode` + `Delegation::parse_header_value` +
//! `verify_delegation` (ADR-0060 section 2). The delegation header is
//! attacker-controlled on every write to a closed mailbox and is parsed
//! before any reach key is resolved; the parser and the verifier must stay
//! panic-free, and a delegation that parses must round-trip byte for byte.
//! Fixed mailbox and clock.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_ds_proof::fuzz_api::delegation_parse(data);
});
