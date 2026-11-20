// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the another-device recovery payload v3 and its row tag
//! (ADR-0018 Path B, ADR-0074 "Joining after a rotation", via
//! `fuzz_api::decode_recovery_payload`). The row prefix is read before any
//! key is involved, and the plaintext carries the grant, the phrase entropy
//! and up to eight predecessor seeds a joining device stores before it binds.
//! The decoder bounds the count before any copy and requires an exact length;
//! whatever decodes must re-compose to the identical bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(same) = lakko_mnemonic::fuzz_api::decode_recovery_payload(data) {
        assert!(same, "a decoded recovery payload did not re-compose to its bytes");
    }
});
