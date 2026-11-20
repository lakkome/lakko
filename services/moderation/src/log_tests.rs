// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests of the moderation log and the internal statement route, over
//! the in-memory stores, an in-process fake witness and fake applying
//! services.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_anon_token_server::{AnonTokenServerConfig, TokenVerifier};
use lakko_crypto::signing::{SigningKey, VerifyingKey};
use lakko_moderation::moderators::{ModeratorEntry, ModeratorList};
use lakko_moderation::software_authenticator::SoftwareAuthenticator;
use lakko_moderation::statement::{reason_commitment, Action, SignedStatement, Statement};
use lakko_moderation::target::{Target, TargetKind};
use lakko_moderation::Reason;
use lakko_transparency::merkle::verify_inclusion;
use lakko_transparency::{sign_witness_cosig, verify_witness_cosig, LogId, SignedRoot, TreeHash};
use serde_json::{json, Value};
use tower::ServiceExt as _;

use crate::internal::{build_internal_app, Forwarder, Forwarders, InternalState};
use crate::ip_limit::IpLimiter;
use crate::list::{self, LoadedList};
use crate::log::{ModerationLog, LEAF_STATEMENT};
use crate::log_store::MemoryLogStore;
use crate::store::MemoryStore;
use crate::witness_client::WitnessClient;
use crate::{build_app, AppState};
use lakko_moderation::apply::ApplyingService;

const ROOT_SEED: [u8; 32] = [9; 32];
const MODERATOR_SEED: [u8; 32] = [21; 32];
const MODERATOR_ID: [u8; 16] = [5; 16];
const SECRET: &[u8] = b"0123456789abcdef-internal";
const ACCOUNT_KEY: [u8; 32] = [7; 32];
const SALT: [u8; 16] = [3; 16];

fn moderator() -> SoftwareAuthenticator {
    SoftwareAuthenticator::from_seed(&MODERATOR_SEED)
}

fn moderator_list() -> ModeratorList {
    ModeratorList {
        list_version: 2,
        issued_at_ms: 1,
        rp_id: "moderation.example".into(),
        origin: "https://moderation.example".into(),
        entries: vec![ModeratorEntry {
            moderator_id: MODERATOR_ID,
            name: "dev".into(),
            key: moderator().moderator_key(),
            credential_id: vec![1],
            x25519_public: [4; 32],
            added_at_ms: 0,
            revoked_at_ms: 0,
        }],
    }
}

fn loaded() -> Arc<LoadedList> {
    let bytes = moderator_list().sign(&ROOT_SEED).unwrap();
    let root = SigningKey::from_seed(&ROOT_SEED).verifying_key().to_bytes();
    Arc::new(list::from_bytes(bytes, &root).unwrap())
}

fn statement(id: u8, action: Action) -> Statement {
    let target = if action.target_kind() == TargetKind::Account {
        Target::account(&ACCOUNT_KEY).unwrap()
    } else {
        Target::new(
            action.target_kind(),
            &vec![1; action.target_kind().target_len()],
        )
        .unwrap()
    };
    Statement {
        statement_id: [id; 16],
        action,
        target,
        reason_commitment: reason_commitment(Reason::Spam, &SALT),
        until_ms: 0,
        // Within the 24 h a statement stays fresh (ADR-0098 F1).
        issued_at_ms: crate::panel::now_ms(),
        moderator_id: MODERATOR_ID,
        list_version: 2,
    }
}

fn signed(s: &Statement, by: &SoftwareAuthenticator) -> Vec<u8> {
    by.sign_statement(&moderator_list(), s)
        .unwrap()
        .encode()
        .unwrap()
}

/// The body names no account key since step 5; the last argument only
/// records which account a test means.
fn body(signed: &[u8], reason: Reason, _account: Option<[u8; 32]>) -> Value {
    json!({
        "signed_statement_b64": B64.encode(signed),
        "reason": reason.as_u8(),
        "reason_salt_b64": B64.encode(SALT),
    })
}

async fn empty_log(witnesses: Vec<WitnessClient>) -> Arc<ModerationLog> {
    Arc::new(
        ModerationLog::open(Arc::new(MemoryLogStore::new()), witnesses)
            .await
            .unwrap(),
    )
}

fn internal(log: &Arc<ModerationLog>, with_list: bool, auth: Option<Forwarder>) -> Router {
    let forwarders = auth.map_or_else(Forwarders::none, |f| Forwarders::none().with(f));
    internal_with(log, with_list, forwarders)
}

fn internal_with(log: &Arc<ModerationLog>, with_list: bool, forwarders: Forwarders) -> Router {
    build_internal_app(InternalState {
        log: log.clone(),
        list: with_list.then(loaded),
        forwarders: Arc::new(forwarders),
        secret: Arc::new(SECRET.to_vec()),
    })
}

fn auth_forwarder(url: &str, secret: &str) -> Forwarder {
    Forwarder::new(ApplyingService::Auth, url, secret).unwrap()
}

fn public(log: &Arc<ModerationLog>) -> Router {
    let config = AnonTokenServerConfig::from_values(
        Some("4242424242424242424242424242424242424242424242424242424242424242"),
        None,
    )
    .unwrap();
    build_app(AppState {
        store: Arc::new(MemoryStore::new()),
        list: Some(loaded()),
        tokens: TokenVerifier::in_memory(config),
        limits: Arc::new(IpLimiter::new(1_000, 1_000.0)),
        log: log.clone(),
        snapshots: Arc::new(crate::snapshots::Snapshots::new(86_400)),
    })
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn post_statement(v: &Value, secret: Option<&[u8]>) -> Request<Body> {
    let mut b = Request::post("/internal/statements").header("content-type", "application/json");
    if let Some(s) = secret {
        b = b.header("x-lakko-internal-secret", B64.encode(s));
    }
    b.body(Body::from(v.to_string())).unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::get(path)
        .header("x-real-ip", "198.51.100.1")
        .body(Body::empty())
        .unwrap()
}

/// A fake auth on a loopback port: records each body and secret header and
/// answers with the given status and body.
async fn fake_auth(
    status: StatusCode,
    answer: Value,
) -> (String, Arc<Mutex<Vec<(Value, String)>>>) {
    #[derive(Clone)]
    struct S {
        seen: Arc<Mutex<Vec<(Value, String)>>>,
        status: StatusCode,
        answer: Value,
    }
    async fn handle(
        State(s): State<S>,
        headers: HeaderMap,
        Json(v): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let secret = headers
            .get("x-lakko-internal-secret")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        s.seen.lock().unwrap().push((v, secret));
        (s.status, Json(s.answer.clone()))
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/internal/moderation/statement", post(handle))
        .with_state(S {
            seen: seen.clone(),
            status,
            answer,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn the_internal_route_needs_its_secret() {
    let log = empty_log(Vec::new()).await;
    let app = internal(&log, true, None);
    let v = body(
        &signed(&statement(1, Action::ChannelDelist), &moderator()),
        Reason::Spam,
        None,
    );
    assert_eq!(
        send(&app, post_statement(&v, None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, post_statement(&v, Some(b"0123456789abcdef-wrong")))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, post_statement(&v, Some(SECRET))).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn without_a_list_statements_are_unavailable() {
    let log = empty_log(Vec::new()).await;
    let app = internal(&log, false, None);
    let v = body(
        &signed(&statement(1, Action::ChannelDelist), &moderator()),
        Reason::Spam,
        None,
    );
    let (s, b) = send(&app, post_statement(&v, Some(SECRET))).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(b["error"], "reporting-unavailable");
    assert_eq!(log.size().await, 0);
}

#[tokio::test]
async fn every_refusal_leaves_the_log_untouched() {
    let log = empty_log(Vec::new()).await;
    let app = internal(&log, true, None);
    let good = signed(&statement(1, Action::Suspend), &moderator());
    let cases: Vec<(Value, StatusCode, &str)> = vec![
        (json!({ "nope": 1 }), StatusCode::BAD_REQUEST, "bad-request"),
        (
            body(b"garbage", Reason::Spam, Some(ACCOUNT_KEY)),
            StatusCode::BAD_REQUEST,
            "bad-statement",
        ),
        (
            // The listed moderator's id with another key: the assertion
            // fails, which is a bad statement (ADR-0098 step 5, the one
            // check every service runs).
            body(
                &signed(
                    &statement(1, Action::Suspend),
                    &SoftwareAuthenticator::from_seed(&[22; 32]),
                ),
                Reason::Spam,
                Some(ACCOUNT_KEY),
            ),
            StatusCode::BAD_REQUEST,
            "bad-statement",
        ),
        (
            body(&good, Reason::Hate, Some(ACCOUNT_KEY)),
            StatusCode::BAD_REQUEST,
            "reason-mismatch",
        ),
        (
            json!({
                "signed_statement_b64": B64.encode(&good),
                "reason": 0,
                "reason_salt_b64": B64.encode(SALT),
            }),
            StatusCode::BAD_REQUEST,
            "reason-mismatch",
        ),
        (
            json!({
                "signed_statement_b64": B64.encode(&good),
                "reason": Reason::Spam.as_u8(),
                "reason_salt_b64": B64.encode([3u8; 15]),
            }),
            StatusCode::BAD_REQUEST,
            "reason-mismatch",
        ),
    ];
    for (v, status, code) in cases {
        let (s, b) = send(&app, post_statement(&v, Some(SECRET))).await;
        assert_eq!(
            (s, b["error"].as_str().unwrap_or_default()),
            (status, code),
            "{v}"
        );
    }
    // A statement naming a list version newer than the loaded one.
    let mut newer = statement(2, Action::Suspend);
    newer.list_version = 3;
    let (s, b) = send(
        &app,
        post_statement(
            &body(
                &signed(&newer, &moderator()),
                Reason::Spam,
                Some(ACCOUNT_KEY),
            ),
            Some(SECRET),
        ),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str().unwrap()),
        (StatusCode::FORBIDDEN, "not-authorised")
    );
    assert_eq!(log.size().await, 0);
}

#[tokio::test]
async fn a_non_auth_action_is_logged_once_and_not_applied() {
    let log = empty_log(Vec::new()).await;
    let app = internal(&log, true, None);
    let s1 = signed(&statement(1, Action::ChannelDelist), &moderator());
    let v = body(&s1, Reason::Spam, None);
    let (s, b) = send(&app, post_statement(&v, Some(SECRET))).await;
    assert_eq!(s, StatusCode::OK);
    let unapplied = json!({ "log_index": 0, "applied": false, "forwarded": false });
    assert_eq!(b, unapplied);
    let (_, b) = send(&app, post_statement(&v, Some(SECRET))).await;
    assert_eq!(b, unapplied);
    assert_eq!(log.size().await, 1);
    // Another statement under the same id is refused.
    let mut other = statement(1, Action::ChannelDelist);
    other.issued_at_ms += 1;
    let (s, b) = send(
        &app,
        post_statement(
            &body(&signed(&other, &moderator()), Reason::Spam, None),
            Some(SECRET),
        ),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str().unwrap()),
        (StatusCode::CONFLICT, "statement-id-conflict")
    );
    // The logged leaf is the signed statement, kind 1, and nothing else.
    let e = log.entry(0, None).await.unwrap();
    assert_eq!(e.leaf[0], LEAF_STATEMENT);
    assert_eq!(&e.leaf[1..], s1.as_slice());
    assert!(SignedStatement::decode(&e.leaf[1..]).is_ok());
}

#[tokio::test]
async fn a_suspension_is_forwarded_to_auth() {
    let log = empty_log(Vec::new()).await;
    let (url, seen) = fake_auth(StatusCode::OK, json!({ "applied": true })).await;
    let app = internal(
        &log,
        true,
        Some(auth_forwarder(&url, "YXV0aC1zZWNyZXQtYXV0aC1zZWNyZXQ=")),
    );
    let s1 = signed(&statement(1, Action::Suspend), &moderator());
    let v = body(&s1, Reason::Spam, Some(ACCOUNT_KEY));
    let (s, b) = send(&app, post_statement(&v, Some(SECRET))).await;
    assert_eq!(s, StatusCode::OK);
    let applied = json!({ "log_index": 0, "applied": true, "forwarded": true });
    assert_eq!(b, applied);
    let (forwarded, secret) = seen.lock().unwrap()[0].clone();
    assert_eq!(secret, "YXV0aC1zZWNyZXQtYXV0aC1zZWNyZXQ=");
    assert_eq!(forwarded["log_index"], 0);
    assert_eq!(forwarded["reason"], Reason::Spam.as_u8());
    assert_eq!(forwarded["signed_statement_b64"], B64.encode(&s1));
    assert_eq!(forwarded["moderator_list_b64"], B64.encode(&loaded().bytes));
    assert_eq!(forwarded["reason_salt_b64"], B64.encode(SALT));
    assert!(forwarded.get("account_key_b64").is_none());
    // The forward is exactly the shared statement body.
    let parsed: lakko_moderation::apply::StatementBody =
        serde_json::from_value(forwarded.clone()).unwrap();
    assert_eq!(parsed.log_index, 0);
    // A resend forwards again, at the same index.
    let (_, b) = send(&app, post_statement(&v, Some(SECRET))).await;
    assert_eq!(b, applied);
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(log.size().await, 1);
}

#[tokio::test]
async fn an_auth_failure_or_refusal_keeps_the_entry() {
    let log = empty_log(Vec::new()).await;
    // Not configured.
    let app = internal(&log, true, None);
    let v = body(
        &signed(&statement(1, Action::Lift), &moderator()),
        Reason::Spam,
        Some(ACCOUNT_KEY),
    );
    let (s, b) = send(&app, post_statement(&v, Some(SECRET))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        b,
        json!({ "log_index": 0, "applied": false, "forwarded": false })
    );
    // 5xx.
    let (url, _) = fake_auth(StatusCode::INTERNAL_SERVER_ERROR, json!({})).await;
    let app = internal(
        &log,
        true,
        Some(auth_forwarder(&url, "c2VjcmV0c2VjcmV0c2VjcmV0")),
    );
    let (s, b) = send(&app, post_statement(&v, Some(SECRET))).await;
    assert_eq!(b, json!({ "error": "apply-failed", "log_index": 0 }));
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    // Unreachable.
    let app = internal(
        &log,
        true,
        Some(auth_forwarder(
            "http://127.0.0.1:1",
            "c2VjcmV0c2VjcmV0c2VjcmV0",
        )),
    );
    let (s, _) = send(&app, post_statement(&v, Some(SECRET))).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    // 4xx.
    let (url, _) = fake_auth(StatusCode::NOT_FOUND, json!({ "error": "unknown-account" })).await;
    let app = internal(
        &log,
        true,
        Some(auth_forwarder(&url, "c2VjcmV0c2VjcmV0c2VjcmV0")),
    );
    let (s, b) = send(&app, post_statement(&v, Some(SECRET))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        b,
        json!({ "error": "apply-refused", "detail": "unknown-account", "log_index": 0 })
    );
    assert_eq!(log.size().await, 1);
}

/// Each action goes to the service that applies it, and only there; the
/// service's own `applied` / `repeat` / `superseded` come back.
#[tokio::test]
async fn every_action_is_forwarded_to_its_own_service() {
    let log = empty_log(Vec::new()).await;
    let (auth_url, auth_seen) = fake_auth(StatusCode::OK, json!({ "applied": true })).await;
    let (disc_url, disc_seen) =
        fake_auth(StatusCode::OK, json!({ "applied": false, "repeat": true })).await;
    let (chan_url, chan_seen) = fake_auth(
        StatusCode::OK,
        json!({ "applied": false, "superseded": true }),
    )
    .await;
    let (store_url, store_seen) = fake_auth(StatusCode::OK, json!({})).await;
    let forwarders = Forwarders::none()
        .with(auth_forwarder(&auth_url, "YQ=="))
        .with(Forwarder::new(ApplyingService::Discovery, &disc_url, "ZA==").unwrap())
        .with(Forwarder::new(ApplyingService::Channels, &chan_url, "Yw==").unwrap())
        .with(Forwarder::new(ApplyingService::Storage, &store_url, "cw==").unwrap());
    let app = internal_with(&log, true, forwarders);
    let cases = [
        (1u8, Action::Lift, json!({ "applied": true })),
        (
            2,
            Action::TakeBackName,
            json!({ "applied": false, "repeat": true }),
        ),
        (
            3,
            Action::GroupBlockHandle,
            json!({ "applied": false, "repeat": true }),
        ),
        (
            4,
            Action::ChannelRemove,
            json!({ "applied": false, "superseded": true }),
        ),
        (5, Action::BlobDelete, json!({ "applied": true })),
    ];
    for (i, (id, action, answer)) in cases.iter().enumerate() {
        let v = body(
            &signed(&statement(*id, *action), &moderator()),
            Reason::Spam,
            None,
        );
        let (s, mut b) = send(&app, post_statement(&v, Some(SECRET))).await;
        assert_eq!(s, StatusCode::OK, "{action:?}");
        assert_eq!(b["log_index"], json!(i), "{action:?}");
        assert_eq!(b["forwarded"], json!(true));
        b.as_object_mut().unwrap().remove("log_index");
        b.as_object_mut().unwrap().remove("forwarded");
        assert_eq!(&b, answer, "{action:?}");
    }
    let count = |seen: &Arc<Mutex<Vec<(Value, String)>>>| seen.lock().unwrap().len();
    assert_eq!(count(&auth_seen), 1);
    assert_eq!(count(&disc_seen), 2);
    assert_eq!(count(&chan_seen), 1);
    assert_eq!(count(&store_seen), 1);
    assert_eq!(chan_seen.lock().unwrap()[0].1, "Yw==");
    assert_eq!(chan_seen.lock().unwrap()[0].0["log_index"], json!(3));
}

#[tokio::test]
async fn the_public_routes_prove_what_was_logged() {
    let log = empty_log(Vec::new()).await;
    let internal_app = internal(&log, true, None);
    for id in 1..=3 {
        let v = body(
            &signed(&statement(id, Action::ChannelDelist), &moderator()),
            Reason::Spam,
            None,
        );
        assert_eq!(
            send(&internal_app, post_statement(&v, Some(SECRET)))
                .await
                .0,
            StatusCode::OK
        );
    }
    let app = public(&log);
    let (s, key) = send(&app, get("/moderation/log/operator-key")).await;
    assert_eq!(s, StatusCode::OK);
    let operator = VerifyingKey::from_bytes(
        &B64.decode(key["operator_pubkey_b64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    let (s, root) = send(&app, get("/moderation/log/root")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(root["log_size"], 3);
    assert_eq!(root["witness_cosigs_b64"], json!([]));
    let tree_root: TreeHash = B64
        .decode(root["tree_root_b64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let signed_root = SignedRoot {
        tree_root,
        log_size: 3,
        timestamp_unix: root["timestamp_unix"].as_u64().unwrap(),
        signature: lakko_crypto::signing::Signature::from_bytes(
            &B64.decode(root["signature_b64"].as_str().unwrap()).unwrap(),
        )
        .unwrap(),
    };
    signed_root.verify(&operator).unwrap();
    for i in 0..3u64 {
        let (s, e) = send(&app, get(&format!("/moderation/log/entry/{i}"))).await;
        assert_eq!(s, StatusCode::OK);
        let leaf = B64.decode(e["leaf_b64"].as_str().unwrap()).unwrap();
        let path: Vec<TreeHash> = e["audit_path_b64"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| B64.decode(h.as_str().unwrap()).unwrap().try_into().unwrap())
            .collect();
        assert!(verify_inclusion(&leaf, i, 3, &path, &tree_root));
    }
    let (s, e) = send(&app, get("/moderation/log/entry/0?size=2")).await;
    assert_eq!((s, e["log_size"].as_u64()), (StatusCode::OK, Some(2)));
    let (s, e) = send(&app, get("/moderation/log/entry/3")).await;
    assert_eq!(
        (s, e["error"].as_str().unwrap()),
        (StatusCode::NOT_FOUND, "no-such-entry")
    );
    let (s, p) = send(&app, get("/moderation/log/consistency?from=1&to=3")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!p["proof_b64"].as_array().unwrap().is_empty());
    let (s, p) = send(&app, get("/moderation/log/consistency?from=3&to=4")).await;
    assert_eq!(
        (s, p["error"].as_str().unwrap()),
        (StatusCode::BAD_REQUEST, "bad-range")
    );
    assert_eq!(
        send(&app, get("/moderation/log/consistency?from=1"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

/// A fake witness on a loopback port: co-signs any root under the log the
/// request names, remembering the names it saw.
async fn fake_witness(key: [u8; 32]) -> (String, Arc<Mutex<Vec<String>>>) {
    #[derive(Clone)]
    struct S {
        key: [u8; 32],
        logs: Arc<Mutex<Vec<String>>>,
    }
    async fn cosign(State(s): State<S>, Json(v): Json<Value>) -> Json<Value> {
        let log_name = v["log"].as_str().unwrap_or_default().to_owned();
        s.logs.lock().unwrap().push(log_name.clone());
        let log = LogId::from_wire_name(&log_name).unwrap();
        let root: TreeHash = B64
            .decode(v["new_root"]["tree_root_b64"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let size = v["new_root"]["log_size"].as_u64().unwrap();
        let cosig = sign_witness_cosig(&SigningKey::from_seed(&s.key), log, &root, size);
        Json(json!({ "cosig_b64": B64.encode(cosig) }))
    }
    let logs = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/witness/cosign", post(cosign))
        .with_state(S {
            key,
            logs: logs.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{addr}"), logs)
}

#[tokio::test]
async fn the_root_carries_moderation_log_cosignatures() {
    let (url, seen) = fake_witness([31; 32]).await;
    let down_url = "http://127.0.0.1:1";
    let witnesses = vec![
        WitnessClient::new(&url, "c2VjcmV0", 5).unwrap(),
        WitnessClient::new(down_url, "c2VjcmV0", 1).unwrap(),
    ];
    let log = empty_log(witnesses).await;
    let internal_app = internal(&log, true, None);
    let v = body(
        &signed(&statement(1, Action::ChannelDelist), &moderator()),
        Reason::Spam,
        None,
    );
    send(&internal_app, post_statement(&v, Some(SECRET))).await;
    let app = public(&log);
    let (_, root) = send(&app, get("/moderation/log/root")).await;
    let cosigs = root["witness_cosigs_b64"].as_array().unwrap();
    assert_eq!(cosigs.len(), 1, "the down witness contributes nothing");
    let cosig = B64.decode(cosigs[0].as_str().unwrap()).unwrap();
    let tree_root: TreeHash = B64
        .decode(root["tree_root_b64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let witness_key = SigningKey::from_seed(&[31; 32]).verifying_key().to_bytes();
    verify_witness_cosig(&cosig, &witness_key, LogId::Moderation, &tree_root, 1).unwrap();
    assert!(verify_witness_cosig(&cosig, &witness_key, LogId::Kt, &tree_root, 1).is_err());
    assert_eq!(*seen.lock().unwrap(), vec!["moderation".to_string()]);
    // An unchanged head serves the cached bundle without asking again.
    let (_, again) = send(&app, get("/moderation/log/root")).await;
    assert_eq!(again["witness_cosigs_b64"], root["witness_cosigs_b64"]);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

/// A fake applying service's list route: the first `failures` pushes get a
/// 503, then `final_status` with `final_body`; every pushed body is kept.
async fn fake_list_route(
    failures: usize,
    final_status: StatusCode,
    final_body: Value,
) -> (String, Arc<Mutex<Vec<(HeaderMap, Value)>>>) {
    type Seen = Arc<Mutex<Vec<(HeaderMap, Value)>>>;
    async fn push(
        State((seen, failures, status, body)): State<(Seen, usize, StatusCode, Value)>,
        headers: HeaderMap,
        Json(v): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let n = {
            let mut s = seen.lock().unwrap();
            s.push((headers, v));
            s.len()
        };
        if n <= failures {
            (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})))
        } else {
            (status, Json(body))
        }
    }
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(crate::internal::LIST_PUSH_PATH, post(push))
        .with_state((seen.clone(), failures, final_status, final_body));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn the_list_is_pushed_until_the_service_answers() {
    use crate::internal::{push_list_until_answered, PushOutcome};
    use std::time::Duration;
    let (url, seen) = fake_list_route(2, StatusCode::OK, json!({ "list_version": 2 })).await;
    let f = Forwarder::new(ApplyingService::Discovery, &url, &B64.encode(SECRET)).unwrap();
    let list = loaded();
    let outcome = push_list_until_answered(
        &f,
        &list,
        Duration::from_millis(5),
        Duration::from_millis(20),
    )
    .await;
    assert_eq!(outcome, PushOutcome::Accepted(Some(2)));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "two failures, then accepted");
    for (headers, body) in seen.iter() {
        assert_eq!(
            headers.get("x-lakko-internal-secret").unwrap(),
            &B64.encode(SECRET)
        );
        assert_eq!(body["moderator_list_b64"], B64.encode(&list.bytes));
    }
}

#[tokio::test]
async fn a_refused_list_is_not_pushed_again() {
    use crate::internal::{push_list_until_answered, PushOutcome};
    use std::time::Duration;
    let (url, seen) =
        fake_list_route(0, StatusCode::CONFLICT, json!({ "error": "stale-list" })).await;
    let f = Forwarder::new(ApplyingService::Channels, &url, &B64.encode(SECRET)).unwrap();
    let outcome = push_list_until_answered(
        &f,
        &loaded(),
        Duration::from_millis(5),
        Duration::from_millis(20),
    )
    .await;
    assert_eq!(outcome, PushOutcome::Refused(Some("stale-list".into())));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_list_push_reaches_every_configured_service() {
    use crate::internal::{spawn_list_push, PushOutcome};
    let (a, seen_a) = fake_list_route(0, StatusCode::OK, json!({ "list_version": 2 })).await;
    let (b, seen_b) = fake_list_route(0, StatusCode::OK, json!({ "list_version": 2 })).await;
    let forwarders = Arc::new(
        Forwarders::none()
            .with(Forwarder::new(ApplyingService::Auth, &a, &B64.encode(SECRET)).unwrap())
            .with(Forwarder::new(ApplyingService::Storage, &b, &B64.encode(SECRET)).unwrap()),
    );
    let handles = spawn_list_push(&forwarders, &loaded());
    assert_eq!(handles.len(), 2);
    for h in handles {
        assert_eq!(h.await.unwrap(), PushOutcome::Accepted(Some(2)));
    }
    assert_eq!(seen_a.lock().unwrap().len(), 1);
    assert_eq!(seen_b.lock().unwrap().len(), 1);
}
