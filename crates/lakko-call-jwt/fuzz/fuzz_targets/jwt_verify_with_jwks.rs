// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — full JWS verification pipeline via
//! `verify_with_jwks`.
//!
//! Wider surface than `parse_unverified`: covers the
//! signature path, kid lookup against a fixed JWKS, the
//! algorithm-confusion check (`alg != EdDSA` rejected),
//! base64-decode of the signature segment, length check
//! on `Signature::from_bytes`, and the `exp` clock
//! comparison. Fixed single-key JWKS + fixed `now_secs`
//! so coverage focuses on parser-side branches rather
//! than on the AEAD-style "key doesn't match anyway" wall
//! that dominates with random keys. The attacker fully
//! controls the JWT string on every client / SFU receive
//! path.

#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;

use lakko_call_jwt::{verify_with_jwks, CallSigningKey, Jwk, Jwks};

/// Build the fixed JWKS once per process. The seed is
/// all-zeros so the harness is reproducible and the kid is
/// deterministic — the fuzzer can learn to produce JWTs
/// whose `kid` header matches.
fn fixture_jwks() -> &'static Jwks {
    static JWKS: OnceLock<Jwks> = OnceLock::new();
    JWKS.get_or_init(|| {
        let key = CallSigningKey::from_seed(&[0u8; 32]);
        Jwks {
            keys: vec![Jwk::from_signing_key(&key)],
        }
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(jwt) = std::str::from_utf8(data) else {
        return;
    };
    // Fixed `now` so the `exp` branch is deterministic —
    // a wall-clock-dependent harness would turn a parser
    // panic into a flaky `Expired` rejection on slow runs.
    let now_secs: u64 = 1_700_000_000;
    let _ = verify_with_jwks(fixture_jwks(), jwt, now_secs);
});
