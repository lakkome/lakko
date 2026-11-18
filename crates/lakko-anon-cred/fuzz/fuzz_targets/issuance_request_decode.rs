// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `lakko_anon_cred::fuzz_api::issuance_request_decode` (ADR-0092 4.7). The
//! assertions live in the fuzz API so the harness stays a one-liner.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_anon_cred::fuzz_api::issuance_request_decode(data);
});
