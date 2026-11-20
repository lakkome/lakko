// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests of the moderators' panel over the in-memory stores, with the
//! software security key and a fake auth / discovery on a loopback port.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_crypto::signing::SigningKey;
use lakko_moderation::apply::ApplyingService;
use lakko_moderation::moderators::{ModeratorEntry, ModeratorList};
use lakko_moderation::software_authenticator::SoftwareAuthenticator;
use lakko_moderation::statement::{reason_commitment, Action, Statement};
use lakko_moderation::target::{Target, TargetKind};
use lakko_moderation::Reason;
use serde_json::{json, Value};
use tower::ServiceExt as _;

use crate::internal::{Forwarder, Forwarders};
use crate::list::{self, LoadedList};
use crate::log::ModerationLog;
use crate::log_store::MemoryLogStore;
use crate::panel::{blob_slots, build_panel_app, now_ms, PanelState, Sessions, LOCK_IDLE_MS};
use crate::panel_view::hex;
use crate::store::{MemoryStore, NewReport, ReportStatus, ReportStore};

const ROOT_SEED: [u8; 32] = [9; 32];
const SEED_A: [u8; 32] = [21; 32];
const SEED_B: [u8; 32] = [22; 32];
const ID_A: [u8; 16] = [5; 16];
const ID_B: [u8; 16] = [6; 16];
const ID_REVOKED: [u8; 16] = [7; 16];
const SALT: [u8; 16] = [3; 16];
const ACCOUNT_KEY: [u8; 32] = [7; 32];

fn auth_key(seed: &[u8; 32]) -> SoftwareAuthenticator {
    SoftwareAuthenticator::from_seed(seed)
}

fn entry(id: [u8; 16], name: &str, seed: &[u8; 32], revoked_at_ms: u64) -> ModeratorEntry {
    ModeratorEntry {
        moderator_id: id,
        name: name.into(),
        key: auth_key(seed).moderator_key(),
        credential_id: vec![id[0]],
        x25519_public: [4; 32],
        added_at_ms: 0,
        revoked_at_ms,
    }
}

fn moderator_list() -> ModeratorList {
    ModeratorList {
        list_version: 2,
        issued_at_ms: 1,
        rp_id: "moderation.example".into(),
        origin: "https://moderation.example".into(),
        entries: vec![
            entry(ID_A, "alice", &SEED_A, 0),
            entry(ID_B, "bob", &SEED_B, 0),
            entry(ID_REVOKED, "carol", &[23; 32], 5),
        ],
    }
}

fn loaded() -> Arc<LoadedList> {
    let bytes = moderator_list().sign(&ROOT_SEED).unwrap();
    let root = SigningKey::from_seed(&ROOT_SEED).verifying_key().to_bytes();
    Arc::new(list::from_bytes(bytes, &root).unwrap())
}

fn account_target() -> Vec<u8> {
    Target::account(&ACCOUNT_KEY).unwrap().bytes().to_vec()
}

struct Harness {
    app: Router,
    store: Arc<MemoryStore>,
    log: Arc<ModerationLog>,
    slots: Arc<tokio::sync::Semaphore>,
}

async fn harness(forwarders: Forwarders, static_dir: Option<PathBuf>) -> Harness {
    let store = Arc::new(MemoryStore::new());
    let log = Arc::new(
        ModerationLog::open(Arc::new(MemoryLogStore::new()), Vec::new())
            .await
            .unwrap(),
    );
    let slots = blob_slots();
    let app = build_panel_app(
        PanelState {
            store: store.clone(),
            list: loaded(),
            log: log.clone(),
            forwarders: Arc::new(forwarders),
            sessions: Arc::new(Sessions::new()),
            blob_slots: slots.clone(),
        },
        static_dir,
    );
    Harness {
        app,
        store,
        log,
        slots,
    }
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

fn get_req(path: &str, token: Option<&str>) -> Request<Body> {
    let mut b = Request::get(path);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

fn post_req(path: &str, token: Option<&str>, body: &Value) -> Request<Body> {
    let mut b = Request::post(path).header("content-type", "application/json");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// Sign in with `seed` as `id`.
async fn sign_in(app: &Router, id: [u8; 16], seed: &[u8; 32]) -> String {
    let (status, start) = send(app, post_req("/panel/api/session/start", None, &json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let challenge: [u8; 32] = B64
        .decode(start["challenge_b64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let assertion = auth_key(seed).assert(&moderator_list(), &challenge);
    let (status, done) = send(
        app,
        post_req(
            "/panel/api/session/finish",
            None,
            &json!({
                "moderator_id_b64": B64.encode(id),
                "authenticator_data_b64": B64.encode(&assertion.authenticator_data),
                "client_data_json_b64": B64.encode(&assertion.client_data_json),
                "signature_b64": B64.encode(&assertion.signature),
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    done["token"].as_str().unwrap().to_owned()
}

async fn report(
    store: &MemoryStore,
    id: u8,
    kind: TargetKind,
    target: &[u8],
    reason: Reason,
    day: u32,
) {
    store
        .insert(NewReport {
            receipt_id: [id; 16],
            target_kind: kind.as_u8(),
            target: target.to_vec(),
            reason: reason.as_u8(),
            conversation_kind: 1,
            evidence_count: 1,
            list_version: 2,
            report: vec![id; 8],
            received_day: day,
        })
        .await
        .unwrap();
}

fn statement(id: u8, action: Action, moderator: [u8; 16], issued_at_ms: u64) -> Statement {
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
        issued_at_ms,
        moderator_id: moderator,
        list_version: 2,
    }
}

fn signed(s: &Statement, seed: &[u8; 32]) -> Vec<u8> {
    auth_key(seed)
        .sign_statement(&moderator_list(), s)
        .unwrap()
        .encode()
        .unwrap()
}

fn decide_body(signed: &[u8], note: Option<&str>) -> Value {
    json!({
        "signed_statement_b64": B64.encode(signed),
        "reason": Reason::Spam.as_u8(),
        "reason_salt_b64": B64.encode(SALT),
        "note": note,
    })
}

#[derive(Clone, Default)]
struct Fake {
    seen: Arc<Mutex<Vec<(String, Value)>>>,
    appeals: Arc<Mutex<Value>>,
    /// Answers to the next statements, oldest first; `{"applied": true}`
    /// once empty.
    answers: Arc<Mutex<Vec<Value>>>,
}

/// A fake auth (or discovery) internal listener.
async fn fake_service(fake: Fake, account_answer: Value) -> String {
    async fn account(State((f, a)): State<(Fake, Value)>, Json(v): Json<Value>) -> Json<Value> {
        f.seen.lock().unwrap().push(("account".into(), v));
        Json(a)
    }
    async fn appeals(State((f, _)): State<(Fake, Value)>) -> Json<Value> {
        Json(f.appeals.lock().unwrap().clone())
    }
    async fn statement(State((f, _)): State<(Fake, Value)>, Json(v): Json<Value>) -> Json<Value> {
        f.seen.lock().unwrap().push(("statement".into(), v));
        let mut answers = f.answers.lock().unwrap();
        if answers.is_empty() {
            Json(json!({ "applied": true }))
        } else {
            Json(answers.remove(0))
        }
    }
    async fn blob(Path(hex): Path<String>) -> (StatusCode, Vec<u8>) {
        if hex == "ab".repeat(32) {
            (StatusCode::OK, b"ciphertext".to_vec())
        } else {
            (
                StatusCode::NOT_FOUND,
                br#"{"error":"blob-not-found"}"#.to_vec(),
            )
        }
    }
    let app = Router::new()
        .route("/internal/moderation/account", post(account))
        .route("/internal/moderation/appeals", get(appeals))
        .route("/internal/moderation/statement", post(statement))
        .route("/internal/moderation/blob/{hex}", get(blob))
        .with_state((fake, account_answer));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}")
}

#[tokio::test]
async fn signing_in_needs_a_fresh_challenge_and_a_current_moderator() {
    let h = harness(Forwarders::none(), None).await;
    let (status, _) = send(&h.app, get_req("/panel/api/me", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&h.app, get_req("/panel/api/me", Some("p1.x.1.y"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (_, start) = send(
        &h.app,
        post_req("/panel/api/session/start", None, &json!({})),
    )
    .await;
    assert_eq!(start["rp_id"], "moderation.example");
    // The revoked moderator is not offered.
    assert_eq!(start["credentials"].as_array().unwrap().len(), 2);
    let challenge: [u8; 32] = B64
        .decode(start["challenge_b64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let finish = |id: [u8; 16], seed: &[u8; 32]| {
        let a = auth_key(seed).assert(&moderator_list(), &challenge);
        json!({
            "moderator_id_b64": B64.encode(id),
            "authenticator_data_b64": B64.encode(&a.authenticator_data),
            "client_data_json_b64": B64.encode(&a.client_data_json),
            "signature_b64": B64.encode(&a.signature),
        })
    };
    // Bob's key under Alice's id: refused, and the challenge is spent.
    let (status, v) = send(
        &h.app,
        post_req("/panel/api/session/finish", None, &finish(ID_A, &SEED_B)),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("bad-assertion"))
    );
    let (status, v) = send(
        &h.app,
        post_req("/panel/api/session/finish", None, &finish(ID_A, &SEED_A)),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("unknown-challenge"))
    );
    // A revoked moderator cannot sign in.
    let (_, start) = send(
        &h.app,
        post_req("/panel/api/session/start", None, &json!({})),
    )
    .await;
    let challenge: [u8; 32] = B64
        .decode(start["challenge_b64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let a = auth_key(&[23; 32]).assert(&moderator_list(), &challenge);
    let (status, v) = send(
        &h.app,
        post_req(
            "/panel/api/session/finish",
            None,
            &json!({
                "moderator_id_b64": B64.encode(ID_REVOKED),
                "authenticator_data_b64": B64.encode(&a.authenticator_data),
                "client_data_json_b64": B64.encode(&a.client_data_json),
                "signature_b64": B64.encode(&a.signature),
            }),
        ),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("not-a-moderator"))
    );

    let token = sign_in(&h.app, ID_A, &SEED_A).await;
    let (status, me) = send(&h.app, get_req("/panel/api/me", Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["name"], "alice");
    assert_eq!(me["moderator_id_b64"], B64.encode(ID_A));
    let (status, mods) = send(&h.app, get_req("/panel/api/moderators", Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mods["moderators"].as_array().unwrap().len(), 3);
    assert_eq!(mods["moderators"][2]["revoked_at_ms"], 5);
    assert!(mods["list_b64"].as_str().is_some());
    // Unknown API paths are JSON 404s, never the app.
    let (status, v) = send(&h.app, get_req("/panel/api/nope", Some(&token))).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("not-found"))
    );
}

#[tokio::test]
async fn the_queue_orders_checked_cases_first_and_filters() {
    let h = harness(Forwarders::none(), None).await;
    let token = sign_in(&h.app, ID_A, &SEED_A).await;
    let account = account_target();
    let channel = vec![2u8; 16];
    report(
        &h.store,
        1,
        TargetKind::Account,
        &account,
        Reason::Spam,
        100,
    )
    .await;
    report(
        &h.store,
        2,
        TargetKind::Account,
        &account,
        Reason::Spam,
        101,
    )
    .await;
    report(
        &h.store,
        3,
        TargetKind::Account,
        &account,
        Reason::Harassment,
        102,
    )
    .await;
    report(
        &h.store,
        4,
        TargetKind::Channel,
        &channel,
        Reason::Hate,
        103,
    )
    .await;

    let (_, q) = send(&h.app, get_req("/panel/api/cases", Some(&token))).await;
    let cases = q["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0]["kind"], "account");
    assert_eq!(cases[0]["reasons"], json!({ "spam": 2, "harassment": 1 }));
    assert_eq!(cases[0]["first_day"], "1970-04-11");
    assert_eq!(cases[0]["status"], "new");

    // A checked message moves the channel first.
    let (status, _) = send(
        &h.app,
        post_req(
            &format!("/panel/api/reports/{}/checks", hex(&[4; 16])),
            Some(&token),
            &json!({ "checked": 1, "unchecked": 0 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &h.app,
        post_req(
            &format!("/panel/api/reports/{}/checks", hex(&[9; 16])),
            Some(&token),
            &json!({ "checked": 1, "unchecked": 0 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, q) = send(&h.app, get_req("/panel/api/cases", Some(&token))).await;
    assert_eq!(q["cases"][0]["kind"], "channel");
    assert_eq!(q["cases"][0]["checked_messages_reports"], 1);

    let (_, q) = send(
        &h.app,
        get_req("/panel/api/cases?reason=harassment", Some(&token)),
    )
    .await;
    assert_eq!(q["cases"].as_array().unwrap().len(), 1);
    let (_, q) = send(
        &h.app,
        get_req("/panel/api/cases?kind=channel&checked=false", Some(&token)),
    )
    .await;
    assert_eq!(q["cases"].as_array().unwrap().len(), 0);
    let (status, _) = send(
        &h.app,
        get_req("/panel/api/cases?status=bogus", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_case_is_locked_by_one_moderator_until_idle() {
    let h = harness(Forwarders::none(), None).await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let bob = sign_in(&h.app, ID_B, &SEED_B).await;
    let account = account_target();
    report(
        &h.store,
        1,
        TargetKind::Account,
        &account,
        Reason::Spam,
        100,
    )
    .await;
    let path = format!("/panel/api/cases/account/{}", hex(&account));

    let (status, v) = send(
        &h.app,
        post_req(&format!("{path}/lock"), Some(&alice), &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["lock"]["name"], "alice");
    let (status, v) = send(
        &h.app,
        post_req(&format!("{path}/lock"), Some(&bob), &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(v["lock"]["name"], "alice");
    let (_, q) = send(
        &h.app,
        get_req("/panel/api/cases?status=in-review", Some(&bob)),
    )
    .await;
    assert_eq!(q["cases"][0]["lock"]["name"], "alice");

    // Bob cannot release Alice's lock; Alice can.
    let (_, v) = send(
        &h.app,
        post_req(&format!("{path}/unlock"), Some(&bob), &json!({})),
    )
    .await;
    assert_eq!(v["unlocked"], false);
    let (_, v) = send(
        &h.app,
        post_req(&format!("{path}/unlock"), Some(&alice), &json!({})),
    )
    .await;
    assert_eq!(v["unlocked"], true);
    let (_, c) = send(&h.app, get_req(&path, Some(&bob))).await;
    assert_eq!(c["case"]["status"], "new");

    // An idle lock is taken over.
    let old = now_ms() - LOCK_IDLE_MS - 1;
    h.store.lock_case(1, &account, &ID_A, old, 0).await.unwrap();
    let (status, v) = send(
        &h.app,
        post_req(&format!("{path}/lock"), Some(&bob), &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["lock"]["name"], "bob");

    let (status, _) = send(
        &h.app,
        post_req(
            &format!("/panel/api/cases/account/{}/lock", hex(&[1; 32])),
            Some(&bob),
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&h.app, get_req("/panel/api/cases/account/abcd", Some(&bob))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deciding_logs_forwards_and_closes_the_case() {
    let fake = Fake::default();
    let url = fake_service(
        fake.clone(),
        json!({ "exists": true, "age_band": "week", "suspended": false, "until_ms": null }),
    )
    .await;
    let forwarders =
        Forwarders::none().with(Forwarder::new(ApplyingService::Auth, &url, "c2VjcmV0").unwrap());
    let h = harness(forwarders, None).await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let account = account_target();
    report(
        &h.store,
        1,
        TargetKind::Account,
        &account,
        Reason::Spam,
        100,
    )
    .await;
    report(
        &h.store,
        2,
        TargetKind::Account,
        &account,
        Reason::Spam,
        101,
    )
    .await;
    let path = format!("/panel/api/cases/account/{}", hex(&account));
    let (_, _) = send(
        &h.app,
        post_req(&format!("{path}/lock"), Some(&alice), &json!({})),
    )
    .await;

    // A statement Bob signed cannot be decided in Alice's session.
    let by_bob = signed(&statement(1, Action::Suspend, ID_B, now_ms()), &SEED_B);
    let (status, v) = send(
        &h.app,
        post_req(
            "/panel/api/decide",
            Some(&alice),
            &decide_body(&by_bob, None),
        ),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("not-your-statement"))
    );
    assert_eq!(h.log.size().await, 0);
    let (status, v) = send(
        &h.app,
        post_req(
            "/panel/api/decide",
            Some(&alice),
            &json!({"signed_statement_b64": "AA==", "reason": 1, "reason_salt_b64": ""}),
        ),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad-statement"))
    );

    let suspend = signed(&statement(2, Action::Suspend, ID_A, now_ms()), &SEED_A);
    let (status, v) = send(
        &h.app,
        post_req(
            "/panel/api/decide",
            Some(&alice),
            &decide_body(&suspend, Some(" repeat spammer ")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["forwarded"], true);
    assert_eq!(v["applied"], true);
    assert_eq!(v["reports_closed"], 2);
    let log_index = v["log_index"].as_u64().unwrap();
    assert_eq!(
        h.store.status(&[1; 16]).await.unwrap(),
        Some(ReportStatus::Action)
    );
    assert_eq!(
        fake.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k == "statement")
            .count(),
        1
    );

    // A resend forwards again and records nothing twice.
    let (status, v) = send(
        &h.app,
        post_req(
            "/panel/api/decide",
            Some(&alice),
            &decide_body(&suspend, None),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["log_index"].as_u64(), Some(log_index));
    assert_eq!(v["reports_closed"], 0);

    let (_, c) = send(&h.app, get_req(&path, Some(&alice))).await;
    assert_eq!(c["case"]["status"], "decided");
    assert!(c["case"]["lock"].is_null());
    assert_eq!(c["reports"].as_array().unwrap().len(), 2);
    assert_eq!(c["reports"][0]["status"], "action");
    let history = c["history"].as_array().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["statement"]["action"], "suspend");
    assert_eq!(history[0]["statement"]["moderator_name"], "alice");
    assert_eq!(history[0]["reason"], "spam");
    assert_eq!(history[0]["note"]["text"], "repeat spammer");
    assert_eq!(c["account"]["auth"]["age_band"], "week");
    assert!(c["account"]["discovery"].is_null());
    assert!(fake
        .seen
        .lock()
        .unwrap()
        .iter()
        .any(|(k, v)| k == "account" && v["target_b64"] == B64.encode(&account)));

    // A new report reopens it.
    report(
        &h.store,
        3,
        TargetKind::Account,
        &account,
        Reason::Hate,
        102,
    )
    .await;
    let (_, c) = send(&h.app, get_req(&path, Some(&alice))).await;
    assert_eq!(c["case"]["status"], "new");

    // No action closes the open report as none and leaves no log entry.
    let (status, v) = send(
        &h.app,
        post_req(
            &format!("{path}/no-action"),
            Some(&alice),
            &json!({ "reason": 3, "note": " quoted a film " }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["reports_closed"], 1);
    assert_eq!(
        h.store.status(&[3; 16]).await.unwrap(),
        Some(ReportStatus::NoAction)
    );
    assert_eq!(h.log.size().await, 1);
    let (_, c) = send(&h.app, get_req(&path, Some(&alice))).await;
    assert_eq!(c["history"][1]["type"], "no-action");
    assert_eq!(c["history"][1]["reason"], "hate");
    assert_eq!(c["history"][1]["note"]["text"], "quoted a film");
    assert_eq!(c["history"][1]["note"]["moderator_name"], "alice");

    // A lift is logged but closes nothing.
    report(
        &h.store,
        4,
        TargetKind::Account,
        &account,
        Reason::Spam,
        103,
    )
    .await;
    let lift = signed(&statement(3, Action::Lift, ID_A, now_ms() + 1), &SEED_A);
    let (status, v) = send(
        &h.app,
        post_req("/panel/api/decide", Some(&alice), &decide_body(&lift, None)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["reports_closed"], 0);
    assert_eq!(
        h.store.status(&[4; 16]).await.unwrap(),
        Some(ReportStatus::Received)
    );

    // The log, newest first, filtered by target.
    let (_, l) = send(
        &h.app,
        get_req(
            &format!("/panel/api/log?target_hex={}", hex(&account)),
            Some(&alice),
        ),
    )
    .await;
    let entries = l["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["statement"]["action"], "lift");
    assert!(l["next_before"].is_null());
    let (_, l) = send(&h.app, get_req("/panel/api/log?before=1", Some(&alice))).await;
    assert_eq!(l["entries"].as_array().unwrap().len(), 1);
    assert_eq!(l["entries"][0]["index"], 0);
}

#[tokio::test]
async fn a_take_back_closes_the_account_case_discovery_names() {
    let name_key = vec![1u8; 64];
    let fake = Fake::default();
    let url = fake_service(
        fake.clone(),
        json!({ "name_lookup_keys_b64": [B64.encode(&name_key)] }),
    )
    .await;
    let h = harness(
        Forwarders::none()
            .with(Forwarder::new(ApplyingService::Discovery, &url, "c2VjcmV0").unwrap()),
        None,
    )
    .await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let account = account_target();
    report(
        &h.store,
        1,
        TargetKind::Account,
        &account,
        Reason::Impersonation,
        100,
    )
    .await;
    let take_back = signed(&statement(4, Action::TakeBackName, ID_A, now_ms()), &SEED_A);
    let mut body = decide_body(&take_back, None);
    body["case"] = json!({ "kind": "account", "target_hex": hex(&account) });
    let (status, v) = send(&h.app, post_req("/panel/api/decide", Some(&alice), &body)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["forwarded"], true);
    assert_eq!(v["reports_closed"], 1);
    let (_, c) = send(
        &h.app,
        get_req(
            &format!("/panel/api/cases/account/{}", hex(&account)),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(c["history"][0]["statement"]["action"], "take-back-name");
    assert_eq!(c["history"][0]["statement"]["target_kind"], "name");
    // A resend still closes that case, though discovery no longer lists the
    // name: the case already holds the statement.
    let reads = fake.seen.lock().unwrap().len();
    let (status, v) = send(&h.app, post_req("/panel/api/decide", Some(&alice), &body)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(fake.seen.lock().unwrap().len(), reads + 1, "no second read");
}

#[tokio::test]
async fn a_decision_cannot_close_a_case_it_is_not_about() {
    let fake = Fake::default();
    // Discovery lists no name for the account.
    let url = fake_service(fake.clone(), json!({ "name_lookup_keys_b64": [] })).await;
    let h = harness(
        Forwarders::none()
            .with(Forwarder::new(ApplyingService::Discovery, &url, "c2VjcmV0").unwrap()),
        None,
    )
    .await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let account = account_target();
    let other = vec![8u8; 32];
    report(&h.store, 1, TargetKind::Account, &other, Reason::Spam, 100).await;
    let mismatch = |status: StatusCode, v: &Value| {
        assert_eq!(
            (status, v["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("case-mismatch")),
            "{v}"
        );
    };
    // A take-back whose name the account does not hold.
    let take_back = signed(&statement(4, Action::TakeBackName, ID_A, now_ms()), &SEED_A);
    let mut body = decide_body(&take_back, None);
    body["case"] = json!({ "kind": "account", "target_hex": hex(&account) });
    let (status, v) = send(&h.app, post_req("/panel/api/decide", Some(&alice), &body)).await;
    mismatch(status, &v);
    // A take-back naming a case of another kind.
    body["case"] = json!({ "kind": "channel", "target_hex": hex(&[1u8; 16]) });
    let (status, v) = send(&h.app, post_req("/panel/api/decide", Some(&alice), &body)).await;
    mismatch(status, &v);
    // A suspension of one account naming another account's case.
    let suspend = signed(&statement(5, Action::Suspend, ID_A, now_ms()), &SEED_A);
    let mut body = decide_body(&suspend, None);
    body["case"] = json!({ "kind": "account", "target_hex": hex(&other) });
    let (status, v) = send(&h.app, post_req("/panel/api/decide", Some(&alice), &body)).await;
    mismatch(status, &v);
    // A blob delete closes no case, and names none but its own.
    let blob = signed(&statement(6, Action::BlobDelete, ID_A, now_ms()), &SEED_A);
    let mut body = decide_body(&blob, None);
    body["case"] = json!({ "kind": "account", "target_hex": hex(&other) });
    let (status, v) = send(&h.app, post_req("/panel/api/decide", Some(&alice), &body)).await;
    mismatch(status, &v);
    // Nothing of these was logged, and the other case is still open.
    assert_eq!(h.log.size().await, 0);
    assert_eq!(
        h.store.status(&[1; 16]).await.unwrap(),
        Some(ReportStatus::Received)
    );
    // Its own target: logged, closes nothing.
    let (status, v) = send(
        &h.app,
        post_req("/panel/api/decide", Some(&alice), &decide_body(&blob, None)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["reports_closed"], 0);
    assert_eq!(h.log.size().await, 1);

    // Without discovery a take-back into an account case cannot be checked.
    let h = harness(Forwarders::none(), None).await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let mut body = decide_body(&take_back, None);
    body["case"] = json!({ "kind": "account", "target_hex": hex(&account) });
    let (status, v) = send(&h.app, post_req("/panel/api/decide", Some(&alice), &body)).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("discovery-unavailable")
        )
    );
    assert_eq!(h.log.size().await, 0);
}

#[tokio::test]
async fn appeals_show_the_decision_and_count_in_the_numbers() {
    let fake = Fake::default();
    let url = fake_service(fake.clone(), json!({})).await;
    let forwarders =
        Forwarders::none().with(Forwarder::new(ApplyingService::Auth, &url, "c2VjcmV0").unwrap());
    let h = harness(forwarders, None).await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let bob = sign_in(&h.app, ID_B, &SEED_B).await;
    let account = account_target();
    report(
        &h.store,
        1,
        TargetKind::Account,
        &account,
        Reason::Spam,
        100,
    )
    .await;
    let now = now_ms();
    let suspend = signed(&statement(2, Action::Suspend, ID_A, now), &SEED_A);
    let (_, v) = send(
        &h.app,
        post_req(
            "/panel/api/decide",
            Some(&alice),
            &decide_body(&suspend, None),
        ),
    )
    .await;
    let log_index = v["log_index"].as_u64().unwrap();
    *fake.appeals.lock().unwrap() = json!({ "appeals": [{
        "target_b64": B64.encode(&account),
        "sealed_b64": B64.encode([1, 2, 3]),
        "statement_id_b64": B64.encode([2u8; 16]),
        "log_index": log_index,
        "until_ms": null,
        "appeal_at_ms": now,
    }]});

    let (status, a) = send(&h.app, get_req("/panel/api/appeals", Some(&alice))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(a["available"], true);
    let first = &a["appeals"][0];
    assert_eq!(first["decision"]["action"], "suspend");
    assert_eq!(first["decided_by_you"], true);
    assert_eq!(first["case"]["status"], "decided");
    let (_, a) = send(&h.app, get_req("/panel/api/appeals", Some(&bob))).await;
    assert_eq!(a["appeals"][0]["decided_by_you"], false);

    // Keeping the suspension is a signed statement through decide; it
    // declines the appeal and closes no report.
    report(
        &h.store,
        2,
        TargetKind::Account,
        &account,
        Reason::Spam,
        101,
    )
    .await;
    let keep = signed(
        &statement(3, Action::KeepSuspension, ID_B, now + 1),
        &SEED_B,
    );
    let (status, v) = send(
        &h.app,
        post_req("/panel/api/decide", Some(&bob), &decide_body(&keep, None)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["applied"], true);
    assert_eq!(v["reports_closed"], 0);
    assert_eq!(
        h.store.status(&[2; 16]).await.unwrap(),
        Some(ReportStatus::Received)
    );
    // A second keep: auth has no pending appeal, and says so.
    fake.answers
        .lock()
        .unwrap()
        .push(json!({ "applied": false, "no_pending_appeal": true }));
    let keep_again = signed(
        &statement(4, Action::KeepSuspension, ID_B, now + 2),
        &SEED_B,
    );
    let (status, v) = send(
        &h.app,
        post_req(
            "/panel/api/decide",
            Some(&bob),
            &decide_body(&keep_again, None),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["applied"], false);
    assert_eq!(v["no_pending_appeal"], true);
    // The decline route is gone.
    let (status, v) = send(
        &h.app,
        post_req("/panel/api/appeals/decline", Some(&bob), &json!({})),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("not-found"))
    );

    let (status, n) = send(&h.app, get_req("/panel/api/numbers?weeks=2", Some(&alice))).await;
    assert_eq!(status, StatusCode::OK);
    let weeks = n["weeks"].as_array().unwrap();
    assert_eq!(weeks.len(), 2);
    assert_eq!(weeks[0]["decisions"]["suspend"], 1);
    assert_eq!(weeks[0]["decisions"]["keep-suspension"], 2);
    assert_eq!(
        weeks[0]["appeals"],
        json!({ "sent": 1, "declined": 1, "lifted": 0 })
    );
    // The report arrived on day 100, long before these weeks.
    assert_eq!(weeks[0]["reports"], json!({}));
    assert!(weeks[0]["week"].as_str().unwrap().contains("-W"));
}

#[tokio::test]
async fn without_auth_the_appeals_are_unavailable() {
    let h = harness(Forwarders::none(), None).await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let (status, a) = send(&h.app, get_req("/panel/api/appeals", Some(&alice))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(a["available"], false);
}

#[tokio::test]
async fn the_app_is_served_with_its_index_as_the_fallback() {
    let dir = std::env::temp_dir().join(format!("lakko-panel-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><title>panel</title>",
    )
    .unwrap();
    std::fs::write(dir.join("assets/app.js"), "console.log(1)").unwrap();
    let h = harness(Forwarders::none(), Some(dir.clone())).await;
    for (path, needle) in [
        ("/", "panel"),
        ("/cases/x", "panel"),
        ("/assets/app.js", "console"),
    ] {
        let resp = h
            .app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains(needle), "{path}");
    }
    let resp = h
        .app
        .clone()
        .oneshot(Request::get("/panel/api/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn evidence_files_are_proxied_from_storage() {
    let url = fake_service(Fake::default(), json!({})).await;
    let h = harness(
        Forwarders::none()
            .with(Forwarder::new(ApplyingService::Storage, &url, "c2VjcmV0").unwrap()),
        None,
    )
    .await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let good = "ab".repeat(32);
    let (status, _) = send(&h.app, get_req(&format!("/panel/api/blob/{good}"), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let resp = h
        .app
        .clone()
        .oneshot(get_req(
            &format!("/panel/api/blob/{}", good.to_uppercase()),
            Some(&alice),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/octet-stream"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ciphertext");
    let (status, v) = send(
        &h.app,
        get_req(
            &format!("/panel/api/blob/{}", "cd".repeat(32)),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("blob-not-found"))
    );
    let (status, v) = send(&h.app, get_req("/panel/api/blob/abcd", Some(&alice))).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad-blob-id"))
    );

    // Storage not configured.
    let h = harness(Forwarders::none(), None).await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let (status, v) = send(
        &h.app,
        get_req(&format!("/panel/api/blob/{good}"), Some(&alice)),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("storage-unavailable"))
    );
    // Storage unreachable.
    let h = harness(
        Forwarders::none().with(
            Forwarder::new(ApplyingService::Storage, "http://127.0.0.1:1", "c2VjcmV0").unwrap(),
        ),
        None,
    )
    .await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let (status, v) = send(
        &h.app,
        get_req(&format!("/panel/api/blob/{good}"), Some(&alice)),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("storage-failed"))
    );
}

#[tokio::test]
async fn a_blob_read_stops_at_its_size_cap() {
    let url = fake_service(Fake::default(), json!({})).await;
    let storage = Forwarder::new(ApplyingService::Storage, &url, "c2VjcmV0").unwrap();
    let path = format!("/internal/moderation/blob/{}", "ab".repeat(32));
    let timeout = std::time::Duration::from_secs(5);
    assert_eq!(
        storage.get_bytes(&path, 10, timeout).await,
        Some((200, b"ciphertext".to_vec()))
    );
    assert_eq!(storage.get_bytes(&path, 9, timeout).await, None);
}

#[tokio::test]
async fn evidence_reads_are_capped_in_flight() {
    let url = fake_service(Fake::default(), json!({})).await;
    let h = harness(
        Forwarders::none()
            .with(Forwarder::new(ApplyingService::Storage, &url, "c2VjcmV0").unwrap()),
        None,
    )
    .await;
    let alice = sign_in(&h.app, ID_A, &SEED_A).await;
    let path = format!("/panel/api/blob/{}", "ab".repeat(32));
    // Every slot taken: the next read is refused at once.
    let held = h
        .slots
        .clone()
        .acquire_many_owned(u32::try_from(crate::panel::MAX_BLOB_READS).unwrap())
        .await
        .unwrap();
    let (status, v) = send(&h.app, get_req(&path, Some(&alice))).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("busy"))
    );
    drop(held);
    let (status, _) = send(&h.app, get_req(&path, Some(&alice))).await;
    assert_eq!(status, StatusCode::OK);
}
