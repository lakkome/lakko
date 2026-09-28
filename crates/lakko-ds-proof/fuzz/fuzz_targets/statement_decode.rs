// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `SignedStatement::decode` + `verify` (ADR-0057). The body of
//! every journal-ownership route is attacker-controlled and decoded before
//! any root or registrar is consulted; the decoder must stay panic-free, and
//! whatever parses must re-encode byte for byte (one statement, one
//! encoding).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_ds_proof::fuzz_api::decode_statement(data);
});
