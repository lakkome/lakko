// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `TokenSecret::verify` + `TokenSecret::verify_refresh` +
//! `TokenSecret::verify_fresh_grant` (the v3 access-token, r2
//! refresh-credential and k1 fresh-auth-grant parsers + verifiers,
//! `fuzz_api::verify_token`, ADR-0073 and ADR-0074). The fuzz input is an
//! attacker-controlled credential string: a client or peer presents an
//! access token on every bearer request and a refresh credential on auth's
//! refresh route, and a fresh-auth grant in a body field or the
//! `x-lakko-fresh-grant` header of auth's fresh-auth-gated routes, so the
//! bytes are untrusted and reach the parser before admission. The harness verifies the same input as all three kinds against a
//! FIXED 32-byte secret (the fuzzer cannot forge a valid HMAC, so it
//! exercises the dot-split → version gate → base64url account_id decode +
//! 16-byte length check → `u64` generation parse → base64url device_id
//! decode + 32-byte length check → `i64` issue-time and expiry parses →
//! base64url tag decode + 32-byte length check → HMAC compute +
//! constant-time compare → expiry check) at a FIXED `now_ms`. Any panic /
//! overflow / slice-out-of-bounds here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_auth_token::fuzz_api::verify_token(data);
});
