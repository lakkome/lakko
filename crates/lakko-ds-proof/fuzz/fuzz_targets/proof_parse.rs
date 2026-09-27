// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `Proof::parse` + `verify` (ADR-0053). The header value is
//! attacker-controlled on every Delivery Service request and is parsed before
//! any key is resolved; the parser must stay panic-free and the verify that
//! follows must never panic on a hostile epoch or time.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_ds_proof::fuzz_api::parse_and_verify(data);
});
