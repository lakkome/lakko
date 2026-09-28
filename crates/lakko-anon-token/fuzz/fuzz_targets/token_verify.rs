// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `Token::decode` + `verify_token` (ADR-0054 decision 2). The
//! token bytes are attacker-controlled on every anonymous request a consuming
//! service accepts and are parsed before any state is consulted; the parser
//! and the verifier must stay panic-free, and a token that decodes must
//! re-encode to the identical bytes. Fixed master secret and clock.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_anon_token::fuzz_api::decode_and_verify(data);
});
