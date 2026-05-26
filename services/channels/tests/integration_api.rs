// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end integration tests for the channels service.
//!
//! Each case boots a real axum server on a random
//! localhost port, drives it with `reqwest`, and asserts
//! the wire-format contract documented in `api.rs`.

use std::net::SocketAddr;

use lakko_svc_channels::{router, AppState, ChannelStore, Config};
use serde_json::json;
use tokio::net::TcpListener;

fn test_config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_post_bytes: 64 * 1024,
        fetch_limit_default: 50,
        fetch_limit_max: 500,
    }
}

async fn spawn() -> SocketAddr {
    let cfg = test_config();
    let store = ChannelStore::new();
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
        .get(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/posts?after=0"))
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
        .get(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/posts?after=1"))
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

#[tokio::test]
async fn payload_round_trips_byte_identical() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
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
    assert_eq!(decoded, payload, "server must forward payload byte-identical");
}
