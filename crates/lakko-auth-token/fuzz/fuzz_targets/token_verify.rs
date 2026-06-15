// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `TokenSecret::verify` (the session-token parser +
//! verifier, `fuzz_api::verify_token`). The fuzz input is an
//! attacker-controlled token string: a client or peer presents it to the
//! server on every protected discovery request, so the bytes are
//! untrusted and reach the parser before admission. The harness verifies
//! against a FIXED 32-byte secret (the fuzzer cannot forge a valid HMAC,
//! so it exercises the dot-split → version gate → base64url account_id
//! decode + 16-byte length check → `i64` expiry parse → base64url tag
//! decode + 32-byte length check → HMAC compute + constant-time compare →
//! expiry check) at a FIXED `now_ms`. Any panic / overflow /
//! slice-out-of-bounds here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_auth_token::fuzz_api::verify_token(data);
});
