// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tests for the channel-poll credential routes (ADR-0092 section 4.2). The
//! dormant and malformed cases run without a database; the issuance flow,
//! the commitment conflict and the key-reset replacement are gated on
//! `LAKKO_AUTH_TEST_DATABASE_URL` and skip when it is unset.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_anon_cred::{
    period_at, CredentialSecret, CredentialVerifier, IssuanceResponse, IssuerPublicKey,
    MasterSecret, PollScope,
};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tower::ServiceExt as _;

use super::AnonCredIssuer;

const MASTER: [u8; 32] = [0x61; 32];

fn build_state(pool: MySqlPool, enabled: bool) -> (crate::AppState, lakko_auth_token::TokenSecret) {
    use opaque_ke::ServerSetup;
    use rand::rngs::OsRng;
    let token_secret = lakko_auth_token::TokenSecret::new(vec![9u8; 64]).expect("token secret");
    let state = crate::AppState {
        db: pool,
        opaque_setup: Arc::new(ServerSetup::<crate::opaque::LakkoCipherSuite>::new(
            &mut OsRng,
        )),
        session_token_secret: Arc::new(token_secret.clone()),
        session_token_ttl_ms: 86_400_000,
        refresh_token_ttl_ms: 30 * 86_400_000,
        fresh_grant_ttl_ms: 600_000,
        webauthn: None,
        webauthn_action_core: None,
        call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
        attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
        turn: None,
        totp_enc_key: None,
        vault_enc_key: None,
        room_cap: Arc::new(crate::room_cap::RoomAdmissionCap::disabled()),
        backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
        anon_tokens: None,
        anon_cred: enabled.then(|| Arc::new(AnonCredIssuer::new(MasterSecret::from_bytes(MASTER)))),
        handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
        recovery_email: None,
        limits: Arc::new(crate::limits::Limits::defaults()),
    };
    (state, token_secret)
}

fn lazy_pool() -> MySqlPool {
    MySqlPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(200))
        .connect_lazy("mysql://nobody@127.0.0.1:1/nodb")
        .expect("lazy pool (never connects)")
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(i64::MAX)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn bearer(secret: &lakko_auth_token::TokenSecret, account: [u8; 16]) -> String {
    let token = secret
        .issue(&account, 1, &[0u8; 32], 0, now_ms() + 3_600_000)
        .expect("issue session token");
    format!("Bearer {token}")
}

fn random_account() -> [u8; 16] {
    let mut a = [0u8; 16];
    lakko_crypto::random::fill(&mut a);
    a
}

fn random_secret() -> CredentialSecret {
    let mut seed = [0u8; 32];
    lakko_crypto::random::fill(&mut seed);
    CredentialSecret::from_lineage_seed(&seed).unwrap()
}

async fn insert_account(pool: &MySqlPool, account: [u8; 16]) {
    sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&account[..])
        .execute(pool)
        .await
        .expect("insert account");
}

async fn send(state: &crate::AppState, req: Request<Body>) -> (StatusCode, Bytes) {
    let resp = crate::build_app(state.clone())
        .oneshot(req)
        .await
        .expect("router oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, bytes)
}

fn json(bytes: &Bytes) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("json body")
}

fn put_commitment(auth: &str, secret: &CredentialSecret) -> Request<Body> {
    let reg = secret.register().unwrap();
    let body = serde_json::json!({ "registration_b64": URL_SAFE_NO_PAD.encode(reg.to_bytes()) });
    Request::builder()
        .method("PUT")
        .uri("/auth/anon-credential/commitment")
        .header("authorization", auth)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn post_issue(auth: &str, request: &[u8]) -> Request<Body> {
    let body = serde_json::json!({ "request_b64": URL_SAFE_NO_PAD.encode(request) });
    Request::builder()
        .method("POST")
        .uri("/auth/anon-credential")
        .header("authorization", auth)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn key_req(period: u64) -> Request<Body> {
    Request::builder()
        .uri(format!("/auth/anon-credential/key/{period}"))
        .body(Body::empty())
        .unwrap()
}

async fn fetch_key(state: &crate::AppState, period: u64) -> IssuerPublicKey {
    let (status, bytes) = send(state, key_req(period)).await;
    assert_eq!(status, StatusCode::OK);
    let v = json(&bytes);
    assert_eq!(v["period"], period);
    let raw = URL_SAFE_NO_PAD
        .decode(v["public_key_b64"].as_str().unwrap())
        .unwrap();
    IssuerPublicKey::decode(period, &raw).unwrap()
}

// ---- DB-free -----------------------------------------------------------------

#[tokio::test]
async fn the_key_route_answers_503_when_dormant() {
    let (state, _) = build_state(lazy_pool(), false);
    let (status, bytes) = send(&state, key_req(period_at(now_secs()))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json(&bytes)["error"], "anon-credential-disabled");
}

#[tokio::test]
async fn the_key_route_serves_previous_current_and_next_period_only() {
    let (state, _) = build_state(lazy_pool(), true);
    let current = period_at(now_secs());
    for p in [current - 1, current, current + 1] {
        let key = fetch_key(&state, p).await;
        assert_eq!(key.period(), p);
    }
    for p in [current - 2, current + 2] {
        let (status, bytes) = send(&state, key_req(p)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json(&bytes)["error"], "period-out-of-range");
    }
}

#[test]
fn the_master_secret_parses_from_hex_or_fails_startup() {
    // The env var is read by `load_anon_credential`; its parser is the
    // crate's, so a malformed value is refused there.
    assert!(MasterSecret::from_hex(&"61".repeat(32)).is_ok());
    assert!(MasterSecret::from_hex("61").is_err());
}

// ---- DB-gated ----------------------------------------------------------------

async fn test_pool(name: &str) -> Option<MySqlPool> {
    let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
        eprintln!("skipping {name}: LAKKO_AUTH_TEST_DATABASE_URL unset");
        return None;
    };
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect test DB");
    crate::MIGRATIONS.run(&pool).await.expect("run migrations");
    Some(pool)
}

/// The whole client flow: register, fetch the published key, request,
/// finalize against that key, present for a poll, and verify the
/// presentation where `services/channels` would.
#[tokio::test]
async fn register_issue_finalize_and_present() {
    let Some(pool) = test_pool("register_issue_finalize_and_present").await else {
        return;
    };
    let (state, secret) = build_state(pool.clone(), true);
    let account = random_account();
    insert_account(&pool, account).await;
    let auth = bearer(&secret, account);
    let cred_secret = random_secret();

    // Issuance before registration is refused.
    let current = period_at(now_secs());
    let key = fetch_key(&state, current).await;
    let request = cred_secret.issuance_request(&key).unwrap().to_bytes();
    let (status, bytes) = send(&state, post_issue(&auth, &request)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json(&bytes)["error"], "commitment-not-registered");

    let (status, _) = send(&state, put_commitment(&auth, &cred_secret)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // A repeat of the same commitment is idempotent.
    let (status, _) = send(&state, put_commitment(&auth, &cred_secret)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, bytes) = send(&state, post_issue(&auth, &request)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let v = json(&bytes);
    assert_eq!(v["period"], current);
    let response = IssuanceResponse::decode(
        &URL_SAFE_NO_PAD
            .decode(v["response_b64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    let credential = cred_secret.finalize_issuance(&key, &response).unwrap();

    // A second issuance in the period works and yields the same tag.
    let request2 = cred_secret.issuance_request(&key).unwrap().to_bytes();
    let (status, bytes) = send(&state, post_issue(&auth, &request2)).await;
    assert_eq!(status, StatusCode::OK);
    let response2 = IssuanceResponse::decode(
        &URL_SAFE_NO_PAD
            .decode(json(&bytes)["response_b64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    let credential2 = cred_secret.finalize_issuance(&key, &response2).unwrap();

    let scope = PollScope::channel_post([7u8; 16], 42);
    let verifier = CredentialVerifier::new(MasterSecret::from_bytes(MASTER));
    let a = cred_secret
        .present(&credential, &key, &scope, b"m")
        .unwrap()
        .to_bytes();
    let b = cred_secret
        .present(&credential2, &key, &scope, b"m")
        .unwrap()
        .to_bytes();
    let va = verifier.verify(&a, &scope, b"m", current).unwrap();
    let vb = verifier.verify(&b, &scope, b"m", current).unwrap();
    assert_eq!(va.tag, vb.tag);

    let issued: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM anon_credential_issued WHERE account_id = ? AND period = ?",
    )
    .bind(&account[..])
    .bind(current)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(issued, 1);
}

#[tokio::test]
async fn a_second_commitment_is_refused_until_a_reset_deletes_the_first() {
    let Some(pool) =
        test_pool("a_second_commitment_is_refused_until_a_reset_deletes_the_first").await
    else {
        return;
    };
    let (state, secret) = build_state(pool.clone(), true);
    let account = random_account();
    insert_account(&pool, account).await;
    let auth = bearer(&secret, account);
    let first = random_secret();
    let second = random_secret();

    assert_eq!(
        send(&state, put_commitment(&auth, &first)).await.0,
        StatusCode::NO_CONTENT
    );
    let (status, bytes) = send(&state, put_commitment(&auth, &second)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json(&bytes)["error"], "commitment-already-registered");

    // The second secret cannot obtain a credential for the first's commitment.
    let key = fetch_key(&state, period_at(now_secs())).await;
    let request = second.issuance_request(&key).unwrap().to_bytes();
    let (status, bytes) = send(&state, post_issue(&auth, &request)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&bytes)["error"], "bad-proof");

    // The key reset's shared deletion step replaces it.
    let mut tx = pool.begin().await.unwrap();
    crate::key_reset::delete_superseded_state(&mut tx, &account)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        send(&state, put_commitment(&auth, &second)).await.0,
        StatusCode::NO_CONTENT
    );
    let (status, _) = send(&state, post_issue(&auth, &request)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn malformed_and_off_period_requests_are_refused() {
    let Some(pool) = test_pool("malformed_and_off_period_requests_are_refused").await else {
        return;
    };
    let (state, secret) = build_state(pool.clone(), true);
    let account = random_account();
    insert_account(&pool, account).await;
    let auth = bearer(&secret, account);
    let cred_secret = random_secret();
    send(&state, put_commitment(&auth, &cred_secret)).await;

    let (status, bytes) = send(&state, post_issue(&auth, b"short")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&bytes)["error"], "malformed");

    let current = period_at(now_secs());
    let next_key = fetch_key(&state, current + 1).await;
    let request = cred_secret.issuance_request(&next_key).unwrap().to_bytes();
    let (status, bytes) = send(&state, post_issue(&auth, &request)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v = json(&bytes);
    assert_eq!(v["error"], "period-not-issuable");
    assert_eq!(v["current_period"], current);
}

#[tokio::test]
async fn deleting_the_account_deletes_both_rows() {
    let Some(pool) = test_pool("deleting_the_account_deletes_both_rows").await else {
        return;
    };
    let (state, secret) = build_state(pool.clone(), true);
    let account = random_account();
    insert_account(&pool, account).await;
    let auth = bearer(&secret, account);
    let cred_secret = random_secret();
    send(&state, put_commitment(&auth, &cred_secret)).await;
    let key = fetch_key(&state, period_at(now_secs())).await;
    let request = cred_secret.issuance_request(&key).unwrap().to_bytes();
    assert_eq!(
        send(&state, post_issue(&auth, &request)).await.0,
        StatusCode::OK
    );

    sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(&account[..])
        .execute(&pool)
        .await
        .unwrap();
    for sql in [
        "SELECT COUNT(*) FROM anon_credential_commitments WHERE account_id = ?",
        "SELECT COUNT(*) FROM anon_credential_issued WHERE account_id = ?",
    ] {
        let n: i64 = sqlx::query_scalar(sql)
            .bind(&account[..])
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "{sql}");
    }
}
