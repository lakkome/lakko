// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests of the blob surface over the file index (no database).

use std::path::{Path, PathBuf};

use super::*;
use crate::build_app;
use crate::pass::PassSecret;
use crate::test_support::{master_hex, state, state_with};
use axum::http::Request;
use http_body_util::BodyExt as _;
use lakko_anon_token::{MasterSecret, DEFAULT_WINDOW_SECS};
use lakko_anon_token_server::{current_epoch, testing::mint_header, TOKEN_HEADER};
use tower::ServiceExt as _;

const DELETE_SEED: [u8; 32] = [0x21; 32];

fn sha256(bytes: &[u8]) -> (BlobId, String) {
    let d: [u8; 32] = Sha256::digest(bytes).into();
    (d, hex::encode(d))
}

fn token(context: TokenContext) -> String {
    let master = MasterSecret::from_hex(&master_hex()).unwrap();
    mint_header(&master, context, current_epoch(DEFAULT_WINDOW_SECS))
}

fn delete_key_header() -> String {
    URL_SAFE_NO_PAD.encode(lakko_vault::proof::public_key(&DELETE_SEED))
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Vec<u8>, HeaderMap) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, body.to_vec(), headers)
}

fn code(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

async fn take(app: &axum::Router, kind: &str, context: TokenContext) -> String {
    let (status, body, _) = send(
        app,
        Request::post(format!("/blob/pass/{kind}"))
            .header(TOKEN_HEADER, token(context))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["kind"], kind);
    v["pass"].as_str().unwrap().to_owned()
}

fn put_req(hex: &str, pass: Option<&str>, body: &[u8]) -> Request<Body> {
    let mut b = Request::put(format!("/blob/{hex}"))
        .header(header::CONTENT_LENGTH, body.len())
        .header(DELETE_KEY_HEADER, delete_key_header());
    if let Some(p) = pass {
        b = b.header(PASS_HEADER, p);
    }
    b.body(Body::from(body.to_vec())).unwrap()
}

fn get_req(method: &str, hex: &str, pass: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(format!("/blob/{hex}"))
        .header(PASS_HEADER, pass)
        .body(Body::empty())
        .unwrap()
}

fn delete_req(hex: &str, seed: Option<&[u8; 32]>) -> Request<Body> {
    let mut b = Request::delete(format!("/blob/{hex}"));
    if let Some(seed) = seed {
        let proof =
            lakko_vault::proof::sign(seed, "DELETE", &format!("/blob/{hex}"), &[], now_ms())
                .unwrap();
        b = b.header(lakko_vault::proof::PROOF_HEADER_NAME, proof);
    }
    b.body(Body::empty()).unwrap()
}

#[test]
fn parse_id_accepts_only_canonical_lower_hex() {
    assert!(parse_id(&"0".repeat(64)).is_ok());
    assert!(parse_id(&"abcdef0123456789".repeat(4)).is_ok());
    for bad in [
        String::new(),
        "0".repeat(63),
        "0".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
        format!("../{}", "0".repeat(61)),
    ] {
        assert_eq!(parse_id(&bad), Err(ApiError::IdMalformed));
    }
}

#[test]
fn blob_path_is_two_level_sharded() {
    let id = parse_id(&"abcdef0123456789".repeat(4)).unwrap();
    let p = blob_path(Path::new("/var/lib/lakko/blobs"), &id);
    assert_eq!(
        p,
        PathBuf::from(format!(
            "/var/lib/lakko/blobs/ab/cd/{}",
            "abcdef0123456789".repeat(4)
        ))
    );
}

#[tokio::test]
async fn a_pass_costs_one_token_of_its_context() {
    let (state, _tmp) = state();
    let app = build_app(state);
    let pass = take(&app, "upload", TokenContext::BlobUpload).await;
    assert!(pass.starts_with("p1."));

    // No token, a token of the other context, a token spent twice.
    let (s, b, _) = send(
        &app,
        Request::post("/blob/pass/read")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::UNAUTHORIZED, "anon-token-required")
    );
    let (s, b, _) = send(
        &app,
        Request::post("/blob/pass/read")
            .header(TOKEN_HEADER, token(TokenContext::BlobUpload))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::UNAUTHORIZED, "anon-token-wrong-context")
    );
    let t = token(TokenContext::BlobRead);
    for expected in [StatusCode::OK, StatusCode::UNAUTHORIZED] {
        let (s, _, _) = send(
            &app,
            Request::post("/blob/pass/read")
                .header(TOKEN_HEADER, t.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(s, expected);
    }
}

#[tokio::test]
async fn without_a_pass_secret_the_gated_routes_answer_503_and_take_no_token() {
    let (state, _tmp) = state_with(crate::pass::PassLimits::default(), false);
    let app = build_app(state);
    let t = token(TokenContext::BlobRead);
    let (s, b, _) = send(
        &app,
        Request::post("/blob/pass/read")
            .header(TOKEN_HEADER, t.clone())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "blob-pass-disabled")
    );
    let (s, b, _) = send(&app, get_req("GET", &"0".repeat(64), "p1.x")).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "blob-pass-disabled")
    );
    let (s, b, _) = send(&app, put_req(&"0".repeat(64), Some("p1.x"), b"x")).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "blob-pass-disabled")
    );
}

#[tokio::test]
async fn upload_then_download_round_trips_under_passes() {
    let (state, tmp) = state();
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let read = take(&app, "read", TokenContext::BlobRead).await;
    let payload = b"hello lakko ciphertext";
    let (id, hex) = sha256(payload);

    let (s, b, _) = send(&app, put_req(&hex, None, payload)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::UNAUTHORIZED, "blob-pass-required")
    );
    // A read pass is not an upload pass.
    let (s, b, _) = send(&app, put_req(&hex, Some(&read), payload)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::UNAUTHORIZED, "blob-pass-invalid")
    );

    let (s, _, _) = send(&app, put_req(&hex, Some(&up), payload)).await;
    assert_eq!(s, StatusCode::CREATED);
    assert!(tmp
        .path()
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(&hex)
        .exists());
    let (s, _, _) = send(&app, put_req(&hex, Some(&up), payload)).await;
    assert_eq!(s, StatusCode::OK);

    let (s, body, _) = send(&app, get_req("GET", &hex, &read)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(&body[..], payload);
    let (s, _, h) = send(&app, get_req("HEAD", &hex, &read)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h.get(header::CONTENT_LENGTH).unwrap().to_str().unwrap(),
        payload.len().to_string()
    );
    // An upload pass does not read.
    let (s, b, _) = send(&app, get_req("GET", &hex, &up)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::UNAUTHORIZED, "blob-pass-invalid")
    );
    let _ = id;
}

#[tokio::test]
async fn an_upload_needs_a_usable_delete_key_and_a_length() {
    let (state, _tmp) = state();
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let payload = b"bytes";
    let (_, hex) = sha256(payload);

    let no_key = Request::put(format!("/blob/{hex}"))
        .header(PASS_HEADER, &up)
        .header(header::CONTENT_LENGTH, payload.len())
        .body(Body::from(payload.to_vec()))
        .unwrap();
    let (s, b, _) = send(&app, no_key).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::BAD_REQUEST, "blob-delete-key-required")
    );

    // The identity point is a small-order key: refused.
    let mut weak = [0u8; 32];
    weak[0] = 1;
    let bad_key = Request::put(format!("/blob/{hex}"))
        .header(PASS_HEADER, &up)
        .header(header::CONTENT_LENGTH, payload.len())
        .header(DELETE_KEY_HEADER, URL_SAFE_NO_PAD.encode(weak))
        .body(Body::from(payload.to_vec()))
        .unwrap();
    let (s, b, _) = send(&app, bad_key).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::BAD_REQUEST, "blob-delete-key-invalid")
    );

    let no_len = Request::put(format!("/blob/{hex}"))
        .header(PASS_HEADER, &up)
        .header(DELETE_KEY_HEADER, delete_key_header())
        .body(Body::from(payload.to_vec()))
        .unwrap();
    let (s, b, _) = send(&app, no_len).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::LENGTH_REQUIRED, "blob-length-required")
    );

    let too_big = Request::put(format!("/blob/{hex}"))
        .header(PASS_HEADER, &up)
        .header(DELETE_KEY_HEADER, delete_key_header())
        .header(header::CONTENT_LENGTH, crate::MAX_BLOB_BYTES + 1)
        .body(Body::empty())
        .unwrap();
    let (s, b, _) = send(&app, too_big).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "blob-too-large")
    );

    let (s, b, _) = send(&app, put_req(&"0".repeat(64), Some(&up), payload)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::BAD_REQUEST, "blob-hash-mismatch")
    );
    let (s, b, _) = send(&app, put_req("nothex", Some(&up), payload)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::BAD_REQUEST, "blob-id-malformed")
    );
}

/// `docs/ux/storage-full.md`: an upload that would leave less free disk than
/// the floor is `507 storage-full`, and the refusal charges nothing, so the
/// same pass uploads once space is back.
#[tokio::test]
async fn a_full_disk_refuses_the_upload_and_charges_nothing() {
    let limits = crate::pass::PassLimits {
        upload_files: 1,
        min_free_bytes: u64::MAX,
        ..crate::pass::PassLimits::default()
    };
    let (mut state, _tmp) = state_with(limits, true);
    let app = build_app(state.clone());
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let (_, a) = sha256(b"a");
    let (s, b, _) = send(&app, put_req(&a, Some(&up), b"a")).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::INSUFFICIENT_STORAGE, "storage-full")
    );
    // Space is back: the same pass still has its one file.
    state.limits.min_free_bytes = 0;
    let app = build_app(state);
    assert_eq!(
        send(&app, put_req(&a, Some(&up), b"a")).await.0,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn a_pass_stops_at_its_budget() {
    let limits = crate::pass::PassLimits {
        upload_files: 1,
        read_downloads: 2,
        read_keeps: 3,
        min_free_bytes: 0,
        ..crate::pass::PassLimits::default()
    };
    let (state, _tmp) = state_with(limits, true);
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let read = take(&app, "read", TokenContext::BlobRead).await;

    let (_, a) = sha256(b"a");
    let (_, b_hex) = sha256(b"b");
    assert_eq!(
        send(&app, put_req(&a, Some(&up), b"a")).await.0,
        StatusCode::CREATED
    );
    let (s, b, _) = send(&app, put_req(&b_hex, Some(&up), b"b")).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (
            StatusCode::TOO_MANY_REQUESTS,
            "blob-upload-budget-exhausted"
        )
    );

    assert_eq!(
        send(&app, get_req("GET", &a, &read)).await.0,
        StatusCode::OK
    );
    // A probe of a missing blob costs a download too.
    assert_eq!(
        send(&app, get_req("HEAD", &b_hex, &read)).await.0,
        StatusCode::NOT_FOUND
    );
    let (s, b, _) = send(&app, get_req("GET", &a, &read)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "blob-read-budget-exhausted")
    );

    let keep = |n: usize| {
        let ids: Vec<String> = (0..n).map(|_| a.clone()).collect();
        Request::post("/blob/keep")
            .header(PASS_HEADER, &read)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({ "ids": ids }).to_string()))
            .unwrap()
    };
    assert_eq!(send(&app, keep(4)).await.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(send(&app, keep(3)).await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn an_expired_or_forged_pass_is_refused() {
    let (state, _tmp) = state();
    let app = build_app(state);
    let secret = PassSecret::from_bytes([0x33; 32]);
    let (_, expired) = secret.issue(PassKind::Read, 1);
    let (s, b, _) = send(&app, get_req("GET", &"0".repeat(64), &expired)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::UNAUTHORIZED, "blob-pass-expired")
    );
    let (_, foreign) = PassSecret::from_bytes([0x34; 32]).issue(PassKind::Read, u64::MAX);
    let (s, b, _) = send(&app, get_req("GET", &"0".repeat(64), &foreign)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::UNAUTHORIZED, "blob-pass-invalid")
    );
}

#[tokio::test]
async fn a_gone_blob_is_404_and_a_row_without_a_file_heals() {
    let (state, tmp) = state();
    let index = state.index.clone();
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let read = take(&app, "read", TokenContext::BlobRead).await;
    let (s, b, _) = send(&app, get_req("GET", &"f".repeat(64), &read)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::NOT_FOUND, "blob-not-found")
    );

    let (id, hex) = sha256(b"vanishing");
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), b"vanishing")).await.0,
        StatusCode::CREATED
    );
    std::fs::remove_file(tmp.path().join(&hex[0..2]).join(&hex[2..4]).join(&hex)).unwrap();
    let (s, b, _) = send(&app, get_req("GET", &hex, &read)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::NOT_FOUND, "blob-not-found")
    );
    assert_eq!(index.get(&id).await.unwrap(), None);
}

#[tokio::test]
async fn a_file_with_no_row_does_not_exist_for_a_client() {
    let (state, tmp) = state();
    let app = build_app(state);
    let read = take(&app, "read", TokenContext::BlobRead).await;
    let (id, hex) = sha256(b"orphan");
    let p = blob_path(tmp.path(), &id);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, b"orphan").unwrap();
    assert_eq!(
        send(&app, get_req("GET", &hex, &read)).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn keep_and_download_raise_the_last_access_day() {
    let (state, _tmp) = state();
    let index = state.index.clone();
    let app = build_app(state);
    let read = take(&app, "read", TokenContext::BlobRead).await;
    let mut row = crate::index::contract::row(7, 100, Some(1));
    row.id = [0x77; 32];
    index.record_upload(row.clone()).await.unwrap();

    let body = serde_json::json!({ "ids": [hex::encode(row.id), "e".repeat(64)] }).to_string();
    let (s, _, _) = send(
        &app,
        Request::post("/blob/keep")
            .header(PASS_HEADER, &read)
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        index.get(&row.id).await.unwrap().unwrap().last_access_day,
        today()
    );

    for bad in [
        "{}".to_string(),
        r#"{"ids":[]}"#.to_string(),
        r#"{"ids":["XYZ"]}"#.to_string(),
        serde_json::json!({ "ids": vec!["0".repeat(64); MAX_KEEP_IDS + 1] }).to_string(),
    ] {
        let (s, b, _) = send(
            &app,
            Request::post("/blob/keep")
                .header(PASS_HEADER, &read)
                .body(Body::from(bad))
                .unwrap(),
        )
        .await;
        assert_eq!(
            (s, code(&b).as_str()),
            (StatusCode::BAD_REQUEST, "keep-malformed")
        );
    }
}

#[tokio::test]
async fn only_the_delete_key_deletes_and_every_refusal_looks_the_same() {
    let (state, tmp) = state();
    let index = state.index.clone();
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let read = take(&app, "read", TokenContext::BlobRead).await;
    let (id, hex) = sha256(b"deletable");
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), b"deletable")).await.0,
        StatusCode::CREATED
    );

    let other = [0x55u8; 32];
    for req in [
        delete_req(&hex, None),
        delete_req(&hex, Some(&other)),
        delete_req(&"e".repeat(64), Some(&DELETE_SEED)),
        delete_req("nothex", Some(&DELETE_SEED)),
    ] {
        let (s, b, _) = send(&app, req).await;
        assert_eq!(
            (s, code(&b).as_str()),
            (StatusCode::FORBIDDEN, "blob-delete-refused")
        );
    }
    // A stale proof.
    let old_proof = lakko_vault::proof::sign(
        &DELETE_SEED,
        "DELETE",
        &format!("/blob/{hex}"),
        &[],
        now_ms() - 10 * 60 * 1000,
    )
    .unwrap();
    let req = Request::delete(format!("/blob/{hex}"))
        .header(lakko_vault::proof::PROOF_HEADER_NAME, old_proof)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);

    let (s, _, _) = send(&app, delete_req(&hex, Some(&DELETE_SEED))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(index.get(&id).await.unwrap(), None);
    assert!(!blob_path(tmp.path(), &id).exists());
    assert_eq!(
        send(&app, get_req("GET", &hex, &read)).await.0,
        StatusCode::NOT_FOUND
    );
    // A second delete of what is gone is the same refusal.
    assert_eq!(
        send(&app, delete_req(&hex, Some(&DELETE_SEED))).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn an_imported_row_cannot_be_deleted() {
    let (state, _tmp) = state();
    let index = state.index.clone();
    let app = build_app(state);
    let id = [0x66u8; 32];
    index
        .import(
            vec![crate::index::BlobRow {
                id,
                size: 1,
                node: "local".into(),
                created_day: 1,
                last_access_day: 1,
                delete_pk: None,
                idle_days: None,
            }],
            "local",
            1,
        )
        .await
        .unwrap();
    let (s, _, _) = send(&app, delete_req(&hex::encode(id), Some(&DELETE_SEED))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(index.get(&id).await.unwrap().is_some());
}

#[tokio::test]
async fn the_first_uploaders_key_survives_a_re_upload() {
    let (state, _tmp) = state();
    let index = state.index.clone();
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let (id, hex) = sha256(b"shared");
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), b"shared")).await.0,
        StatusCode::CREATED
    );
    let other_key = URL_SAFE_NO_PAD.encode(lakko_vault::proof::public_key(&[0x55; 32]));
    let req = Request::put(format!("/blob/{hex}"))
        .header(PASS_HEADER, &up)
        .header(header::CONTENT_LENGTH, 6)
        .header(DELETE_KEY_HEADER, other_key)
        .body(Body::from(&b"shared"[..]))
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::OK);
    assert_eq!(
        index.get(&id).await.unwrap().unwrap().delete_pk,
        Some(lakko_vault::proof::public_key(&DELETE_SEED))
    );
}

/// The same surface over the MariaDB index and the DB spent-set
/// (`LAKKO_STORAGE_TEST_DATABASE_URL`; skips when unset).
#[tokio::test]
async fn the_routes_work_over_the_mariadb_index() {
    let Some(pool) =
        crate::index::sql::tests::test_pool("the_routes_work_over_the_mariadb_index").await
    else {
        return;
    };
    let (mut state, _tmp) = state();
    state.index = std::sync::Arc::new(crate::index::sql::SqlIndex::new(pool.clone()));
    let config =
        lakko_anon_token_server::AnonTokenServerConfig::from_values(Some(&master_hex()), None)
            .unwrap();
    state.tokens = lakko_anon_token_server::TokenVerifier::new(pool, config);
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let read = take(&app, "read", TokenContext::BlobRead).await;
    // A random payload so reruns against a persistent database start clean.
    let mut payload = [0u8; 24];
    lakko_crypto::random::fill(&mut payload);
    let (_, hex) = sha256(&payload);
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), &payload)).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), &payload)).await.0,
        StatusCode::OK
    );
    let (s, body, _) = send(&app, get_req("GET", &hex, &read)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(&body[..], &payload[..]);
    assert_eq!(
        send(&app, delete_req(&hex, Some(&[0x55; 32]))).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, delete_req(&hex, Some(&DELETE_SEED))).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&app, get_req("GET", &hex, &read)).await.0,
        StatusCode::NOT_FOUND
    );
}

/// ADR-0087 section 13: an upload may name its own idle life, which the row
/// keeps; a malformed value is refused before the pass is charged.
#[tokio::test]
async fn an_upload_names_its_idle_life() {
    let (state, _tmp) = state();
    let index = state.index.clone();
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let payload = b"a channel file";
    let (id, hex) = sha256(payload);

    for bad in ["0", "256", "-1", "+45", " 45", "4 5", "", "1000"] {
        let mut req = put_req(&hex, Some(&up), payload);
        req.headers_mut().insert(
            IDLE_DAYS_HEADER,
            header::HeaderValue::from_str(bad).unwrap(),
        );
        let (s, b, _) = send(&app, req).await;
        assert_eq!(
            (s, code(&b).as_str()),
            (StatusCode::BAD_REQUEST, "blob-idle-days-invalid"),
            "{bad:?}"
        );
    }
    assert_eq!(index.get(&id).await.unwrap(), None);

    let mut req = put_req(&hex, Some(&up), payload);
    req.headers_mut()
        .insert(IDLE_DAYS_HEADER, header::HeaderValue::from_static("45"));
    assert_eq!(send(&app, req).await.0, StatusCode::CREATED);
    assert_eq!(index.get(&id).await.unwrap().unwrap().idle_days, Some(45));

    // A repeated upload without the header keeps the first idle life.
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), payload)).await.0,
        StatusCode::OK
    );
    assert_eq!(index.get(&id).await.unwrap().unwrap().idle_days, Some(45));

    // Without the header a blob has none.
    let (other, other_hex) = sha256(b"a chat file");
    assert_eq!(
        send(&app, put_req(&other_hex, Some(&up), b"a chat file"))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(index.get(&other).await.unwrap().unwrap().idle_days, None);
}

// ============================================================================
// The same routes over an S3 bucket (ADR-0065 amendment of 2026-11-19),
// gated on the test object store (see `crate::store::s3::tests`).
// ============================================================================

/// [`state`] with its bytes in a fresh test bucket, or `None` without one.
async fn s3_state() -> Option<(
    AppState,
    tempfile::TempDir,
    std::sync::Arc<crate::store::s3::S3Store>,
)> {
    let (mut state, tmp) = state();
    let store = std::sync::Arc::new(
        crate::store::s3::tests::test_store(tmp.path().join(".lakko-upload")).await?,
    );
    state.store = store.clone();
    Some((state, tmp, store))
}

#[tokio::test]
async fn over_s3_upload_download_head_and_delete() {
    let Some((state, tmp, store)) = s3_state().await else {
        return;
    };
    let index = state.index.clone();
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let read = take(&app, "read", TokenContext::BlobRead).await;
    let payload = b"hello lakko ciphertext in a bucket";
    let (id, hex) = sha256(payload);
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), payload)).await.0,
        StatusCode::CREATED
    );
    // Nothing of the blob stays on the local disk.
    assert!(!blob_path(tmp.path(), &id).exists());
    let leftovers = std::fs::read_dir(tmp.path().join(".lakko-upload"))
        .unwrap()
        .count();
    assert_eq!(leftovers, 0);
    assert_eq!(
        crate::store::BlobStore::head(store.as_ref(), &id)
            .await
            .unwrap(),
        Some(payload.len() as u64)
    );
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), payload)).await.0,
        StatusCode::OK
    );
    let (s, body, h) = send(&app, get_req("GET", &hex, &read)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(&body[..], payload);
    assert_eq!(
        h.get(header::CONTENT_LENGTH).unwrap().to_str().unwrap(),
        payload.len().to_string()
    );
    assert_eq!(
        send(&app, get_req("HEAD", &hex, &read)).await.0,
        StatusCode::OK
    );
    // A body that does not hash to the path leaves no object and no row.
    let (bad_id, bad_hex) = sha256(b"other");
    let (s, b, _) = send(&app, put_req(&bad_hex, Some(&up), b"wrong")).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::BAD_REQUEST, "blob-hash-mismatch")
    );
    assert_eq!(
        crate::store::BlobStore::head(store.as_ref(), &bad_id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(index.get(&bad_id).await.unwrap(), None);

    assert_eq!(
        send(&app, delete_req(&hex, Some(&DELETE_SEED))).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(index.get(&id).await.unwrap(), None);
    assert_eq!(
        crate::store::BlobStore::head(store.as_ref(), &id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        send(&app, get_req("GET", &hex, &read)).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn over_s3_a_row_without_its_object_heals() {
    let Some((state, _tmp, store)) = s3_state().await else {
        return;
    };
    let index = state.index.clone();
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let read = take(&app, "read", TokenContext::BlobRead).await;
    let (id, hex) = sha256(b"vanishing object");
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), b"vanishing object"))
            .await
            .0,
        StatusCode::CREATED
    );
    crate::store::BlobStore::delete(store.as_ref(), &id)
        .await
        .unwrap();
    let (s, b, _) = send(&app, get_req("GET", &hex, &read)).await;
    assert_eq!(
        (s, code(&b).as_str()),
        (StatusCode::NOT_FOUND, "blob-not-found")
    );
    assert_eq!(index.get(&id).await.unwrap(), None);
}

#[tokio::test]
async fn over_s3_a_50_mib_upload_streams_through() {
    let Some((state, tmp, _store)) = s3_state().await else {
        return;
    };
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let read = take(&app, "read", TokenContext::BlobRead).await;
    let payload: Vec<u8> = (0..crate::MAX_BLOB_BYTES)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let (_, hex) = sha256(&payload);
    // Sent in 1 MiB chunks so the route sees a stream, not one buffer.
    let chunks: Vec<Result<Bytes, std::io::Error>> = payload
        .chunks(1024 * 1024)
        .map(|c| Ok(Bytes::copy_from_slice(c)))
        .collect();
    let req = Request::put(format!("/blob/{hex}"))
        .header(header::CONTENT_LENGTH, payload.len())
        .header(DELETE_KEY_HEADER, delete_key_header())
        .header(PASS_HEADER, &up)
        .body(Body::from_stream(futures::stream::iter(chunks)))
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::CREATED);
    assert_eq!(
        std::fs::read_dir(tmp.path().join(".lakko-upload"))
            .unwrap()
            .count(),
        0
    );
    let (s, body, _) = send(&app, get_req("GET", &hex, &read)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.len(), payload.len());
    assert!(body == payload);
}

#[tokio::test]
async fn over_s3_the_sweep_deletes_the_object() {
    let Some((state, _tmp, store)) = s3_state().await else {
        return;
    };
    let index = state.index.clone();
    let app = build_app(state);
    let up = take(&app, "upload", TokenContext::BlobUpload).await;
    let (id, hex) = sha256(b"expiring object");
    assert_eq!(
        send(&app, put_req(&hex, Some(&up), b"expiring object"))
            .await
            .0,
        StatusCode::CREATED
    );
    let far = crate::index::today() + 400;
    let n = crate::sweep::sweep_once(index.as_ref(), store.as_ref(), "local", far, 365)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(index.get(&id).await.unwrap(), None);
    assert_eq!(
        crate::store::BlobStore::head(store.as_ref(), &id)
            .await
            .unwrap(),
        None
    );
}
