// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end CI gate test — drives a **live** dockerised
//! `lakko-svc-sfu` over real TCP / WebSocket (G3-4).
//!
//! Sibling of `tests/integration_signaling.rs`:
//!
//! - That test boots axum **in-process** on a random
//!   localhost port + verifies the wire contract one
//!   function call away from the upgrade handler.
//! - This test points at a **running container** that has
//!   gone through Docker's full process-isolation +
//!   filesystem cap + non-root user posture + the real
//!   JWKS fetcher resolving an nginx sidecar by DNS.
//!
//! The contract on the wire is identical; the value is
//! that this gate catches Dockerfile drift, env-var
//! plumbing drift, and JWKS-fetcher network-path
//! regressions that the in-process test cannot.
//!
//! ## Why `#[ignore]`?
//!
//! Standing up the docker-compose stack takes ~15s on a
//! warm cache and ~2 min on cold — far above the budget
//! of the workspace's per-PR `cargo test` job. CI runs
//! this test from a dedicated workflow
//! (`.github/workflows/sfu-e2e.yml`) with
//! `--include-ignored`; the developer-facing flow is
//! documented in [`ci/sfu-e2e/README.md`](../../../ci/sfu-e2e/README.md).
//!
//! ## Fixture seed
//!
//! The signing seed is fixed at `[0x42; 32]` so the
//! container-side JWKS (which lists the matching public
//! key) can be committed as a static file in
//! `ci/sfu-e2e/test-jwks/jwks.json`. The seed can be
//! overridden via `LAKKO_SFU_E2E_SEED_HEX` for local
//! experimentation; the container side would need a
//! matching `jwks.json` regen for any non-default value.

#![cfg(test)]

use std::time::{SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use lakko_call_jwt::{
    mint_jwt, CallJwtClaims, CallRole, CallSigningKey, JWT_AUDIENCE, JWT_ISSUER, JWT_TTL_SECS,
};
use lakko_svc_sfu::protocol::ServerFrame;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

/// WebSocket URL the test connects to. Overridable so
/// the CI workflow can point at a non-default port if it
/// needs to (e.g. parallel matrix runs sharing a worker).
fn live_ws_url() -> String {
    std::env::var("LAKKO_SFU_E2E_URL")
        .unwrap_or_else(|_| "ws://127.0.0.1:7880/v1/connect".to_owned())
}

/// Fixture seed. Hex-encoded so the env var has a
/// human-readable shape; the default matches the seed
/// the committed `ci/sfu-e2e/test-jwks/jwks.json` was
/// derived from.
fn fixture_seed() -> [u8; 32] {
    let hex = std::env::var("LAKKO_SFU_E2E_SEED_HEX").unwrap_or_else(|_| "42".repeat(32));
    let mut out = [0u8; 32];
    let bytes = hex_to_bytes(&hex).expect("seed env must be 64 hex chars");
    assert_eq!(bytes.len(), 32, "seed must decode to 32 bytes");
    out.copy_from_slice(&bytes);
    out
}

/// Decode a hex string. Tiny helper so the test does not
/// take a `hex` dependency for one call.
fn hex_to_bytes(s: &str) -> Result<Vec<u8>, &'static str> {
    if s.len() % 2 != 0 {
        return Err("hex length must be even");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks_exact(2) {
        let hi = nibble(chunk[0])?;
        let lo = nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn nibble(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("non-hex byte"),
    }
}

/// Mint a valid call-admission JWT signed by the fixture
/// key. The account + call ids are 32-char lowercase hex,
/// matching the on-wire contract `services/auth` enforces
/// in production.
fn mint_test_jwt(
    signing: &CallSigningKey,
    account_id: &str,
    call_id: &str,
    role: CallRole,
) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock not before epoch")
        .as_secs();
    let claims = CallJwtClaims {
        iss: JWT_ISSUER.into(),
        aud: JWT_AUDIENCE.into(),
        sub: account_id.into(),
        call_id: call_id.into(),
        role,
        iat: now,
        exp: now + JWT_TTL_SECS,
    };
    mint_jwt(signing, &claims).expect("mint_jwt against fixture key cannot fail")
}

/// Live E2E happy-path: dockerised SFU admits a peer
/// whose JWT is signed by the fixture key.
///
/// Asserts:
/// - The WS upgrade completes (no HTTP 401).
/// - The first outbound frame is a `ServerFrame::Welcome`
///   with the inbound call_id echoed back.
/// - The peer roster excludes the just-admitted peer
///   itself.
///
/// Failure modes this gate catches that
/// `integration_signaling.rs` cannot:
/// - Dockerfile builds but the binary fails to start in
///   the runtime stage (missing libssl3, etc.).
/// - `LAKKO_SFU_*` env-var plumbing drifts.
/// - The JWKS fetcher's HTTP path is broken under a
///   real-network round trip (vs the in-memory test
///   constructor).
#[tokio::test]
#[ignore = "requires running ci/sfu-e2e docker compose stack — see ci/sfu-e2e/README.md"]
async fn live_sfu_welcome_round_trip() {
    let seed = fixture_seed();
    let signing = CallSigningKey::from_seed(&seed);
    let call_id = "deadbeef0000000000000000000000aa";
    let account = "cafef00d0000000000000000000000bb";
    let jwt = mint_test_jwt(&signing, account, call_id, CallRole::Participant);

    // Build the WS request offering the Lakko subprotocol
    // and the token subprotocol (`lakko-sfu-token.<jwt>`)
    // in one `Sec-WebSocket-Protocol` header. The token
    // never rides the URL; the signaling endpoint refuses
    // an upgrade that offers no token.
    let mut request = live_ws_url()
        .as_str()
        .into_client_request()
        .expect("WS URL is well-formed");
    request.headers_mut().insert(
        "sec-websocket-protocol",
        format!("lakko-sfu-v1,lakko-sfu-token.{jwt}")
            .parse()
            .unwrap(),
    );

    let (mut ws, http_response) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .expect("WS connect timed out after 10s — is the docker stack up?")
    .expect("WS connect must succeed with a fixture-signed token");

    assert_eq!(
        http_response.status().as_u16(),
        101,
        "WS upgrade must return 101 Switching Protocols"
    );

    // First server-side frame on a fresh connection is
    // always Welcome. Anything else (Error, Ping) would
    // indicate the SFU rejected admission post-upgrade.
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("welcome frame must arrive within 5s")
        .expect("WS stream must yield at least one frame")
        .expect("WS frame must decode without transport error");

    let text = match next {
        Message::Text(s) => s,
        other => panic!("expected text Welcome, got {other:?}"),
    };
    let frame: ServerFrame = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("welcome frame must decode as ServerFrame; got {text}: {e}"));

    match frame {
        ServerFrame::Welcome {
            peer_id: _,
            call_id: echoed_call_id,
            server_ts: _,
            roster,
        } => {
            assert_eq!(
                echoed_call_id, call_id,
                "Welcome must echo the call_id from the JWT"
            );
            assert!(
                roster.is_empty(),
                "fresh call: the just-admitted peer is the only member; \
                 roster excludes self so it must be empty here, got {roster:?}"
            );
        },
        other => panic!("expected Welcome, got {other:?}"),
    }

    // Clean shutdown — the SFU emits PeerLeft for the
    // remaining roster (none here); the test doesn't
    // assert on that frame because there are no other
    // peers to observe it. The close handshake itself
    // exercises the writer-half task's graceful drain.
    let _ = ws.send(Message::Close(None)).await;
}

/// Live E2E reject-path: bad JWT must surface as HTTP 401
/// on the upgrade itself, NOT as a successful upgrade
/// followed by an in-protocol Error frame.
///
/// This catches the regression where the SFU accidentally
/// upgrades-then-rejects (the contract is upgrade-OR-
/// reject; never both).
#[tokio::test]
#[ignore = "requires running ci/sfu-e2e docker compose stack — see ci/sfu-e2e/README.md"]
async fn live_sfu_bad_jwt_returns_401() {
    let mut request = live_ws_url()
        .as_str()
        .into_client_request()
        .expect("WS URL is well-formed");
    request.headers_mut().insert(
        "sec-websocket-protocol",
        "lakko-sfu-v1,lakko-sfu-token.not-a-jwt".parse().unwrap(),
    );

    let res = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .expect("WS connect timed out — is the docker stack up?");

    match res {
        Ok(_) => panic!("bad JWT must NOT upgrade — server should return HTTP 401"),
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(
                resp.status().as_u16(),
                401,
                "bad JWT must return 401, got {}",
                resp.status()
            );
        },
        Err(other) => panic!("expected HTTP 401, got transport error {other:?}"),
    }
}
