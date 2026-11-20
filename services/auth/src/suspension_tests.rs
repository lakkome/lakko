// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests for ADR-0098 section 4: account suspension.
//!
//! The internal statement route is driven through its own router
//! ([`crate::suspension::internal_router`]) with a real moderator list
//! signed by a test root and a statement signed by a software Ed25519
//! "security key" (the assertion is built here exactly as
//! `lakko_moderation::webauthn::verify_assertion` expects it). Every other
//! route goes through the public router, so the middlewares and the status /
//! body codes are the ones a client meets.
//!
//! Gated on `LAKKO_AUTH_TEST_DATABASE_URL` (each test skips when it is
//! unset):
//!
//! ```text
//! LAKKO_AUTH_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_auth_test \
//!   cargo test -p lakko-svc-auth suspension
//! ```

#![allow(clippy::too_many_lines)]

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, StatusCode};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_attest::AccountStatementKind;
use lakko_crypto::hash::sha256;
use lakko_crypto::signing::SigningKey;
use lakko_moderation::moderators::{ModeratorEntry, ModeratorKey, ModeratorList};
use lakko_moderation::seal::moderator_report_secret;
use lakko_moderation::statement::{reason_commitment, Action, SignedStatement, Statement};
use lakko_moderation::target::Target;
use lakko_moderation::webauthn::Assertion;
use lakko_moderation::Reason;
use opaque_ke::ClientLogin;
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tower::ServiceExt as _;

use crate::opaque::LakkoCipherSuite;
use crate::recall_tests::{
    b64u, build_state, call, code, fresh_key, fresh_seed, json_of, login, login_finish,
    new_account, password_registration, pool, post, pubkey, recovery_login, reg_finish, reg_start,
    roomy, statement, takeover, text, Account,
};
use crate::session::now_ms;
use crate::suspension::{internal_router, InternalConfig, SECRET_HEADER};
use crate::AppState;

const DEVICE: [u8; 32] = [0xC3; 32];
const ROOT_SEED: [u8; 32] = [0x42; 32];
const MODERATOR_SEED: [u8; 32] = [0x17; 32];
const SECRET: [u8; 32] = [0x5E; 32];
const RP_ID: &str = "moderation.example";
/// The list version every test signs under. The stored version only grows
/// and tests share one database, so every test uses the highest version
/// there is; the stale-list test signs below it.
const V: u64 = u64::MAX;
const ORIGIN: &str = "https://moderation.example";

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

fn root_public() -> [u8; 32] {
    SigningKey::from_seed(&ROOT_SEED).verifying_key().to_bytes()
}

fn config() -> InternalConfig {
    InternalConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        secret: Arc::new(SECRET.to_vec()),
        root_key: root_public(),
    }
}

/// A moderator list of `version` with one Ed25519 moderator (id `[1; 16]`).
fn moderator_list(version: u64) -> ModeratorList {
    ModeratorList {
        list_version: version,
        issued_at_ms: 1,
        rp_id: RP_ID.into(),
        origin: ORIGIN.into(),
        entries: vec![ModeratorEntry {
            moderator_id: [1; 16],
            name: "m".into(),
            key: ModeratorKey::Ed25519(
                SigningKey::from_seed(&MODERATOR_SEED)
                    .verifying_key()
                    .to_bytes(),
            ),
            credential_id: vec![1],
            x25519_public: moderator_report_secret(&[9; 32])
                .unwrap()
                .public_key()
                .to_bytes(),
            added_at_ms: 0,
            revoked_at_ms: 0,
        }],
    }
}

/// A WebAuthn assertion by the software Ed25519 key over `challenge`, built
/// as `lakko_moderation::webauthn::verify_assertion` checks it.
fn assertion(challenge: &[u8; 32]) -> Assertion {
    let client_data_json = format!(
        r#"{{"type":"webauthn.get","challenge":"{}","origin":"{ORIGIN}","crossOrigin":false}}"#,
        URL_SAFE_NO_PAD.encode(challenge)
    )
    .into_bytes();
    let mut authenticator_data = sha256(RP_ID.as_bytes()).to_vec();
    authenticator_data.push(0x05); // user present + user verified
    authenticator_data.extend_from_slice(&[0, 0, 0, 1]);
    let mut message = authenticator_data.clone();
    message.extend_from_slice(&sha256(&client_data_json));
    Assertion {
        authenticator_data,
        client_data_json,
        signature: SigningKey::from_seed(&MODERATOR_SEED)
            .sign(&message)
            .to_bytes()
            .to_vec(),
    }
}

struct Signed {
    wire: Vec<u8>,
    statement_id: [u8; 16],
    salt: [u8; 16],
}

fn sign(
    action: Action,
    target: Target,
    reason: Reason,
    until_ms: u64,
    moderator_id: [u8; 16],
    list_version: u64,
) -> Signed {
    sign_at(
        action,
        target,
        reason,
        until_ms,
        moderator_id,
        list_version,
        u64::try_from(now_ms()).unwrap(),
    )
}

fn sign_at(
    action: Action,
    target: Target,
    reason: Reason,
    until_ms: u64,
    moderator_id: [u8; 16],
    list_version: u64,
    issued_at_ms: u64,
) -> Signed {
    let mut statement_id = [0u8; 16];
    OsRng.fill_bytes(&mut statement_id);
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let statement = Statement {
        statement_id,
        action,
        target,
        reason_commitment: reason_commitment(reason, &salt),
        until_ms,
        issued_at_ms,
        moderator_id,
        list_version,
    };
    let signed = SignedStatement {
        statement: statement.encode(),
        assertion: assertion(&statement.challenge()),
    }
    .encode()
    .unwrap();
    Signed {
        wire: signed,
        statement_id,
        salt,
    }
}

/// The statement body; it names no account key (ADR-0098 step 5), the
/// last argument only documents which account the caller means.
fn body(signed: &Signed, list: &ModeratorList, reason: Reason, _account_key: &[u8; 32]) -> Value {
    json!({
        "signed_statement_b64": B64.encode(&signed.wire),
        "moderator_list_b64": B64.encode(list.sign(&ROOT_SEED).unwrap()),
        "log_index": 7,
        "reason": reason.as_u8(),
        "reason_salt_b64": B64.encode(signed.salt),
    })
}

async fn internal(state: &AppState, secret: Option<&[u8]>, body: &Value) -> (StatusCode, Bytes) {
    internal_at(
        state,
        secret,
        Method::POST,
        "/internal/moderation/statement",
        Some(body),
    )
    .await
}

async fn internal_at(
    state: &AppState,
    secret: Option<&[u8]>,
    method: Method,
    uri: &str,
    body: Option<&Value>,
) -> (StatusCode, Bytes) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(secret) = secret {
        req = req.header(SECRET_HEADER, B64.encode(secret));
    }
    let body = body.map_or_else(Body::empty, |b| Body::from(serde_json::to_vec(b).unwrap()));
    let resp = internal_router(state.clone(), config())
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.into_body().collect().await.unwrap().to_bytes())
}

async fn suspend(state: &AppState, key: &[u8; 32], until_ms: u64) -> Signed {
    let list = moderator_list(V);
    let signed = sign(
        Action::Suspend,
        Target::account(key).unwrap(),
        Reason::Spam,
        until_ms,
        [1; 16],
        V,
    );
    let (st, bytes) = internal(
        state,
        Some(&SECRET),
        &body(&signed, &list, Reason::Spam, key),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(json_of(&bytes), json!({ "applied": true }));
    signed
}

async fn lift(state: &AppState, key: &[u8; 32]) {
    let list = moderator_list(V);
    let signed = sign(
        Action::Lift,
        Target::account(key).unwrap(),
        Reason::Spam,
        0,
        [1; 16],
        V,
    );
    let (st, bytes) = internal(
        state,
        Some(&SECRET),
        &body(&signed, &list, Reason::Spam, key),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
}

/// A password login, any outcome.
async fn password_login(
    state: &AppState,
    account: &Account,
    password: &[u8],
) -> (StatusCode, Value) {
    let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, password).unwrap();
    let (st, bytes) = post(
        state,
        "/login/start",
        None,
        &json!({ "handle": account.handle, "request": B64.encode(cl.message.serialize()) }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    let v = json_of(&bytes);
    let finalize = login_finish(cl.state, password, v["response"].as_str().unwrap()).unwrap();
    let (st, bytes) = post(
        state,
        "/login/finish",
        None,
        &json!({
            "session_id": v["session_id"],
            "finalize": finalize,
            "device_id_b64": b64u(&DEVICE),
        }),
    )
    .await;
    (st, json_of(&bytes))
}

async fn get_suspension(state: &AppState, bearer: &str) -> (StatusCode, Value) {
    let (st, bytes) = call(
        state,
        Method::GET,
        "/account/suspension",
        Some(bearer),
        None,
        None,
    )
    .await;
    (st, json_of(&bytes))
}

async fn refresh(state: &AppState, refresh_token: &str) -> (StatusCode, Value) {
    let (st, bytes) = post(
        state,
        "/auth/session/refresh",
        None,
        &json!({ "refresh_token": refresh_token }),
    )
    .await;
    (st, json_of(&bytes))
}

/// The restricted session of a suspended landing's body.
fn restricted(v: &Value) -> String {
    v["restricted_token"].as_str().unwrap().to_string()
}

/// Suspend, read, appeal, repeat, lift: the whole life of a suspension as
/// a client meets it.
#[tokio::test]
async fn a_suspension_from_statement_to_lift() {
    let pool = db_or_skip!("a_suspension_from_statement_to_lift");
    let state = build_state(pool.clone(), roomy());
    let key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &key).await;
    let before = login(&state, &account.handle, b"pw", &DEVICE)
        .await
        .expect("signs in");

    let signed = suspend(&state, &pubkey(&key), 0).await;
    let (statement_id, log_index): (Vec<u8>, u64) = sqlx::query_as(
        "SELECT statement_id, log_index FROM account_suspensions WHERE account_id = ?",
    )
    .bind(&account.id[..])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(statement_id, signed.statement_id);
    assert_eq!(log_index, 7);

    // A landing: 403 with the restricted session, a refresh credential and
    // (the password sign-in) a fresh-auth grant; never a session token.
    let (st, landing) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(landing["error"], "account-suspended");
    assert_eq!(landing["account_id_b64"], b64u(&account.id));
    assert!(landing.get("session_token").is_none());
    let s1 = restricted(&landing);
    assert!(s1.starts_with("s1."));
    assert!(landing["fresh_grant"].as_str().is_some());
    assert!(landing["refresh_token"]
        .as_str()
        .unwrap()
        .starts_with("r2."));

    // An access token from before: every bearer route says suspended, with
    // no credentials in the body.
    let bearer = before["session_token"].as_str().unwrap();
    let (st, bytes) = call(
        &state,
        Method::GET,
        "/webauthn/credentials",
        Some(bearer),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(json_of(&bytes), json!({ "error": "account-suspended" }));
    // The restricted session opens no ordinary bearer route.
    let (st, _) = call(
        &state,
        Method::GET,
        "/webauthn/credentials",
        Some(&s1),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Refresh: the same 403 with fresh credentials, for the old r2 and the
    // new one alike.
    for r2 in [
        before["refresh_token"].as_str().unwrap(),
        landing["refresh_token"].as_str().unwrap(),
    ] {
        let (st, v) = refresh(&state, r2).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert_eq!(v["error"], "account-suspended");
        assert!(restricted(&v).starts_with("s1."));
        assert!(v.get("fresh_grant").is_none());
    }

    // The suspension, as the restricted session reads it.
    let (st, v) = get_suspension(&state, &s1).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        v,
        json!({
            "suspended": true,
            "reason": Reason::Spam.as_u8(),
            "reason_name": "spam",
            "until_ms": null,
            "appeal": "none",
        })
    );

    // The appeal: shape checked, stored once.
    let (st, _) = post(
        &state,
        "/account/suspension/appeal",
        Some(&s1),
        &json!({ "sealed_b64": b64u(b"not a seal") }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let sealed = lakko_moderation::appeal::seal_appeal("please", &moderator_list(V)).unwrap();
    let appeal = json!({ "sealed_b64": B64.encode(&sealed) });
    let (st, bytes) = post(&state, "/account/suspension/appeal", Some(&s1), &appeal).await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{}", text(&bytes));
    let (st, bytes) = post(&state, "/account/suspension/appeal", Some(&s1), &appeal).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&bytes), "appeal-already-sent");
    assert_eq!(get_suspension(&state, &s1).await.1["appeal"], "sent");
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT appeal FROM account_suspensions WHERE account_id = ?")
            .bind(&account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, sealed);

    // The same statement again: a repeat, nothing changes.
    let (st, bytes) = internal(
        &state,
        Some(&SECRET),
        &body(&signed, &moderator_list(V), Reason::Spam, &pubkey(&key)),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&bytes), json!({ "applied": false, "repeat": true }));
    assert_eq!(get_suspension(&state, &s1).await.1["appeal"], "sent");

    // Lift: the restricted session reads "not suspended", the refresh
    // credential gets a normal pair, the password signs in again.
    lift(&state, &pubkey(&key)).await;
    assert_eq!(
        get_suspension(&state, &s1).await.1,
        json!({ "suspended": false })
    );
    let (st, v) = post(&state, "/account/suspension/appeal", Some(&s1), &appeal).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&v), "not-suspended");
    let (st, v) = refresh(&state, landing["refresh_token"].as_str().unwrap()).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["session_token"].as_str().unwrap().starts_with("v3."));
    assert!(login(&state, &account.handle, b"pw", &DEVICE)
        .await
        .is_some());
}

/// Every landing route refuses a suspended account, after authenticating.
#[tokio::test]
async fn every_landing_route_refuses_a_suspended_account() {
    let pool = db_or_skip!("every_landing_route_refuses_a_suspended_account");
    let state = build_state(pool.clone(), roomy());
    let key = fresh_key();
    let seed = fresh_seed();
    let account = new_account(&state, b"pw", &seed, &key).await;
    suspend(&state, &pubkey(&key), 0).await;

    // The recovery login.
    assert!(!recovery_login(&state, &account.handle, &seed).await);

    // A device grant: spent, and the new device lands suspended.
    let token = [0x6Au8; 32];
    sqlx::query(
        "INSERT INTO device_grants (account_id, grant_hash, expires_at) \
         VALUES (?, ?, NOW(3) + INTERVAL 60 SECOND)",
    )
    .bind(&account.id[..])
    .bind(&Sha256::digest(token)[..])
    .execute(&pool)
    .await
    .unwrap();
    let (st, bytes) = post(
        &state,
        "/device/authorize/redeem",
        None,
        &json!({ "grant_token_b64": b64u(&token), "device_id_b64": b64u(&DEVICE) }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let v = json_of(&bytes);
    assert_eq!(v["error"], "account-suspended");
    let verified = state
        .session_token_secret
        .verify_restricted(&restricted(&v), now_ms())
        .unwrap();
    assert_eq!(verified.account_id, account.id);
    assert_eq!(verified.device_id, DEVICE);
    assert!(v.get("fresh_grant").is_none());

    // The password reset with the phrase: refused after the phrase proof,
    // and the password envelope is unchanged.
    let envelope = password_registration(&pool, &account.id).await;
    let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, &seed).unwrap();
    let (cs, request) = reg_start(b"new");
    let (st, bytes) = post(
        &state,
        "/password/reset/start",
        None,
        &json!({
            "handle": account.handle,
            "recovery_request": B64.encode(cl.message.serialize()),
            "new_password_request": request,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    let v = json_of(&bytes);
    let finalize = login_finish(cl.state, &seed, v["recovery_response"].as_str().unwrap()).unwrap();
    let upload = reg_finish(cs, b"new", v["new_password_response"].as_str().unwrap());
    let (st, bytes) = post(
        &state,
        "/password/reset/finish",
        None,
        &json!({
            "session_id": v["session_id"],
            "recovery_finalize": finalize,
            "new_password_upload": upload,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(restricted(&json_of(&bytes)).starts_with("s1."));
    assert_eq!(password_registration(&pool, &account.id).await, envelope);

    // The password login refused before anything landed: no session token
    // was minted, and no password state changed (the next login still works
    // the same way).
    let (st, _) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

/// The restricted session signs out and deletes the account; its floor
/// applies to it like to any session.
#[tokio::test]
async fn the_restricted_session_signs_out_and_deletes() {
    let pool = db_or_skip!("the_restricted_session_signs_out_and_deletes");
    let state = build_state(pool.clone(), roomy());
    let key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &key).await;
    suspend(&state, &pubkey(&key), 0).await;

    let (_, first) = password_login(&state, &account, b"pw").await;
    let s1 = restricted(&first);
    let (st, _) = call(
        &state,
        Method::POST,
        "/auth/session/logout",
        Some(&s1),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(
        get_suspension(&state, &s1).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (st, v) = refresh(&state, first["refresh_token"].as_str().unwrap()).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"], "refresh-invalid");

    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    let (_, second) = password_login(&state, &account, b"pw").await;
    let s1 = restricted(&second);
    // Without the grant: the usual refusal.
    let (st, bytes) = call(&state, Method::DELETE, "/account", Some(&s1), None, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&bytes), "fresh-auth-required");
    let grant = second["fresh_grant"].as_str().unwrap();
    let (st, bytes) = call(
        &state,
        Method::DELETE,
        "/account",
        Some(&s1),
        Some((crate::session::FRESH_GRANT_HEADER, grant)),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{}", text(&bytes));
    let left: i64 = sqlx::query_scalar(
        "SELECT CAST(COUNT(*) AS SIGNED) FROM account_suspensions WHERE account_id = ?",
    )
    .bind(&account.id[..])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0, "the suspension row went with the account");
}

/// A suspension that has ended is no suspension, and the sweep removes it.
#[tokio::test]
async fn an_ended_suspension_is_not_one() {
    let pool = db_or_skip!("an_ended_suspension_is_not_one");
    let state = build_state(pool.clone(), roomy());
    let key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &key).await;
    let until = u64::try_from(now_ms() + 60_000).unwrap();
    suspend(&state, &pubkey(&key), until).await;
    let (st, _) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // The end passes (moved, not waited for: the suite runs slowly).
    sqlx::query("UPDATE account_suspensions SET until_ms = ? WHERE account_id = ?")
        .bind(now_ms() - 1)
        .bind(&account.id[..])
        .execute(&pool)
        .await
        .unwrap();
    let (st, _) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::OK);
    crate::suspension::sweep(&pool, now_ms()).await.unwrap();
    let left: i64 = sqlx::query_scalar(
        "SELECT CAST(COUNT(*) AS SIGNED) FROM account_suspensions WHERE account_id = ?",
    )
    .bind(&account.id[..])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

async fn recall_status(
    state: &AppState,
    key: &SigningKey,
    account: &[u8; 16],
) -> (StatusCode, Bytes) {
    post(
        state,
        "/account/recall/status",
        None,
        &json!({
            "statement_b64": statement(
                key,
                AccountStatementKind::RecallStatus,
                account,
                &[],
                now_ms(),
            ),
        }),
    )
    .await
}

/// The recall routes refuse a suspended account after the anchor check.
#[tokio::test]
async fn a_suspended_account_is_not_recalled() {
    let pool = db_or_skip!("a_suspended_account_is_not_recalled");
    let state = build_state(pool.clone(), roomy());
    let old_key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &old_key).await;
    let new_key = fresh_key();
    takeover(&state, &account, b"pw", &fresh_seed(), &new_key).await;
    assert_eq!(
        recall_status(&state, &old_key, &account.id).await.0,
        StatusCode::OK
    );
    // The account's recorded key is the new one now.
    suspend(&state, &pubkey(&new_key), 0).await;
    let (st, bytes) = recall_status(&state, &old_key, &account.id).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(json_of(&bytes), json!({ "error": "account-suspended" }));
    // A stranger's statement is still the uniform refusal.
    let (st, bytes) = post(
        &state,
        "/account/recall/status",
        None,
        &json!({
            "statement_b64": statement(
                &fresh_key(),
                AccountStatementKind::RecallStatus,
                &account.id,
                &[],
                now_ms(),
            ),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&bytes), "recall-refused");
}

/// The internal route's refusals, each before anything is written.
#[tokio::test]
async fn the_internal_route_refuses() {
    let pool = db_or_skip!("the_internal_route_refuses");
    let state = build_state(pool.clone(), roomy());
    let key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &key).await;
    let key = pubkey(&key);
    let list = moderator_list(V);
    let good = sign(
        Action::Suspend,
        Target::account(&key).unwrap(),
        Reason::Spam,
        0,
        [1; 16],
        V,
    );

    // The secret.
    let b = body(&good, &list, Reason::Spam, &key);
    assert_eq!(internal(&state, None, &b).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        internal(&state, Some(&[0u8; 32]), &b).await.0,
        StatusCode::UNAUTHORIZED
    );

    let refused = |st: StatusCode, bytes: &Bytes, want: StatusCode, want_code: &str| {
        assert_eq!(st, want, "{}", text(bytes));
        assert_eq!(code(bytes), want_code);
    };
    // A list signed by another root.
    let mut other = b.clone();
    other["moderator_list_b64"] = json!(B64.encode(list.sign(&[1; 32]).unwrap()));
    let (st, bytes) = internal(&state, Some(&SECRET), &other).await;
    refused(st, &bytes, StatusCode::BAD_REQUEST, "bad-list");
    // The wrong reason, the wrong salt.
    let (st, bytes) = internal(
        &state,
        Some(&SECRET),
        &body(&good, &list, Reason::Harassment, &key),
    )
    .await;
    refused(st, &bytes, StatusCode::BAD_REQUEST, "reason-mismatch");
    let mut salted = b.clone();
    salted["reason_salt_b64"] = json!(B64.encode([0u8; 16]));
    let (st, bytes) = internal(&state, Some(&SECRET), &salted).await;
    refused(st, &bytes, StatusCode::BAD_REQUEST, "reason-mismatch");
    // An unknown reason byte, checked after everything else.
    let mut unknown_reason = b.clone();
    unknown_reason["reason"] = json!(0);
    let (st, bytes) = internal(&state, Some(&SECRET), &unknown_reason).await;
    refused(st, &bytes, StatusCode::BAD_REQUEST, "reason-mismatch");
    // A key no account recorded.
    let stranger = pubkey(&fresh_key());
    let unknown = sign(
        Action::Suspend,
        Target::account(&stranger).unwrap(),
        Reason::Spam,
        0,
        [1; 16],
        V,
    );
    let (st, bytes) = internal(
        &state,
        Some(&SECRET),
        &body(&unknown, &list, Reason::Spam, &stranger),
    )
    .await;
    refused(st, &bytes, StatusCode::NOT_FOUND, "unknown-account");
    // Another service's action.
    let hide = sign(
        Action::HideFromSearch,
        Target::account(&key).unwrap(),
        Reason::Spam,
        0,
        [1; 16],
        V,
    );
    let (st, bytes) = internal(
        &state,
        Some(&SECRET),
        &body(&hide, &list, Reason::Spam, &key),
    )
    .await;
    refused(st, &bytes, StatusCode::BAD_REQUEST, "wrong-service");
    // A moderator not on the list.
    let unlisted = sign(
        Action::Suspend,
        Target::account(&key).unwrap(),
        Reason::Spam,
        0,
        [2; 16],
        V,
    );
    let (st, bytes) = internal(
        &state,
        Some(&SECRET),
        &body(&unlisted, &list, Reason::Spam, &key),
    )
    .await;
    refused(st, &bytes, StatusCode::FORBIDDEN, "not-authorised");
    // A broken assertion.
    let mut broken = good.wire.clone();
    let end_index = broken.len() - 1;
    broken[end_index] ^= 1;
    let mut bad_sig = b.clone();
    bad_sig["signed_statement_b64"] = json!(B64.encode(&broken));
    let (st, bytes) = internal(&state, Some(&SECRET), &bad_sig).await;
    refused(st, &bytes, StatusCode::BAD_REQUEST, "bad-statement");
    // F1: a statement signed more than a day ago, or more than five minutes
    // ahead of auth's clock.
    let now = u64::try_from(now_ms()).unwrap();
    for (issued, want) in [
        (now - 25 * 3_600_000, "stale-statement"),
        (now + 10 * 60_000, "future-statement"),
    ] {
        let s = sign_at(
            Action::Suspend,
            Target::account(&key).unwrap(),
            Reason::Spam,
            0,
            [1; 16],
            V,
            issued,
        );
        let (st, bytes) =
            internal(&state, Some(&SECRET), &body(&s, &list, Reason::Spam, &key)).await;
        refused(st, &bytes, StatusCode::BAD_REQUEST, want);
    }

    // Nothing was suspended by any of those.
    let (st, _) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::OK);
}

/// A statement under a list older than one auth has seen is refused.
#[tokio::test]
async fn an_older_list_is_refused() {
    let pool = db_or_skip!("an_older_list_is_refused");
    let state = build_state(pool.clone(), roomy());
    let key = fresh_key();
    new_account(&state, b"pw", &fresh_seed(), &key).await;
    let key = pubkey(&key);
    // The stored version is V after this (it may be already).
    lift(&state, &key).await;
    let older = sign(
        Action::Lift,
        Target::account(&key).unwrap(),
        Reason::Spam,
        0,
        [1; 16],
        V - 1,
    );
    let (st, bytes) = internal(
        &state,
        Some(&SECRET),
        &body(&older, &moderator_list(V - 1), Reason::Spam, &key),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&bytes), "stale-list");
}

/// F1: the moderation service pushes its list; auth raises its floor and
/// answers the floor. An older list is `409 stale-list`, one under another
/// root `400 bad-list`, and the route is behind the secret.
#[tokio::test]
async fn a_pushed_list_raises_the_floor() {
    let pool = db_or_skip!("a_pushed_list_raises_the_floor");
    let state = build_state(pool.clone(), roomy());
    let push = |list: Vec<u8>| {
        let state = state.clone();
        async move {
            internal_at(
                &state,
                Some(&SECRET),
                Method::POST,
                "/internal/moderation/list",
                Some(&json!({ "moderator_list_b64": B64.encode(list) })),
            )
            .await
        }
    };
    let (st, bytes) = push(moderator_list(V).sign(&ROOT_SEED).unwrap()).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(json_of(&bytes), json!({ "list_version": V }));
    let floor: u64 =
        sqlx::query_scalar("SELECT list_version FROM moderation_list_state WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(floor, V);
    let (st, bytes) = push(moderator_list(V - 1).sign(&ROOT_SEED).unwrap()).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&bytes), "stale-list");
    let (st, bytes) = push(moderator_list(V).sign(&[1; 32]).unwrap()).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(code(&bytes), "bad-list");
    let (st, _) = internal_at(
        &state,
        None,
        Method::POST,
        "/internal/moderation/list",
        Some(&json!({ "moderator_list_b64": B64.encode(moderator_list(V).sign(&ROOT_SEED).unwrap()) })),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // A statement under the floor's own list that is refused after the
    // list check leaves the floor where it is. (The floor sits at `V` in a
    // database the tests share, so a raise is not observable here; the
    // comparison itself is `lakko_moderation::apply::list_floor`, tested
    // there.)
    let key = pubkey(&fresh_key());
    let s = sign(
        Action::Suspend,
        Target::account(&key).unwrap(),
        Reason::Spam,
        0,
        [1; 16],
        V,
    );
    let mut wrong_reason = body(&s, &moderator_list(V), Reason::Spam, &key);
    wrong_reason["reason"] = json!(Reason::Hate.as_u8());
    let (st, bytes) = internal(&state, Some(&SECRET), &wrong_reason).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(code(&bytes), "reason-mismatch");
    let floor: u64 =
        sqlx::query_scalar("SELECT list_version FROM moderation_list_state WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(floor, V);
}

/// A Suspend that reaches auth after a later Lift (it was made first, but
/// auth was unreachable and it was resent) changes nothing: decisions apply
/// in the order moderators made them.
#[tokio::test]
async fn an_older_decision_resent_after_a_newer_one_changes_nothing() {
    let pool = db_or_skip!("an_older_decision_resent_after_a_newer_one_changes_nothing");
    let state = build_state(pool.clone(), roomy());
    let key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &key).await;
    let public = pubkey(&key);
    let list = moderator_list(V);
    let now = u64::try_from(now_ms()).unwrap();

    let older_suspend = sign_at(
        Action::Suspend,
        Target::account(&public).unwrap(),
        Reason::Spam,
        0,
        [1; 16],
        V,
        now - 60_000,
    );
    let newer_lift = sign_at(
        Action::Lift,
        Target::account(&public).unwrap(),
        Reason::Spam,
        0,
        [1; 16],
        V,
        now - 30_000,
    );
    let (st, bytes) = internal(
        &state,
        Some(&SECRET),
        &body(&newer_lift, &list, Reason::Spam, &public),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(json_of(&bytes), json!({ "applied": true }));

    let (st, bytes) = internal(
        &state,
        Some(&SECRET),
        &body(&older_suspend, &list, Reason::Spam, &public),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(
        json_of(&bytes),
        json!({ "applied": false, "superseded": true })
    );
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM account_suspensions WHERE account_id = ?")
            .bind(&account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rows, 0, "the older Suspend must not suspend");

    // A newer Suspend still applies.
    let (st, _) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::OK);
    suspend(&state, &public, 0).await;
    let (st, landing) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(landing["error"], "account-suspended");
}

/// The panel reads: an account by its target, the appeals waiting; a signed
/// KeepSuspension declining the appeal; and the secret gate on them.
#[tokio::test]
async fn the_panel_reads_an_account_its_appeal_and_a_keep_declines_it() {
    let pool = db_or_skip!("the_panel_reads_an_account_its_appeal_and_a_keep_declines_it");
    let state = build_state(pool.clone(), roomy());
    let key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &key).await;
    let public = pubkey(&key);
    let target = Target::account(&public).unwrap();
    let target_b64 = B64.encode(target.bytes());
    let read = |v: Value| {
        let state = state.clone();
        async move {
            internal_at(
                &state,
                Some(&SECRET),
                Method::POST,
                "/internal/moderation/account",
                Some(&v),
            )
            .await
        }
    };

    // Not suspended, a new account.
    let (st, bytes) = read(json!({ "target_b64": target_b64 })).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(
        json_of(&bytes),
        json!({ "exists": true, "age_band": "day", "suspended": false, "until_ms": null })
    );
    // An account a week and a half old.
    sqlx::query("UPDATE accounts SET created_at = NOW() - INTERVAL 10 DAY WHERE id = ?")
        .bind(&account.id[..])
        .execute(&pool)
        .await
        .unwrap();
    let until = u64::try_from(now_ms() + 3_600_000).unwrap();
    let signed = suspend(&state, &public, until).await;
    let (_, bytes) = read(json!({ "target_b64": target_b64 })).await;
    assert_eq!(
        json_of(&bytes),
        json!({
            "exists": true,
            "age_band": "month",
            "suspended": true,
            "until_ms": until,
        })
    );
    // A target nobody has, and one of the wrong length.
    let (_, bytes) = read(json!({ "target_b64": B64.encode([0xEEu8; 32]) })).await;
    assert_eq!(
        json_of(&bytes),
        json!({ "exists": false, "age_band": null, "suspended": false, "until_ms": null })
    );
    let (st, bytes) = read(json!({ "target_b64": B64.encode([1u8; 31]) })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(code(&bytes), "bad-target");

    // The appeal, then the list.
    let (_, landing) = password_login(&state, &account, b"pw").await;
    let s1 = restricted(&landing);
    let sealed = lakko_moderation::appeal::seal_appeal("please", &moderator_list(V)).unwrap();
    let (st, _) = post(
        &state,
        "/account/suspension/appeal",
        Some(&s1),
        &json!({ "sealed_b64": B64.encode(&sealed) }),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, bytes) = internal_at(
        &state,
        Some(&SECRET),
        Method::GET,
        "/internal/moderation/appeals",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    let all = json_of(&bytes);
    let mine: Vec<&Value> = all["appeals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["target_b64"] == json!(target_b64))
        .collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0]["sealed_b64"], json!(B64.encode(&sealed)));
    assert_eq!(
        mine[0]["statement_id_b64"],
        json!(B64.encode(signed.statement_id))
    );
    assert_eq!(mine[0]["log_index"], json!(7));
    assert_eq!(mine[0]["until_ms"], json!(until));
    assert!(mine[0]["appeal_at_ms"].as_i64().is_some());

    // F5: keeping the suspension is a signed KeepSuspension statement. One
    // issued before the suspension was made changes nothing; one after it
    // declines the appeal; a second finds nothing pending.
    let keep = |issued_at_ms: u64| {
        let state = state.clone();
        async move {
            let signed = sign_at(
                Action::KeepSuspension,
                Target::account(&public).unwrap(),
                Reason::Spam,
                0,
                [1; 16],
                V,
                issued_at_ms,
            );
            internal(
                &state,
                Some(&SECRET),
                &body(&signed, &moderator_list(V), Reason::Spam, &public),
            )
            .await
        }
    };
    let now = u64::try_from(now_ms()).unwrap();
    let (st, bytes) = keep(now - 60_000).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(
        json_of(&bytes),
        json!({ "applied": false, "no_pending_appeal": true })
    );
    assert_eq!(get_suspension(&state, &s1).await.1["appeal"], "sent");
    let (st, bytes) = keep(now + 1).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(json_of(&bytes), json!({ "applied": true }));
    assert_eq!(get_suspension(&state, &s1).await.1["appeal"], "declined");
    let (st, bytes) = keep(now + 2).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(
        json_of(&bytes),
        json!({ "applied": false, "no_pending_appeal": true })
    );
    // A keep changes no standing.
    let (st, _) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, bytes) = internal_at(
        &state,
        Some(&SECRET),
        Method::GET,
        "/internal/moderation/appeals",
        None,
    )
    .await;
    assert!(!json_of(&bytes)["appeals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["target_b64"] == json!(target_b64)));
    // There is no unsigned decline any more.
    let (st, _) = internal_at(
        &state,
        Some(&SECRET),
        Method::POST,
        "/internal/moderation/appeals/decline",
        Some(&json!({
            "target_b64": target_b64,
            "statement_id_b64": B64.encode(signed.statement_id),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // The reads are behind the secret too.
    let (st, _) = internal_at(
        &state,
        None,
        Method::GET,
        "/internal/moderation/appeals",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = internal_at(
        &state,
        Some(&[1u8; 32]),
        Method::POST,
        "/internal/moderation/account",
        Some(&json!({ "target_b64": target_b64 })),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

/// F3: a key change keeps the old target in the account's history. A
/// statement naming the old key's target still finds the account, as does
/// one naming the new key's; the panel reads both.
#[tokio::test]
async fn a_key_change_keeps_the_old_target() {
    let pool = db_or_skip!("a_key_change_keeps_the_old_target");
    let state = build_state(pool.clone(), roomy());
    let old_key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &old_key).await;
    let new_key = fresh_key();
    takeover(&state, &account, b"pw", &fresh_seed(), &new_key).await;
    let target: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT identity_target FROM accounts WHERE id = ?")
            .bind(&account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        target.as_deref(),
        Some(&crate::identity_target::target_of(&pubkey(&new_key))[..])
    );
    for key in [&old_key, &new_key] {
        let owner = crate::identity_target::owner_of(
            &pool,
            &crate::identity_target::target_of(&pubkey(key)),
            now_ms(),
        )
        .await
        .unwrap();
        assert_eq!(owner, Some(account.id));
    }
    let old = pubkey(&old_key);
    suspend(&state, &old, 0).await;
    let (st, _) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, bytes) = internal_at(
        &state,
        Some(&SECRET),
        Method::POST,
        "/internal/moderation/account",
        Some(&json!({ "target_b64": B64.encode(Target::account(&old).unwrap().bytes()) })),
    )
    .await;
    assert_eq!(json_of(&bytes)["suspended"], json!(true));
    lift(&state, &pubkey(&new_key)).await;
    let (st, _) = password_login(&state, &account, b"pw").await;
    assert_eq!(st, StatusCode::OK);
}

/// Kept rows of `key`'s target: `(kept, kept_until_ms)`, `None` for no row.
async fn history_row(pool: &sqlx::MySqlPool, key: &[u8; 32]) -> Option<(i64, Option<i64>)> {
    sqlx::query_as(
        "SELECT CAST(kept AS SIGNED), kept_until_ms FROM account_identity_targets \
         WHERE target = ?",
    )
    .bind(&crate::identity_target::target_of(key)[..])
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// A fresh account with no identity key, and an access token for it.
async fn keyless_account(state: &AppState) -> ([u8; 16], String) {
    let id = *uuid::Uuid::now_v7().as_bytes();
    sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&id[..])
        .execute(&state.db)
        .await
        .unwrap();
    let pair = crate::session::issue_pair_at(state, &id, 1, &DEVICE).unwrap();
    (id, pair.session_token)
}

/// The genesis mint of `key` for `id`, with its identity proof.
async fn genesis(
    state: &AppState,
    id: &[u8; 16],
    bearer: &str,
    key: &SigningKey,
) -> (StatusCode, Bytes) {
    post(
        state,
        "/auth/genesis/attest",
        Some(bearer),
        &json!({
            "pubkey_b64": b64u(&pubkey(key)),
            "identity_proof_b64": crate::recall_tests::identity_proof(id, key),
        }),
    )
    .await
}

/// F3, deletion: an account deleted while suspended keeps its identity keys
/// taken until the suspension ends (no end: for good), so another account
/// cannot record them; an account deleted while not suspended frees them in
/// the same transaction; the sweep and a claim drop a kept row whose end
/// has passed.
#[tokio::test]
async fn a_deleted_suspended_account_keeps_its_key_taken_until_the_end() {
    let pool = db_or_skip!("a_deleted_suspended_account_keeps_its_key_taken_until_the_end");
    let state = build_state(pool.clone(), roomy());

    // Suspended with no end, deleted through the restricted session.
    let key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &key).await;
    suspend(&state, &pubkey(&key), 0).await;
    let (_, landing) = password_login(&state, &account, b"pw").await;
    let (st, bytes) = call(
        &state,
        Method::DELETE,
        "/account",
        Some(&restricted(&landing)),
        Some((
            crate::session::FRESH_GRANT_HEADER,
            landing["fresh_grant"].as_str().unwrap(),
        )),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{}", text(&bytes));
    assert_eq!(history_row(&pool, &pubkey(&key)).await, Some((1, None)));
    let (other, bearer) = keyless_account(&state).await;
    let (st, bytes) = genesis(&state, &other, &bearer, &key).await;
    assert_eq!(st, StatusCode::CONFLICT, "{}", text(&bytes));
    assert_eq!(code(&bytes), "identity-key-taken");
    // A Suspend or a keep about the deleted account finds no account.
    for action in [Action::Suspend, Action::KeepSuspension] {
        let signed = sign(
            action,
            Target::account(&pubkey(&key)).unwrap(),
            Reason::Spam,
            0,
            [1; 16],
            V,
        );
        let (st, bytes) = internal(
            &state,
            Some(&SECRET),
            &body(&signed, &moderator_list(V), Reason::Spam, &pubkey(&key)),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{}", text(&bytes));
        assert_eq!(code(&bytes), "unknown-account");
    }
    // The sweep leaves a row with no end.
    crate::identity_target::sweep_kept(&pool, now_ms() + 1_000_000_000)
        .await
        .unwrap();
    assert_eq!(history_row(&pool, &pubkey(&key)).await, Some((1, None)));

    // Suspended until a time, deleted by the unclaimed-account sweep (no
    // handle, no passkey, old enough): kept until that time.
    let timed_key = fresh_key();
    let (timed, bearer) = keyless_account(&state).await;
    assert_eq!(
        genesis(&state, &timed, &bearer, &timed_key).await.0,
        StatusCode::OK
    );
    let until = u64::try_from(now_ms() + 3_600_000).unwrap();
    suspend(&state, &pubkey(&timed_key), until).await;
    sqlx::query("UPDATE accounts SET created_at = NOW() - INTERVAL 2 DAY WHERE id = ?")
        .bind(&timed[..])
        .execute(&pool)
        .await
        .unwrap();
    crate::unclaimed::prune_unclaimed(&pool, 86_400)
        .await
        .unwrap();
    assert_eq!(
        history_row(&pool, &pubkey(&timed_key)).await,
        Some((1, Some(i64::try_from(until).unwrap())))
    );
    let (third, bearer) = keyless_account(&state).await;
    let (st, bytes) = genesis(&state, &third, &bearer, &timed_key).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&bytes), "identity-key-taken");
    // Once the end has passed the key is free: a claim takes it (the kept
    // row is dropped in the same transaction).
    sqlx::query("UPDATE account_identity_targets SET kept_until_ms = ? WHERE target = ?")
        .bind(now_ms() - 1)
        .bind(&crate::identity_target::target_of(&pubkey(&timed_key))[..])
        .execute(&pool)
        .await
        .unwrap();
    let (st, bytes) = genesis(&state, &third, &bearer, &timed_key).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(
        history_row(&pool, &pubkey(&timed_key)).await,
        Some((0, None))
    );

    // The sweep drops a kept row whose end has passed, and only that one.
    let swept_key = fresh_key();
    let (swept, bearer) = keyless_account(&state).await;
    assert_eq!(
        genesis(&state, &swept, &bearer, &swept_key).await.0,
        StatusCode::OK
    );
    sqlx::query(
        "UPDATE account_identity_targets SET kept = 1, kept_until_ms = ? WHERE account_id = ?",
    )
    .bind(now_ms() - 1)
    .bind(&swept[..])
    .execute(&pool)
    .await
    .unwrap();
    crate::identity_target::sweep_kept(&pool, now_ms())
        .await
        .unwrap();
    assert_eq!(history_row(&pool, &pubkey(&swept_key)).await, None);
    assert_eq!(history_row(&pool, &pubkey(&key)).await, Some((1, None)));

    // A Lift releases the no-end account's kept keys, through the ordering
    // and replay tables: one issued before the suspension is superseded, one
    // after it releases, the same one again is a repeat.
    let lift_at = |issued_at_ms: u64| {
        sign_at(
            Action::Lift,
            Target::account(&pubkey(&key)).unwrap(),
            Reason::Spam,
            0,
            [1; 16],
            V,
            issued_at_ms,
        )
    };
    let lift_body = |signed: &Signed| body(signed, &moderator_list(V), Reason::Spam, &pubkey(&key));
    let early = lift_at(u64::try_from(now_ms() - 3_600_000).unwrap());
    let (st, bytes) = internal(&state, Some(&SECRET), &lift_body(&early)).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(
        json_of(&bytes),
        json!({ "applied": false, "superseded": true })
    );
    assert_eq!(history_row(&pool, &pubkey(&key)).await, Some((1, None)));
    let late = lift_at(u64::try_from(now_ms()).unwrap());
    let (st, bytes) = internal(&state, Some(&SECRET), &lift_body(&late)).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(
        json_of(&bytes),
        json!({ "applied": true, "released_kept": true })
    );
    assert_eq!(history_row(&pool, &pubkey(&key)).await, None);
    let (st, bytes) = internal(&state, Some(&SECRET), &lift_body(&late)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&bytes), json!({ "applied": false, "repeat": true }));
    // Released: another account may record the key now.
    let (fifth, bearer) = keyless_account(&state).await;
    assert_eq!(
        genesis(&state, &fifth, &bearer, &key).await.0,
        StatusCode::OK
    );
    // Nothing kept and no live account: a Lift is an unknown account.
    let stranger = pubkey(&fresh_key());
    let signed = sign(
        Action::Lift,
        Target::account(&stranger).unwrap(),
        Reason::Spam,
        0,
        [1; 16],
        V,
    );
    let (st, bytes) = internal(
        &state,
        Some(&SECRET),
        &body(&signed, &moderator_list(V), Reason::Spam, &stranger),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(code(&bytes), "unknown-account");

    // Not suspended: deleting the account frees its key at once.
    let free_key = fresh_key();
    let (free, bearer) = keyless_account(&state).await;
    assert_eq!(
        genesis(&state, &free, &bearer, &free_key).await.0,
        StatusCode::OK
    );
    let mut conn = pool.acquire().await.unwrap();
    let mut tx = sqlx::Acquire::begin(&mut *conn).await.unwrap();
    assert!(
        !crate::identity_target::on_account_deleted(&mut tx, &free, now_ms())
            .await
            .unwrap()
    );
    sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(&free[..])
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(history_row(&pool, &pubkey(&free_key)).await, None);
    let (fourth, bearer) = keyless_account(&state).await;
    assert_eq!(
        genesis(&state, &fourth, &bearer, &free_key).await.0,
        StatusCode::OK
    );
}
