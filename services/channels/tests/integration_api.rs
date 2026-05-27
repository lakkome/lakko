// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end integration tests for the channels service.
//!
//! Each case boots a real axum server on a random
//! localhost port, drives it with `reqwest`, and asserts
//! the wire-format contract documented in `api.rs`.

use std::net::SocketAddr;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_svc_channels::{router, AppState, Config, InMemoryChannelStore};
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpListener;

fn test_config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_post_bytes: 64 * 1024,
        fetch_limit_default: 50,
        fetch_limit_max: 500,
        database_url: None,
        retention_secs: 0,
        subscriber_retention_secs: 0,
        // Integration tests drive prune manually; the
        // periodic loop is disabled (0 = no background
        // task spawned).
        prune_interval_secs: 0,
    }
}

async fn spawn() -> SocketAddr {
    let cfg = test_config();
    let store = Arc::new(InMemoryChannelStore::new());
    let state = AppState::new(cfg, store);
    let app = router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

const CHANNEL_HEX: &str = "0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn health_responds_ok() {
    let addr = spawn().await;
    let resp = reqwest::get(format!("http://{addr}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn create_channel_then_publish_then_fetch() {
    let addr = spawn().await;
    let client = reqwest::Client::new();

    // 1. Create.
    let resp = client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["channel_id"].as_str().unwrap(), CHANNEL_HEX);

    // 2. Publish two posts.
    for n in 1u8..=2 {
        let resp = client
            .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/posts"))
            .header("content-type", "application/octet-stream")
            .body(vec![n, n, n])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "publish {n} should be 201");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["cursor"].as_u64().unwrap(), u64::from(n));
    }

    // 3. Fetch from cursor 0.
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/posts?after=0"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let posts = body["posts"].as_array().unwrap();
    assert_eq!(posts.len(), 2);
    assert_eq!(body["next_after"].as_u64().unwrap(), 2);

    // 4. Fetch from cursor 1 — returns only the second post.
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/posts?after=1"
        ))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let posts = body["posts"].as_array().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["cursor"].as_u64().unwrap(), 2);
}

#[tokio::test]
async fn publish_to_unknown_channel_is_404() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/posts"))
        .body(vec![1, 2, 3])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "unknown-channel");
}

#[tokio::test]
async fn publish_empty_post_is_400() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    let resp = client
        .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/posts"))
        .body(Vec::<u8>::new())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "empty-post");
}

#[tokio::test]
async fn bad_channel_id_is_400() {
    let addr = spawn().await;
    let resp = reqwest::get(format!("http://{addr}/v1/channels/zzzz/posts"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad-channel-id");
}

#[tokio::test]
async fn fetch_unknown_channel_is_404() {
    let addr = spawn().await;
    let resp = reqwest::get(format!(
        "http://{addr}/v1/channels/{CHANNEL_HEX}/posts?after=0"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn fetch_respects_limit() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    for i in 0..10u8 {
        client
            .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/posts"))
            .body(vec![i])
            .send()
            .await
            .unwrap();
    }
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/posts?after=0&limit=3"
        ))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["posts"].as_array().unwrap().len(), 3);
}

const SUB_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn subscribe_to_unknown_channel_is_404() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
        ))
        .json(&json!({ "subscriber_pubkey_hex": SUB_HEX }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "unknown-channel");
}

#[tokio::test]
async fn subscribe_then_list_then_deliver_then_fetch() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    // Create.
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    // Subscribe.
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
        ))
        .json(&json!({ "subscriber_pubkey_hex": SUB_HEX }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["subscriber_id"].as_str().unwrap(), SUB_HEX);
    // List.
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let subs = body["subscribers"].as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].as_str().unwrap(), SUB_HEX);
    // Fetch before delivery → 404 envelope-not-ready.
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelope"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "envelope-not-ready");
    // Deliver an envelope (80 bytes — SEED_ENVELOPE_LEN).
    let envelope: Vec<u8> = (0..80u8).collect();
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelope"
        ))
        .body(envelope.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    // Fetch — must return the envelope base64-encoded.
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelope"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let b64 = body["envelope_b64"].as_str().unwrap();
    let decoded = URL_SAFE_NO_PAD.decode(b64).unwrap();
    assert_eq!(decoded, envelope, "envelope must round-trip byte-identical");
}

#[tokio::test]
async fn deliver_envelope_with_wrong_length_is_400() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
        ))
        .json(&json!({ "subscriber_pubkey_hex": SUB_HEX }))
        .send()
        .await
        .unwrap();
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelope"
        ))
        .body(vec![0u8; 79]) // off by one
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad-envelope-len");
}

#[tokio::test]
async fn deliver_to_unsubscribed_pubkey_is_404() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    // No subscribe; jump straight to deliver.
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelope"
        ))
        .body(vec![0u8; 80])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "unknown-subscriber");
}

#[tokio::test]
async fn bad_subscriber_id_is_400() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
        ))
        .json(&json!({ "subscriber_pubkey_hex": "shortie" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad-subscriber-id");
}

/// SubscribeRequest's expected envelope length must match
/// `lakko_channels::subscribe::SEED_ENVELOPE_LEN`. Compile-
/// time assertion so a future bump in either crate is
/// caught by the build.
const _ASSERT_ENVELOPE_LEN: () =
    assert!(lakko_svc_channels::api::EXPECTED_ENVELOPE_LEN == lakko_channels::SEED_ENVELOPE_LEN);

const SUB_HEX_B: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";

async fn create_channel_with_two_subs(client: &reqwest::Client, addr: SocketAddr) {
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    for sub in [SUB_HEX, SUB_HEX_B] {
        client
            .post(format!(
                "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
            ))
            .json(&json!({ "subscriber_pubkey_hex": sub }))
            .send()
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn genesis_epoch_is_zero() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    let resp = client
        .get(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/epoch"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["epoch"].as_u64().unwrap(), 0);
}

#[tokio::test]
async fn epoch_for_unknown_channel_is_404() {
    let addr = spawn().await;
    let resp = reqwest::get(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/epoch"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn rotate_bumps_epoch_and_delivers_envelopes() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_two_subs(&client, addr).await;
    let bytes_for_alice: Vec<u8> = (0..80u8).collect();
    let bytes_for_bob: Vec<u8> = (10..90u8).collect();
    let alice_envelope_b64 = URL_SAFE_NO_PAD.encode(&bytes_for_alice);
    let bob_envelope_b64 = URL_SAFE_NO_PAD.encode(&bytes_for_bob);
    let resp = client
        .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/rotate"))
        .json(&json!({
            "envelopes": [
                { "subscriber_id": SUB_HEX,   "envelope_b64": alice_envelope_b64 },
                { "subscriber_id": SUB_HEX_B, "envelope_b64": bob_envelope_b64 },
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["epoch"].as_u64().unwrap(), 1);

    // Cross-check via GET /epoch.
    let resp = client
        .get(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/epoch"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["epoch"].as_u64().unwrap(), 1);

    // Each subscriber receives its own envelope.
    for (sub, expected) in [(SUB_HEX, &bytes_for_alice), (SUB_HEX_B, &bytes_for_bob)] {
        let resp = client
            .get(format!(
                "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{sub}/envelope"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let decoded = URL_SAFE_NO_PAD
            .decode(body["envelope_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(&decoded, expected);
    }
}

#[tokio::test]
async fn rotate_aborts_on_wrong_envelope_length() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_two_subs(&client, addr).await;
    let short_env = URL_SAFE_NO_PAD.encode(vec![0u8; 79]);
    let resp = client
        .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/rotate"))
        .json(&json!({
            "envelopes": [
                { "subscriber_id": SUB_HEX, "envelope_b64": short_env },
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad-envelope-len");

    // Epoch must NOT have advanced (atomic abort).
    let resp = client
        .get(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/epoch"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["epoch"].as_u64().unwrap(), 0);
}

#[tokio::test]
async fn rotate_rejects_unsubscribed_recipient() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    // No subscribe at all.
    let env_b64 = URL_SAFE_NO_PAD.encode(vec![0u8; 80]);
    let resp = client
        .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/rotate"))
        .json(&json!({
            "envelopes": [
                { "subscriber_id": SUB_HEX, "envelope_b64": env_b64 },
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "unknown-subscriber");
}

#[tokio::test]
async fn rotate_empty_batch_still_bumps_epoch() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    let resp = client
        .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/rotate"))
        .json(&json!({ "envelopes": [] }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["epoch"].as_u64().unwrap(), 1);
}

#[tokio::test]
async fn rotate_rejects_bad_envelope_b64() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_two_subs(&client, addr).await;
    let resp = client
        .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/rotate"))
        .json(&json!({
            "envelopes": [
                { "subscriber_id": SUB_HEX, "envelope_b64": "!!!not-base64!!!" },
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad-envelope-b64");
}

#[tokio::test]
async fn payload_round_trips_byte_identical() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    let payload: Vec<u8> = (0..=255u8).collect();
    client
        .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/posts"))
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/posts?after=0"
        ))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let b64 = body["posts"][0]["payload_b64"].as_str().unwrap();
    let decoded = URL_SAFE_NO_PAD.decode(b64).unwrap();
    assert_eq!(
        decoded, payload,
        "server must forward payload byte-identical"
    );
}
