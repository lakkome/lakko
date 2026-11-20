// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0096 section 1.5: drive the SFU's HTTP and signaling routes with
//! room and peer ids in the requests, then check that the rendered
//! `/metrics` carries route templates and closed-set labels only, and
//! nothing shaped like an identifier.
//!
//! Its own test binary, so the process-wide recorder it installs is not
//! shared with the signaling tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use lakko_call_jwt::{
    mint_jwt, CallJwtClaims, CallRole, CallSigningKey, Jwk, Jwks, JWT_AUDIENCE, JWT_ISSUER,
    JWT_TTL_SECS,
};
use lakko_svc_sfu::{
    build_app,
    config::Config,
    jwks::JwksCache,
    prom,
    protocol::{ClientFrame, RelayTarget},
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

/// Label names the SFU's own metrics use beyond lakko-metrics' base set.
const SFU_LABELS: &[&str] = &["direction", "kind", "reason", "why"];

const CALL_ID: [u8; 16] = [0x5a; 16];
const PEER_A: &str = "deadbeefdeadbeefdeadbeefdeadbeef";
const PEER_B: &str = "0123456789abcdef0123456789abcdef";

fn config() -> Config {
    Config {
        signaling_bind: "127.0.0.1:0".parse().unwrap(),
        metrics_bind: "127.0.0.1:0".parse().unwrap(),
        jwks_url: "https://localhost/.well-known/jwks.json".into(),
        jwks_refresh: std::time::Duration::from_secs(300),
        handshake_timeout: std::time::Duration::from_secs(10),
        peer_msg_rate_per_sec: 100,
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

fn mint(signing: &CallSigningKey, peer: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = CallJwtClaims {
        iss: JWT_ISSUER.into(),
        aud: JWT_AUDIENCE.into(),
        room: lakko_sfu_proto::room_hex(&CALL_ID),
        peer_id: peer.into(),
        role: CallRole::Publisher,
        iat: now,
        exp: now + JWT_TTL_SECS,
    };
    mint_jwt(signing, &claims).unwrap()
}

fn connect_request(
    addr: std::net::SocketAddr,
    jwt: &str,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut request = format!("ws://{addr}/v1/connect")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&format!("lakko-sfu-v1,lakko-sfu-token.{jwt}")).unwrap(),
    );
    request
}

#[tokio::test]
async fn sfu_metrics_carry_no_identifier() {
    let metrics = prom::install().unwrap();

    let signing = CallSigningKey::from_seed(&[0x33; 32]);
    let jwks = Jwks {
        keys: vec![Jwk::from_signing_key(&signing)],
    };
    let (router, _peers) = build_app(config(), JwksCache::from_jwks_for_test(jwks));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    // Probes and the room-state route, whose path is a room id.
    let room = lakko_sfu_proto::room_hex(&CALL_ID);
    for path in [
        "/health".to_owned(),
        "/version".to_owned(),
        format!("/v1/rooms/{room}"),
    ] {
        let status = reqwest::get(format!("http://{addr}{path}"))
            .await
            .unwrap()
            .status();
        assert!(status.is_success(), "{path}: {status}");
    }
    // A path no route matches, carrying the room id.
    reqwest::get(format!("http://{addr}/nothing/{room}"))
        .await
        .unwrap();

    // Two peers admitted, a relayed frame, a ping, one refused upgrade.
    let (mut a, _) =
        tokio_tungstenite::connect_async(connect_request(addr, &mint(&signing, PEER_A)))
            .await
            .unwrap();
    let (mut b, _) =
        tokio_tungstenite::connect_async(connect_request(addr, &mint(&signing, PEER_B)))
            .await
            .unwrap();
    a.next().await.unwrap().unwrap();
    b.next().await.unwrap().unwrap();
    let relay = ClientFrame::SignalRelay {
        to: RelayTarget::Peer {
            peer_id: PEER_B.into(),
        },
        payload_b64: "AAAA".into(),
    };
    a.send(Message::Text(serde_json::to_string(&relay).unwrap()))
        .await
        .unwrap();
    a.send(Message::Text(r#"{"kind":"ping","nonce":7}"#.into()))
        .await
        .unwrap();
    let refused = tokio_tungstenite::connect_async(connect_request(addr, "not-a-jwt")).await;
    assert!(refused.is_err());
    // Let the server record the frames before rendering.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let text = metrics.render();
    lakko_metrics::exposition::assert_clean(&text, SFU_LABELS, &[]);
    assert!(
        text.contains(r#"route="/v1/rooms/{room}""#),
        "the room route is counted under its template:\n{text}"
    );
    assert!(text.contains("lakko_sfu_peers_admitted_total 2"), "{text}");
    assert!(
        text.contains(
            r#"lakko_sfu_signaling_frames_total{direction="inbound",kind="signal-relay"}"#
        ),
        "{text}"
    );
    assert!(!text.contains(PEER_A) && !text.contains(&room), "{text}");
}
