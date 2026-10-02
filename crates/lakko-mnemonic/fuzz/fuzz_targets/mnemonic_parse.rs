// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the BIP39 recovery parsers (`Mnemonic::from_phrase` +
//! `from_entropy`, via `fuzz_api::parse_mnemonic`). `from_phrase` parses
//! a user-typed or device-relayed recovery phrase; `from_entropy`
//! rebuilds the phrase from the entropy an ADR-0049 word vault releases,
//! which a malicious server can influence. The harness runs both on the same
//! input — the raw bytes through `from_entropy` (32-byte length gate →
//! BIP39 checksum reconstruction) and, when valid UTF-8, the string
//! through `from_phrase` (12-word count gate → wordlist lookup →
//! checksum). Any panic / overflow here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mnemonic::fuzz_api::parse_mnemonic(data);
});
