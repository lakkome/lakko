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
use lakko_channels::seed::ContentKeySeed;
use lakko_channels::{open_member_card, seal_member_card, EpochId};
use lakko_crypto::signing::SigningKey;
use lakko_svc_channels::api::{member_card_signed_bytes, MEMBER_CARD_SIG_HEADER};
use lakko_svc_channels::{router, AppState, ChannelId, Config, InMemoryChannelStore, SubscriberId};
use serde_json::json;
use tokio::net::TcpListener;

const CHANNEL_HEX: &str = "00112233445566778899aabbccddeeff";

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
    };
    let app = router(AppState::new(cfg, Arc::new(InMemoryChannelStore::new())));
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
            profile,
        )
        .unwrap()
    }
}

async fn setup(client: &reqwest::Client, addr: SocketAddr, members: &[&Member]) {
    let resp = client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({ "channel_id": CHANNEL_HEX }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    for m in members {
        let resp = client
            .post(format!(
                "http://{addr}/v1/channels/{CHANNEL_HEX}/subscribers"
            ))
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
    let profile = open_member_card(
        &ContentKeySeed::from_bytes_for_test([1; 32]),
        &served,
        &channel,
        &alice.pk(),
    )
    .unwrap();
    assert_eq!(profile, b"profile v3 of alice");
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
        let resp = client
            .post(format!("http://{addr}/v1/channels/{CHANNEL_HEX}/rotate"))
            .json(&json!({ "envelopes": [] }))
            .send()
            .await
            .unwrap();
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
