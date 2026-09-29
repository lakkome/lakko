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
use lakko_anon_token::{MasterSecret, TokenContext, DEFAULT_WINDOW_SECS};
use lakko_anon_token_server::{
    current_epoch, testing::mint_header, AnonTokenServerConfig, TokenVerifier, TOKEN_HEADER,
};
use lakko_channels::copublisher::{ChannelCoPublisherSigner, COPUBLISHER_PROOF_HEADER_NAME};
use lakko_channels::owner::{ChannelOwnerSigner, OwnerRequest, OWNER_PROOF_HEADER_NAME};
use lakko_svc_channels::{router, AppState, Config, InMemoryChannelStore};
use serde_json::json;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;

/// The anonymous-token master secret every test server verifies under.
const MASTER_HEX: &str = "0303030303030303030303030303030303030303030303030303030303030303";
/// The owner's account seed; the owner key is derived from it per channel.
const ACCOUNT_SEED: [u8; 32] = [5; 32];

fn master() -> MasterSecret {
    MasterSecret::from_bytes([3; 32])
}

fn token_verifier() -> TokenVerifier {
    TokenVerifier::in_memory(AnonTokenServerConfig::from_values(Some(MASTER_HEX), None).unwrap())
}

/// A fresh spendable token of `context`.
fn token(context: TokenContext) -> String {
    mint_header(&master(), context, current_epoch(DEFAULT_WINDOW_SECS))
}

fn subscribe_token() -> String {
    token(TokenContext::ChannelSubscribe)
}

fn owner_signer_for(channel_hex: &str) -> ChannelOwnerSigner {
    let id: [u8; 16] = hex::decode(channel_hex).unwrap().try_into().unwrap();
    ChannelOwnerSigner::from_account_seed(&ACCOUNT_SEED, &id)
}

fn create_body_for(channel_hex: &str) -> serde_json::Value {
    json!({
        "channel_id": channel_hex,
        "owner_pubkey_hex": hex::encode(owner_signer_for(channel_hex).verifying_key().to_bytes()),
    })
}

fn create_body() -> serde_json::Value {
    create_body_for(CHANNEL_HEX)
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// A distinct nonce per proof in this test process.
fn fresh_nonce() -> [u8; 16] {
    static NEXT: AtomicU8 = AtomicU8::new(0);
    let mut n = rand_bytes();
    n[0] = NEXT.fetch_add(1, Ordering::Relaxed);
    n
}

fn rand_bytes() -> [u8; 16] {
    let mut out = [0u8; 16];
    lakko_crypto::random::fill(&mut out);
    out
}

/// The owner proof header for `CHANNEL_HEX`'s owner over one request.
fn owner_proof(method: &str, path: &str, query: &str, body: &[u8]) -> String {
    owner_proof_for(CHANNEL_HEX, method, path, query, body)
}

fn owner_proof_for(
    channel_hex: &str,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
) -> String {
    let req = OwnerRequest {
        method,
        canonical_path: path,
        query,
        body,
    };
    owner_signer_for(channel_hex)
        .sign_at(&req, now_ms(), fresh_nonce())
        .encode()
}

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
        internal_bind: "127.0.0.1:0".parse().unwrap(),
    }
}

async fn spawn() -> SocketAddr {
    let cfg = test_config();
    let store = Arc::new(
        InMemoryChannelStore::new().with_envelope_retention(cfg.envelope_retention_epochs),
    );
    let state = AppState::new(cfg, store, token_verifier());
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
        .json(&create_body())
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
        .json(&create_body())
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
        .json(&create_body())
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
        .header(TOKEN_HEADER, subscribe_token())
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
        .json(&create_body())
        .send()
        .await
        .unwrap();
    // Subscribe.
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
        ))
        .header(TOKEN_HEADER, subscribe_token())
        .json(&json!({ "subscriber_pubkey_hex": SUB_HEX }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["subscriber_id"].as_str().unwrap(), SUB_HEX);
    // List — owner-signed.
    let path = format!("/v1/channels/{CHANNEL_HEX}/subscribers");
    let resp = client
        .get(format!("http://{addr}{path}"))
        .header(OWNER_PROOF_HEADER_NAME, owner_proof("GET", &path, "", b""))
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
        .json(&create_body())
        .send()
        .await
        .unwrap();
    client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
        ))
        .header(TOKEN_HEADER, subscribe_token())
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
        .json(&create_body())
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
        .json(&create_body())
        .send()
        .await
        .unwrap();
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
        ))
        .header(TOKEN_HEADER, subscribe_token())
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
        .json(&create_body())
        .send()
        .await
        .unwrap();
    for sub in [SUB_HEX, SUB_HEX_B] {
        client
            .post(format!(
                "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
            ))
            .header(TOKEN_HEADER, subscribe_token())
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
        .json(&create_body())
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
        .json(&create_body())
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
        .json(&create_body())
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
        .json(&create_body())
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
        .json(&create_body())
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
        .json(&create_body())
        .send()
        .await
        .unwrap();
    client
        .post(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
        ))
        .header(TOKEN_HEADER, subscribe_token())
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
        .json(&create_body())
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

// ---- ADR-0059 S2: owner key, token-gated subscribe, directory -------------

use lakko_channels::listing::{compose_channel_listing, ChannelListing};

const OTHER_CHANNEL_HEX: &str = "fedcba9876543210fedcba9876543210";
const INTERNAL_SECRET: [u8; 16] = [9; 16];

/// A server plus its internal moderation listener.
async fn spawn_with_internal() -> (SocketAddr, SocketAddr) {
    let cfg = test_config();
    let store = Arc::new(InMemoryChannelStore::new());
    let state = AppState::new(cfg, store, token_verifier());
    let public = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (a, b) = (public.local_addr().unwrap(), internal.local_addr().unwrap());
    let internal_app = lakko_svc_channels::internal::internal_router(
        Arc::clone(&state),
        Arc::new(INTERNAL_SECRET.to_vec()),
    );
    let app = router(state);
    tokio::spawn(async move {
        let _ = axum::serve(public, app).await;
    });
    tokio::spawn(async move {
        let _ = axum::serve(internal, internal_app).await;
    });
    (a, b)
}

fn listing(
    channel_hex: &str,
    name: &str,
    topic: u8,
    language: &str,
    updated_at_ms: u64,
) -> Vec<u8> {
    compose_channel_listing(&ChannelListing {
        channel_id: hex::decode(channel_hex).unwrap().try_into().unwrap(),
        handle: format!("h{}", &channel_hex[..8]),
        name: name.to_string(),
        description: "A channel about things".to_string(),
        topic,
        language: language.to_string(),
        updated_at_ms,
    })
    .unwrap()
}

async fn create(client: &reqwest::Client, addr: SocketAddr, channel_hex: &str) {
    let resp = client
        .post(format!("http://{addr}/v1/channels"))
        .json(&create_body_for(channel_hex))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

async fn put_listing(
    client: &reqwest::Client,
    addr: SocketAddr,
    channel_hex: &str,
    body: Vec<u8>,
) -> reqwest::Response {
    let path = format!("/v1/channels/{channel_hex}/listing");
    let proof = owner_proof_for(channel_hex, "PUT", &path, "", &body);
    client
        .put(format!("http://{addr}{path}"))
        .header(OWNER_PROOF_HEADER_NAME, proof)
        .header("content-type", "application/octet-stream")
        .body(body)
        .send()
        .await
        .unwrap()
}

async fn code_of(resp: reqwest::Response) -> String {
    let body: serde_json::Value = resp.json().await.unwrap();
    body["code"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn the_owner_key_is_fixed_at_creation() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create(&client, addr, CHANNEL_HEX).await;
    // The same key again is idempotent.
    create(&client, addr, CHANNEL_HEX).await;
    // Another key cannot take the channel over.
    let resp = client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({
            "channel_id": CHANNEL_HEX,
            "owner_pubkey_hex": hex::encode(
                owner_signer_for(OTHER_CHANNEL_HEX).verifying_key().to_bytes()
            ),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    assert_eq!(code_of(resp).await, "channel-exists");
    // A missing or malformed key is refused.
    for bad in [
        json!({ "channel_id": CHANNEL_HEX }),
        json!({ "channel_id": CHANNEL_HEX, "owner_pubkey_hex": "abcd" }),
    ] {
        let resp = client
            .post(format!("http://{addr}/v1/channels"))
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_client_error());
    }
}

#[tokio::test]
async fn subscribing_spends_one_channel_subscribe_token() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create(&client, addr, CHANNEL_HEX).await;
    let url = format!("http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers");
    let body = json!({ "subscriber_pubkey_hex": SUB_HEX });

    let resp = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(code_of(resp).await, "anon-token-required");

    let wrong = token(TokenContext::DirectorySearch);
    let resp = client
        .post(&url)
        .header(TOKEN_HEADER, &wrong)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(code_of(resp).await, "anon-token-wrong-context");

    let t = subscribe_token();
    let resp = client
        .post(&url)
        .header(TOKEN_HEADER, &t)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let resp = client
        .post(&url)
        .header(TOKEN_HEADER, &t)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(code_of(resp).await, "anon-token-spent");

    // A malformed body costs no token: the same token still spends after.
    let fresh = subscribe_token();
    let resp = client
        .post(&url)
        .header(TOKEN_HEADER, &fresh)
        .json(&json!({ "subscriber_pubkey_hex": "short" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let resp = client
        .post(&url)
        .header(TOKEN_HEADER, &fresh)
        .json(&json!({ "subscriber_pubkey_hex": SUB_HEX_B }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

#[tokio::test]
async fn the_subscriber_list_is_owner_signed_and_a_proof_is_single_use() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create(&client, addr, CHANNEL_HEX).await;
    let path = format!("/v1/channels/{CHANNEL_HEX}/subscribers");
    let url = format!("http://{addr}{path}");

    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(code_of(resp).await, "owner-proof-required");

    // Another channel's owner key does not open this channel.
    let foreign = owner_proof_for(OTHER_CHANNEL_HEX, "GET", &path, "", b"");
    let resp = client
        .get(&url)
        .header(OWNER_PROOF_HEADER_NAME, foreign)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // A proof over another method does not either.
    let wrong_method = owner_proof("DELETE", &path, "", b"");
    let resp = client
        .get(&url)
        .header(OWNER_PROOF_HEADER_NAME, wrong_method)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let proof = owner_proof("GET", &path, "", b"");
    let resp = client
        .get(&url)
        .header(OWNER_PROOF_HEADER_NAME, &proof)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // The same proof again is a replay.
    let resp = client
        .get(&url)
        .header(OWNER_PROOF_HEADER_NAME, &proof)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(code_of(resp).await, "owner-proof-required");
}

#[tokio::test]
async fn a_listing_is_stored_served_in_its_bucket_searched_and_withdrawn() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create(&client, addr, CHANNEL_HEX).await;
    let body = listing(CHANNEL_HEX, "Rust Weekly", 3, "en", 1_000);

    // Without a proof, nothing is stored.
    let resp = client
        .put(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/listing"))
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = put_listing(&client, addr, CHANNEL_HEX, body.clone()).await;
    assert_eq!(resp.status(), 204);

    // Single listing.
    let resp = client
        .get(format!("http://{addr}/v1/directory/listing/{CHANNEL_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["cache-control"], "public, max-age=300");
    let item: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(item["channel_id"], CHANNEL_HEX);
    assert_eq!(item["subscribers"], "<10");
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(item["listing_b64"].as_str().unwrap())
            .unwrap(),
        body
    );

    // Bucket.
    let resp = client
        .get(format!("http://{addr}/v1/directory/bucket/3/en"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["cache-control"], "public, max-age=3600");
    let bucket: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(bucket["items"].as_array().unwrap().len(), 1);
    let resp = client
        .get(format!("http://{addr}/v1/directory/bucket/3/de"))
        .send()
        .await
        .unwrap();
    let other: serde_json::Value = resp.json().await.unwrap();
    assert!(other["items"].as_array().unwrap().is_empty());
    for bad in ["200/en", "3/EN", "3/english", "x/en"] {
        let resp = client
            .get(format!("http://{addr}/v1/directory/bucket/{bad}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "{bad}");
    }

    // Search: needs a directory-search token; case-insensitive.
    let search = format!("http://{addr}/v1/directory/search?q=WEEKLY");
    let resp = client.get(&search).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client
        .get(&search)
        .header(TOKEN_HEADER, token(TokenContext::DirectorySearch))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let found: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    let resp = client
        .get(format!("http://{addr}/v1/directory/search?q=x"))
        .header(TOKEN_HEADER, token(TokenContext::DirectorySearch))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Withdraw.
    let path = format!("/v1/channels/{CHANNEL_HEX}/listing");
    let resp = client
        .delete(format!("http://{addr}{path}"))
        .header(
            OWNER_PROOF_HEADER_NAME,
            owner_proof("DELETE", &path, "", b""),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let resp = client
        .get(format!("http://{addr}/v1/directory/listing/{CHANNEL_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn items_carry_the_band_not_the_count_and_unknown_topics_file_under_other() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create(&client, addr, CHANNEL_HEX).await;
    for i in 0u8..12 {
        let resp = client
            .post(format!(
                "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
            ))
            .header(TOKEN_HEADER, subscribe_token())
            .json(&json!({ "subscriber_pubkey_hex": hex::encode([i; 32]) }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }
    // Topic 250 is not a known topic: it is listed under Other (0).
    let resp = put_listing(
        &client,
        addr,
        CHANNEL_HEX,
        listing(CHANNEL_HEX, "Misc", 250, "fi", 1),
    )
    .await;
    assert_eq!(resp.status(), 204);
    let bucket: serde_json::Value = client
        .get(format!("http://{addr}/v1/directory/bucket/0/fi"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let items = bucket["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["subscribers"], "10+");
    assert!(items[0].get("count").is_none());
}

#[tokio::test]
async fn moderation_runs_only_on_the_internal_listener() {
    let (addr, internal) = spawn_with_internal().await;
    let client = reqwest::Client::new();
    create(&client, addr, CHANNEL_HEX).await;
    assert_eq!(
        put_listing(
            &client,
            addr,
            CHANNEL_HEX,
            listing(CHANNEL_HEX, "Spam", 1, "en", 1)
        )
        .await
        .status(),
        204
    );
    let moderate = format!("http://{internal}/internal/v1/channels/{CHANNEL_HEX}/moderation");
    let secret = base64::engine::general_purpose::STANDARD.encode(INTERNAL_SECRET);

    // Not on the public router.
    let resp = client
        .post(format!(
            "http://{addr}/internal/v1/channels/{CHANNEL_HEX}/moderation"
        ))
        .json(&json!({ "action": "block", "reason": "spam" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    // Without the secret, refused.
    let resp = client
        .post(&moderate)
        .json(&json!({ "action": "block", "reason": "spam" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    // A bad reason code is refused.
    let resp = client
        .post(&moderate)
        .header("x-lakko-internal-secret", &secret)
        .json(&json!({ "action": "block", "reason": "Not A Code" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let resp = client
        .post(&moderate)
        .header("x-lakko-internal-secret", &secret)
        .json(&json!({ "action": "block", "reason": "spam" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let resp = client
        .get(format!("http://{addr}/v1/directory/listing/{CHANNEL_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let resp = put_listing(
        &client,
        addr,
        CHANNEL_HEX,
        listing(CHANNEL_HEX, "Spam", 1, "en", 2),
    )
    .await;
    assert_eq!(resp.status(), 403);
    assert_eq!(code_of(resp).await, "listing-blocked");

    let resp = client
        .post(&moderate)
        .header("x-lakko-internal-secret", &secret)
        .json(&json!({ "action": "unblock", "reason": "appeal-granted" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let resp = put_listing(
        &client,
        addr,
        CHANNEL_HEX,
        listing(CHANNEL_HEX, "Fine", 1, "en", 3),
    )
    .await;
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
async fn a_stale_misdirected_or_malformed_listing_is_refused() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create(&client, addr, CHANNEL_HEX).await;
    let resp = put_listing(
        &client,
        addr,
        CHANNEL_HEX,
        listing(CHANNEL_HEX, "New", 3, "en", 1_000),
    )
    .await;
    assert_eq!(resp.status(), 204);
    // A listing not newer than the stored one is refused.
    let resp = put_listing(
        &client,
        addr,
        CHANNEL_HEX,
        listing(CHANNEL_HEX, "Old", 3, "en", 1_000),
    )
    .await;
    assert_eq!(resp.status(), 409);
    assert_eq!(code_of(resp).await, "stale-listing");
    // A listing naming another channel is refused.
    let resp = put_listing(
        &client,
        addr,
        CHANNEL_HEX,
        listing(OTHER_CHANNEL_HEX, "X", 3, "en", 2_000),
    )
    .await;
    assert_eq!(resp.status(), 400);
    assert_eq!(code_of(resp).await, "listing-channel-mismatch");
    // Bytes the codec refuses are refused.
    let resp = put_listing(&client, addr, CHANNEL_HEX, b"not a listing".to_vec()).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(code_of(resp).await, "bad-listing");
}

/// A co-publisher's account seed (another account than the owner's).
const COPUB_SEED: [u8; 32] = [6; 32];

fn copub_signer_for(channel_hex: &str, seed: &[u8; 32]) -> ChannelCoPublisherSigner {
    let id: [u8; 16] = hex::decode(channel_hex).unwrap().try_into().unwrap();
    ChannelCoPublisherSigner::from_account_seed(seed, &id)
}

fn copub_proof(channel_hex: &str, seed: &[u8; 32], method: &str, path: &str) -> String {
    let req = OwnerRequest {
        method,
        canonical_path: path,
        query: "",
        body: b"",
    };
    copub_signer_for(channel_hex, seed)
        .sign_at(&req, now_ms(), fresh_nonce())
        .encode()
}

async fn appoint(
    client: &reqwest::Client,
    addr: SocketAddr,
    channel_hex: &str,
    key_hex: &str,
    method: reqwest::Method,
) -> reqwest::Response {
    let path = format!("/v1/channels/{channel_hex}/copublishers/{key_hex}");
    client
        .request(method.clone(), format!("http://{addr}{path}"))
        .header(
            OWNER_PROOF_HEADER_NAME,
            owner_proof_for(channel_hex, method.as_str(), &path, "", b""),
        )
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn an_appointed_copublisher_reads_the_subscriber_list_until_revoked() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create(&client, addr, CHANNEL_HEX).await;
    let path = format!("/v1/channels/{CHANNEL_HEX}/subscribers");
    let url = format!("http://{addr}{path}");
    let key_hex = hex::encode(
        copub_signer_for(CHANNEL_HEX, &COPUB_SEED)
            .verifying_key()
            .to_bytes(),
    );

    // Not appointed yet: the same single refusal.
    let resp = client
        .get(&url)
        .header(
            COPUBLISHER_PROOF_HEADER_NAME,
            copub_proof(CHANNEL_HEX, &COPUB_SEED, "GET", &path),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(code_of(resp).await, "owner-proof-required");

    // A co-publisher cannot appoint itself: the route is owner-only.
    let appoint_path = format!("/v1/channels/{CHANNEL_HEX}/copublishers/{key_hex}");
    let resp = client
        .put(format!("http://{addr}{appoint_path}"))
        .header(
            COPUBLISHER_PROOF_HEADER_NAME,
            copub_proof(CHANNEL_HEX, &COPUB_SEED, "PUT", &appoint_path),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = appoint(&client, addr, CHANNEL_HEX, &key_hex, reqwest::Method::PUT).await;
    assert_eq!(resp.status(), 204);
    // Idempotent.
    let resp = appoint(&client, addr, CHANNEL_HEX, &key_hex, reqwest::Method::PUT).await;
    assert_eq!(resp.status(), 204);

    let proof = copub_proof(CHANNEL_HEX, &COPUB_SEED, "GET", &path);
    let resp = client
        .get(&url)
        .header(COPUBLISHER_PROOF_HEADER_NAME, &proof)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // Replay refused through the shared nonce table.
    let resp = client
        .get(&url)
        .header(COPUBLISHER_PROOF_HEADER_NAME, &proof)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // The appointed key opens only its own channel.
    create(&client, addr, OTHER_CHANNEL_HEX).await;
    let other_path = format!("/v1/channels/{OTHER_CHANNEL_HEX}/subscribers");
    let resp = client
        .get(format!("http://{addr}{other_path}"))
        .header(
            COPUBLISHER_PROOF_HEADER_NAME,
            copub_proof(CHANNEL_HEX, &COPUB_SEED, "GET", &other_path),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Nor the owner-only listing route.
    let listing_path = format!("/v1/channels/{CHANNEL_HEX}/listing");
    let resp = client
        .delete(format!("http://{addr}{listing_path}"))
        .header(
            COPUBLISHER_PROOF_HEADER_NAME,
            copub_proof(CHANNEL_HEX, &COPUB_SEED, "DELETE", &listing_path),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Revoked: refused again.
    let resp = appoint(
        &client,
        addr,
        CHANNEL_HEX,
        &key_hex,
        reqwest::Method::DELETE,
    )
    .await;
    assert_eq!(resp.status(), 204);
    let resp = client
        .get(&url)
        .header(
            COPUBLISHER_PROOF_HEADER_NAME,
            copub_proof(CHANNEL_HEX, &COPUB_SEED, "GET", &path),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn appointments_are_bounded_and_refuse_a_bad_key() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    create(&client, addr, CHANNEL_HEX).await;
    let resp = appoint(
        &client,
        addr,
        CHANNEL_HEX,
        &"ab".repeat(31),
        reqwest::Method::PUT,
    )
    .await;
    assert_eq!(resp.status(), 400);
    assert_eq!(code_of(resp).await, "bad-copublisher-key");
    for i in 0..lakko_channels::MAX_COPUBLISHERS {
        let seed = [u8::try_from(i).unwrap() + 100; 32];
        let key_hex = hex::encode(
            copub_signer_for(CHANNEL_HEX, &seed)
                .verifying_key()
                .to_bytes(),
        );
        let resp = appoint(&client, addr, CHANNEL_HEX, &key_hex, reqwest::Method::PUT).await;
        assert_eq!(resp.status(), 204, "appointment {i}");
    }
    let key_hex = hex::encode(
        copub_signer_for(CHANNEL_HEX, &COPUB_SEED)
            .verifying_key()
            .to_bytes(),
    );
    let resp = appoint(&client, addr, CHANNEL_HEX, &key_hex, reqwest::Method::PUT).await;
    assert_eq!(resp.status(), 409);
    assert_eq!(code_of(resp).await, "too-many-copublishers");
}
