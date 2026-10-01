// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0060 section 3 - channel member cards over the wire, against the
//! in-memory store (the service's usual test pattern). The cards are real
//! `lakko_channels::seal_member_card` output, so the server's frame reader is
//! exercised on what a client actually uploads.

use std::net::SocketAddr;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_token::{MasterSecret, TokenContext, DEFAULT_WINDOW_SECS};
use lakko_anon_token_server::{
    current_epoch, testing::mint_header, AnonTokenServerConfig, TokenVerifier, TOKEN_HEADER,
};
use lakko_channels::owner::{ChannelOwnerSigner, OwnerRequest, OWNER_PROOF_HEADER_NAME};
use lakko_channels::seed::ContentKeySeed;
use lakko_channels::{open_member_card, seal_member_card, EpochId};
use lakko_crypto::signing::SigningKey;
use lakko_svc_channels::api::{member_card_signed_bytes, MEMBER_CARD_SIG_HEADER};
use lakko_svc_channels::{router, AppState, ChannelId, Config, InMemoryChannelStore, SubscriberId};
use serde_json::json;
use tokio::net::TcpListener;

const CHANNEL_HEX: &str = "00112233445566778899aabbccddeeff";

/// The anonymous-token master secret the test server verifies under
/// (ADR-0054: subscribing spends a `channel-subscribe` token).
const MASTER_HEX: &str = "0303030303030303030303030303030303030303030303030303030303030303";

fn subscribe_token() -> String {
    mint_header(
        &MasterSecret::from_bytes([3; 32]),
        TokenContext::ChannelSubscribe,
        current_epoch(DEFAULT_WINDOW_SECS),
    )
}

/// The channel owner (ADR-0059): its key is registered at creation and signs
/// the publisher routes (rotate, the subscriber list).
fn owner() -> ChannelOwnerSigner {
    let id: [u8; 16] = hex::decode(CHANNEL_HEX).unwrap().try_into().unwrap();
    ChannelOwnerSigner::from_account_seed(&[5; 32], &id)
}

/// Send `req` with an owner proof over its exact method, path, query and
/// body bytes.
async fn owner_send(req: reqwest::RequestBuilder) -> reqwest::Response {
    let (client, req) = req.build_split();
    let mut req = req.unwrap();
    let body = req
        .body()
        .and_then(reqwest::Body::as_bytes)
        .unwrap_or(&[])
        .to_vec();
    let mut nonce = [0u8; 16];
    lakko_crypto::random::fill(&mut nonce);
    let proof = owner()
        .sign_at(
            &OwnerRequest {
                method: req.method().as_str(),
                canonical_path: req.url().path(),
                query: req.url().query().unwrap_or(""),
                body: &body,
            },
            now_ms(),
            nonce,
        )
        .encode();
    req.headers_mut()
        .insert(OWNER_PROOF_HEADER_NAME, proof.parse().unwrap());
    client.execute(req).await.unwrap()
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
    let app = router(AppState::new(
        cfg,
        Arc::new(InMemoryChannelStore::new()),
        verifier,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

struct Member {
    key: SigningKey,
}

impl Member {
    fn new(seed: u8) -> Self {
        Self {
            key: SigningKey::from_seed(&[seed; 32]),
        }
    }
    fn pk(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }
    fn hex(&self) -> String {
        hex::encode(self.pk())
    }
    fn sign(&self, card: &[u8]) -> String {
        let channel = ChannelId::from_hex(CHANNEL_HEX).unwrap();
        let sub = SubscriberId::from_bytes(&self.pk()).unwrap();
        let signed = URL_SAFE_NO_PAD.encode(
            self.key
                .sign(&member_card_signed_bytes(&channel, &sub, card))
                .to_bytes(),
        );
        // The client helper signs the same bytes (Ed25519 is deterministic).
        assert_eq!(
            signed,
            URL_SAFE_NO_PAD.encode(lakko_channels::sign_member_card_request(
                &self.key,
                channel.as_bytes(),
                card
            ))
        );
        signed
    }
    fn card(&self, epoch: u32, profile: &[u8]) -> Vec<u8> {
        let channel: [u8; 16] = hex::decode(CHANNEL_HEX).unwrap().try_into().unwrap();
        seal_member_card(
            &ContentKeySeed::from_bytes_for_test([u8::try_from(epoch).unwrap() + 1; 32]),
            EpochId::new(epoch),
            &channel,
            &self.pk(),
            &self.account_seed(),
            profile,
        )
        .unwrap()
    }
    /// The member's account identity seed (distinct from its subscriber
    /// key: the card binds the one to the other).
    fn account_seed(&self) -> [u8; 32] {
        let mut seed = self.key.to_seed();
        seed[0] ^= 0xA5;
        seed
    }
    fn unsubscribe_header(&self, issued_at_ms: u64) -> String {
        let channel: [u8; 16] = hex::decode(CHANNEL_HEX).unwrap().try_into().unwrap();
        format!(
            "{issued_at_ms}.{}",
            URL_SAFE_NO_PAD.encode(lakko_channels::sign_unsubscribe_request(
                &self.key,
                &channel,
                issued_at_ms
            ))
        )
    }
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

async fn setup(client: &reqwest::Client, addr: SocketAddr, members: &[&Member]) {
    let resp = client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({
            "channel_id": CHANNEL_HEX,
            "owner_pubkey_hex": hex::encode(owner().verifying_key().to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    for m in members {
        let resp = client
            .post(format!(
                "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
            ))
            .header(TOKEN_HEADER, subscribe_token())
            .json(&json!({ "subscriber_pubkey_hex": m.hex() }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }
}

async fn put(
    client: &reqwest::Client,
    addr: SocketAddr,
    path_hex: &str,
    card: Vec<u8>,
    sig: Option<String>,
) -> (u16, serde_json::Value) {
    let mut req = client.put(format!(
        "http://{addr}/v1/channels/{CHANNEL_HEX}/members/{path_hex}"
    ));
    if let Some(sig) = sig {
        req = req.header(MEMBER_CARD_SIG_HEADER, sig);
    }
    let resp = req.body(card).send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(serde_json::Value::Null))
}

async fn list(client: &reqwest::Client, addr: SocketAddr, query: &str) -> (u16, serde_json::Value) {
    let resp = client
        .get(format!(
            "http://{addr}/v1/channels/{CHANNEL_HEX}/members{query}"
        ))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
async fn a_signed_card_round_trips_and_opens_for_a_seed_holder() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    let alice = Member::new(1);
    setup(&client, addr, &[&alice]).await;

    let card = alice.card(0, b"profile v3 of alice");
    let (status, _) = put(
        &client,
        addr,
        &alice.hex(),
        card.clone(),
        Some(alice.sign(&card)),
    )
    .await;
    assert_eq!(status, 204);

    let (status, body) = list(&client, addr, "").await;
    assert_eq!(status, 200);
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(
        members[0]["subscriber_pk_b64"].as_str().unwrap(),
        URL_SAFE_NO_PAD.encode(alice.pk())
    );
    let served = URL_SAFE_NO_PAD
        .decode(members[0]["card_b64"].as_str().unwrap())
        .unwrap();
    assert_eq!(served, card);
    let channel: [u8; 16] = hex::decode(CHANNEL_HEX).unwrap().try_into().unwrap();
    let opened = open_member_card(
        &ContentKeySeed::from_bytes_for_test([1; 32]),
        &served,
        &channel,
        &alice.pk(),
    )
    .unwrap();
    assert_eq!(opened.profile, b"profile v3 of alice");
    assert_eq!(
        opened.identity_pk,
        SigningKey::from_seed(&alice.account_seed())
            .verifying_key()
            .to_bytes()
    );
    assert!(body["next_after"].is_null());
}

#[tokio::test]
async fn only_the_named_subscriber_can_write_or_delete_its_card() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    let alice = Member::new(1);
    let mallory = Member::new(2);
    setup(&client, addr, &[&alice, &mallory]).await;
    let card = alice.card(0, b"alice");

    // No signature, and a signature by another subscriber.
    let (status, body) = put(&client, addr, &alice.hex(), card.clone(), None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (401, Some("bad-member-signature"))
    );
    let (status, _) = put(
        &client,
        addr,
        &alice.hex(),
        card.clone(),
        Some(mallory.sign(&card)),
    )
    .await;
    assert_eq!(status, 401);
    // A signature over another card.
    let other = alice.card(0, b"alice, edited");
    let (status, _) = put(
        &client,
        addr,
        &alice.hex(),
        card.clone(),
        Some(alice.sign(&other)),
    )
    .await;
    assert_eq!(status, 401);

    let (status, _) = put(
        &client,
        addr,
        &alice.hex(),
        card.clone(),
        Some(alice.sign(&card)),
    )
    .await;
    assert_eq!(status, 204);

    let delete = |sig: Option<String>| {
        let client = client.clone();
        let path = alice.hex();
        async move {
            let mut req = client.delete(format!(
                "http://{addr}/v1/channels/{CHANNEL_HEX}/members/{path}"
            ));
            if let Some(sig) = sig {
                req = req.header(MEMBER_CARD_SIG_HEADER, sig);
            }
            req.send().await.unwrap().status().as_u16()
        }
    };
    // An upload signature does not authorise a delete.
    assert_eq!(delete(Some(alice.sign(&card))).await, 401);
    assert_eq!(delete(Some(mallory.sign(&[]))).await, 401);
    assert_eq!(delete(Some(alice.sign(&[]))).await, 204);
    assert_eq!(delete(Some(alice.sign(&[]))).await, 204);
    let (_, body) = list(&client, addr, "").await;
    assert!(body["members"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_card_is_framed_registered_and_epoch_bounded() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    let alice = Member::new(1);
    let stranger = Member::new(3);
    setup(&client, addr, &[&alice]).await;

    // Not a card: too short, too long, an unknown version.
    for bad in [
        vec![1u8; 10],
        vec![1u8; lakko_channels::MEMBER_CARD_MAX_LEN + 1],
        {
            let mut c = alice.card(0, b"x");
            c[0] = 9;
            c
        },
    ] {
        let (status, body) = put(
            &client,
            addr,
            &alice.hex(),
            bad.clone(),
            Some(alice.sign(&bad)),
        )
        .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (400, Some("bad-member-card"))
        );
    }
    // Not registered.
    let card = stranger.card(0, b"s");
    let (status, _) = put(
        &client,
        addr,
        &stranger.hex(),
        card.clone(),
        Some(stranger.sign(&card)),
    )
    .await;
    assert_eq!(status, 404);
    // A future epoch.
    let future = alice.card(1, b"future");
    let (status, body) = put(
        &client,
        addr,
        &alice.hex(),
        future.clone(),
        Some(alice.sign(&future)),
    )
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("member-card-epoch-in-future"))
    );
    // Rotate twice, upload at 2, then an older card is refused.
    for _ in 0..2 {
        let resp = owner_send(
            client
                .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/rotate"))
                .json(&json!({ "envelopes": [] })),
        )
        .await;
        assert_eq!(resp.status(), 200);
    }
    let current = alice.card(2, b"now");
    let (status, _) = put(
        &client,
        addr,
        &alice.hex(),
        current.clone(),
        Some(alice.sign(&current)),
    )
    .await;
    assert_eq!(status, 204);
    let old = alice.card(1, b"then");
    let (status, body) = put(
        &client,
        addr,
        &alice.hex(),
        old.clone(),
        Some(alice.sign(&old)),
    )
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("member-card-stale"))
    );
}

#[tokio::test]
async fn the_member_list_pages_by_subscriber_key() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    let members: Vec<Member> = (10u8..15).map(Member::new).collect();
    let refs: Vec<&Member> = members.iter().collect();
    setup(&client, addr, &refs).await;
    for m in &members {
        let card = m.card(0, b"p");
        let (status, _) = put(&client, addr, &m.hex(), card.clone(), Some(m.sign(&card))).await;
        assert_eq!(status, 204);
    }
    let mut expected: Vec<[u8; 32]> = members.iter().map(Member::pk).collect();
    expected.sort_unstable();

    let mut seen = Vec::new();
    let mut query = "?limit=2".to_string();
    loop {
        let (status, body) = list(&client, addr, &query).await;
        assert_eq!(status, 200);
        for m in body["members"].as_array().unwrap() {
            seen.push(m["subscriber_pk_b64"].as_str().unwrap().to_owned());
        }
        match body["next_after"].as_str() {
            Some(after) => query = format!("?limit=2&after={after}"),
            None => break,
        }
    }
    let expected: Vec<String> = expected.iter().map(|k| URL_SAFE_NO_PAD.encode(k)).collect();
    assert_eq!(seen, expected);

    let (status, body) = list(&client, addr, "?after=not-a-key").await;
    assert_eq!((status, body["code"].as_str()), (400, Some("bad-after")));
    let status = client
        .get(format!(
            "http://{addr}/v1/channels/ffffffffffffffffffffffffffffffff/members"
        ))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_signed_unsubscribe_removes_the_subscriber_and_its_card() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    let alice = Member::new(1);
    let bob = Member::new(2);
    setup(&client, addr, &[&alice, &bob]).await;
    for m in [&alice, &bob] {
        let card = m.card(0, b"me");
        let (status, _) = put(&client, addr, &m.hex(), card.clone(), Some(m.sign(&card))).await;
        assert_eq!(status, 204);
    }
    let unsubscribe = |path: String, header: Option<String>| {
        let client = client.clone();
        async move {
            let mut req = client.delete(format!(
                "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers/{path}"
            ));
            if let Some(h) = header {
                req = req.header(lakko_channels::UNSUBSCRIBE_SIG_HEADER, h);
            }
            let resp = req.send().await.unwrap();
            let status = resp.status().as_u16();
            let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
            (status, body["code"].as_str().map(str::to_owned))
        }
    };
    let now = now_ms();
    // Unsigned, signed by another subscriber, malformed, stale: refused, and
    // nothing is removed.
    assert_eq!(
        unsubscribe(alice.hex(), None).await,
        (401, Some("bad-unsubscribe-signature".into()))
    );
    assert_eq!(
        unsubscribe(alice.hex(), Some(bob.unsubscribe_header(now)))
            .await
            .0,
        401
    );
    assert_eq!(
        unsubscribe(alice.hex(), Some("+1.AAAA".into())).await.0,
        401
    );
    assert_eq!(
        unsubscribe(
            alice.hex(),
            Some(alice.unsubscribe_header(now - lakko_channels::UNSUBSCRIBE_WINDOW_MS - 60_000))
        )
        .await,
        (401, Some("stale-unsubscribe".into()))
    );
    let (_, body) = list(&client, addr, "").await;
    assert_eq!(body["members"].as_array().unwrap().len(), 2);

    // Signed and fresh: 204, idempotent; the card and the registration go.
    assert_eq!(
        unsubscribe(alice.hex(), Some(alice.unsubscribe_header(now)))
            .await
            .0,
        204
    );
    assert_eq!(
        unsubscribe(alice.hex(), Some(alice.unsubscribe_header(now)))
            .await
            .0,
        204
    );
    let (_, body) = list(&client, addr, "").await;
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(
        members[0]["subscriber_pk_b64"].as_str().unwrap(),
        URL_SAFE_NO_PAD.encode(bob.pk())
    );
    let resp = owner_send(client.get(format!(
        "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
    )))
    .await;
    let subs = resp.text().await.unwrap();
    assert!(!subs.contains(&alice.hex()), "{subs}");
    assert!(subs.contains(&bob.hex()), "{subs}");
    // A card upload for the departed subscriber is refused: it is no longer
    // registered.
    let card = alice.card(0, b"me again");
    let (status, _) = put(
        &client,
        addr,
        &alice.hex(),
        card.clone(),
        Some(alice.sign(&card)),
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn an_unsubscribe_from_an_unknown_channel_is_404_after_the_signature() {
    let addr = spawn().await;
    let client = reqwest::Client::new();
    let alice = Member::new(1);
    setup(&client, addr, &[&alice]).await;
    let now = now_ms();
    let other = "ffeeddccbbaa99887766554433221100";
    let resp = client
        .delete(format!(
            "http://{addr}/v1/channels/{other}/subscribers/{}",
            alice.hex()
        ))
        .header(lakko_channels::UNSUBSCRIBE_SIG_HEADER, {
            let channel: [u8; 16] = hex::decode(other).unwrap().try_into().unwrap();
            format!(
                "{now}.{}",
                URL_SAFE_NO_PAD.encode(lakko_channels::sign_unsubscribe_request(
                    &alice.key, &channel, now
                ))
            )
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}
