// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `parse_jwks` JSON-document decoder.
//!
//! `parse_jwks(doc_bytes: &[u8])` is `serde_json::from_slice`
//! into the `Jwks` shape. The auth service publishes the
//! JWKS document at `/.well-known/jwks.json`; clients
//! fetch it over plain HTTPS. If the publish endpoint
//! (or any cache between it and the client) is ever
//! compromised, an attacker can serve arbitrary bytes
//! through this parser. Any panic / overflow / stack
//! abuse on attacker-controlled JSON is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_call_jwt::parse_jwks;

fuzz_target!(|data: &[u8]| {
    let _ = parse_jwks(data);
});
