// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tests of ADR-0099 step 5 at storage: the moderator's BlobDelete through
//! the internal route, over the file index (no database; the SQL index meets
//! the same contract in `index::sql`), and what the public routes answer
//! afterwards.

use super::*;
use crate::blob::DELETE_KEY_HEADER;
use crate::build_app;
use crate::pass::PASS_HEADER;
use crate::test_support::{master_hex, state};
use axum::body::Body;
use axum::http::{header, Request};
use http_body_util::BodyExt as _;
use lakko_anon_token::{MasterSecret, TokenContext, DEFAULT_WINDOW_SECS};
use lakko_anon_token_server::{current_epoch, testing::mint_header, TOKEN_HEADER};
use lakko_crypto::signing::SigningKey;
use lakko_moderation::moderators::{ModeratorEntry, ModeratorList};
use lakko_moderation::software_authenticator::SoftwareAuthenticator;
use lakko_moderation::statement::{reason_commitment, Action, Statement};
use lakko_moderation::target::Target;
use lakko_moderation::Reason;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tower::ServiceExt as _;

const ROOT_SEED: [u8; 32] = [0x42; 32];
const MODERATOR_SEED: [u8; 32] = [0x17; 32];
const SECRET: [u8; 32] = [0x5E; 32];

fn config() -> InternalConfig {
    InternalConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        secret: Arc::new(SECRET.to_vec()),
        root_key: SigningKey::from_seed(&ROOT_SEED).verifying_key().to_bytes(),
    }
}

fn list(version: u64) -> ModeratorList {
    ModeratorList {
        list_version: version,
        issued_at_ms: 1,
        rp_id: "moderation.example".into(),
        origin: "https://moderation.example".into(),
        entries: vec![ModeratorEntry {
            moderator_id: [1; 16],
            name: "m".into(),
            key: SoftwareAuthenticator::from_seed(&MODERATOR_SEED).moderator_key(),
            credential_id: vec![1],
            x25519_public: [9; 32],
            added_at_ms: 0,
            revoked_at_ms: 0,
        }],
    }
}

fn body(action: Action, target: &[u8], statement_id: u8, version: u64) -> Value {
    let list = list(version);
    let salt = [statement_id; 16];
    let statement = Statement {
        statement_id: [statement_id; 16],
        action,
        target: Target::new(action.target_kind(), target).unwrap(),
        reason_commitment: reason_commitment(Reason::ChildSafety, &salt),
        until_ms: 0,
        // Within the 24 h a statement stays fresh (ADR-0099 F1).
        issued_at_ms: now_ms(),
        moderator_id: [1; 16],
        list_version: version,
    };
    let signed = SoftwareAuthenticator::from_seed(&MODERATOR_SEED)
        .sign_statement(&list, &statement)
        .unwrap()
        .encode()
        .unwrap();
    serde_json::to_value(StatementBody::new(
        &signed,
        &list.sign(&ROOT_SEED).unwrap(),
        5,
        Reason::ChildSafety,
        &salt,
    ))
    .unwrap()
}

async fn send(app: axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn statement(
    state: &AppState,
    secret: Option<[u8; 32]>,
    body: &Value,
) -> (StatusCode, Value) {
    let mut b = Request::post("/internal/moderation/statement")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(s) = secret {
        b = b.header(SECRET_HEADER, STANDARD.encode(s));
    }
    send(
        internal_router(state.clone(), config()),
        b.body(Body::from(body.to_string())).unwrap(),
    )
    .await
}

fn token(context: TokenContext) -> String {
    let master = MasterSecret::from_hex(&master_hex()).unwrap();
    mint_header(&master, context, current_epoch(DEFAULT_WINDOW_SECS))
}

async fn pass(app: &axum::Router, kind: &str, context: TokenContext) -> String {
    let (status, v) = send(
        app.clone(),
        Request::post(format!("/blob/pass/{kind}"))
            .header(TOKEN_HEADER, token(context))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    v["pass"].as_str().unwrap().to_owned()
}

async fn upload(app: &axum::Router, upload_pass: &str, bytes: &[u8]) -> (StatusCode, Value) {
    let hex = hex::encode(Sha256::digest(bytes));
    send(
        app.clone(),
        Request::put(format!("/blob/{hex}"))
            .header(header::CONTENT_LENGTH, bytes.len())
            .header(
                DELETE_KEY_HEADER,
                URL_SAFE_NO_PAD.encode(lakko_vault::proof::public_key(&[0x21; 32])),
            )
            .header(PASS_HEADER, upload_pass)
            .body(Body::from(bytes.to_vec()))
            .unwrap(),
    )
    .await
}

#[test]
fn the_internal_config_needs_both_secret_and_root() {
    let secret = STANDARD.encode([7u8; 32]);
    let root = URL_SAFE_NO_PAD.encode([9u8; 32]);
    assert!(parse_internal(None, None, None).unwrap().is_none());
    assert!(parse_internal(None, Some(&secret), None).unwrap().is_none());
    assert!(parse_internal(None, None, Some(&root)).unwrap().is_none());
    let cfg = parse_internal(None, Some(&secret), Some(&root))
        .unwrap()
        .unwrap();
    assert_eq!(cfg.bind.to_string(), DEFAULT_INTERNAL_BIND);
    assert!(parse_internal(None, Some(&STANDARD.encode([1u8; 8])), Some(&root)).is_err());
    assert!(parse_internal(None, Some(&secret), Some("short")).is_err());
}

#[tokio::test]
async fn a_moderator_deletes_a_blob_and_it_cannot_come_back() {
    let (state, _tmp) = state();
    let app = build_app(state.clone());
    let bytes = b"evidence named in a report";
    let id: [u8; 32] = Sha256::digest(bytes).into();
    let hex = hex::encode(id);
    let up = pass(&app, "upload", TokenContext::BlobUpload).await;
    assert_eq!(upload(&app, &up, bytes).await.0, StatusCode::CREATED);
    let path = crate::blob::blob_path(&state.blob_root, &id);
    assert!(path.exists());

    let delete = body(Action::BlobDelete, &id, 1, 3);
    // The secret is required.
    assert_eq!(
        statement(&state, None, &delete).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        statement(&state, Some([0x11; 32]), &delete).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        statement(&state, Some(SECRET), &delete).await,
        (StatusCode::OK, json!({ "applied": true }))
    );
    assert!(!path.exists());
    assert_eq!(
        statement(&state, Some(SECRET), &delete).await,
        (StatusCode::OK, json!({ "applied": false, "repeat": true }))
    );
    assert_eq!(
        statement(&state, Some(SECRET), &body(Action::BlobDelete, &id, 2, 3)).await,
        (
            StatusCode::OK,
            json!({ "applied": false, "already_removed": true })
        )
    );

    // A read is a gone blob; an upload of the id is refused.
    let read = pass(&app, "read", TokenContext::BlobRead).await;
    let (status, v) = send(
        app.clone(),
        Request::get(format!("/blob/{hex}"))
            .header(PASS_HEADER, &read)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("blob-not-found"))
    );
    let (status, v) = upload(&app, &up, bytes).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::GONE, Some("blob-removed"))
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn statements_storage_does_not_apply_are_refused() {
    let (state, _tmp) = state();
    let (status, v) = statement(
        &state,
        Some(SECRET),
        &body(Action::ChannelRemove, &[3; 16], 1, 3),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("wrong-service"))
    );
    let mut wrong = body(Action::BlobDelete, &[4; 32], 2, 3);
    wrong["reason"] = json!(Reason::Spam.as_u8());
    let (status, v) = statement(&state, Some(SECRET), &wrong).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("reason-mismatch"))
    );
    // A list older than one already applied.
    assert_eq!(
        statement(
            &state,
            Some(SECRET),
            &body(Action::BlobDelete, &[5; 32], 3, 3)
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, v) = statement(
        &state,
        Some(SECRET),
        &body(Action::BlobDelete, &[6; 32], 4, 2),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::CONFLICT, Some("stale-list"))
    );
}

async fn internal_get(
    state: &AppState,
    path: &str,
    secret: Option<[u8; 32]>,
) -> (StatusCode, Vec<u8>) {
    let mut b = Request::get(path);
    if let Some(s) = secret {
        b = b.header(SECRET_HEADER, STANDARD.encode(s));
    }
    let resp = internal_router(state.clone(), config())
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

#[tokio::test]
async fn the_panel_reads_a_blob_through_the_internal_listener() {
    let (state, _tmp) = state();
    let app = build_app(state.clone());
    let bytes = b"sealed evidence attachment";
    let id: [u8; 32] = Sha256::digest(bytes).into();
    let path = format!("/internal/moderation/blob/{}", hex::encode(id));
    let up = pass(&app, "upload", TokenContext::BlobUpload).await;
    assert_eq!(upload(&app, &up, bytes).await.0, StatusCode::CREATED);
    let before = state.index.get(&id).await.unwrap().unwrap();

    // The secret is required.
    assert_eq!(
        internal_get(&state, &path, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        internal_get(&state, &path, Some([0x11; 32])).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, got) = internal_get(&state, &path, Some(SECRET)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got, bytes.to_vec());
    // Not an access: the row is unchanged.
    assert_eq!(state.index.get(&id).await.unwrap().unwrap(), before);

    let (status, v) = internal_get(
        &state,
        &format!("/internal/moderation/blob/{}", hex::encode([7u8; 32])),
        Some(SECRET),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        serde_json::from_slice::<Value>(&v).unwrap()["error"],
        "blob-not-found"
    );
    let (status, _) = internal_get(&state, "/internal/moderation/blob/ABCD", Some(SECRET)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // After a moderator's delete it is gone here too.
    assert_eq!(
        statement(&state, Some(SECRET), &body(Action::BlobDelete, &id, 1, 3))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        internal_get(&state, &path, Some(SECRET)).await.0,
        StatusCode::NOT_FOUND
    );
}

async fn push_list(
    state: &AppState,
    secret: Option<[u8; 32]>,
    body: &Value,
) -> (StatusCode, Value) {
    let mut b =
        Request::post("/internal/moderation/list").header(header::CONTENT_TYPE, "application/json");
    if let Some(s) = secret {
        b = b.header(SECRET_HEADER, STANDARD.encode(s));
    }
    send(
        internal_router(state.clone(), config()),
        b.body(Body::from(body.to_string())).unwrap(),
    )
    .await
}

fn list_body(version: u64, root_seed: &[u8; 32]) -> Value {
    serde_json::to_value(ListBody::new(&list(version).sign(root_seed).unwrap())).unwrap()
}

#[tokio::test]
async fn the_list_floor_rises_by_push_and_by_a_refused_statement() {
    let (state, _tmp) = state();
    assert_eq!(
        push_list(&state, None, &list_body(3, &ROOT_SEED)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        push_list(&state, Some(SECRET), &list_body(3, &ROOT_SEED)).await,
        (StatusCode::OK, json!({ "list_version": 3 }))
    );
    let (status, v) = push_list(&state, Some(SECRET), &list_body(4, &[0x01; 32])).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad-list"))
    );
    let (status, v) = push_list(&state, Some(SECRET), &list_body(2, &ROOT_SEED)).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::CONFLICT, Some("stale-list"))
    );
    // A statement under a newer list that is itself refused still raises
    // the floor to that list.
    let mut wrong = body(Action::BlobDelete, &[8; 32], 1, 5);
    wrong["reason"] = json!(Reason::Spam.as_u8());
    let (status, v) = statement(&state, Some(SECRET), &wrong).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("reason-mismatch"))
    );
    let (status, v) = statement(
        &state,
        Some(SECRET),
        &body(Action::BlobDelete, &[8; 32], 2, 4),
    )
    .await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::CONFLICT, Some("stale-list"))
    );
    // A statement from the future is refused after the floor.
    let mut future = body(Action::BlobDelete, &[8; 32], 3, 5);
    let ahead = {
        let l = list(5);
        let salt = [3u8; 16];
        let st = Statement {
            statement_id: [3; 16],
            action: Action::BlobDelete,
            target: Target::new(Action::BlobDelete.target_kind(), &[8; 32]).unwrap(),
            reason_commitment: reason_commitment(Reason::ChildSafety, &salt),
            until_ms: 0,
            issued_at_ms: now_ms() + 10 * 60_000,
            moderator_id: [1; 16],
            list_version: 5,
        };
        SoftwareAuthenticator::from_seed(&MODERATOR_SEED)
            .sign_statement(&l, &st)
            .unwrap()
            .encode()
            .unwrap()
    };
    future["signed_statement_b64"] = json!(STANDARD.encode(ahead));
    let (status, v) = statement(&state, Some(SECRET), &future).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("future-statement"))
    );
}
