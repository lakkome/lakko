// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `MlsMessageIn` wire-format parser.
//!
//! Drives arbitrary bytes through `lakko_mls::fuzz_api::parse_mls_message`.
//! Any panic / abort is a bug; reject-as-malformed is the only acceptable
//! failure mode.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::parse_mls_message(data);
});
