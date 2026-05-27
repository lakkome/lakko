// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — SS-2022 `decode_response` outer-header
//! parser. See `shadowsocks_decode_request.rs` for the
//! design rationale. The response path has an extra
//! invariant — the echoed request-salt MUST match the
//! caller-supplied `expected_request_salt` — so the fuzzer
//! also exercises the `WrongResponseSalt` branch.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_transport::shadowsocks::decode_response;

fuzz_target!(|data: &[u8]| {
    let psk = [0u8; 32];
    let expected_request_salt = [0xABu8; 32];
    let now_unix: u64 = 1_700_000_000;
    let _ = decode_response(&psk, data, now_unix, &expected_request_salt);
});
