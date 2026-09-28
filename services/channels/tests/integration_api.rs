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
        // Match the production default (ADR-0031) so the
        // retention tests below exercise the real window.
        envelope_retention_epochs: 4,
    }
}

async fn spawn() -> SocketAddr {
    let cfg = test_config();
    let store = Arc::new(
        InMemoryChannelStore::new().with_envelope_retention(cfg.envelope_retention_epochs),
    );
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

// ADR-0058: a subscriber key is a valid Ed25519 public key (the RFC 8032
// section 7.1 test 2 and test 1 keys, in ascending order).
const SUB_HEX: &str = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c";

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

#[tokio::test]
async fn a_subscriber_key_that_is_not_a_full_order_ed25519_point_is_400() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    // The identity point (order 1), and y = 2 (not on the curve).
    let identity = format!("01{}", "00".repeat(31));
    let off_curve = format!("02{}", "00".repeat(31));
    for bad in [identity, off_curve] {
        let resp = client
            .post(format!(
                "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
            ))
            .json(&json!({ "subscriber_pubkey_hex": bad }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["code"].as_str().unwrap(), "bad-subscriber-id");
    }
}

/// SubscribeRequest's expected envelope length must match
/// `lakko_channels::subscribe::SEED_ENVELOPE_LEN`. Compile-
/// time assertion so a future bump in either crate is
/// caught by the build.
const _ASSERT_ENVELOPE_LEN: () =
    assert!(lakko_svc_channels::api::EXPECTED_ENVELOPE_LEN == lakko_channels::SEED_ENVELOPE_LEN);

const SUB_HEX_B: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

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

// ---------------------------------------------------------
// ADR-0030 — staged prepare + atomic commit rotation fanout
// ---------------------------------------------------------

async fn envelope_for(addr: SocketAddr, client: &reqwest::Client, sub: &str) -> Vec<u8> {
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{sub}/envelope"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    URL_SAFE_NO_PAD
        .decode(body["envelope_b64"].as_str().unwrap())
        .unwrap()
}

#[tokio::test]
async fn stage_then_commit_end_to_end() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_two_subs(&client, addr).await;
    let alice: Vec<u8> = (0..80u8).collect();
    let bob: Vec<u8> = (10..90u8).collect();

    // Stage chunk 1 (alice) — the epoch must NOT move.
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/rotate/stage"
        ))
        .json(&json!({
            "target_epoch": 1,
            "envelopes": [
                { "subscriber_id": SUB_HEX, "envelope_b64": URL_SAFE_NO_PAD.encode(&alice) },
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["target_epoch"].as_u64().unwrap(), 1);
    assert_eq!(body["staged"].as_u64().unwrap(), 1);

    // Epoch still 0 mid-stage.
    let resp = client
        .get(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/epoch"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["epoch"].as_u64().unwrap(), 0);

    // Stage chunk 2 (bob) — resumable second chunk.
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/rotate/stage"
        ))
        .json(&json!({
            "target_epoch": 1,
            "envelopes": [
                { "subscriber_id": SUB_HEX_B, "envelope_b64": URL_SAFE_NO_PAD.encode(&bob) },
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Commit.
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/rotate/commit"
        ))
        .json(&json!({ "target_epoch": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["epoch"].as_u64().unwrap(), 1);

    // Both staged envelopes are now live + byte-identical.
    assert_eq!(envelope_for(addr, &client, SUB_HEX).await, alice);
    assert_eq!(envelope_for(addr, &client, SUB_HEX_B).await, bob);
}

#[tokio::test]
async fn staged_envelope_is_not_served_before_commit() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_two_subs(&client, addr).await;
    // Deliver an "epoch 0" envelope directly.
    let old: Vec<u8> = vec![0x11; 80];
    client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelope"
        ))
        .body(old.clone())
        .send()
        .await
        .unwrap();
    // Stage a NEW envelope for epoch 1 but do not commit.
    client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/rotate/stage"
        ))
        .json(&json!({
            "target_epoch": 1,
            "envelopes": [
                { "subscriber_id": SUB_HEX, "envelope_b64": URL_SAFE_NO_PAD.encode(vec![0x22u8; 80]) },
            ]
        }))
        .send()
        .await
        .unwrap();
    // The read path still returns the OLD envelope.
    assert_eq!(
        envelope_for(addr, &client, SUB_HEX).await,
        old,
        "staged envelope must NOT be served before commit"
    );
}

#[tokio::test]
async fn stage_with_wrong_target_epoch_is_409() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_two_subs(&client, addr).await;
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/rotate/stage"
        ))
        .json(&json!({
            "target_epoch": 5,
            "envelopes": [
                { "subscriber_id": SUB_HEX, "envelope_b64": URL_SAFE_NO_PAD.encode(vec![0u8; 80]) },
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "stale-target-epoch");
}

#[tokio::test]
async fn commit_with_wrong_target_epoch_is_409() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_two_subs(&client, addr).await;
    // Commit epoch 2 while current is 0 (valid target is 1).
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/rotate/commit"
        ))
        .json(&json!({ "target_epoch": 2 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "stale-target-epoch");
    // Epoch untouched.
    let resp = client
        .get(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/epoch"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["epoch"].as_u64().unwrap(), 0);
}

#[tokio::test]
async fn stage_chunk_over_cap_is_413() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_two_subs(&client, addr).await;
    // One entry over the cap. Entries are cheap dummies —
    // the cap check runs before any decode / store call.
    let over = lakko_svc_channels::api::MAX_STAGE_CHUNK_ENTRIES + 1;
    let envelopes: Vec<serde_json::Value> = (0..over)
        .map(|_| json!({ "subscriber_id": SUB_HEX, "envelope_b64": "" }))
        .collect();
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/rotate/stage"
        ))
        .json(&json!({ "target_epoch": 1, "envelopes": envelopes }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "chunk-too-large");
}

#[tokio::test]
async fn stage_to_unsubscribed_recipient_is_409() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    // No subscribe at all.
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/rotate/stage"
        ))
        .json(&json!({
            "target_epoch": 1,
            "envelopes": [
                { "subscriber_id": SUB_HEX, "envelope_b64": URL_SAFE_NO_PAD.encode(vec![0u8; 80]) },
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "unknown-subscriber");
}

// ---------------------------------------------------------
// ADR-0031 — multi-epoch envelope retention + catch-up
// ---------------------------------------------------------

/// Create the channel + subscribe one subscriber (SUB_HEX).
async fn create_channel_with_one_sub(client: &reqwest::Client, addr: SocketAddr) {
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
}

/// Drive `rotations` single-shot rotations, each delivering a
/// distinct 80-byte envelope to SUB_HEX; returns the epoch
/// after the last rotation.
async fn rotate_sub_n_times(client: &reqwest::Client, addr: SocketAddr, rotations: u8) {
    for e in 1..=rotations {
        let env: Vec<u8> = vec![e; 80];
        let resp = client
            .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/rotate"))
            .json(&json!({
                "envelopes": [
                    { "subscriber_id": SUB_HEX, "envelope_b64": URL_SAFE_NO_PAD.encode(&env) },
                ]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }
}

#[tokio::test]
async fn fetch_envelopes_returns_retained_set_ascending() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_one_sub(&client, addr).await;
    // Five rotations -> envelopes filed at epochs 1..=5.
    // Default retention is 4, so epoch 1 is pruned.
    rotate_sub_n_times(&client, addr, 5).await;

    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelopes"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let envs = body["envelopes"].as_array().unwrap();
    // Highest 4 epochs, ascending.
    assert_eq!(envs.len(), 4);
    let epochs: Vec<u64> = envs.iter().map(|e| e["epoch"].as_u64().unwrap()).collect();
    assert_eq!(epochs, vec![2, 3, 4, 5], "epoch 1 must be pruned");
    // Each entry decodes to its epoch's marker byte.
    for entry in envs {
        let epoch = u8::try_from(entry["epoch"].as_u64().unwrap()).unwrap();
        let decoded = URL_SAFE_NO_PAD
            .decode(entry["envelope_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, vec![epoch; 80]);
    }

    // The singular fetch returns the highest retained epoch.
    let single = envelope_for(addr, &client, SUB_HEX).await;
    assert_eq!(single, vec![5u8; 80]);
}

#[tokio::test]
async fn fetch_envelopes_empty_when_none_delivered() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_one_sub(&client, addr).await;
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelopes"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["envelopes"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn fetch_envelopes_unknown_subscriber_is_404() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    // Channel exists but the subscriber never registered.
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelopes"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "unknown-subscriber");
}

#[tokio::test]
async fn deliver_with_explicit_epoch_files_there() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_one_sub(&client, addr).await;
    // Raise the current epoch to 3 with three empty rotations.
    for _ in 0..3 {
        client
            .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/rotate"))
            .json(&json!({ "envelopes": [] }))
            .send()
            .await
            .unwrap();
    }
    // Deliver at explicit epoch 2 (<= current 3) -> 204.
    let env: Vec<u8> = (0..80u8).collect();
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelope?epoch=2"
        ))
        .body(env.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    // The plural fetch shows exactly epoch 2.
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelopes"
        ))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let envs = body["envelopes"].as_array().unwrap();
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0]["epoch"].as_u64().unwrap(), 2);
    let decoded = URL_SAFE_NO_PAD
        .decode(envs[0]["envelope_b64"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, env);
}

#[tokio::test]
async fn deliver_with_future_epoch_is_409() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create_channel_with_one_sub(&client, addr).await;
    // current_epoch is 0; asking to file at epoch 99 is refused.
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{SUB_HEX}/envelope?epoch=99"
        ))
        .body(vec![0u8; 80])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "envelope-epoch-in-future");
}
