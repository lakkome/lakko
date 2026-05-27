// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — SS-2022 `decode_request` outer-header
//! parser.
//!
//! Fixed all-zero PSK + fresh `ReplayWindow` per call.
//! The fuzzer feeds arbitrary bytes as the wire — most
//! inputs fail at AEAD-open (expected), the interesting
//! cases are the pre-AEAD length / type / timestamp
//! branches and any post-AEAD `var_len` arithmetic. Goal:
//! prove the parser never panics / overflows on attacker-
//! controlled input. Pinned in audit-pack invariant T-10.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_transport::shadowsocks::{decode_request, ReplayWindow};

fuzz_target!(|data: &[u8]| {
    let psk = [0u8; 32];
    let mut window = ReplayWindow::new();
    // Fixed `now` so the timestamp-skew branch is
    // deterministic — otherwise a slow CI machine drifts
    // the harness's wall clock and turns a parse panic
    // into a TimestampSkew rejection.
    let now_unix: u64 = 1_700_000_000;
    let _ = decode_request(&psk, data, now_unix, &mut window);
});
