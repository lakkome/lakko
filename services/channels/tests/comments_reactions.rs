// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Comments and reactions under a post (ADR-0087 section 13a), driven over
//! HTTP against the in-memory store with the real client-side keys from
//! `lakko-channels`.

use std::net::SocketAddr;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_token::{MasterSecret, TokenContext, DEFAULT_WINDOW_SECS};
use lakko_anon_token_server::{
    current_epoch, testing::mint_header, AnonTokenServerConfig, TokenVerifier, TOKEN_HEADER,
};
use lakko_channels::epoch::EpochId;
use lakko_channels::owner::{ChannelOwnerSigner, OwnerRequest, OWNER_PROOF_HEADER_NAME};
use lakko_channels::{
    seal_comment, Comment, CommentAuthor, CommentPlace, CommentReactionKey,
    CommentReactionWriteKey, CommentWriteKey, ContentKeySeed, ReactionKey, ReactionWriteKey,
    WRITE_PROOF_HEADER_NAME,
};
use lakko_svc_channels::{router, AppState, Config, InMemoryChannelStore};
use serde_json::{json, Value};
use tokio::net::TcpListener;

const MASTER_HEX: &str = "0303030303030303030303030303030303030303030303030303030303030303";
const OWNER_SEED: [u8; 32] = [5; 32];
const ALICE: [u8; 32] = [0x41; 32];
const BOB: [u8; 32] = [0x42; 32];
const CAROL: [u8; 32] = [0x43; 32];
const CHANNEL: [u8; 16] = [0x0C; 16];
const THUMBS: &str = "\u{1F44D}";

fn channel_hex() -> String {
    hex::encode(CHANNEL)
}

fn token(context: TokenContext) -> String {
    mint_header(
        &MasterSecret::from_bytes([3; 32]),
        context,
        current_epoch(DEFAULT_WINDOW_SECS),
    )
}

fn nonce() -> [u8; 16] {
    let mut n = [0u8; 16];
    lakko_crypto::random::fill(&mut n);
    n
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

fn owner() -> ChannelOwnerSigner {
    ChannelOwnerSigner::from_account_seed(&OWNER_SEED, &CHANNEL)
}

fn owner_proof(method: &str, path: &str, body: &[u8]) -> String {
    owner()
        .sign_at(
            &OwnerRequest {
                method,
                canonical_path: path,
                query: "",
                body,
            },
            now_ms(),
            nonce(),
        )
        .encode()
}

async fn spawn() -> SocketAddr {
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_post_bytes: 64 * 1024,
        fetch_limit_default: 50,
        fetch_limit_max: 500,
        database_url: None,
        retention_secs: 0,
        subscriber_retention_secs: 0,
        prune_interval_secs: 0,
        envelope_retention_epochs: 4,
        internal_bind: "127.0.0.1:0".parse().unwrap(),
    };
    let verifier = TokenVerifier::in_memory(
        AnonTokenServerConfig::from_values(Some(MASTER_HEX), None).unwrap(),
    );
    let state = AppState::new(cfg, Arc::new(InMemoryChannelStore::new()), verifier);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// Create the channel and publish two posts (cursors 1 and 2).
async fn channel_with_posts(addr: SocketAddr) -> reqwest::Client {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({
            "channel_id": channel_hex(),
            "owner_pubkey_hex": hex::encode(owner().verifying_key().to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    for n in 1u8..=2 {
        let path = format!("/v1/channels/{}/posts", channel_hex());
        let body = vec![n; 8];
        let resp = client
            .post(format!("http://{addr}{path}"))
            .header(OWNER_PROOF_HEADER_NAME, owner_proof("POST", &path, &body))
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }
    client
}

fn label_of(post: u64, emoji: &str) -> [u8; 16] {
    ReactionKey::derive(
        &ContentKeySeed::from_bytes_for_test([9; 32]),
        &CHANNEL,
        EpochId::new(0),
    )
    .unwrap()
    .label(post, emoji)
    .unwrap()
}

async fn react(
    client: &reqwest::Client,
    addr: SocketAddr,
    account: &[u8; 32],
    post: u64,
    label: [u8; 16],
) -> reqwest::Response {
    let key = ReactionWriteKey::new(account, &CHANNEL, post, label);
    client
        .post(format!(
            "http://{addr}/v1/channels/{}/posts/{post}/reactions",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_add(now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, token(TokenContext::ChannelReact))
        .json(&json!({
            "writer_key_hex": hex::encode(key.public_key()),
            "label_hex": hex::encode(label),
        }))
        .send()
        .await
        .unwrap()
}

async fn counts(client: &reqwest::Client, addr: SocketAddr, posts: &str) -> reqwest::Response {
    client
        .get(format!(
            "http://{addr}/v1/channels/{}/reactions?posts={posts}",
            channel_hex()
        ))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn reactions_are_added_counted_deduplicated_and_taken_off() {
    let addr = spawn().await;
    let client = channel_with_posts(addr).await;
    let label = label_of(1, THUMBS);

    let resp = react(&client, addr, &ALICE, 1, label).await;
    assert_eq!(resp.status(), 201);
    assert_eq!(resp.json::<Value>().await.unwrap()["added"], true);
    // The same reaction again is one row.
    let resp = react(&client, addr, &ALICE, 1, label).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["added"], false);
    // Another person's same reaction is a second row under another key.
    assert_eq!(react(&client, addr, &BOB, 1, label).await.status(), 201);
    let heart = label_of(1, "\u{2764}\u{FE0F}");
    assert_eq!(react(&client, addr, &BOB, 1, heart).await.status(), 201);

    let resp = counts(&client, addr, "1,2,1").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["cache-control"].to_str().unwrap(),
        "public, max-age=15"
    );
    let body: Value = resp.json().await.unwrap();
    let posts = body["posts"].as_array().unwrap();
    assert_eq!(posts.len(), 2, "a repeated cursor is read once");
    assert_eq!(posts[0]["cursor"], 1);
    assert_eq!(posts[0]["labels"][0]["label_hex"], hex::encode(label));
    assert_eq!(posts[0]["labels"][0]["count"], 2);
    assert_eq!(posts[0]["labels"][1]["count"], 1);
    assert!(posts[1]["labels"].as_array().unwrap().is_empty());

    // Take Alice's off with her key for that one reaction.
    let key = ReactionWriteKey::new(&ALICE, &CHANNEL, 1, label);
    let path = format!(
        "http://{addr}/v1/channels/{}/posts/1/reactions/{}/{}",
        channel_hex(),
        hex::encode(key.public_key()),
        hex::encode(label)
    );
    // Without a proof, or with Bob's key's proof, it stays.
    assert_eq!(client.delete(&path).send().await.unwrap().status(), 401);
    let bob = ReactionWriteKey::new(&BOB, &CHANNEL, 1, label);
    let resp = client
        .delete(&path)
        .header(
            WRITE_PROOF_HEADER_NAME,
            bob.sign_delete(now_ms(), nonce()).encode(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client
        .delete(&path)
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_delete(now_ms(), nonce()).encode(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let body: Value = counts(&client, addr, "1").await.json().await.unwrap();
    let labels = body["posts"][0]["labels"].as_array().unwrap();
    assert_eq!(labels.len(), 2);
    assert!(labels.iter().all(|l| l["count"] == 1));
}

/// The gate runs in order: the proof, its nonce, then the token. A request
/// with no valid proof is refused before any token is looked at, a replayed
/// proof is refused on its nonce, and a valid proof without a token is
/// refused by the token gate.
// One scenario: each refusal is checked against the request the previous
// step let through.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn the_gate_is_proof_then_nonce_then_token() {
    let addr = spawn().await;
    let client = channel_with_posts(addr).await;
    let label = label_of(1, THUMBS);
    let key = ReactionWriteKey::new(&ALICE, &CHANNEL, 1, label);
    let url = format!(
        "http://{addr}/v1/channels/{}/posts/1/reactions",
        channel_hex()
    );
    let body = json!({
        "writer_key_hex": hex::encode(key.public_key()),
        "label_hex": hex::encode(label),
    });

    // No proof, no token: the proof refusal.
    let resp = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["code"],
        "write-proof-required"
    );
    // A proof for another label.
    let other = ReactionWriteKey::new(&ALICE, &CHANNEL, 1, label_of(1, "\u{1F525}"));
    let resp = client
        .post(&url)
        .header(
            WRITE_PROOF_HEADER_NAME,
            other.sign_add(now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, token(TokenContext::ChannelReact))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // A valid proof and no token: refused by the token gate, the nonce spent.
    let proof = key.sign_add(now_ms(), nonce()).encode();
    let resp = client
        .post(&url)
        .header(WRITE_PROOF_HEADER_NAME, &proof)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_ne!(
        resp.json::<Value>().await.unwrap()["code"],
        "write-proof-required"
    );
    // The same proof again, now with a token: its nonce was recorded.
    let resp = client
        .post(&url)
        .header(WRITE_PROOF_HEADER_NAME, &proof)
        .header(TOKEN_HEADER, token(TokenContext::ChannelReact))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["code"],
        "write-proof-required"
    );
    // A comment token does not pay for a reaction.
    let resp = client
        .post(&url)
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_add(now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, token(TokenContext::ChannelComment))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    // A post that does not exist: refused before the token is spent, so the
    // same token then pays for a real add.
    let ghost_token = token(TokenContext::ChannelReact);
    let ghost = ReactionWriteKey::new(&ALICE, &CHANNEL, 99, label_of(99, THUMBS));
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{}/posts/99/reactions",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            ghost.sign_add(now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, &ghost_token)
        .json(&json!({
            "writer_key_hex": hex::encode(ghost.public_key()),
            "label_hex": hex::encode(label_of(99, THUMBS)),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.json::<Value>().await.unwrap()["code"], "unknown-post");
    let real_label = label_of(2, THUMBS);
    let real = ReactionWriteKey::new(&ALICE, &CHANNEL, 2, real_label);
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{}/posts/2/reactions",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            real.sign_add(now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, &ghost_token)
        .json(&json!({
            "writer_key_hex": hex::encode(real.public_key()),
            "label_hex": hex::encode(real_label),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "the token was not spent on the 404");
}

#[tokio::test]
async fn the_counts_read_is_bounded() {
    let addr = spawn().await;
    let client = channel_with_posts(addr).await;
    let fifty: Vec<String> = (1..=50).map(|n: u64| n.to_string()).collect();
    assert_eq!(counts(&client, addr, &fifty.join(",")).await.status(), 200);
    let fifty_one: Vec<String> = (1..=51).map(|n: u64| n.to_string()).collect();
    assert_eq!(
        counts(&client, addr, &fifty_one.join(",")).await.status(),
        400
    );
    assert_eq!(counts(&client, addr, "1,x").await.status(), 400);
    assert_eq!(counts(&client, addr, "").await.status(), 400);
}

fn sealed_comment(
    account: &[u8; 32],
    comment_nonce: &[u8; 16],
    text: &str,
) -> (Vec<u8>, CommentWriteKey) {
    let key = CommentWriteKey::new(account, &CHANNEL, 1, comment_nonce);
    let public = key.public_key();
    let sealed = seal_comment(
        &ContentKeySeed::from_bytes_for_test([9; 32]),
        &CommentPlace {
            channel_id: &CHANNEL,
            epoch: EpochId::new(0),
            post_cursor: 1,
            writer_key: &public,
        },
        &CommentAuthor::from_account_seed(account, &CHANNEL),
        &Comment {
            name: "Ana".to_string(),
            avatar: None,
            text: text.to_string(),
            reply_to: None,
        },
    )
    .unwrap();
    (sealed, key)
}

async fn post_comment(
    client: &reqwest::Client,
    addr: SocketAddr,
    sealed: &[u8],
    key: &CommentWriteKey,
) -> reqwest::Response {
    client
        .post(format!(
            "http://{addr}/v1/channels/{}/posts/1/comments",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_add(sealed, now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, token(TokenContext::ChannelComment))
        .json(&json!({
            "writer_key_hex": hex::encode(key.public_key()),
            "sealed_b64": URL_SAFE_NO_PAD.encode(sealed),
        }))
        .send()
        .await
        .unwrap()
}

// One scenario: the page, the author delete and the publisher delete share
// the comments it writes.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn comments_are_added_paged_and_deleted_by_author_or_publisher() {
    let addr = spawn().await;
    let client = channel_with_posts(addr).await;
    let mut written = Vec::new();
    for i in 0..25 {
        let (sealed, key) = sealed_comment(&ALICE, &nonce(), &format!("comment {i}"));
        let resp = post_comment(&client, addr, &sealed, &key).await;
        assert_eq!(resp.status(), 201);
        let cursor = resp.json::<Value>().await.unwrap()["cursor"]
            .as_u64()
            .unwrap();
        written.push((cursor, sealed, key));
    }
    let page_url = |after: u64| {
        format!(
            "http://{addr}/v1/channels/{}/posts/1/comments?after={after}",
            channel_hex()
        )
    };
    let first: Value = client
        .get(page_url(0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = first["comments"].as_array().unwrap();
    assert_eq!(rows.len(), 20);
    assert_eq!(rows[0]["cursor"].as_u64().unwrap(), written[0].0);
    assert_eq!(
        rows[0]["sealed_b64"].as_str().unwrap(),
        URL_SAFE_NO_PAD.encode(&written[0].1)
    );
    assert_eq!(
        rows[0]["writer_key_hex"].as_str().unwrap(),
        hex::encode(written[0].2.public_key())
    );
    let next = first["next_after"].as_u64().unwrap();
    let second: Value = client
        .get(page_url(next))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["comments"].as_array().unwrap().len(), 5);

    let delete_url = |cursor: u64| {
        format!(
            "http://{addr}/v1/channels/{}/posts/1/comments/{cursor}",
            channel_hex()
        )
    };
    // The author deletes with that comment's key; another comment's key
    // (even the same person's) does not.
    let (cursor, _, key) = &written[0];
    let (_, _, other_key) = &written[1];
    let resp = client
        .delete(delete_url(*cursor))
        .header(
            WRITE_PROOF_HEADER_NAME,
            other_key.sign_delete(*cursor, now_ms(), nonce()).encode(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client
        .delete(delete_url(*cursor))
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_delete(*cursor, now_ms(), nonce()).encode(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let resp = client
        .delete(delete_url(*cursor))
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_delete(*cursor, now_ms(), nonce()).encode(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // A publisher deletes any comment with the owner proof.
    let (cursor, _, _) = &written[1];
    let path = format!("/v1/channels/{}/posts/1/comments/{cursor}", channel_hex());
    let resp = client
        .delete(format!("http://{addr}{path}"))
        .header(OWNER_PROOF_HEADER_NAME, owner_proof("DELETE", &path, &[]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    // A forged owner proof is refused.
    let resp = client
        .delete(delete_url(written[2].0))
        .header(
            OWNER_PROOF_HEADER_NAME,
            owner_proof("DELETE", "/elsewhere", &[]),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let left: Value = client
        .get(page_url(0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        left["comments"].as_array().unwrap()[0]["cursor"]
            .as_u64()
            .unwrap(),
        written[2].0
    );
}

#[tokio::test]
async fn a_comment_is_bounded_and_its_proof_binds_the_bytes() {
    let addr = spawn().await;
    let client = channel_with_posts(addr).await;
    let (sealed, key) = sealed_comment(&ALICE, &nonce(), "hi");
    let url = format!(
        "http://{addr}/v1/channels/{}/posts/1/comments",
        channel_hex()
    );
    // Too long.
    let long = vec![1u8; 2049];
    let resp = client
        .post(&url)
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_add(&long, now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, token(TokenContext::ChannelComment))
        .json(&json!({
            "writer_key_hex": hex::encode(key.public_key()),
            "sealed_b64": URL_SAFE_NO_PAD.encode(&long),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    // A proof over other bytes.
    let mut swapped = sealed.clone();
    swapped[20] ^= 1;
    let resp = client
        .post(&url)
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_add(&sealed, now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, token(TokenContext::ChannelComment))
        .json(&json!({
            "writer_key_hex": hex::encode(key.public_key()),
            "sealed_b64": URL_SAFE_NO_PAD.encode(&swapped),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    // A comment on a post that does not exist spends no token.
    let comment_token = token(TokenContext::ChannelComment);
    let ghost = CommentWriteKey::new(&ALICE, &CHANNEL, 99, &nonce());
    let ghost_body = json!({
        "writer_key_hex": hex::encode(ghost.public_key()),
        "sealed_b64": URL_SAFE_NO_PAD.encode(&sealed),
    });
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{}/posts/99/comments",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            ghost.sign_add(&sealed, now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, &comment_token)
        .json(&ghost_body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body = json!({
        "writer_key_hex": hex::encode(key.public_key()),
        "sealed_b64": URL_SAFE_NO_PAD.encode(&sealed),
    });
    let resp = client
        .post(&url)
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_add(&sealed, now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, &comment_token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "the token was not spent on the 404");
}

fn comment_label_of(post: u64, comment: u64, emoji: &str) -> [u8; 16] {
    CommentReactionKey::derive(
        &ContentKeySeed::from_bytes_for_test([9; 32]),
        &CHANNEL,
        EpochId::new(0),
    )
    .unwrap()
    .label(post, comment, emoji)
    .unwrap()
}

async fn react_on_comment(
    client: &reqwest::Client,
    addr: SocketAddr,
    account: &[u8; 32],
    comment: u64,
    label: [u8; 16],
) -> reqwest::Response {
    let key = CommentReactionWriteKey::new(account, &CHANNEL, 1, comment, label);
    client
        .post(format!(
            "http://{addr}/v1/channels/{}/posts/1/comments/{comment}/reactions",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_add(now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, token(TokenContext::ChannelReact))
        .json(&json!({
            "writer_key_hex": hex::encode(key.public_key()),
            "label_hex": hex::encode(label),
        }))
        .send()
        .await
        .unwrap()
}

/// 2026-11-05: reactions on a comment, counted per comment; a post's proof
/// does not add one, a missing comment costs no token, a deleted comment
/// takes its reactions with it.
// One scenario over the comments it writes.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reactions_on_comments_are_added_counted_and_taken_off() {
    let addr = spawn().await;
    let client = channel_with_posts(addr).await;
    let (sealed, key) = sealed_comment(&ALICE, &nonce(), "first");
    let comment = post_comment(&client, addr, &sealed, &key)
        .await
        .json::<Value>()
        .await
        .unwrap()["cursor"]
        .as_u64()
        .unwrap();
    let label = comment_label_of(1, comment, THUMBS);
    assert_eq!(
        react_on_comment(&client, addr, &ALICE, comment, label)
            .await
            .status(),
        201
    );
    assert_eq!(
        react_on_comment(&client, addr, &ALICE, comment, label)
            .await
            .status(),
        200
    );
    assert_eq!(
        react_on_comment(&client, addr, &BOB, comment, label)
            .await
            .status(),
        201
    );

    // A post-reaction proof does not add a reaction on the comment.
    let post_key = ReactionWriteKey::new(&CAROL, &CHANNEL, 1, label);
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{}/posts/1/comments/{comment}/reactions",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            post_key.sign_add(now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, token(TokenContext::ChannelReact))
        .json(&json!({
            "writer_key_hex": hex::encode(post_key.public_key()),
            "label_hex": hex::encode(label),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // A comment that is not there: 404 before the token is spent.
    let ghost_token = token(TokenContext::ChannelReact);
    let ghost_label = comment_label_of(1, 999, THUMBS);
    let ghost = CommentReactionWriteKey::new(&ALICE, &CHANNEL, 1, 999, ghost_label);
    let resp = client
        .post(format!(
            "http://{addr}/v1/channels/{}/posts/1/comments/999/reactions",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            ghost.sign_add(now_ms(), nonce()).encode(),
        )
        .header(TOKEN_HEADER, &ghost_token)
        .json(&json!({
            "writer_key_hex": hex::encode(ghost.public_key()),
            "label_hex": hex::encode(ghost_label),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["code"],
        "unknown-comment"
    );

    let counts_url = |list: &str| {
        format!(
            "http://{addr}/v1/channels/{}/posts/1/comment-reactions?comments={list}",
            channel_hex()
        )
    };
    let resp = client
        .get(counts_url(&format!("{comment},999")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["cache-control"].to_str().unwrap(),
        "public, max-age=15"
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["comments"][0]["cursor"], comment);
    assert_eq!(
        body["comments"][0]["labels"][0]["label_hex"],
        hex::encode(label)
    );
    assert_eq!(body["comments"][0]["labels"][0]["count"], 2);
    assert!(body["comments"][1]["labels"].as_array().unwrap().is_empty());
    let fifty_one: Vec<String> = (1..=51).map(|n: u64| n.to_string()).collect();
    assert_eq!(
        client
            .get(counts_url(&fifty_one.join(",")))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

    // Bob takes his off with his key.
    let bob = CommentReactionWriteKey::new(&BOB, &CHANNEL, 1, comment, label);
    let resp = client
        .delete(format!(
            "http://{addr}/v1/channels/{}/posts/1/comments/{comment}/reactions/{}/{}",
            channel_hex(),
            hex::encode(bob.public_key()),
            hex::encode(label)
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            bob.sign_delete(now_ms(), nonce()).encode(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let body: Value = client
        .get(counts_url(&comment.to_string()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["comments"][0]["labels"][0]["count"], 1);

    // Deleting the comment takes its reactions.
    let resp = client
        .delete(format!(
            "http://{addr}/v1/channels/{}/posts/1/comments/{comment}",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_delete(comment, now_ms(), nonce()).encode(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let body: Value = client
        .get(counts_url(&comment.to_string()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["comments"][0]["labels"].as_array().unwrap().is_empty());
}

/// 2026-11-05: the batched counts read carries each post's comment count.
#[tokio::test]
async fn the_counts_read_carries_the_comment_count() {
    let addr = spawn().await;
    let client = channel_with_posts(addr).await;
    let mut cursors = Vec::new();
    for i in 0..3 {
        let (sealed, key) = sealed_comment(&ALICE, &nonce(), &format!("c{i}"));
        let cursor = post_comment(&client, addr, &sealed, &key)
            .await
            .json::<Value>()
            .await
            .unwrap()["cursor"]
            .as_u64()
            .unwrap();
        cursors.push((cursor, key));
    }
    let body: Value = counts(&client, addr, "1,2").await.json().await.unwrap();
    assert_eq!(body["posts"][0]["comments"], 3);
    assert_eq!(body["posts"][1]["comments"], 0);
    let (cursor, key) = &cursors[0];
    let resp = client
        .delete(format!(
            "http://{addr}/v1/channels/{}/posts/1/comments/{cursor}",
            channel_hex()
        ))
        .header(
            WRITE_PROOF_HEADER_NAME,
            key.sign_delete(*cursor, now_ms(), nonce()).encode(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let body: Value = counts(&client, addr, "1").await.json().await.unwrap();
    assert_eq!(body["posts"][0]["comments"], 2);
}

/// 2026-11-05: the owner deletes the channel; afterwards its reads answer
/// `410 channel-deleted`, its id cannot be registered again, a repeat
/// delete is `204`, and nobody but the owner deletes.
#[tokio::test]
async fn the_owner_deletes_the_channel_and_reads_answer_gone() {
    let addr = spawn().await;
    let client = channel_with_posts(addr).await;
    let path = format!("/v1/channels/{}", channel_hex());
    // No proof: refused.
    assert_eq!(
        client
            .delete(format!("http://{addr}{path}"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    // A stranger's owner-shaped proof: refused.
    let stranger = ChannelOwnerSigner::from_account_seed(&[0x77; 32], &CHANNEL);
    let proof = stranger
        .sign_at(
            &OwnerRequest {
                method: "DELETE",
                canonical_path: &path,
                query: "",
                body: &[],
            },
            now_ms(),
            nonce(),
        )
        .encode();
    let resp = client
        .delete(format!("http://{addr}{path}"))
        .header(OWNER_PROOF_HEADER_NAME, proof)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = client
        .delete(format!("http://{addr}{path}"))
        .header(OWNER_PROOF_HEADER_NAME, owner_proof("DELETE", &path, &[]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    for url in [
        format!("http://{addr}{path}/posts"),
        format!("http://{addr}{path}/epoch"),
        format!(
            "http://{addr}{path}/subscribers/{}/envelope",
            hex::encode([0x21u8; 32])
        ),
    ] {
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status(), 410, "{url}");
        assert_eq!(
            resp.json::<Value>().await.unwrap()["code"],
            "channel-deleted"
        );
    }
    let resp = client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({
            "channel_id": channel_hex(),
            "owner_pubkey_hex": hex::encode(owner().verifying_key().to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 410);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["code"],
        "channel-deleted"
    );
    // A repeat is 204, with no proof needed.
    assert_eq!(
        client
            .delete(format!("http://{addr}{path}"))
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    // An unknown channel stays 404.
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{}/posts",
            hex::encode([0x0Du8; 16])
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
