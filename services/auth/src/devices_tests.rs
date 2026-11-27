// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests for ADR-0074 sprint T1: per-device session revocation
//! (`/auth/session/logout`, `/auth/session/bind-device`,
//! `/account/devices/revoke`, the floor check in `require_session` and in the
//! refresh route) and the recorded identity key (`/auth/genesis/attest`).
//!
//! Every test drives the real router (`crate::build_app`), so the bearer
//! middleware, the extractors and the status / body codes are the ones a
//! client meets. Accounts are inserted directly and credentials are minted
//! with the service's own issuer, since nothing here is about OPAQUE.
//!
//! Gated on `LAKKO_AUTH_TEST_DATABASE_URL` (each test skips when it is
//! unset):
//!
//! ```text
//! LAKKO_AUTH_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_auth_test \
//!   cargo test -p lakko-svc-auth devices_tests
//! ```

#![allow(clippy::many_single_char_names, clippy::too_many_lines)]

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, StatusCode};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_attest::{AccountStatement, AccountStatementKind};
use lakko_crypto::signing::SigningKey;
use opaque_ke::ServerSetup;
use rand::rngs::OsRng;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tower::ServiceExt as _;
use uuid::Uuid;

use crate::limits::{Limits, Scope, Window};
use crate::opaque::LakkoCipherSuite;
use crate::session::{now_ms, SessionPair};
use crate::AppState;

const DEVICE_A: [u8; 32] = [0xA1; 32];
const DEVICE_B: [u8; 32] = [0xB2; 32];
const UNBOUND: [u8; 32] = [0u8; 32];

async fn pool() -> Option<MySqlPool> {
    let url = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL").ok()?;
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect test DB");
    crate::MIGRATIONS.run(&pool).await.expect("migrations");
    Some(pool)
}

fn build_state(pool: MySqlPool, limits: Limits) -> AppState {
    let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
    AppState {
        db: pool,
        opaque_setup: Arc::new(ServerSetup::<LakkoCipherSuite>::new(&mut OsRng)),
        session_token_secret: Arc::new(token_secret),
        session_token_ttl_ms: 900_000,
        refresh_token_ttl_ms: 30 * 86_400_000,
        fresh_grant_ttl_ms: 600_000,
        webauthn: None,
        webauthn_action_core: None,
        call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
        attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
        turn: None,
        totp_enc_key: None,
        vault_enc_key: None,
        room_cap: std::sync::Arc::new(crate::room_cap::RoomAdmissionCap::disabled()),
        backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
        anon_tokens: None,
        anon_cred: None,
        handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
        recovery_email: None,
        owned_channels: Some(std::sync::Arc::new(
            crate::owned_channels::OwnedChannelsMaster::from_bytes([0x0C; 32]),
        )),
        reset_factor_grant_ttl_ms: 600_000,
        limits: Arc::new(limits),
    }
}

/// Insert a fresh account at generation 1 with no recorded identity key.
async fn new_account(pool: &MySqlPool) -> [u8; 16] {
    let id = *Uuid::now_v7().as_bytes();
    sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&id[..])
        .execute(pool)
        .await
        .expect("insert account");
    id
}

/// A session pair for `device` at the account's generation, issued now.
fn pair(state: &AppState, account: &[u8; 16], device: &[u8; 32]) -> SessionPair {
    crate::session::issue_pair_at(state, account, 1, device).expect("pair")
}

fn grant(state: &AppState, account: &[u8; 16]) -> String {
    crate::session::issue_fresh_grant(state, account, 1, lakko_auth_token::GrantMethod::Password)
        .expect("grant")
        .fresh_grant
}

/// A deterministic account identity key per test account.
/// A fresh random identity key on every call: a key belongs to one account
/// forever (ADR-0099 F3) in a database the tests share across runs. The tag
/// names the key in the test; two calls never agree.
fn account_key(_tag: u8) -> SigningKey {
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut OsRng, &mut seed);
    SigningKey::from_seed(&seed)
}

/// The account a v3 bearer names.
fn account_of(bearer: &str) -> [u8; 16] {
    URL_SAFE_NO_PAD
        .decode(bearer.split('.').nth(1).expect("account part"))
        .expect("base64")
        .try_into()
        .expect("16 bytes")
}

/// `key`'s identity proof for `account` (ADR-0099 F3).
fn identity_proof(account: &[u8; 16], key: &SigningKey) -> String {
    b64(&lakko_attest::sign_identity_proof(&key.to_seed(), account))
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A `device-revoke` statement for `device`, signed by `key` at `unix_ms`.
fn revoke_statement(
    key: &SigningKey,
    account: &[u8; 16],
    device: &[u8; 32],
    unix_ms: i64,
) -> String {
    b64(&AccountStatement::new(
        AccountStatementKind::DeviceRevoke,
        *account,
        unix_ms,
        [0x5A; 16],
        device,
    )
    .sign(key))
}

async fn call(
    state: &AppState,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    body: Option<&Value>,
) -> (StatusCode, Bytes) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let req = match body {
        Some(v) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(v).unwrap())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = crate::build_app(state.clone())
        .oneshot(req)
        .await
        .expect("router oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, bytes)
}

async fn post(
    state: &AppState,
    uri: &str,
    bearer: Option<&str>,
    body: &Value,
) -> (StatusCode, Bytes) {
    call(state, Method::POST, uri, bearer, Some(body)).await
}

/// A bearer route that needs nothing but the session: the passkey list.
async fn probe(state: &AppState, bearer: &str) -> (StatusCode, Bytes) {
    call(
        state,
        Method::GET,
        "/webauthn/credentials",
        Some(bearer),
        None,
    )
    .await
}

async fn refresh(state: &AppState, refresh_token: &str) -> (StatusCode, Bytes) {
    post(
        state,
        "/auth/session/refresh",
        None,
        &json!({ "refresh_token": refresh_token }),
    )
    .await
}

fn code(body: &Bytes) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Record `key` as the account's identity key through the genesis mint.
async fn record_key(state: &AppState, bearer: &str, key: &SigningKey) -> StatusCode {
    post(
        state,
        "/auth/genesis/attest",
        Some(bearer),
        &json!({
            "pubkey_b64": b64(&key.verifying_key().to_bytes()),
            "identity_proof_b64": identity_proof(&account_of(bearer), key),
        }),
    )
    .await
    .0
}

/// Sleep past the millisecond the last credential or floor was stamped in,
/// so "issued after the floor" is true by the clock and not by luck.
async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
}

async fn floor_of(pool: &MySqlPool, account: &[u8; 16], device: &[u8; 32]) -> Option<(i64, u8)> {
    sqlx::query_as(
        "SELECT not_before_ms, reason FROM device_session_floors \
         WHERE account_id = ? AND device_id = ?",
    )
    .bind(&account[..])
    .bind(&device[..])
    .fetch_optional(pool)
    .await
    .expect("floor read")
}

macro_rules! db_or_skip {
    ($name:literal) => {{
        let Some(pool) = pool().await else {
            eprintln!(concat!(
                "skipping ",
                $name,
                ": LAKKO_AUTH_TEST_DATABASE_URL unset"
            ));
            return;
        };
        pool
    }};
}

/// Logout revokes the caller's own device and nothing else: its bearer and
/// its refresh credential stop at once, another device of the account and an
/// unbound session keep working, and a later sign-in on the same device works
/// again.
#[tokio::test]
async fn logout_revokes_only_the_callers_device() {
    let pool = db_or_skip!("logout_revokes_only_the_callers_device");
    let state = build_state(pool.clone(), Limits::defaults());
    let account = new_account(&pool).await;
    let a = pair(&state, &account, &DEVICE_A);
    let b = pair(&state, &account, &DEVICE_B);
    let unbound = pair(&state, &account, &UNBOUND);

    assert_eq!(probe(&state, &a.session_token).await.0, StatusCode::OK);
    assert_eq!(probe(&state, &b.session_token).await.0, StatusCode::OK);

    // No bearer: the route is behind the session middleware.
    assert_eq!(
        call(&state, Method::POST, "/auth/session/logout", None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );

    let (st, _) = call(
        &state,
        Method::POST,
        "/auth/session/logout",
        Some(&a.session_token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, reason) = floor_of(&pool, &account, &DEVICE_A).await.expect("floor");
    assert_eq!(reason, crate::session::FLOOR_REASON_LOGOUT);

    // Device A: the bearer is the plain bodiless 401, the refresh credential
    // the ordinary `refresh-invalid`. Neither is `device-revoked`: a logout
    // is not a removal and must not make the device wipe itself.
    let (st, body) = probe(&state, &a.session_token).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(body.is_empty(), "a logged-out bearer gets the plain 401");
    let (st, body) = refresh(&state, &a.refresh_token).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&body), "refresh-invalid");
    // A second logout with the dead bearer is refused like any other call.
    assert_eq!(
        call(
            &state,
            Method::POST,
            "/auth/session/logout",
            Some(&a.session_token),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );

    // Device B and the unbound session are untouched.
    assert_eq!(probe(&state, &b.session_token).await.0, StatusCode::OK);
    assert_eq!(refresh(&state, &b.refresh_token).await.0, StatusCode::OK);
    assert_eq!(
        probe(&state, &unbound.session_token).await.0,
        StatusCode::OK
    );
    assert!(floor_of(&pool, &account, &DEVICE_B).await.is_none());

    // Another account's device with the same id is untouched too.
    let other = new_account(&pool).await;
    let other_a = pair(&state, &other, &DEVICE_A);
    assert_eq!(
        probe(&state, &other_a.session_token).await.0,
        StatusCode::OK
    );

    // A later sign-in on device A is issued above the floor.
    tick().await;
    let again = pair(&state, &account, &DEVICE_A);
    assert_eq!(probe(&state, &again.session_token).await.0, StatusCode::OK);
    assert_eq!(
        refresh(&state, &again.refresh_token).await.0,
        StatusCode::OK
    );

    // Logging out an unbound session answers 204 and writes no floor.
    let (st, _) = call(
        &state,
        Method::POST,
        "/auth/session/logout",
        Some(&unbound.session_token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(floor_of(&pool, &account, &UNBOUND).await.is_none());
    assert_eq!(
        probe(&state, &unbound.session_token).await.0,
        StatusCode::OK
    );
}

/// The whole revoke contract: both proofs are needed, the removed device's
/// bearer and refresh are refused with `device-revoked`, and every other
/// device is unaffected.
#[tokio::test]
async fn revoke_needs_the_grant_and_the_account_key_and_stops_only_the_target() {
    let pool = db_or_skip!("revoke_needs_the_grant_and_the_account_key_and_stops_only_the_target");
    let state = build_state(pool.clone(), Limits::defaults());
    let account = new_account(&pool).await;
    let key = account_key(0x31);
    let a = pair(&state, &account, &DEVICE_A);
    let b = pair(&state, &account, &DEVICE_B);
    let fresh = grant(&state, &account);
    let statement = || revoke_statement(&key, &account, &DEVICE_B, now_ms());
    let body = |grant: Option<&str>, device: &[u8; 32], statement: &str| {
        let mut v = json!({ "device_id_b64": b64(device), "statement_b64": statement });
        if let Some(g) = grant {
            v["fresh_grant"] = json!(g);
        }
        v
    };
    let revoke = |v: Value| {
        let state = state.clone();
        let bearer = a.session_token.clone();
        async move { post(&state, "/account/devices/revoke", Some(&bearer), &v).await }
    };

    // (0) No recorded identity key yet: even a well-signed statement with a
    //     live grant is refused. Nothing is charged, nothing is written.
    let (st, bytes) = revoke(body(Some(&fresh), &DEVICE_B, &statement())).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&bytes), "account-key-required");

    assert_eq!(
        record_key(&state, &a.session_token, &key).await,
        StatusCode::OK
    );

    // (1) The grant is checked first: without one, with a malformed one,
    //     with another account's, the answer is `fresh-auth-required` even
    //     though the statement is good.
    let stranger = new_account(&pool).await;
    for bad in [
        None,
        Some("k1.not-a-grant".to_string()),
        Some(grant(&state, &stranger)),
        // A session token is not a grant.
        Some(a.session_token.clone()),
    ] {
        let (st, bytes) = revoke(body(bad.as_deref(), &DEVICE_B, &statement())).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{bad:?}");
        assert_eq!(code(&bytes), "fresh-auth-required", "{bad:?}");
    }

    // (2) The password alone cannot revoke (the remote-wipe hazard): a live
    //     grant with no statement that verifies under the RECORDED key is
    //     refused, whatever is wrong with the statement.
    let other_key = account_key(0x32);
    let refused: Vec<(&str, String)> = vec![
        ("empty", String::new()),
        ("not base64", "!!!".to_string()),
        ("too short", b64(&[1u8; 137])),
        ("all zero", b64(&[0u8; 138])),
        (
            "signed by a key that is not the account's",
            revoke_statement(&other_key, &account, &DEVICE_B, now_ms()),
        ),
        (
            "signed for another device",
            revoke_statement(&key, &account, &DEVICE_A, now_ms()),
        ),
        (
            "signed for another account",
            revoke_statement(&key, &stranger, &DEVICE_B, now_ms()),
        ),
        (
            "signed too long ago",
            revoke_statement(&key, &account, &DEVICE_B, now_ms() - 121_000),
        ),
        (
            "signed in the future",
            revoke_statement(&key, &account, &DEVICE_B, now_ms() + 121_000),
        ),
        (
            "another kind",
            b64(&AccountStatement::new(
                AccountStatementKind::SessionUpgrade,
                account,
                now_ms(),
                [0; 16],
                &DEVICE_B,
            )
            .sign(&key)),
        ),
    ];
    for (why, bad) in &refused {
        let (st, bytes) = revoke(body(Some(&fresh), &DEVICE_B, bad)).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{why}");
        assert_eq!(code(&bytes), "account-key-required", "{why}");
    }
    assert!(
        floor_of(&pool, &account, &DEVICE_B).await.is_none(),
        "no refusal wrote a floor"
    );
    assert_eq!(probe(&state, &b.session_token).await.0, StatusCode::OK);

    // (3) Malformed targets are a 400: a bad id, the all-zero id, and the
    //     caller's own device (even with a valid statement for it).
    let (st, _) = post(
        &state,
        "/account/devices/revoke",
        Some(&a.session_token),
        &json!({ "fresh_grant": fresh, "device_id_b64": "AAAA", "statement_b64": statement() }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = revoke(body(
        Some(&fresh),
        &UNBOUND,
        &revoke_statement(&key, &account, &UNBOUND, now_ms()),
    ))
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = revoke(body(
        Some(&fresh),
        &DEVICE_A,
        &revoke_statement(&key, &account, &DEVICE_A, now_ms()),
    ))
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a device cannot revoke itself");
    assert!(floor_of(&pool, &account, &DEVICE_A).await.is_none());

    // (4) Both proofs: 204, and the floor is a removal.
    let (st, _) = revoke(body(Some(&fresh), &DEVICE_B, &statement())).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, reason) = floor_of(&pool, &account, &DEVICE_B).await.expect("floor");
    assert_eq!(reason, crate::session::FLOOR_REASON_REMOVED);

    // The removed device: bearer and refresh both answer `device-revoked`.
    let (st, bytes) = probe(&state, &b.session_token).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&bytes), "device-revoked");
    let (st, bytes) = refresh(&state, &b.refresh_token).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&bytes), "device-revoked");
    // It cannot log itself out into the softer verdict, mint a genesis
    // attestation, or revoke the device that removed it.
    for (method, uri, payload) in [
        (Method::POST, "/auth/session/logout", None),
        (
            Method::POST,
            "/auth/genesis/attest",
            Some(json!({ "pubkey_b64": b64(&key.verifying_key().to_bytes()) })),
        ),
        (
            Method::POST,
            "/account/devices/revoke",
            Some(body(
                Some(&fresh),
                &DEVICE_A,
                &revoke_statement(&key, &account, &DEVICE_A, now_ms()),
            )),
        ),
    ] {
        let (st, bytes) = call(
            &state,
            method,
            uri,
            Some(&b.session_token),
            payload.as_ref(),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(code(&bytes), "device-revoked", "{uri}");
    }
    assert!(floor_of(&pool, &account, &DEVICE_A).await.is_none());

    // The removing device and every other session are unaffected.
    assert_eq!(probe(&state, &a.session_token).await.0, StatusCode::OK);
    assert_eq!(refresh(&state, &a.refresh_token).await.0, StatusCode::OK);
    let other = pair(&state, &stranger, &DEVICE_B);
    assert_eq!(
        probe(&state, &other.session_token).await.0,
        StatusCode::OK,
        "the same device id under another account is another device"
    );

    // Revoking again is idempotent and the floor never moves down.
    let (before, _) = floor_of(&pool, &account, &DEVICE_B).await.unwrap();
    tick().await;
    let (st, _) = revoke(body(Some(&fresh), &DEVICE_B, &statement())).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (after, reason) = floor_of(&pool, &account, &DEVICE_B).await.unwrap();
    assert!(after >= before);
    assert_eq!(reason, crate::session::FLOOR_REASON_REMOVED);

    // A new sign-in on that device id (the owner got the device back and
    // signed in) is issued above the floor.
    tick().await;
    let back = pair(&state, &account, &DEVICE_B);
    assert_eq!(probe(&state, &back.session_token).await.0, StatusCode::OK);
    // And its logout does not soften the removal for the old credentials.
    let (st, _) = call(
        &state,
        Method::POST,
        "/auth/session/logout",
        Some(&back.session_token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, bytes) = refresh(&state, &b.refresh_token).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&bytes), "device-revoked");
}

/// The revoke limiter is charged only after both proofs were accepted, and
/// answers 429 (never 401) past its budget.
#[tokio::test]
async fn revoke_is_limited_per_account_after_the_proofs() {
    let pool = db_or_skip!("revoke_is_limited_per_account_after_the_proofs");
    let limits = Limits::defaults().with_name_window(Scope::DeviceRevoke, Window::new(2, 3600));
    let state = build_state(pool.clone(), limits);
    let account = new_account(&pool).await;
    let key = account_key(0x41);
    let a = pair(&state, &account, &DEVICE_A);
    let fresh = grant(&state, &account);
    assert_eq!(
        record_key(&state, &a.session_token, &key).await,
        StatusCode::OK
    );

    let revoke = |device: [u8; 32], statement: String, grant: Option<String>| {
        let state = state.clone();
        let bearer = a.session_token.clone();
        async move {
            let mut v = json!({ "device_id_b64": b64(&device), "statement_b64": statement });
            if let Some(g) = grant {
                v["fresh_grant"] = json!(g);
            }
            post(&state, "/account/devices/revoke", Some(&bearer), &v).await
        }
    };

    // Refusals of either proof spend nothing: ten of each, then the budget
    // of two is still whole.
    for i in 0..10u8 {
        let device = [i + 1; 32];
        let good = revoke_statement(&key, &account, &device, now_ms());
        assert_eq!(
            revoke(device, good, None).await.0,
            StatusCode::FORBIDDEN,
            "no grant"
        );
        let forged = revoke_statement(&account_key(0x42), &account, &device, now_ms());
        assert_eq!(
            revoke(device, forged, Some(fresh.clone())).await.0,
            StatusCode::FORBIDDEN,
            "wrong key"
        );
    }
    for i in 0..2u8 {
        let device = [0xC0 + i; 32];
        let statement = revoke_statement(&key, &account, &device, now_ms());
        assert_eq!(
            revoke(device, statement, Some(fresh.clone())).await.0,
            StatusCode::NO_CONTENT
        );
    }
    let device = [0xC9; 32];
    let statement = revoke_statement(&key, &account, &device, now_ms());
    let (st, bytes) = revoke(device, statement, Some(fresh.clone())).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&bytes), "too-many-attempts");
    assert!(floor_of(&pool, &account, &device).await.is_none());
}

/// The genesis mint records the first key and afterwards signs only that
/// key; the key reset is the one writer that moves it.
#[tokio::test]
async fn the_genesis_mint_signs_only_the_recorded_key() {
    let pool = db_or_skip!("the_genesis_mint_signs_only_the_recorded_key");
    let state = build_state(pool.clone(), Limits::defaults());
    let account = new_account(&pool).await;
    let a = pair(&state, &account, &DEVICE_A);
    let first = account_key(0x51);
    let second = account_key(0x52);
    let first_pub = first.verifying_key().to_bytes();

    assert_eq!(
        crate::devices::identity_pubkey_of(&pool, &account)
            .await
            .unwrap(),
        None
    );

    // ADR-0099 F3: no proof, a proof by another key, a proof for another
    // account: 400, nothing recorded.
    for proof in [
        None,
        Some(identity_proof(&account, &second)),
        Some(identity_proof(&[0; 16], &first)),
        Some("@@".to_string()),
    ] {
        let (st, bytes) = post(
            &state,
            "/auth/genesis/attest",
            Some(&a.session_token),
            &json!({ "pubkey_b64": b64(&first_pub), "identity_proof_b64": proof }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(code(&bytes), "identity-proof-invalid");
    }
    assert_eq!(
        crate::devices::identity_pubkey_of(&pool, &account)
            .await
            .unwrap(),
        None
    );

    // The first mint records the key and returns an attestation that binds
    // the account, the derived username hash and that key.
    let (st, bytes) = post(
        &state,
        "/auth/genesis/attest",
        Some(&a.session_token),
        &json!({
            "pubkey_b64": b64(&first_pub),
            "identity_proof_b64": identity_proof(&account, &first),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let attestation = URL_SAFE_NO_PAD
        .decode(v["genesis_attest_b64"].as_str().unwrap())
        .unwrap();
    let attest_vk = lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32]).verifying;
    let username_hash = lakko_transparency::account_username_hash(&account);
    lakko_attest::verify_genesis_binding(
        &attestation,
        &attest_vk,
        &account,
        &username_hash,
        &first_pub,
    )
    .expect("the attestation binds the recorded key");
    assert_eq!(
        lakko_attest::verify_genesis_binding(
            &attestation,
            &attest_vk,
            &account,
            &username_hash,
            &second.verifying_key().to_bytes(),
        ),
        Err(lakko_attest::GenesisAttestError::PubkeyMismatch)
    );
    assert_eq!(
        crate::devices::identity_pubkey_of(&pool, &account)
            .await
            .unwrap(),
        Some(first_pub)
    );

    // The same key again: idempotent.
    assert_eq!(
        record_key(&state, &a.session_token, &first).await,
        StatusCode::OK
    );

    // Any other key: 409, from any session of the account, and the recorded
    // key does not move.
    let b = pair(&state, &account, &DEVICE_B);
    for bearer in [&a.session_token, &b.session_token] {
        let (st, bytes) = post(
            &state,
            "/auth/genesis/attest",
            Some(bearer),
            &json!({
                "pubkey_b64": b64(&second.verifying_key().to_bytes()),
                "identity_proof_b64": identity_proof(&account, &second),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(code(&bytes), "identity-key-mismatch");
    }
    assert_eq!(
        crate::devices::identity_pubkey_of(&pool, &account)
            .await
            .unwrap(),
        Some(first_pub)
    );

    // An unusable key is a 400 and records nothing, on a fresh account.
    let fresh_account = new_account(&pool).await;
    let f = pair(&state, &fresh_account, &DEVICE_A);
    let mut identity_point = [0u8; 32];
    identity_point[0] = 1;
    for bad in [b64(&identity_point), b64(&[0x22; 31]), "@@".to_string()] {
        let (st, _) = post(
            &state,
            "/auth/genesis/attest",
            Some(&f.session_token),
            &json!({ "pubkey_b64": bad }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        crate::devices::identity_pubkey_of(&pool, &fresh_account)
            .await
            .unwrap(),
        None
    );

    // ADR-0099 F3: a key another account recorded is refused, with its own
    // proof, and the second account records nothing.
    let (st, bytes) = post(
        &state,
        "/auth/genesis/attest",
        Some(&f.session_token),
        &json!({
            "pubkey_b64": b64(&first_pub),
            "identity_proof_b64": identity_proof(&fresh_account, &first),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&bytes), "identity-key-taken");
    assert_eq!(
        crate::devices::identity_pubkey_of(&pool, &fresh_account)
            .await
            .unwrap(),
        None
    );
    // The history row names the first account, and outlives it.
    let owner = crate::identity_target::owner_of(
        &pool,
        &crate::identity_target::target_of(&first_pub),
        now_ms(),
    )
    .await
    .unwrap();
    assert_eq!(owner, Some(account));

    // Two first mints racing for one account record exactly one key: one
    // answers 200 and the other 409.
    let racer = new_account(&pool).await;
    let r = pair(&state, &racer, &DEVICE_A);
    let (third, fourth) = (account_key(0x53), account_key(0x54));
    let (x, y) = tokio::join!(
        record_key(&state, &r.session_token, &third),
        record_key(&state, &r.session_token, &fourth)
    );
    let mut outcomes = [x, y];
    outcomes.sort();
    assert_eq!(outcomes, [StatusCode::OK, StatusCode::CONFLICT]);
    let recorded = crate::devices::identity_pubkey_of(&pool, &racer)
        .await
        .unwrap()
        .expect("one key recorded");
    assert!(
        recorded == third.verifying_key().to_bytes()
            || recorded == fourth.verifying_key().to_bytes(),
        "the recorded key is one of the two"
    );
    // Only the winner's target was claimed.
    let mut claimed = 0;
    for k in [&third, &fourth] {
        let owner = crate::identity_target::owner_of(
            &pool,
            &crate::identity_target::target_of(&k.verifying_key().to_bytes()),
            now_ms(),
        )
        .await
        .unwrap();
        if owner == Some(racer) {
            claimed += 1;
        }
    }
    assert_eq!(claimed, 1);
}

/// `bind-device` replaces an unbound pair with one naming the device, once.
#[tokio::test]
async fn bind_device_names_the_device_once() {
    let pool = db_or_skip!("bind_device_names_the_device_once");
    let state = build_state(pool.clone(), Limits::defaults());
    let account = new_account(&pool).await;
    let unbound = pair(&state, &account, &UNBOUND);
    let bind = |bearer: String, device: String, refresh_token: String| {
        let state = state.clone();
        async move {
            post(
                &state,
                "/auth/session/bind-device",
                Some(&bearer),
                &json!({ "device_id_b64": device, "refresh_token": refresh_token }),
            )
            .await
        }
    };
    let verify = |token: &str| {
        state
            .session_token_secret
            .verify(token, now_ms())
            .expect("access token verifies")
    };

    // Refusals: a bad id, the all-zero id, no refresh credential that
    // verifies, another account's refresh credential, an access token in the
    // refresh field.
    for bad in ["AAAA".to_string(), b64(&UNBOUND)] {
        let (st, _) = bind(
            unbound.session_token.clone(),
            bad,
            unbound.refresh_token.clone(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }
    let stranger = new_account(&pool).await;
    let strangers = pair(&state, &stranger, &UNBOUND);
    for bad in [
        "r2.nope".to_string(),
        strangers.refresh_token.clone(),
        unbound.session_token.clone(),
    ] {
        let (st, bytes) = bind(unbound.session_token.clone(), b64(&DEVICE_A), bad).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(code(&bytes), "refresh-invalid");
    }

    // The bind: a pair naming the device, at the same generation.
    let (st, bytes) = bind(
        unbound.session_token.clone(),
        b64(&DEVICE_A),
        unbound.refresh_token.clone(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let bound: Value = serde_json::from_slice(&bytes).unwrap();
    let bound_access = bound["session_token"].as_str().unwrap().to_string();
    let bound_refresh = bound["refresh_token"].as_str().unwrap().to_string();
    let claims = verify(&bound_access);
    assert_eq!(claims.account_id, account);
    assert_eq!(claims.device_id, DEVICE_A);
    assert_eq!(claims.key_generation, 1);
    assert_eq!(
        state
            .session_token_secret
            .verify_refresh(&bound_refresh, now_ms())
            .unwrap()
            .device_id,
        DEVICE_A
    );
    assert_eq!(bound["key_generation"], json!(1));

    // The refresh keeps the device.
    let (st, bytes) = refresh(&state, &bound_refresh).await;
    assert_eq!(st, StatusCode::OK);
    let refreshed: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        verify(refreshed["session_token"].as_str().unwrap()).device_id,
        DEVICE_A
    );

    // A retry with the bound pair for the same device: a fresh pair.
    let (st, _) = bind(bound_access.clone(), b64(&DEVICE_A), bound_refresh.clone()).await;
    assert_eq!(st, StatusCode::OK);
    // A bound session cannot be moved to another device, and a bound bearer
    // cannot be paired with an unbound refresh credential (or the reverse).
    for (bearer, refresh_token, device) in [
        (&bound_access, &bound_refresh, DEVICE_B),
        (&bound_access, &unbound.refresh_token, DEVICE_B),
        (&unbound.session_token, &bound_refresh, DEVICE_B),
        (&unbound.session_token, &bound_refresh, DEVICE_A),
    ] {
        let (st, bytes) = bind(bearer.clone(), b64(&device), refresh_token.clone()).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(code(&bytes), "device-already-bound");
    }

    // A bound session of a removed device cannot bind itself back.
    sqlx::query(
        "INSERT INTO device_session_floors (account_id, device_id, not_before_ms, reason) \
         VALUES (?, ?, ?, 2)",
    )
    .bind(&account[..])
    .bind(&DEVICE_A[..])
    .bind(now_ms())
    .execute(&pool)
    .await
    .unwrap();
    let (st, bytes) = bind(bound_access, b64(&DEVICE_A), bound_refresh).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&bytes), "device-revoked");
}

/// A session-landing route names the device the client asserted: the
/// device-authorize redeem with `device_id_b64`, and without it the unbound
/// id. A malformed id is refused before the single-use grant is consumed.
#[tokio::test]
async fn a_landing_route_binds_the_asserted_device() {
    let pool = db_or_skip!("a_landing_route_binds_the_asserted_device");
    let state = build_state(pool.clone(), Limits::defaults());
    let account = new_account(&pool).await;
    let mint = |tag: u8| {
        let pool = pool.clone();
        async move {
            let token = [tag; 32];
            sqlx::query(
                "INSERT INTO device_grants (account_id, grant_hash, expires_at) \
                 VALUES (?, ?, NOW(3) + INTERVAL 60 SECOND)",
            )
            .bind(&account[..])
            .bind(&Sha256::digest(token)[..])
            .execute(&pool)
            .await
            .unwrap();
            b64(&token)
        }
    };
    let redeem = |body: Value| {
        let state = state.clone();
        async move { post(&state, "/device/authorize/redeem", None, &body).await }
    };
    let device_of = |bytes: &Bytes| {
        let v: Value = serde_json::from_slice(bytes).unwrap();
        let access = state
            .session_token_secret
            .verify(v["session_token"].as_str().unwrap(), now_ms())
            .unwrap();
        let refresh = state
            .session_token_secret
            .verify_refresh(v["refresh_token"].as_str().unwrap(), now_ms())
            .unwrap();
        assert_eq!(access.device_id, refresh.device_id);
        assert_eq!(access.issued_at_ms, refresh.issued_at_ms);
        assert!(access.issued_at_ms <= now_ms() && access.issued_at_ms > now_ms() - 60_000);
        access.device_id
    };

    let grant_token = mint(0x71).await;
    // A malformed device id: 400, and the grant is still live.
    let (st, _) = redeem(json!({ "grant_token_b64": grant_token, "device_id_b64": "AAAA" })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, bytes) =
        redeem(json!({ "grant_token_b64": grant_token, "device_id_b64": b64(&DEVICE_B) })).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(device_of(&bytes), DEVICE_B);

    // Without the field the session names no device.
    let grant_token = mint(0x72).await;
    let (st, bytes) = redeem(json!({ "grant_token_b64": grant_token })).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(device_of(&bytes), UNBOUND);
}

/// A credential of an older key generation is always told
/// `key-generation-superseded`, even when a "removed" floor names its device
/// (the generation is read before the floor since sprint T3). `device-revoked`
/// makes a device wipe everything and keep no recall record, and a floor is
/// written by whoever holds the account's CURRENT identity key: after a
/// hostile reset that is the attacker, who must not be able to make the
/// owner's devices destroy the key a recall needs.
#[tokio::test]
async fn a_superseded_device_is_told_superseded_even_when_a_floor_names_it() {
    let pool = db_or_skip!("a_superseded_device_is_told_superseded_even_when_a_floor_names_it");
    let state = build_state(pool.clone(), Limits::defaults());
    let account = new_account(&pool).await;
    let a = pair(&state, &account, &DEVICE_A);
    let b = pair(&state, &account, &DEVICE_B);

    // While the generation stands, the floor is what device B meets.
    crate::session::raise_floor(
        &pool,
        &account,
        &DEVICE_B,
        now_ms(),
        crate::session::FLOOR_REASON_REMOVED,
    )
    .await
    .unwrap();
    let (st, bytes) = probe(&state, &b.session_token).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&bytes), "device-revoked");
    let (st, bytes) = refresh(&state, &b.refresh_token).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&bytes), "device-revoked");

    sqlx::query("UPDATE accounts SET identity_version = 2 WHERE id = ?")
        .bind(&account[..])
        .execute(&pool)
        .await
        .unwrap();

    // After the generation moved, both devices are superseded and nothing
    // else: the one with the floor too.
    for device in [&a, &b] {
        let (st, bytes) = probe(&state, &device.session_token).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(code(&bytes), "key-generation-superseded");
        let (st, bytes) = refresh(&state, &device.refresh_token).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(code(&bytes), "key-generation-superseded");
    }

    // The floor never moves down and a removal is never softened.
    crate::session::raise_floor(
        &pool,
        &account,
        &DEVICE_B,
        1,
        crate::session::FLOOR_REASON_LOGOUT,
    )
    .await
    .unwrap();
    let (not_before, reason) = floor_of(&pool, &account, &DEVICE_B).await.unwrap();
    assert!(not_before > 1);
    assert_eq!(reason, crate::session::FLOOR_REASON_REMOVED);

    // Deleting the account takes its floors with it.
    sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(&account[..])
        .execute(&pool)
        .await
        .unwrap();
    assert!(floor_of(&pool, &account, &DEVICE_B).await.is_none());
}
