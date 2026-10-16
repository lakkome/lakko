// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end integration tests for the SFU's signaling
//! layer (S-3-B-tests).
//!
//! Each test boots a real axum server on a random localhost
//! port, points a tokio-tungstenite WS client at it, and
//! drives the wire protocol from [`crate::protocol`]
//! through real socket I/O. The JWKS cache is pre-loaded
//! via the `from_jwks_for_test` constructor so we never
//! hit the network.
//!
//! These tests pin the contract every client (web / iOS /
//! Android) will exercise once the WebRTC pod lands:
//!
//! - admission: valid JWT → welcome frame with the current
//!   roster
//! - admission: bad JWT / missing token → HTTP 401, no
//!   upgrade
//! - cross-peer fanout: B joins → A sees `peer-joined`
//! - cross-peer fanout: B leaves → A sees `peer-left`
//! - signal-relay unicast: A → B, only B receives
//! - signal-relay broadcast: A → call, B receives, A does
//!   not
//! - ping / pong round trip
//! - oversize frame is rejected with the right error code
//!
//! No real WebRTC, no real media — that arrives with
//! `S-3-B-routing-media`. The tests cover **exactly** the
//! signaling-layer contract S-3-B-routing-signaling
//! published.

use std::time::{SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use lakko_call_jwt::{
    kid_for, mint_jwt, CallJwtClaims, CallRole, CallSigningKey, Jwk, Jwks, JWT_AUDIENCE,
    JWT_ISSUER, JWT_TTL_SECS,
};
use lakko_svc_sfu::{
    build_app,
    config::Config,
    jwks::JwksCache,
    protocol::{ClientFrame, ErrorCode, RelayTarget, ServerFrame},
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as ClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

// ---------------------------------------------------------
// Fixtures
// ---------------------------------------------------------

/// Build a config bound to ephemeral localhost. The
/// signaling / metrics binds are placeholders — tests pick
/// up the actual port from the `TcpListener` returned by
/// `spawn_server`.
fn test_config() -> Config {
    Config {
        signaling_bind: "127.0.0.1:0".parse().expect("valid socket"),
        metrics_bind: "127.0.0.1:0".parse().expect("valid socket"),
        jwks_url: "https://localhost/.well-known/jwks.json".into(),
        jwks_refresh: std::time::Duration::from_secs(300),
        handshake_timeout: std::time::Duration::from_secs(10),
        peer_msg_rate_per_sec: 100,
        // Signaling-only — the integration tests exercise
        // the WS fanout, not the media half.
        media_bind: None,
        media_advertise_ip: None,
        media_diagnostics: None,
        video_fec: false,
        uplink_fallback: false,
        protection: lakko_svc_sfu::media::LossProtection::default(),
        bitrate_control: lakko_svc_sfu::media::BitrateControl::Client,
        downlink_control: false,
        signaling_ping_interval: std::time::Duration::from_secs(20),
        signaling_idle_timeout: std::time::Duration::from_secs(60),
    }
}

/// Test signing key + matching JWKS doc. The seed is
/// arbitrary; the kid is derived deterministically from
/// the public half.
struct TestKey {
    signing: CallSigningKey,
    jwks: Jwks,
}

fn test_key(seed: u8) -> TestKey {
    let signing = CallSigningKey::from_seed(&[seed; 32]);
    let jwk = Jwk::from_signing_key(&signing);
    assert_eq!(jwk.kid, kid_for(&signing.verifying));
    TestKey {
        signing,
        jwks: Jwks { keys: vec![jwk] },
    }
}

/// The room of a 32-hex call id (`lakko_sfu_proto::room_hex`).
fn room_of(call_id: &str) -> String {
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&call_id[i * 2..i * 2 + 2], 16).expect("hex call id");
    }
    lakko_sfu_proto::room_hex(&bytes)
}

/// Mint a valid call-admission JWT (ADR-0078 section 2): `peer` is a hex
/// string left-padded to the 32-hex peer id, `call_id` a 32-hex call id
/// whose room the token names. No account, no call id in the token.
fn mint(key: &TestKey, peer: &str, call_id: &str, role: CallRole) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = CallJwtClaims {
        iss: JWT_ISSUER.into(),
        aud: JWT_AUDIENCE.into(),
        room: room_of(call_id),
        peer_id: format!("{peer:0>32}"),
        role,
        iat: now,
        exp: now + JWT_TTL_SECS,
    };
    mint_jwt(&key.signing, &claims).expect("mint succeeds for valid claims")
}

/// Boot the SFU on a random port. Returns the bound
/// address (so tests can build the WS URL) plus a join
/// handle the test can let drop to tear the server down at
/// the end of the case.
async fn spawn_server(jwks: Jwks) -> std::net::SocketAddr {
    spawn_server_with(test_config(), jwks).await
}

/// [`spawn_server`] with a caller-shaped config (the liveness
/// tests shrink the ping / idle windows to seconds).
async fn spawn_server_with(cfg: Config, jwks: Jwks) -> std::net::SocketAddr {
    let jwks_cache = JwksCache::from_jwks_for_test(jwks);
    let (router, _peers) = build_app(cfg, jwks_cache);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        // axum::serve hands ownership of the listener to
        // hyper; the task lives until the test process
        // terminates.
        let _ = axum::serve(listener, router).await;
    });
    addr
}

/// Build the WS connect request for a given server addr + JWT: the
/// token rides `Sec-WebSocket-Protocol` as `lakko-sfu-token.<jwt>`
/// next to `lakko-sfu-v1`, never the URL.
fn ws_url(addr: &std::net::SocketAddr, jwt: &str) -> ClientRequest {
    ws_request_offering(addr, &format!("lakko-sfu-v1,lakko-sfu-token.{jwt}"))
}

/// A connect request offering exactly `protocols` (the raw
/// `Sec-WebSocket-Protocol` value).
fn ws_request_offering(addr: &std::net::SocketAddr, protocols: &str) -> ClientRequest {
    let mut request = format!("ws://{addr}/v1/connect")
        .into_client_request()
        .expect("ws request");
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(protocols).expect("protocol header"),
    );
    request
}

/// `GET` the call-state route with `Authorization: Bearer <jwt>`.
async fn get_state(url: &str) -> reqwest::Response {
    reqwest::get(url).await.expect("state request")
}

/// Read one ServerFrame from a tungstenite WS stream.
/// Panics on EOF / parse failure — these are tests, panics
/// are the right thing to do. Protocol Ping / Pong frames are
/// the liveness layer, not signaling: tungstenite answers a
/// Ping on its own and surfaces both, so they are skipped here.
async fn recv_frame<S>(ws: &mut S) -> ServerFrame
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let msg = ws
            .next()
            .await
            .expect("frame should be available")
            .expect("ws read");
        match msg {
            Message::Text(text) => {
                return serde_json::from_str::<ServerFrame>(&text).expect("server frame parses");
            },
            Message::Ping(_) | Message::Pong(_) => {},
            other => panic!("expected Text frame, got {other:?}"),
        }
    }
}

/// Send one ClientFrame via tungstenite.
async fn send_frame<S>(ws: &mut S, frame: ClientFrame)
where
    S: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let json = serde_json::to_string(&frame).expect("client frame serialises");
    ws.send(Message::Text(json)).await.expect("ws send");
}

// ---------------------------------------------------------
// Tests
// ---------------------------------------------------------

#[tokio::test]
async fn health_returns_200() {
    let key = test_key(0x01);
    let addr = spawn_server(key.jwks).await;
    let resp = reqwest::get(format!("http://{addr}/health"))
        .await
        .expect("health request");
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.expect("body");
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn ws_admission_with_valid_jwt_emits_welcome() {
    let key = test_key(0x02);
    let addr = spawn_server(key.jwks.clone()).await;
    let jwt = mint(
        &key,
        "deadbeefdeadbeefdeadbeefdeadbeef",
        "0123456789abcdef0123456789abcdef",
        CallRole::Publisher,
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt))
        .await
        .expect("ws connect");

    let frame = recv_frame(&mut ws).await;
    match frame {
        ServerFrame::Welcome {
            peer_id,
            room,
            roster,
            ..
        } => {
            assert_eq!(room, room_of("0123456789abcdef0123456789abcdef"));
            assert_eq!(peer_id, "deadbeefdeadbeefdeadbeefdeadbeef");
            assert!(roster.is_empty(), "first peer sees empty roster");
        },
        other => panic!("expected Welcome, got {other:?}"),
    }
}

#[tokio::test]
async fn ws_admission_with_bad_jwt_is_rejected() {
    let key = test_key(0x03);
    let addr = spawn_server(key.jwks).await;
    let result = tokio_tungstenite::connect_async(ws_url(&addr, "not-a-jwt")).await;
    assert!(result.is_err(), "bad token must not upgrade");
}

#[tokio::test]
async fn two_peers_in_same_call_see_each_other() {
    let key = test_key(0x04);
    let addr = spawn_server(key.jwks.clone()).await;
    let call_id = "1111111111111111aaaaaaaaaaaaaaaa";

    // Alice connects first.
    let alice_jwt = mint(
        &key,
        "a000000000000000a000000000000000",
        call_id,
        CallRole::Publisher,
    );
    let (mut alice, _) = tokio_tungstenite::connect_async(ws_url(&addr, &alice_jwt))
        .await
        .expect("alice connects");
    let alice_welcome = recv_frame(&mut alice).await;
    let alice_peer_id = match alice_welcome {
        ServerFrame::Welcome {
            peer_id, roster, ..
        } => {
            assert!(roster.is_empty());
            peer_id
        },
        other => panic!("alice welcome unexpected {other:?}"),
    };

    // Bob connects, expects to see Alice in his welcome
    // roster and Alice should subsequently see a peer-joined.
    let bob_jwt = mint(
        &key,
        "b000000000000000b000000000000000",
        call_id,
        CallRole::Publisher,
    );
    let (mut bob, _) = tokio_tungstenite::connect_async(ws_url(&addr, &bob_jwt))
        .await
        .expect("bob connects");
    let bob_welcome = recv_frame(&mut bob).await;
    let bob_peer_id = match bob_welcome {
        ServerFrame::Welcome {
            peer_id, roster, ..
        } => {
            assert_eq!(roster.len(), 1);
            assert_eq!(roster[0].peer_id, alice_peer_id);
            peer_id
        },
        other => panic!("bob welcome unexpected {other:?}"),
    };

    let alice_sees_bob = recv_frame(&mut alice).await;
    match alice_sees_bob {
        ServerFrame::PeerJoined { peer_id, .. } => {
            assert_eq!(peer_id, bob_peer_id);
        },
        other => panic!("alice expected PeerJoined, got {other:?}"),
    }
}

#[tokio::test]
async fn signal_relay_unicast_only_reaches_target() {
    let key = test_key(0x05);
    let addr = spawn_server(key.jwks.clone()).await;
    let call_id = "2222222222222222cccccccccccccccc";

    let alice_jwt = mint(&key, "a1", call_id, CallRole::Publisher);
    let bob_jwt = mint(&key, "b1", call_id, CallRole::Publisher);
    let carol_jwt = mint(&key, "c1", call_id, CallRole::Publisher);

    let (mut alice, _) = tokio_tungstenite::connect_async(ws_url(&addr, &alice_jwt))
        .await
        .unwrap();
    let _ = recv_frame(&mut alice).await; // welcome

    let (mut bob, _) = tokio_tungstenite::connect_async(ws_url(&addr, &bob_jwt))
        .await
        .unwrap();
    let bob_welcome = recv_frame(&mut bob).await;
    let bob_peer_id = match bob_welcome {
        ServerFrame::Welcome { peer_id, .. } => peer_id,
        other => panic!("{other:?}"),
    };

    let (mut carol, _) = tokio_tungstenite::connect_async(ws_url(&addr, &carol_jwt))
        .await
        .unwrap();
    let _ = recv_frame(&mut carol).await; // welcome

    // Drain pending peer-joined frames on alice + bob from
    // carol's arrival so we don't confuse them with the
    // relay below.
    let _ = recv_frame(&mut alice).await; // bob joined
    let _ = recv_frame(&mut alice).await; // carol joined
    let _ = recv_frame(&mut bob).await; // carol joined

    // Alice signals bob.
    send_frame(
        &mut alice,
        ClientFrame::SignalRelay {
            to: RelayTarget::Peer {
                peer_id: bob_peer_id.clone(),
            },
            payload_b64: "AAAA".into(),
        },
    )
    .await;

    let bob_received = recv_frame(&mut bob).await;
    match bob_received {
        ServerFrame::SignalRelay { payload_b64, .. } => {
            assert_eq!(payload_b64, "AAAA");
        },
        other => panic!("bob expected SignalRelay, got {other:?}"),
    }

    // Carol should NOT receive it; assert by checking that
    // the next frame on carol does not arrive within a
    // timeout. A short timeout is enough — the SFU's mpsc
    // is millisecond-scale; if the relay leaked it would
    // be queued already.
    let race = tokio::time::timeout(std::time::Duration::from_millis(150), carol.next()).await;
    assert!(race.is_err(), "carol must not receive the unicast");
}

#[tokio::test]
async fn signal_relay_broadcast_skips_sender() {
    let key = test_key(0x06);
    let addr = spawn_server(key.jwks.clone()).await;
    let call_id = "3333333333333333dddddddddddddddd";

    let alice_jwt = mint(&key, "a2", call_id, CallRole::Publisher);
    let bob_jwt = mint(&key, "b2", call_id, CallRole::Publisher);

    let (mut alice, _) = tokio_tungstenite::connect_async(ws_url(&addr, &alice_jwt))
        .await
        .unwrap();
    let _ = recv_frame(&mut alice).await; // welcome

    let (mut bob, _) = tokio_tungstenite::connect_async(ws_url(&addr, &bob_jwt))
        .await
        .unwrap();
    let _ = recv_frame(&mut bob).await; // welcome
    let _ = recv_frame(&mut alice).await; // bob joined

    send_frame(
        &mut alice,
        ClientFrame::SignalRelay {
            to: RelayTarget::Call,
            payload_b64: "ZZZ".into(),
        },
    )
    .await;

    let bob_got = recv_frame(&mut bob).await;
    match bob_got {
        ServerFrame::SignalRelay { payload_b64, .. } => assert_eq!(payload_b64, "ZZZ"),
        other => panic!("bob expected SignalRelay, got {other:?}"),
    }

    // Alice (the sender) must not receive her own broadcast.
    let race = tokio::time::timeout(std::time::Duration::from_millis(150), alice.next()).await;
    assert!(race.is_err(), "alice must not see her own broadcast");
}

#[tokio::test]
async fn disconnect_propagates_peer_left() {
    let key = test_key(0x07);
    let addr = spawn_server(key.jwks.clone()).await;
    let call_id = "4444444444444444eeeeeeeeeeeeeeee";

    let alice_jwt = mint(&key, "a3", call_id, CallRole::Publisher);
    let bob_jwt = mint(&key, "b3", call_id, CallRole::Publisher);

    let (mut alice, _) = tokio_tungstenite::connect_async(ws_url(&addr, &alice_jwt))
        .await
        .unwrap();
    let _ = recv_frame(&mut alice).await;

    let (mut bob, _) = tokio_tungstenite::connect_async(ws_url(&addr, &bob_jwt))
        .await
        .unwrap();
    let bob_welcome = recv_frame(&mut bob).await;
    let bob_peer_id = match bob_welcome {
        ServerFrame::Welcome { peer_id, .. } => peer_id,
        other => panic!("{other:?}"),
    };
    let _ = recv_frame(&mut alice).await; // bob joined

    drop(bob);

    let frame = recv_frame(&mut alice).await;
    match frame {
        ServerFrame::PeerLeft { peer_id } => assert_eq!(peer_id, bob_peer_id),
        other => panic!("alice expected PeerLeft, got {other:?}"),
    }
}

#[tokio::test]
async fn ping_round_trip() {
    let key = test_key(0x08);
    let addr = spawn_server(key.jwks.clone()).await;
    let jwt = mint(
        &key,
        "a7",
        "5555555555555555ffffffffffffffff",
        CallRole::Publisher,
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt))
        .await
        .unwrap();
    let _ = recv_frame(&mut ws).await; // welcome

    send_frame(&mut ws, ClientFrame::Ping { nonce: 0xCAFE_BABE }).await;
    let frame = recv_frame(&mut ws).await;
    match frame {
        ServerFrame::Pong { nonce } => assert_eq!(nonce, 0xCAFE_BABE),
        other => panic!("expected Pong, got {other:?}"),
    }
}

#[tokio::test]
async fn cross_call_isolation() {
    let key = test_key(0x09);
    let addr = spawn_server(key.jwks.clone()).await;

    let alice_jwt = mint(
        &key,
        "e1",
        "6666666666666666aaaaaaaaaaaaaaaa",
        CallRole::Publisher,
    );
    let eve_jwt = mint(
        &key,
        "e2",
        "7777777777777777bbbbbbbbbbbbbbbb",
        CallRole::Publisher,
    );

    let (mut alice, _) = tokio_tungstenite::connect_async(ws_url(&addr, &alice_jwt))
        .await
        .unwrap();
    let _ = recv_frame(&mut alice).await; // welcome

    let (mut eve, _) = tokio_tungstenite::connect_async(ws_url(&addr, &eve_jwt))
        .await
        .unwrap();
    let _ = recv_frame(&mut eve).await; // welcome

    // Alice should NOT see eve-joined (different call).
    let race = tokio::time::timeout(std::time::Duration::from_millis(150), alice.next()).await;
    assert!(race.is_err(), "cross-call peer-joined leaked");
}

// ---------------------------------------------------------
// Signaling liveness
// ---------------------------------------------------------

/// A config whose liveness windows are seconds, so a test can
/// wait them out.
fn liveness_config(ping_secs: u64, idle_secs: u64) -> Config {
    Config {
        signaling_ping_interval: std::time::Duration::from_secs(ping_secs),
        signaling_idle_timeout: std::time::Duration::from_secs(idle_secs),
        ..test_config()
    }
}

/// The defect: a peer whose network went away without a close
/// frame stayed registered (and on every other participant's
/// roster) until TCP or nginx gave up, up to an hour. Here B
/// stops reading its socket, so its stack never answers the
/// server's pings; the server must drop it inside the idle
/// window and A must hear `peer-left`.
#[tokio::test]
async fn a_peer_that_stops_answering_pings_is_dropped_and_announced_as_left() {
    let key = test_key(0x21);
    let addr = spawn_server_with(liveness_config(1, 2), key.jwks.clone()).await;
    let call = "0123456789abcdef0123456789abcdef";
    let jwt_a = mint(
        &key,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        call,
        CallRole::Publisher,
    );
    let jwt_b = mint(
        &key,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        call,
        CallRole::Publisher,
    );

    let (mut a, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt_a))
        .await
        .expect("A connects");
    assert!(matches!(
        recv_frame(&mut a).await,
        ServerFrame::Welcome { .. }
    ));

    let (mut b, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt_b))
        .await
        .expect("B connects");
    assert!(matches!(
        recv_frame(&mut b).await,
        ServerFrame::Welcome { .. }
    ));
    let joined = recv_frame(&mut a).await;
    let b_id = match joined {
        ServerFrame::PeerJoined { peer_id, .. } => peer_id,
        other => panic!("expected peer-joined, got {other:?}"),
    };

    // B goes quiet: it is never polled again, so tungstenite
    // never answers a ping on its behalf. The socket stays open
    // from the server's point of view — exactly a phone whose
    // network vanished.
    let _b_held_open = b;

    // A keeps reading (which is what answers A's own pings) and
    // must see B leave well inside the window plus one interval.
    let left = tokio::time::timeout(std::time::Duration::from_secs(6), recv_frame(&mut a))
        .await
        .expect("A hears about B inside the idle window");
    match left {
        ServerFrame::PeerLeft { peer_id } => assert_eq!(peer_id, b_id),
        other => panic!("expected peer-left, got {other:?}"),
    }
}

/// The control: a peer that answers the pings (any stack that is
/// polled does) is never dropped, however long it says nothing at
/// the app layer.
#[tokio::test]
async fn a_peer_that_answers_pings_is_kept_through_several_idle_windows() {
    let key = test_key(0x22);
    let addr = spawn_server_with(liveness_config(1, 2), key.jwks.clone()).await;
    let call = "0123456789abcdef0123456789abcdef";
    let jwt_a = mint(
        &key,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        call,
        CallRole::Publisher,
    );
    let jwt_b = mint(
        &key,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        call,
        CallRole::Publisher,
    );

    let (mut a, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt_a))
        .await
        .expect("A connects");
    assert!(matches!(
        recv_frame(&mut a).await,
        ServerFrame::Welcome { .. }
    ));
    let (mut b, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt_b))
        .await
        .expect("B connects");
    assert!(matches!(
        recv_frame(&mut b).await,
        ServerFrame::Welcome { .. }
    ));
    assert!(matches!(
        recv_frame(&mut a).await,
        ServerFrame::PeerJoined { .. }
    ));

    // B keeps its socket polled in the background — the pongs
    // go out — but sends nothing itself for three idle windows.
    let b_reader = tokio::spawn(async move {
        let mut b = b;
        let mut frames = Vec::new();
        let until = tokio::time::Instant::now() + std::time::Duration::from_secs(7);
        loop {
            match tokio::time::timeout_at(until, b.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => frames.push(text.clone()),
                Ok(Some(Ok(_))) => {},
                Ok(Some(Err(_)) | None) | Err(_) => break,
            }
        }
        frames
    });

    // A must hear nothing about B in that time.
    let heard = tokio::time::timeout(std::time::Duration::from_secs(6), recv_frame(&mut a)).await;
    assert!(
        heard.is_err(),
        "A heard {heard:?} about a peer that was answering its pings"
    );
    let b_frames = b_reader.await.expect("B reader");
    assert!(b_frames.is_empty(), "B received {b_frames:?} while idle");
}

// ---------------------------------------------------------
// When a call is over (`call_lifetime.rs`)
// ---------------------------------------------------------

/// The state route follows the room, and a join after the empty grace is
/// refused with `CallEnded` in place of the Welcome. The wait is the real
/// grace: this test is about the wiring, the rule itself is unit-tested
/// against an injected clock.
#[tokio::test]
async fn a_call_left_empty_past_the_grace_is_over_for_the_next_joiner() {
    let key = test_key(0x21);
    let addr = spawn_server(key.jwks.clone()).await;
    let call = "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";
    let jwt_a = mint(
        &key,
        "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1",
        call,
        CallRole::Participant,
    );
    let jwt_b = mint(
        &key,
        "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2",
        call,
        CallRole::Participant,
    );
    let state = format!("http://{addr}/v1/rooms/{}", room_of(call));

    // Never seen: a first join founds it.
    let body: serde_json::Value = get_state(&state).await.json().await.expect("state json");
    assert_eq!(body["state"], "unknown");

    let (mut ws_a, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt_a))
        .await
        .expect("ws connect");
    assert!(matches!(
        recv_frame(&mut ws_a).await,
        ServerFrame::Welcome { .. }
    ));
    let body: serde_json::Value = get_state(&state).await.json().await.expect("state json");
    assert_eq!(body["state"], "live");
    assert_eq!(body["peers"], 1);

    // The last one out starts the grace.
    ws_a.close(None).await.expect("close");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let body: serde_json::Value = get_state(&state).await.json().await.expect("state json");
    assert_eq!(body["state"], "closing");

    tokio::time::sleep(lakko_svc_sfu::call_lifetime::CALL_EMPTY_GRACE).await;
    let body: serde_json::Value = get_state(&state).await.json().await.expect("state json");
    assert_eq!(body["state"], "ended");

    // A late joiner is told so in place of the Welcome, and the socket closes.
    let (mut ws_b, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt_b))
        .await
        .expect("ws connect");
    match recv_frame(&mut ws_b).await {
        ServerFrame::Error { code, .. } => assert_eq!(code, ErrorCode::CallEnded),
        other => panic!("expected CallEnded, got {other:?}"),
    }
}

/// ADR-0078 section 2: the probe route takes no token at all - knowing the
/// room id is the capability - and refuses a path that is not a room id.
#[tokio::test]
async fn the_room_probe_needs_no_jwt() {
    let key = test_key(0x22);
    let addr = spawn_server(key.jwks.clone()).await;
    let url = format!(
        "http://{addr}/v1/rooms/{}",
        room_of("22222222222222222222222222222222")
    );
    let bare = reqwest::get(&url).await.expect("state request");
    assert_eq!(bare.status(), 200);
    let body: serde_json::Value = bare.json().await.expect("state json");
    assert_eq!(body["state"], "unknown");
    assert_eq!(body["peers"], 0);
    for bad in [
        "22222222222222222222222222222222",
        "nothex",
        &"A".repeat(64),
    ] {
        let resp = reqwest::get(format!("http://{addr}/v1/rooms/{bad}"))
            .await
            .expect("state request");
        assert_eq!(resp.status(), 400, "{bad}");
    }
}

/// The old token-gated `/v1/calls/{call_id}` route is gone.
#[tokio::test]
async fn the_call_id_route_is_gone() {
    let key = test_key(0x23);
    let addr = spawn_server(key.jwks.clone()).await;
    let resp = reqwest::get(format!(
        "http://{addr}/v1/calls/33333333333333333333333333333333"
    ))
    .await
    .expect("state request");
    assert_eq!(resp.status(), 404);
}

/// One token admits one live connection: a second socket presenting the
/// same token (the same peer id) while the first is up gets no Welcome.
#[tokio::test]
async fn a_token_admits_one_live_socket() {
    let key = test_key(0x25);
    let addr = spawn_server(key.jwks.clone()).await;
    let jwt = mint(
        &key,
        "f6",
        "55555555555555555555555555555555",
        CallRole::Participant,
    );
    let (mut first, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt))
        .await
        .expect("ws connect");
    assert!(matches!(
        recv_frame(&mut first).await,
        ServerFrame::Welcome { .. }
    ));
    let (mut second, _) = tokio_tungstenite::connect_async(ws_url(&addr, &jwt))
        .await
        .expect("ws connect");
    let next = tokio::time::timeout(std::time::Duration::from_secs(2), second.next()).await;
    if let Ok(Some(Ok(Message::Text(t)))) = next {
        panic!("second socket got a frame: {t}");
    }
}

/// The SFU answers with `lakko-sfu-v1` only, never echoing the token
/// subprotocol the client offered.
#[tokio::test]
async fn ws_admission_answers_with_the_signaling_subprotocol_only() {
    let key = test_key(0x24);
    let addr = spawn_server(key.jwks.clone()).await;
    let jwt = mint(
        &key,
        "e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5",
        "44444444444444444444444444444444",
        CallRole::Participant,
    );
    // The browser form: a space after the comma.
    let (_ws, response) = tokio_tungstenite::connect_async(ws_request_offering(
        &addr,
        &format!("lakko-sfu-v1, lakko-sfu-token.{jwt}"),
    ))
    .await
    .expect("ws connect");
    let selected = response
        .headers()
        .get_all("sec-websocket-protocol")
        .iter()
        .map(|v| v.to_str().expect("ascii").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(selected, vec!["lakko-sfu-v1".to_owned()]);
}

/// A connect with no token subprotocol, or with the token in the query
/// string (the retired form), does not upgrade.
#[tokio::test]
async fn ws_admission_without_a_token_subprotocol_is_rejected() {
    let key = test_key(0x25);
    let addr = spawn_server(key.jwks.clone()).await;
    let jwt = mint(
        &key,
        "f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6",
        "55555555555555555555555555555555",
        CallRole::Participant,
    );
    let bare = tokio_tungstenite::connect_async(ws_request_offering(&addr, "lakko-sfu-v1")).await;
    assert!(bare.is_err(), "no token must not upgrade");

    let mut query = format!("ws://{addr}/v1/connect?token={jwt}")
        .into_client_request()
        .expect("ws request");
    query.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static("lakko-sfu-v1"),
    );
    let query = tokio_tungstenite::connect_async(query).await;
    assert!(query.is_err(), "a query-string token must not upgrade");
}
