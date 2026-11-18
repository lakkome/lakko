// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel poll votes (ADR-0090 section 4.3), driven over HTTP against the
//! in-memory store with a real credential: registered, issued and presented
//! with `lakko-anon-cred` the way a client does, under the master the
//! service holds.

use std::net::SocketAddr;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_cred::{
    period_at, Credential, CredentialSecret, CredentialVerifier, IssuerSecretKey, MasterSecret,
    PollScope,
};
use lakko_anon_token_server::{AnonTokenServerConfig, TokenVerifier};
use lakko_channels::epoch::EpochId;
use lakko_channels::owner::{ChannelOwnerSigner, OwnerRequest, OWNER_PROOF_HEADER_NAME};
use lakko_channels::{poll_option_labels, vote_message, ContentKeySeed, ReactionKey, VoteAction};
use lakko_svc_channels::{router, AppState, Config, InMemoryChannelStore};
use serde_json::{json, Value};
use tokio::net::TcpListener;

const TOKEN_MASTER_HEX: &str = "0303030303030303030303030303030303030303030303030303030303030303";
const CRED_MASTER: [u8; 32] = [0x77; 32];
const OWNER_SEED: [u8; 32] = [5; 32];
const CHANNEL: [u8; 16] = [0x0D; 16];

fn channel_hex() -> String {
    hex::encode(CHANNEL)
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

fn nonce() -> [u8; 16] {
    let mut n = [0u8; 16];
    lakko_crypto::random::fill(&mut n);
    n
}

fn owner() -> ChannelOwnerSigner {
    ChannelOwnerSigner::from_account_seed(&OWNER_SEED, &CHANNEL)
}

async fn spawn(with_votes: bool) -> SocketAddr {
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
    let tokens = TokenVerifier::in_memory(
        AnonTokenServerConfig::from_values(Some(TOKEN_MASTER_HEX), None).unwrap(),
    );
    let votes = with_votes.then(|| CredentialVerifier::new(MasterSecret::from_bytes(CRED_MASTER)));
    let state = AppState::new_with_votes(cfg, Arc::new(InMemoryChannelStore::new()), tokens, votes);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

async fn channel_with_post(addr: SocketAddr) -> reqwest::Client {
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
    let path = format!("/v1/channels/{}/posts", channel_hex());
    let body = vec![1u8; 8];
    let proof = owner()
        .sign_at(
            &OwnerRequest {
                method: "POST",
                canonical_path: &path,
                query: "",
                body: &body,
            },
            now_ms(),
            nonce(),
        )
        .encode();
    let resp = client
        .post(format!("http://{addr}{path}"))
        .header(OWNER_PROOF_HEADER_NAME, proof)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    client
}

/// A voter: its secret and the credential of the current period, issued
/// in-process by the issuer half of the same master.
struct Voter {
    secret: CredentialSecret,
    credential: Credential,
}

fn voter() -> Voter {
    let mut seed = [0u8; 32];
    lakko_crypto::random::fill(&mut seed);
    let secret = CredentialSecret::from_lineage_seed(&seed).unwrap();
    let period = period_at(now_ms() / 1000);
    let key = IssuerSecretKey::derive(&MasterSecret::from_bytes(CRED_MASTER), period).unwrap();
    let request = secret.issuance_request(&key.public_key()).unwrap();
    let response = key.issue(&request, &secret.commitment()).unwrap();
    let credential = secret
        .finalize_issuance(&key.public_key(), &response)
        .unwrap();
    Voter { secret, credential }
}

fn labels(post: u64) -> Vec<[u8; 16]> {
    let key = ReactionKey::derive(
        &ContentKeySeed::from_bytes_for_test([9; 32]),
        &CHANNEL,
        EpochId::new(0),
    )
    .unwrap();
    poll_option_labels(&key, post, 3).unwrap()
}

/// `(tag_hex, body)` of a vote: sorted labels, bound into the presentation.
fn vote_body(
    v: &Voter,
    post: u64,
    action: VoteAction,
    mut chosen: Vec<[u8; 16]>,
    at_ms: u64,
) -> (String, Value) {
    chosen.sort_unstable();
    let period = v.credential.period();
    let key = IssuerSecretKey::derive(&MasterSecret::from_bytes(CRED_MASTER), period)
        .unwrap()
        .public_key();
    let scope = PollScope::channel_post(CHANNEL, post);
    let message = vote_message(action, at_ms, &chosen).unwrap();
    let presentation = v
        .secret
        .present(&v.credential, &key, &scope, &message)
        .unwrap();
    let tag_hex = hex::encode(presentation.tag());
    let presentation_b64 = URL_SAFE_NO_PAD.encode(presentation.to_bytes());
    let body = match action {
        VoteAction::Put => json!({
            "presentation_b64": presentation_b64,
            "labels_hex": chosen.iter().map(hex::encode).collect::<Vec<_>>(),
            "at_ms": at_ms,
        }),
        VoteAction::Delete => json!({ "presentation_b64": presentation_b64, "at_ms": at_ms }),
    };
    (tag_hex, body)
}

fn vote_url(addr: SocketAddr, post: u64, tag_hex: &str) -> String {
    format!(
        "http://{addr}/v1/channels/{}/posts/{post}/votes/{tag_hex}",
        channel_hex()
    )
}

async fn put(
    client: &reqwest::Client,
    addr: SocketAddr,
    post: u64,
    tag: &str,
    body: &Value,
) -> reqwest::Response {
    client
        .put(vote_url(addr, post, tag))
        .json(body)
        .send()
        .await
        .unwrap()
}

async fn votes_of(client: &reqwest::Client, addr: SocketAddr) -> Value {
    let body: Value = client
        .get(format!(
            "http://{addr}/v1/channels/{}/reactions?posts=1",
            channel_hex()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["posts"][0]["votes"].clone()
}

#[tokio::test]
async fn one_account_one_vote_replace_retract_and_no_replay() {
    let addr = spawn(true).await;
    let client = channel_with_post(addr).await;
    let options = labels(1);
    let alice = voter();
    let bob = voter();
    let t0 = now_ms();

    let (alice_tag, first) = vote_body(&alice, 1, VoteAction::Put, vec![options[0]], t0);
    let resp = put(&client, addr, 1, &alice_tag, &first).await;
    assert_eq!(resp.status(), 201);
    assert_eq!(resp.json::<Value>().await.unwrap()["replaced"], false);

    let (bob_tag, bob_vote) = vote_body(&bob, 1, VoteAction::Put, vec![options[0], options[2]], t0);
    assert_ne!(alice_tag, bob_tag);
    assert_eq!(
        put(&client, addr, 1, &bob_tag, &bob_vote).await.status(),
        201
    );

    let v = votes_of(&client, addr).await;
    assert_eq!(v["voters"], 2);
    assert_eq!(v["labels"][0]["label_hex"], hex::encode(options[0]));
    assert_eq!(v["labels"][0]["count"], 2);

    // Alice votes again from a fresh presentation: same tag, a replace.
    let (tag_again, change) = vote_body(&alice, 1, VoteAction::Put, vec![options[1]], t0 + 1);
    assert_eq!(tag_again, alice_tag, "one account, one tag per poll");
    let resp = put(&client, addr, 1, &alice_tag, &change).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["replaced"], true);
    assert_eq!(votes_of(&client, addr).await["voters"], 2);

    // Replaying her first request cannot restore the old vote.
    let resp = put(&client, addr, 1, &alice_tag, &first).await;
    assert_eq!(resp.status(), 409);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "stale-vote");
    assert_eq!(body["stored_at_ms"], t0 + 1);

    // A presentation bound to other labels than the body is refused.
    let mut forged = change.clone();
    forged["labels_hex"] = json!([hex::encode(options[2])]);
    forged["at_ms"] = json!(t0 + 2);
    let resp = put(&client, addr, 1, &alice_tag, &forged).await;
    assert_eq!(resp.status(), 401);
    // A put presentation cannot be used as a retract.
    let resp = client
        .delete(vote_url(addr, 1, &alice_tag))
        .json(&json!({ "presentation_b64": change["presentation_b64"], "at_ms": t0 + 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    // Bob's presentation under Alice's tag is refused.
    let resp = put(&client, addr, 1, &alice_tag, &bob_vote).await;
    assert_eq!(resp.status(), 401);

    // Alice retracts.
    let (_, retract) = vote_body(&alice, 1, VoteAction::Delete, vec![], t0 + 3);
    let resp = client
        .delete(vote_url(addr, 1, &alice_tag))
        .json(&retract)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let v = votes_of(&client, addr).await;
    assert_eq!(v["voters"], 1);
    // Replaying her replace does not bring the vote back.
    assert_eq!(
        put(&client, addr, 1, &alice_tag, &change).await.status(),
        409
    );
}

#[tokio::test]
async fn the_gate_refuses_before_storing() {
    let addr = spawn(true).await;
    let client = channel_with_post(addr).await;
    let options = labels(1);
    let v = voter();
    let now = now_ms();

    // Out of the time window.
    let (tag, old) = vote_body(&v, 1, VoteAction::Put, vec![options[0]], now - 10 * 60_000);
    let resp = put(&client, addr, 1, &tag, &old).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["code"],
        "vote-time-out-of-window"
    );

    // Unsorted labels.
    let (tag, mut body) = vote_body(&v, 1, VoteAction::Put, vec![options[0], options[1]], now);
    let list = body["labels_hex"].as_array().unwrap().clone();
    body["labels_hex"] = json!([list[1], list[0]]);
    let resp = put(&client, addr, 1, &tag, &body).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(resp.json::<Value>().await.unwrap()["code"], "bad-labels");

    // A post that is not there.
    let (tag, body) = vote_body(&v, 9, VoteAction::Put, vec![options[0]], now);
    assert_eq!(put(&client, addr, 9, &tag, &body).await.status(), 404);

    // A presentation for post 1 sent to post 1 under a bad tag.
    let (_, body) = vote_body(&v, 1, VoteAction::Put, vec![options[0]], now);
    let resp = put(&client, addr, 1, "zz", &body).await;
    assert_eq!(resp.status(), 400);

    assert_eq!(votes_of(&client, addr).await["voters"], 0);
}

#[tokio::test]
async fn votes_answer_503_without_the_master() {
    let addr = spawn(false).await;
    let client = channel_with_post(addr).await;
    let v = voter();
    let (tag, body) = vote_body(&v, 1, VoteAction::Put, vec![labels(1)[0]], now_ms());
    let resp = put(&client, addr, 1, &tag, &body).await;
    assert_eq!(resp.status(), 503);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["code"],
        "anon-credential-disabled"
    );
}
