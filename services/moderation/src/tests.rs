// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests over the in-memory store and an in-memory spent-set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use lakko_anon_token::TokenContext;
use lakko_anon_token_server::testing::mint_header;
use lakko_anon_token_server::{AnonTokenServerConfig, TokenVerifier};
use lakko_moderation::moderators::{ModeratorEntry, ModeratorKey, ModeratorList};
use lakko_moderation::report::{seal_report, ConversationKind, EvidenceItem, ReportContent};
use lakko_moderation::seal::{moderator_report_secret, Recipient};
use lakko_moderation::target::Target;
use lakko_moderation::Reason;
use tower::ServiceExt as _;

use crate::ip_limit::IpLimiter;
use crate::store::{MemoryStore, ReportStatus};
use crate::{build_app, list, AppState};

const MASTER_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";
const ROOT_SEED: [u8; 32] = [9; 32];

fn config() -> AnonTokenServerConfig {
    AnonTokenServerConfig::from_values(Some(MASTER_HEX), None)
        .unwrap()
        .unwrap()
}

fn signed_list(version: u64) -> Vec<u8> {
    let secret = moderator_report_secret(&[1; 32]).unwrap();
    ModeratorList {
        list_version: version,
        issued_at_ms: 1,
        rp_id: "moderation.example".into(),
        origin: "https://moderation.example".into(),
        entries: vec![ModeratorEntry {
            moderator_id: [1; 16],
            name: "m".into(),
            key: ModeratorKey::Ed25519([3; 32]),
            credential_id: vec![1],
            x25519_public: secret.public_key().to_bytes(),
            added_at_ms: 0,
            revoked_at_ms: 0,
        }],
    }
    .sign(&ROOT_SEED)
    .unwrap()
}

fn root_public() -> [u8; 32] {
    lakko_crypto::signing::SigningKey::from_seed(&ROOT_SEED)
        .verifying_key()
        .to_bytes()
}

struct Harness {
    app: axum::Router,
    store: Arc<MemoryStore>,
    list: Vec<u8>,
}

async fn harness(with_list: bool) -> Harness {
    let bytes = signed_list(2);
    let store = Arc::new(MemoryStore::new());
    let state = AppState {
        store: store.clone(),
        list: with_list.then(|| Arc::new(list::from_bytes(bytes.clone(), &root_public()).unwrap())),
        tokens: TokenVerifier::in_memory(Some(config())),
        limits: Arc::new(IpLimiter::new(1_000, 1_000.0)),
        log: empty_log().await,
        snapshots: Arc::new(crate::snapshots::Snapshots::new(86_400)),
    };
    Harness {
        app: build_app(state),
        store,
        list: bytes,
    }
}

fn report(list_version: u64, recipients: &[Recipient]) -> Vec<u8> {
    seal_report(
        Target::account(&[7; 32]).unwrap(),
        Reason::Spam,
        ConversationKind::OneToOne,
        &ReportContent {
            comment: "spam".into(),
            evidence: vec![EvidenceItem::ContactRequest {
                wire: vec![1, 2, 3],
            }],
        },
        list_version,
        recipients,
    )
    .unwrap()
}

fn current_recipients(list_bytes: &[u8]) -> Vec<Recipient> {
    ModeratorList::verify(list_bytes, &root_public())
        .unwrap()
        .recipients()
}

fn token() -> String {
    let c = config();
    mint_header(&c.master, TokenContext::Report, c.current_epoch())
}

pub(crate) async fn empty_log() -> Arc<crate::log::ModerationLog> {
    Arc::new(
        crate::log::ModerationLog::open(
            Arc::new(crate::log_store::MemoryLogStore::new()),
            Vec::new(),
        )
        .await
        .unwrap(),
    )
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, body)
}

fn post(body: Vec<u8>, token: Option<&str>) -> Request<Body> {
    let mut b = Request::post("/moderation/report").header("x-real-ip", "198.51.100.1");
    if let Some(t) = token {
        b = b.header("x-lakko-anon-token", t);
    }
    b.body(Body::from(body)).unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::get(path)
        .header("x-real-ip", "198.51.100.1")
        .body(Body::empty())
        .unwrap()
}

fn error_code(body: &[u8]) -> String {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap();
    v["error"].as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn a_report_is_received_and_its_status_read() {
    let h = harness(true).await;
    let (s, body) = send(&h.app, get("/moderation/moderators")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, h.list);

    let r = report(2, &current_recipients(&h.list));
    let (s, body) = send(&h.app, post(r.clone(), Some(&token()))).await;
    assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let receipt = v["receipt_id"].as_str().unwrap().to_owned();
    assert_eq!(receipt.len(), 22);

    let rows = h.store.all();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].report, r);
    assert_eq!(rows[0].reason, Reason::Spam.as_u8());
    assert_eq!(rows[0].evidence_count, 1);
    assert_eq!(rows[0].list_version, 2);

    let path = format!("/moderation/report/{receipt}");
    let (s, body) = send(&h.app, get(&path)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"],
        "received"
    );
    h.store
        .set_status(&rows[0].receipt_id, ReportStatus::NoAction);
    let (_, body) = send(&h.app, get(&path)).await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"],
        "none"
    );
}

#[tokio::test]
async fn a_bad_report_costs_no_token() {
    let h = harness(true).await;
    let t = token();
    let (s, body) = send(&h.app, post(vec![1, 2, 3], Some(&t))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "bad-report");
    // A list version the service does not know.
    let (s, body) = send(
        &h.app,
        post(report(3, &current_recipients(&h.list)), Some(&t)),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "unknown-list-version");
    // Sealed to nobody on the list.
    let stranger = moderator_report_secret(&[5; 32]).unwrap();
    let nobody = [Recipient {
        moderator_id: [8; 16],
        x25519_public: stranger.public_key().to_bytes(),
    }];
    let (s, body) = send(&h.app, post(report(2, &nobody), Some(&t))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "no-current-moderator");
    // The same token still buys a good report.
    let (s, _) = send(
        &h.app,
        post(report(2, &current_recipients(&h.list)), Some(&t)),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    // And is spent now.
    let (s, body) = send(
        &h.app,
        post(report(2, &current_recipients(&h.list)), Some(&t)),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-spent");
}

#[tokio::test]
async fn a_report_needs_a_report_token() {
    let h = harness(true).await;
    let r = report(2, &current_recipients(&h.list));
    let (s, body) = send(&h.app, post(r.clone(), None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-required");
    let c = config();
    let other = mint_header(&c.master, TokenContext::DsKnock, c.current_epoch());
    let (s, body) = send(&h.app, post(r, Some(&other))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-wrong-context");
    assert!(h.store.all().is_empty());
}

#[tokio::test]
async fn without_a_list_reporting_is_unavailable() {
    let h = harness(false).await;
    let (s, body) = send(&h.app, get("/moderation/moderators")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_code(&body), "reporting-unavailable");
    let (s, body) = send(
        &h.app,
        post(report(2, &current_recipients(&h.list)), Some(&token())),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_code(&body), "reporting-unavailable");
}

#[tokio::test]
async fn a_receipt_is_checked_before_it_is_looked_up() {
    let h = harness(true).await;
    let (s, body) = send(&h.app, get("/moderation/report/short")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "bad-receipt");
    let (s, body) = send(&h.app, get("/moderation/report/AAAAAAAAAAAAAAAAAAAAAA")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "unknown-receipt");
}

#[tokio::test]
async fn the_routes_are_limited_per_network() {
    let bytes = signed_list(2);
    let state = AppState {
        store: Arc::new(MemoryStore::new()),
        list: Some(Arc::new(list::from_bytes(bytes, &root_public()).unwrap())),
        tokens: TokenVerifier::in_memory(Some(config())),
        limits: Arc::new(IpLimiter::new(2, 0.0)),
        log: empty_log().await,
        snapshots: Arc::new(crate::snapshots::Snapshots::new(86_400)),
    };
    let app = build_app(state);
    for _ in 0..2 {
        assert_eq!(
            send(&app, get("/moderation/moderators")).await.0,
            StatusCode::OK
        );
    }
    let (s, body) = send(&app, get("/moderation/moderators")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(&body), "rate-limited");
    // No source address at all: refused.
    let (s, _) = send(
        &app,
        Request::get("/moderation/moderators")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    // Health is not limited.
    let (s, _) = send(&app, get("/health")).await;
    assert_eq!(s, StatusCode::OK);
}

#[test]
fn a_list_that_does_not_verify_is_refused_and_the_spent_set_schema_is_the_shared_one() {
    assert!(list::from_bytes(signed_list(1), &[0; 32]).is_err());
    assert!(list::parse_root_key("not-a-key").is_err());
    assert_eq!(
        include_str!("../migrations/0002_anon_token_spent.sql"),
        lakko_anon_token_server::SPENT_SET_MIGRATION_SQL
    );
}
