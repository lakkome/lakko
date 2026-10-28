// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Mint a fixture JWT for the `ci/sfu-e2e/` G3-4-mobile-call
//! integration tests.
//!
//! The CI workflow brings up the dockerised SFU against the
//! sibling `gen_test_jwks` example's JWKS file, then needs a
//! valid JWT to hand to the iOS / Android platform tests so
//! they can do the WebSocket signaling round-trip. The
//! production client SDK does NOT expose `mint_jwt` (clients
//! receive JWTs from `services/auth`, never mint them); this
//! example fills the test-side gap without touching the
//! shipped FFI surface.
//!
//! Run with:
//!
//! ```bash
//! cargo run --quiet --release -p lakko-call-jwt \
//!   --example mint_test_jwt -- \
//!   --call-id deadbeef0000000000000000000000aa \
//!   --peer-id cafef00d0000000000000000000000bb \
//!   --role participant \
//!   --ttl-secs 600
//! # → eyJhbGciOiJFZERTQSIsImtpZCI6Ii4uLiJ9.<...>.<...>
//! ```
//!
//! `--role` accepts `participant`, `publisher`, or
//! `subscriber` — same three variants the `CallRole` enum
//! ships.
//!
//! Every flag has a sensible default for ad-hoc smoke runs
//! (a fixed call id + peer id, role=participant, ttl=600 s). The
//! token carries the room derived from the call id
//! (`lakko_sfu_proto::room_hex`, ADR-0078 section 2), never the call
//! id itself and no account; the CI workflow passes the call id so the
//! room matches the value the Welcome-frame assertion pins.
//!
//! The seed is `0x42` repeated 32 times — same fixture
//! `gen_test_jwks` + `services/sfu/tests/e2e_dockerized.rs`
//! pin. Mismatch here would surface as a JWKS `UnknownKid`
//! refusal on the SFU side.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use lakko_call_jwt::{
    mint_jwt, CallJwtClaims, CallRole, CallSigningKey, JWT_AUDIENCE, JWT_ISSUER, JWT_TTL_SECS,
};

// A single flat arg-parse + mint + print example binary; the length is
// inherent to a self-contained CLI helper, not a refactor smell.
#[allow(clippy::too_many_lines)]
fn main() -> ExitCode {
    let mut call_id = "deadbeef0000000000000000000000aa".to_owned();
    let mut peer_id = "cafef00d0000000000000000000000bb".to_owned();
    let mut role = CallRole::Participant;
    let mut ttl_secs: u64 = JWT_TTL_SECS;

    // Tiny ad-hoc arg parser — we deliberately do not pull
    // `clap` into the dev-time fixture binary. Recognises:
    //
    //   --call-id <hex>      | -c <hex>
    //   --peer-id <hex>      | -p <hex>
    //   --role participant|publisher|subscriber
    //   --ttl-secs <u64>     | -t <u64>
    //   --help               | -h
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let key = args[i].as_str();
        let next = || args.get(i + 1).cloned();
        match key {
            "--help" | "-h" => {
                eprintln!(
                    "usage: mint_test_jwt [--call-id <hex>] [--peer-id <hex>] \
                     [--role participant|publisher|subscriber] [--ttl-secs <u64>]"
                );
                return ExitCode::SUCCESS;
            },
            "--call-id" | "-c" => {
                if let Some(v) = next() {
                    call_id = v;
                    i += 2;
                    continue;
                }
                eprintln!("--call-id needs a value");
                return ExitCode::from(2);
            },
            "--peer-id" | "-p" => {
                if let Some(v) = next() {
                    peer_id = v;
                    i += 2;
                    continue;
                }
                eprintln!("--peer-id needs a value");
                return ExitCode::from(2);
            },
            "--role" => {
                if let Some(v) = next() {
                    role = match v.as_str() {
                        "participant" => CallRole::Participant,
                        "publisher" => CallRole::Publisher,
                        "subscriber" => CallRole::Subscriber,
                        other => {
                            eprintln!(
                                "--role must be 'participant' / 'publisher' / 'subscriber', \
                                 got {other:?}"
                            );
                            return ExitCode::from(2);
                        },
                    };
                    i += 2;
                    continue;
                }
                eprintln!("--role needs a value");
                return ExitCode::from(2);
            },
            "--ttl-secs" | "-t" => {
                if let Some(v) = next() {
                    match v.parse::<u64>() {
                        Ok(n) => ttl_secs = n,
                        Err(e) => {
                            eprintln!("--ttl-secs must be u64: {e}");
                            return ExitCode::from(2);
                        },
                    }
                    i += 2;
                    continue;
                }
                eprintln!("--ttl-secs needs a value");
                return ExitCode::from(2);
            },
            other => {
                eprintln!("unknown arg: {other:?}");
                return ExitCode::from(2);
            },
        }
    }

    let Some(call_id_bytes) = decode_call_id(&call_id) else {
        eprintln!("--call-id must be 32 hex characters");
        return ExitCode::from(2);
    };
    let room = lakko_sfu_proto::room_hex(&call_id_bytes);
    let signing = CallSigningKey::from_seed(&[0x42_u8; 32]);
    let Ok(since_epoch) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        eprintln!("system clock is before the unix epoch — refusing to mint");
        return ExitCode::from(1);
    };
    let now = since_epoch.as_secs();
    let claims = CallJwtClaims {
        iss: JWT_ISSUER.into(),
        aud: JWT_AUDIENCE.into(),
        room,
        peer_id,
        role,
        iat: now,
        exp: now + ttl_secs,
    };
    match mint_jwt(&signing, &claims) {
        Ok(jwt) => {
            // No trailing newline — easier to capture into
            // `$(...)` from the shell or pipe straight into
            // a workflow env-var setter.
            print!("{jwt}");
            ExitCode::SUCCESS
        },
        Err(e) => {
            eprintln!("mint_jwt failed: {e}");
            ExitCode::from(1)
        },
    }
}

/// The 16 raw bytes of a 32-hex-character call id.
fn decode_call_id(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}
