// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests for ADR-0074 sprint T3: the recall anchors and their capture
//! rule, `POST /account/recall/{status,start,finish}`, and the deletion
//! refusal while a recall window is open.
//!
//! Every test drives the real router with the real OPAQUE client, so the
//! password proofs, the statement checks, the limiters and the status / body
//! codes are the ones a client meets. Accounts are inserted directly (signup
//! is not what is under test); everything after that goes through the routes.
//!
//! Gated on `LAKKO_AUTH_TEST_DATABASE_URL` (each test skips when it is
//! unset):
//!
//! ```text
//! LAKKO_AUTH_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_auth_test \
//!   cargo test -p lakko-svc-auth recall_tests
//! ```

#![allow(clippy::many_single_char_names, clippy::too_many_lines)]

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, StatusCode};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_attest::{
    recall_body, recall_start_body, AccountStatement, AccountStatementKind, KeyResetAttestation,
};
use lakko_crypto::signing::{SigningKey, VerifyingKey};
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse, ServerLogin,
    ServerLoginStartParameters, ServerRegistration, ServerSetup,
};
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde_json::{json, Value};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tower::ServiceExt as _;
use uuid::Uuid;

use crate::limits::{Category, Limits, Scope, Window};
use crate::opaque::{build_credential_id, LakkoCipherSuite, KIND_PASSWORD, KIND_RECOVERY};
use crate::recall::{MAX_ANCHORS, MAX_ATTEMPTS};
use crate::session::now_ms;
use crate::AppState;

pub(crate) const OWNER_DEVICE: [u8; 32] = [0xA1; 32];
const ATTACKER_DEVICE: [u8; 32] = [0xE7; 32];
pub(crate) const NEW_DEVICE: [u8; 32] = [0xB2; 32];

/// The one body every refusal that is not a wrong password carries.
const REFUSED: &str = r#"{"error":"recall-refused"}"#;

pub(crate) async fn pool() -> Option<MySqlPool> {
    let url = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL").ok()?;
    let pool = MySqlPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .expect("connect test DB");
    crate::MIGRATIONS.run(&pool).await.expect("migrations");
    Some(pool)
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

/// Limits wide enough that only the limit a test is about can trip.
pub(crate) fn roomy() -> Limits {
    Limits::defaults()
        .with_ip_window(Category::Attempt, Window::new(100_000, 60))
        .with_name_window(Scope::LoginPassword, Window::new(1000, 900))
        .with_name_window(Scope::PasswordReset, Window::new(1000, 900))
        .with_name_window(Scope::KeyReset, Window::new(1000, 900))
        .with_name_window(Scope::RecoveryReplace, Window::new(1000, 900))
        .with_name_window(Scope::Recall, Window::new(1000, 900))
}

pub(crate) fn build_state(pool: MySqlPool, limits: Limits) -> AppState {
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
        throttle: crate::throttle::ThrottleConfig::disabled(),
        backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
        anon_tokens: None,
        handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
        recovery_email: None,
        limits: Arc::new(limits),
    }
}

pub(crate) fn b64u(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A random identity key: accounts of different tests must not share one.
pub(crate) fn fresh_key() -> SigningKey {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    SigningKey::from_seed(&seed)
}

pub(crate) fn pubkey(key: &SigningKey) -> [u8; 32] {
    key.verifying_key().to_bytes()
}

pub(crate) fn fresh_seed() -> Vec<u8> {
    let mut seed = vec![0u8; 64];
    OsRng.fill_bytes(&mut seed);
    seed
}

pub(crate) async fn call(
    state: &AppState,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    extra: Option<(&str, &str)>,
    body: Option<&Value>,
) -> (StatusCode, Bytes) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some((name, value)) = extra {
        req = req.header(name, value);
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

pub(crate) async fn post(
    state: &AppState,
    uri: &str,
    bearer: Option<&str>,
    body: &Value,
) -> (StatusCode, Bytes) {
    call(state, Method::POST, uri, bearer, None, Some(body)).await
}

pub(crate) fn json_of(bytes: &Bytes) -> Value {
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

pub(crate) fn code(bytes: &Bytes) -> String {
    json_of(bytes)["error"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn text(bytes: &Bytes) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A bearer route that needs nothing but the session.
async fn probe(state: &AppState, bearer: &str) -> (StatusCode, Bytes) {
    call(
        state,
        Method::GET,
        "/webauthn/credentials",
        Some(bearer),
        None,
        None,
    )
    .await
}

// ----------------------------------------------------------------
// OPAQUE, client side
// ----------------------------------------------------------------

/// The registration record a client would have stored for `secret`.
fn registration(state: &AppState, account: &[u8; 16], kind: u8, secret: &[u8]) -> Vec<u8> {
    let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut OsRng, secret).unwrap();
    let response = ServerRegistration::<LakkoCipherSuite>::start(
        &state.opaque_setup,
        cs.message,
        &build_credential_id(account, kind),
    )
    .unwrap();
    let fin = cs
        .state
        .finish(
            &mut OsRng,
            secret,
            response.message,
            ClientRegistrationFinishParameters::default(),
        )
        .unwrap();
    ServerRegistration::<LakkoCipherSuite>::finish(fin.message)
        .serialize()
        .to_vec()
}

pub(crate) fn reg_start(secret: &[u8]) -> (ClientRegistration<LakkoCipherSuite>, String) {
    let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut OsRng, secret).unwrap();
    (cs.state, B64.encode(cs.message.serialize()))
}

pub(crate) fn reg_finish(
    cs: ClientRegistration<LakkoCipherSuite>,
    secret: &[u8],
    response_b64: &str,
) -> String {
    let response =
        RegistrationResponse::<LakkoCipherSuite>::deserialize(&B64.decode(response_b64).unwrap())
            .unwrap();
    let fin = cs
        .finish(
            &mut OsRng,
            secret,
            response,
            ClientRegistrationFinishParameters::default(),
        )
        .unwrap();
    B64.encode(fin.message.serialize())
}

/// Finish a client login; `None` when the secret is wrong (the client learns
/// that here, before it sends anything).
pub(crate) fn login_finish(
    cl: ClientLogin<LakkoCipherSuite>,
    secret: &[u8],
    response_b64: &str,
) -> Option<String> {
    let response =
        CredentialResponse::<LakkoCipherSuite>::deserialize(&B64.decode(response_b64).unwrap())
            .unwrap();
    cl.finish(secret, response, ClientLoginFinishParameters::default())
        .ok()
        .map(|fin| B64.encode(fin.message.serialize()))
}

/// A well-formed `CredentialFinalization` that proves nothing to our server:
/// the finish of an unrelated exchange.
fn unrelated_finalization() -> String {
    let setup = ServerSetup::<LakkoCipherSuite>::new(&mut OsRng);
    let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut OsRng, b"x").unwrap();
    let rs = ServerRegistration::<LakkoCipherSuite>::start(&setup, cs.message, b"id").unwrap();
    let fin = cs
        .state
        .finish(
            &mut OsRng,
            b"x",
            rs.message,
            ClientRegistrationFinishParameters::default(),
        )
        .unwrap();
    let record = ServerRegistration::<LakkoCipherSuite>::finish(fin.message);
    let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, b"x").unwrap();
    let sl = ServerLogin::<LakkoCipherSuite>::start(
        &mut OsRng,
        &setup,
        Some(record),
        cl.message,
        b"id",
        ServerLoginStartParameters::default(),
    )
    .unwrap();
    let fin = cl
        .state
        .finish(b"x", sl.message, ClientLoginFinishParameters::default())
        .unwrap();
    B64.encode(fin.message.serialize())
}

// ----------------------------------------------------------------
// Accounts and the ordinary routes
// ----------------------------------------------------------------

pub(crate) struct Account {
    pub(crate) id: [u8; 16],
    pub(crate) handle: String,
}

/// Insert an account with a password, a recovery credential, a login handle
/// and `key` recorded as its identity key (what a signup followed by the
/// genesis mint leaves).
pub(crate) async fn new_account(
    state: &AppState,
    password: &[u8],
    recovery_seed: &[u8],
    key: &SigningKey,
) -> Account {
    let id = *Uuid::now_v7().as_bytes();
    sqlx::query("INSERT INTO accounts (id, identity_pubkey) VALUES (?, ?)")
        .bind(&id[..])
        .bind(&pubkey(key)[..])
        .execute(&state.db)
        .await
        .expect("insert account");
    for (kind, secret) in [(KIND_PASSWORD, password), (KIND_RECOVERY, recovery_seed)] {
        sqlx::query(
            "INSERT INTO opaque_registrations (account_id, kind, registration_bytes) \
             VALUES (?, ?, ?)",
        )
        .bind(&id[..])
        .bind(kind)
        .bind(registration(state, &id, kind, secret))
        .execute(&state.db)
        .await
        .expect("insert registration");
    }
    let (handle_bytes, handle) = crate::login_handle::random_handle();
    crate::login_handle::bind_for_test(&state.db, &id, &handle_bytes).await;
    Account { id, handle }
}

/// A full password login. `None` when the password is wrong.
pub(crate) async fn login(
    state: &AppState,
    handle: &str,
    password: &[u8],
    device: &[u8; 32],
) -> Option<Value> {
    let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, password).unwrap();
    let (st, body) = post(
        state,
        "/login/start",
        None,
        &json!({ "handle": handle, "request": B64.encode(cl.message.serialize()) }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let v = json_of(&body);
    let finalize = login_finish(cl.state, password, v["response"].as_str().unwrap())?;
    let (st, body) = post(
        state,
        "/login/finish",
        None,
        &json!({
            "session_id": v["session_id"],
            "finalize": finalize,
            "device_id_b64": b64u(device),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    Some(json_of(&body))
}

/// A recovery-phrase login; `true` when it lands a session.
pub(crate) async fn recovery_login(state: &AppState, handle: &str, seed: &[u8]) -> bool {
    let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, seed).unwrap();
    let (st, body) = post(
        state,
        "/recovery/login/start",
        None,
        &json!({ "handle": handle, "request": B64.encode(cl.message.serialize()) }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let v = json_of(&body);
    let Some(finalize) = login_finish(cl.state, seed, v["response"].as_str().unwrap()) else {
        return false;
    };
    let (st, _) = post(
        state,
        "/recovery/login/finish",
        None,
        &json!({ "session_id": v["session_id"], "finalize": finalize }),
    )
    .await;
    st == StatusCode::OK
}

pub(crate) fn token(v: &Value) -> String {
    v["session_token"].as_str().unwrap().to_string()
}

fn refresh_token(v: &Value) -> String {
    v["refresh_token"].as_str().unwrap().to_string()
}

pub(crate) fn grant(v: &Value) -> String {
    v["fresh_grant"].as_str().unwrap().to_string()
}

/// Reset the account keys from a signed-in session with a live grant.
pub(crate) async fn reset_keys(
    state: &AppState,
    session: &Value,
    new_seed: &[u8],
    new_key: &SigningKey,
) -> (StatusCode, Bytes) {
    let bearer = token(session);
    let fresh = grant(session);
    let (cs, request) = reg_start(new_seed);
    let (st, body) = post(
        state,
        "/account/keys/reset/start",
        Some(&bearer),
        &json!({ "fresh_grant": fresh, "recovery_request": request }),
    )
    .await;
    if st != StatusCode::OK {
        return (st, body);
    }
    let upload = reg_finish(
        cs,
        new_seed,
        json_of(&body)["recovery_response"].as_str().unwrap(),
    );
    post(
        state,
        "/account/keys/reset/finish",
        Some(&bearer),
        &json!({
            "fresh_grant": fresh,
            "recovery_upload": upload,
            "new_signature_pubkey_b64": b64u(&pubkey(new_key)),
        }),
    )
    .await
}

/// Sign in with the password and reset the keys: what someone who knows only
/// the password does. Returns the session the reset answered with.
pub(crate) async fn takeover(
    state: &AppState,
    account: &Account,
    password: &[u8],
    new_seed: &[u8],
    new_key: &SigningKey,
) -> Value {
    let session = login(state, &account.handle, password, &ATTACKER_DEVICE)
        .await
        .expect("the password is right");
    let (st, body) = reset_keys(state, &session, new_seed, new_key).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    json_of(&body)
}

/// Replace the password with the recovery phrase's seed (`/password/reset`).
async fn reset_password(
    state: &AppState,
    handle: &str,
    seed: &[u8],
    new_password: &[u8],
) -> StatusCode {
    let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, seed).unwrap();
    let (cs, request) = reg_start(new_password);
    let (st, body) = post(
        state,
        "/password/reset/start",
        None,
        &json!({
            "handle": handle,
            "recovery_request": B64.encode(cl.message.serialize()),
            "new_password_request": request,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let v = json_of(&body);
    let finalize = login_finish(cl.state, seed, v["recovery_response"].as_str().unwrap())
        .expect("the seed is right");
    let upload = reg_finish(
        cs,
        new_password,
        v["new_password_response"].as_str().unwrap(),
    );
    post(
        state,
        "/password/reset/finish",
        None,
        &json!({
            "session_id": v["session_id"],
            "recovery_finalize": finalize,
            "new_password_upload": upload,
        }),
    )
    .await
    .0
}

// ----------------------------------------------------------------
// The recall, client side
// ----------------------------------------------------------------

pub(crate) fn statement(
    key: &SigningKey,
    kind: AccountStatementKind,
    account: &[u8; 16],
    body: &[u8],
    unix_ms: i64,
) -> String {
    let mut nonce = [0u8; 16];
    OsRng.fill_bytes(&mut nonce);
    b64u(&AccountStatement::new(kind, *account, unix_ms, nonce, body).sign(key))
}

async fn status(state: &AppState, key: &SigningKey, account: &[u8; 16]) -> (StatusCode, Bytes) {
    post(
        state,
        "/account/recall/status",
        None,
        &json!({
            "statement_b64": statement(key, AccountStatementKind::RecallStatus, account, &[], now_ms()),
        }),
    )
    .await
}

/// The anchor `status` names for `key`, or `None` when it is refused.
async fn anchor_seq(state: &AppState, key: &SigningKey, account: &[u8; 16]) -> Option<u64> {
    let (st, body) = status(state, key, account).await;
    if st == StatusCode::OK {
        json_of(&body)["anchor_seq"].as_u64()
    } else {
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(text(&body), REFUSED);
        None
    }
}

/// One opened attempt: what `recall/start` answered and what the client
/// keeps to finish it.
pub(crate) struct Attempt {
    pub(crate) login: ClientLogin<LakkoCipherSuite>,
    pub(crate) password_reg: ClientRegistration<LakkoCipherSuite>,
    pub(crate) recovery_reg: ClientRegistration<LakkoCipherSuite>,
    pub(crate) answer: Value,
}

async fn recall_start(
    state: &AppState,
    key: &SigningKey,
    account: &[u8; 16],
    seq: u64,
    old_password: &[u8],
    new_password: &[u8],
    new_seed: &[u8],
) -> Result<Attempt, (StatusCode, Bytes)> {
    let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, old_password).unwrap();
    let login_request = cl.message.serialize().to_vec();
    let (password_reg, password_request) = reg_start(new_password);
    let (recovery_reg, recovery_request) = reg_start(new_seed);
    let (st, body) = post(
        state,
        "/account/recall/start",
        None,
        &json!({
            "account_id_b64": b64u(account),
            "anchor_seq": seq,
            "statement_b64": statement(
                key,
                AccountStatementKind::RecallStart,
                account,
                &recall_start_body(seq, &login_request),
                now_ms(),
            ),
            "login_request": B64.encode(&login_request),
            "password_request": password_request,
            "recovery_request": recovery_request,
        }),
    )
    .await;
    if st != StatusCode::OK {
        return Err((st, body));
    }
    Ok(Attempt {
        login: cl.state,
        password_reg,
        recovery_reg,
        answer: json_of(&body),
    })
}

/// The body of `recall/finish` for an opened attempt, or `None` when the old
/// password is wrong (the client cannot finish the login).
pub(crate) fn finish_body(
    attempt: Attempt,
    key: &SigningKey,
    account: &[u8; 16],
    seq: u64,
    old_password: &[u8],
    new_password: &[u8],
    new_seed: &[u8],
    new_key: &SigningKey,
) -> Option<Value> {
    let a = &attempt.answer;
    let login_finish = login_finish(
        attempt.login,
        old_password,
        a["login_response"].as_str().unwrap(),
    )?;
    let password_upload = reg_finish(
        attempt.password_reg,
        new_password,
        a["password_response"].as_str().unwrap(),
    );
    let recovery_upload = reg_finish(
        attempt.recovery_reg,
        new_seed,
        a["recovery_response"].as_str().unwrap(),
    );
    let nonce: [u8; 32] = URL_SAFE_NO_PAD
        .decode(a["server_nonce_b64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let body = recall_body(
        &nonce,
        &pubkey(new_key),
        &B64.decode(&password_upload).unwrap(),
        &B64.decode(&recovery_upload).unwrap(),
    );
    Some(json!({
        "session_id": a["session_id"],
        "account_id_b64": b64u(account),
        "anchor_seq": seq,
        "login_finish": login_finish,
        "password_upload": password_upload,
        "recovery_upload": recovery_upload,
        "new_signature_pubkey_b64": b64u(&pubkey(new_key)),
        "device_id_b64": b64u(&NEW_DEVICE),
        "statement_b64": statement(key, AccountStatementKind::Recall, account, &body, now_ms()),
    }))
}

/// How a whole recall ended.
#[derive(Debug)]
enum Recall {
    /// `status` or `start` refused.
    Refused(StatusCode, String),
    /// The client could not finish the login: the old password is wrong.
    WrongPasswordLocally,
    /// `finish` answered.
    Finished(StatusCode, Value),
}

/// The whole recall as an old device runs it: status, start, finish.
async fn recall(
    state: &AppState,
    key: &SigningKey,
    account: &[u8; 16],
    old_password: &[u8],
    new_password: &[u8],
    new_seed: &[u8],
    new_key: &SigningKey,
) -> Recall {
    let (st, body) = status(state, key, account).await;
    if st != StatusCode::OK {
        return Recall::Refused(st, text(&body));
    }
    let seq = json_of(&body)["anchor_seq"].as_u64().unwrap();
    let attempt = match recall_start(
        state,
        key,
        account,
        seq,
        old_password,
        new_password,
        new_seed,
    )
    .await
    {
        Ok(attempt) => attempt,
        Err((st, body)) => return Recall::Refused(st, text(&body)),
    };
    let Some(body) = finish_body(
        attempt,
        key,
        account,
        seq,
        old_password,
        new_password,
        new_seed,
        new_key,
    ) else {
        return Recall::WrongPasswordLocally;
    };
    let (st, bytes) = post(state, "/account/recall/finish", None, &body).await;
    Recall::Finished(st, json_of(&bytes))
}

fn expect_recalled(outcome: Recall) -> Value {
    match outcome {
        Recall::Finished(StatusCode::OK, v) => v,
        other => panic!("expected a successful recall, got {other:?}"),
    }
}

// ----------------------------------------------------------------
// Database probes
// ----------------------------------------------------------------

/// `(seq, identity_pubkey, password_registration, expires_at_ms, attempts)`
/// of every anchor of the account, oldest first.
async fn anchors(pool: &MySqlPool, account: &[u8; 16]) -> Vec<(u64, Vec<u8>, Vec<u8>, i64, u32)> {
    sqlx::query_as(
        "SELECT seq, identity_pubkey, password_registration, expires_at_ms, attempts \
         FROM recall_anchors WHERE account_id = ? ORDER BY seq",
    )
    .bind(&account[..])
    .fetch_all(pool)
    .await
    .expect("anchors")
}

pub(crate) async fn password_registration(pool: &MySqlPool, account: &[u8; 16]) -> Vec<u8> {
    sqlx::query_scalar(
        "SELECT registration_bytes FROM opaque_registrations WHERE account_id = ? AND kind = ?",
    )
    .bind(&account[..])
    .bind(KIND_PASSWORD)
    .fetch_one(pool)
    .await
    .expect("password registration")
}

async fn identity_of(pool: &MySqlPool, account: &[u8; 16]) -> (u64, Vec<u8>) {
    sqlx::query_as("SELECT identity_version, identity_pubkey FROM accounts WHERE id = ?")
        .bind(&account[..])
        .fetch_one(pool)
        .await
        .expect("account row")
}

pub(crate) async fn count(pool: &MySqlPool, table: &str, account: &[u8; 16]) -> i64 {
    // `table` is a literal chosen by the test below, never input.
    let sql = match table {
        "totp_secrets" => "SELECT COUNT(*) FROM totp_secrets WHERE account_id = ?",
        "webauthn_credentials" => "SELECT COUNT(*) FROM webauthn_credentials WHERE account_id = ?",
        "device_grants" => "SELECT COUNT(*) FROM device_grants WHERE account_id = ?",
        "device_session_floors" => {
            "SELECT COUNT(*) FROM device_session_floors WHERE account_id = ?"
        },
        "recall_states" => "SELECT COUNT(*) FROM recall_states WHERE account_id = ?",
        "recall_outcomes" => "SELECT COUNT(*) FROM recall_outcomes WHERE account_id = ?",
        "passkey_vaults" => "SELECT COUNT(*) FROM passkey_vaults WHERE account_id = ?",
        "recall_anchors" => "SELECT COUNT(*) FROM recall_anchors WHERE account_id = ?",
        "login_handles" => "SELECT COUNT(*) FROM login_handles WHERE account_id = ?",
        other => panic!("unknown table {other}"),
    };
    sqlx::query_scalar(sql)
        .bind(&account[..])
        .fetch_one(pool)
        .await
        .expect("count")
}

/// What an attacker who holds the account adds to keep it: an authenticator,
/// a passkey, a device grant, a floor on the owner's device.
async fn plant_attacker_factors(pool: &MySqlPool, account: &[u8; 16]) {
    sqlx::query(
        "INSERT INTO totp_secrets (account_id, secret_enc, confirmed_at, last_accepted_step) \
         VALUES (?, ?, NOW(3), 0)",
    )
    .bind(&account[..])
    .bind(&[0x5Eu8; 48][..])
    .execute(pool)
    .await
    .expect("totp");
    let mut credential_id = b"recall-test-".to_vec();
    credential_id.extend_from_slice(account);
    sqlx::query(
        "INSERT INTO webauthn_credentials (credential_id, account_id, passkey_bytes) \
         VALUES (?, ?, ?)",
    )
    .bind(&credential_id)
    .bind(&account[..])
    .bind(&b"opaque"[..])
    .execute(pool)
    .await
    .expect("passkey");
    let mut grant_hash = [0x6Du8; 32];
    grant_hash[..16].copy_from_slice(account);
    sqlx::query(
        "INSERT INTO device_grants (account_id, grant_hash, expires_at) \
         VALUES (?, ?, NOW(3) + INTERVAL 300 SECOND)",
    )
    .bind(&account[..])
    .bind(&grant_hash[..])
    .execute(pool)
    .await
    .expect("device grant");
    crate::session::raise_floor(
        pool,
        account,
        &OWNER_DEVICE,
        now_ms(),
        crate::session::FLOOR_REASON_REMOVED,
    )
    .await
    .expect("floor");
}

async fn expire_anchors(pool: &MySqlPool, account: &[u8; 16]) {
    sqlx::query("UPDATE recall_anchors SET expires_at_ms = ? WHERE account_id = ?")
        .bind(now_ms() - 1)
        .bind(&account[..])
        .execute(pool)
        .await
        .expect("expire");
}

// ----------------------------------------------------------------
// The capture rule
// ----------------------------------------------------------------

/// Concurrent changes under one identity key capture exactly one anchor,
/// holding the password as it was before the first of them, and no later
/// change (another password change, a key reset) overwrites it. A change
/// under the NEXT identity key captures its own anchor above it.
#[tokio::test]
async fn concurrent_changes_capture_one_anchor_and_nothing_overwrites_it() {
    let pool = db_or_skip!("concurrent_changes_capture_one_anchor_and_nothing_overwrites_it");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let seed0 = fresh_seed();
    let account = new_account(&state, b"password zero", &seed0, &k0).await;
    let original = password_registration(&pool, &account.id).await;
    assert!(anchors(&pool, &account.id).await.is_empty());

    // Six password changes at once, all proving the same recovery phrase.
    let changes = (0u8..6).map(|i| {
        let state = state.clone();
        let handle = account.handle.clone();
        let seed = seed0.clone();
        tokio::spawn(async move {
            reset_password(&state, &handle, &seed, format!("password {i}").as_bytes()).await
        })
    });
    for change in changes.collect::<Vec<_>>() {
        assert_eq!(change.await.unwrap(), StatusCode::OK);
    }

    let rows = anchors(&pool, &account.id).await;
    assert_eq!(rows.len(), 1, "one anchor for one identity key");
    let (seq, key, registration, expires_first, attempts) = rows[0].clone();
    assert_eq!(seq, 1);
    assert_eq!(key, pubkey(&k0));
    assert_eq!(
        registration, original,
        "the anchor holds the password from before the first change"
    );
    assert_eq!(attempts, 0);
    let window = state.limits.recall_window_ms();
    assert!(expires_first > now_ms() + window - 60_000 && expires_first <= now_ms() + window);
    assert_ne!(password_registration(&pool, &account.id).await, original);

    // A seventh change, later: still one anchor, byte for byte the same.
    assert_eq!(
        reset_password(&state, &account.handle, &seed0, b"password seven").await,
        StatusCode::OK
    );
    assert_eq!(anchors(&pool, &account.id).await, rows);

    // A key reset under the same identity key: the anchor's contents stand
    // and only its expiry may move out.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let k1 = fresh_key();
    let seed1 = fresh_seed();
    takeover(&state, &account, b"password seven", &seed1, &k1).await;
    let rows = anchors(&pool, &account.id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, pubkey(&k0));
    assert_eq!(rows[0].2, original);
    assert!(rows[0].3 >= expires_first);
    assert_eq!(identity_of(&pool, &account.id).await.1, pubkey(&k1));

    // The first change under the NEW key captures its own anchor, above the
    // first, with the password in force at that moment.
    let before_second = password_registration(&pool, &account.id).await;
    assert_eq!(
        reset_password(&state, &account.handle, &seed1, b"password eight").await,
        StatusCode::OK
    );
    let rows = anchors(&pool, &account.id).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].0, &rows[0].1, &rows[0].2),
        (1, &pubkey(&k0).to_vec(), &original)
    );
    assert_eq!(
        (rows[1].0, &rows[1].1, &rows[1].2),
        (2, &pubkey(&k1).to_vec(), &before_second)
    );
}

/// An account with no recorded identity key captures nothing, and past the
/// cap a change captures nothing while the older anchors stand.
#[tokio::test]
async fn no_recorded_key_and_the_cap_capture_nothing() {
    let pool = db_or_skip!("no_recorded_key_and_the_cap_capture_nothing");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let seed0 = fresh_seed();
    let account = new_account(&state, b"pw", &seed0, &k0).await;
    sqlx::query("UPDATE accounts SET identity_pubkey = NULL WHERE id = ?")
        .bind(&account.id[..])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        reset_password(&state, &account.handle, &seed0, b"pw2").await,
        StatusCode::OK
    );
    assert!(anchors(&pool, &account.id).await.is_empty());

    // Fill the list by hand, then change the password under a ninth key.
    sqlx::query("UPDATE accounts SET identity_pubkey = ? WHERE id = ?")
        .bind(&pubkey(&k0)[..])
        .bind(&account.id[..])
        .execute(&pool)
        .await
        .unwrap();
    for seq in 1..=u64::try_from(MAX_ANCHORS).unwrap() {
        sqlx::query(
            "INSERT INTO recall_anchors \
                (account_id, seq, identity_pubkey, password_registration, key_generation, \
                 created_at_ms, expires_at_ms) VALUES (?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(&account.id[..])
        .bind(seq)
        .bind(&pubkey(&fresh_key())[..])
        .bind(&b"older"[..])
        .bind(now_ms())
        .bind(now_ms() + 86_400_000)
        .execute(&pool)
        .await
        .unwrap();
    }
    let before = anchors(&pool, &account.id).await;
    assert_eq!(
        reset_password(&state, &account.handle, &seed0, b"pw3").await,
        StatusCode::OK
    );
    assert_eq!(anchors(&pool, &account.id).await, before);
}

// ----------------------------------------------------------------
// The recall
// ----------------------------------------------------------------

/// Scenario 1 of the ADR. Someone who knows only the password signs in,
/// resets the keys, changes the password and adds factors of their own. The
/// owner, on a device from before, takes the account back with the old key
/// and the old password; afterwards nothing the attacker held works.
#[tokio::test]
async fn the_owner_recalls_after_a_password_only_takeover() {
    let pool = db_or_skip!("the_owner_recalls_after_a_password_only_takeover");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let seed0 = fresh_seed();
    let account = new_account(&state, b"the old password", &seed0, &k0).await;
    let owner = login(&state, &account.handle, b"the old password", &OWNER_DEVICE)
        .await
        .unwrap();
    assert_eq!(probe(&state, &token(&owner)).await.0, StatusCode::OK);
    // Before any change there is nothing to recall.
    assert_eq!(anchor_seq(&state, &k0, &account.id).await, None);

    // The takeover: reset the keys, then replace the password with the new
    // phrase, then plant factors.
    let ka = fresh_key();
    let seed_a = fresh_seed();
    let attacker = takeover(&state, &account, b"the old password", &seed_a, &ka).await;
    assert_eq!(attacker["key_generation"], 2);
    assert_eq!(
        reset_password(&state, &account.handle, &seed_a, b"attacker password").await,
        StatusCode::OK
    );
    plant_attacker_factors(&pool, &account.id).await;
    let attacker = login(
        &state,
        &account.handle,
        b"attacker password",
        &ATTACKER_DEVICE,
    )
    .await
    .expect("the attacker's password works now");
    assert!(
        login(&state, &account.handle, b"the old password", &OWNER_DEVICE)
            .await
            .is_none()
    );

    // The owner's device: superseded, never "revoked", although the attacker
    // put a "removed" floor on it. This is what lets it keep its recall key.
    let (st, body) = probe(&state, &token(&owner)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&body), "key-generation-superseded");
    let (st, body) = post(
        &state,
        "/auth/session/refresh",
        None,
        &json!({ "refresh_token": refresh_token(&owner) }),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&body), "key-generation-superseded");

    // The attacker cannot delete the account while the window is open.
    let (st, body) = call(
        &state,
        Method::DELETE,
        "/account",
        Some(&token(&attacker)),
        Some(("x-lakko-fresh-grant", &grant(&attacker))),
        None,
    )
    .await;
    // (The planted passkey makes this the action-grant branch: refused there
    // already. The no-passkey branch is its own test below.)
    assert_eq!(st, StatusCode::FORBIDDEN, "{}", text(&body));

    // Status, for the old key only.
    let (st, body) = status(&state, &k0, &account.id).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let s = json_of(&body);
    assert_eq!(s["anchor_seq"], 1);
    assert_eq!(s["attempts_remaining"], MAX_ATTEMPTS);
    assert!(s["changed_at_ms"].as_i64().unwrap() <= now_ms());
    assert!(s["expires_at_ms"].as_i64().unwrap() > now_ms() + 29 * 86_400_000);
    // The attacker's keys have standing only below the owner's: KA has an
    // anchor of its own (captured at the password change) and it is seq 2.
    assert_eq!(anchor_seq(&state, &ka, &account.id).await, Some(2));

    // A password login the attacker has in flight while the recall runs.
    let in_flight =
        ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, b"attacker password").unwrap();
    let (st, body) = post(
        &state,
        "/login/start",
        None,
        &json!({
            "handle": account.handle,
            "request": B64.encode(in_flight.message.serialize()),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let in_flight_start = json_of(&body);
    let in_flight_finalize = login_finish(
        in_flight.state,
        b"attacker password",
        in_flight_start["response"].as_str().unwrap(),
    )
    .unwrap();

    // The recall.
    let k1 = fresh_key();
    let seed1 = fresh_seed();
    let recalled = expect_recalled(
        recall(
            &state,
            &k0,
            &account.id,
            b"the old password",
            b"the owner's new password",
            &seed1,
            &k1,
        )
        .await,
    );
    assert_eq!(recalled["key_generation"], 3);
    let attestation = URL_SAFE_NO_PAD
        .decode(recalled["key_reset_attest_b64"].as_str().unwrap())
        .unwrap();
    let attest_key =
        VerifyingKey::from_bytes(&state.attest_key.signing.verifying_key().to_bytes()).unwrap();
    let claims = KeyResetAttestation::open(&attestation, &attest_key).expect("attestation");
    assert_eq!(claims.key_generation, 3);
    assert_eq!(claims.new_signature_pubkey, pubkey(&k1));
    assert_eq!(
        claims,
        KeyResetAttestation::for_account(&account.id, 3, pubkey(&k1), claims.signed_at_unix_millis)
    );

    // The account is the owner's: new identity key, new generation, the
    // session and the grant the recall answered with both work.
    let (generation, identity) = identity_of(&pool, &account.id).await;
    assert_eq!(generation, 3);
    assert_eq!(identity, pubkey(&k1));
    assert_eq!(probe(&state, &token(&recalled)).await.0, StatusCode::OK);
    let (st, body) = post(
        &state,
        "/device/authorize/start",
        Some(&token(&recalled)),
        &json!({ "fresh_grant": grant(&recalled) }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));

    // Everything the attacker held is dead.
    for credential in [token(&attacker)] {
        let (st, body) = probe(&state, &credential).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(code(&body), "key-generation-superseded");
    }
    let (st, body) = post(
        &state,
        "/auth/session/refresh",
        None,
        &json!({ "refresh_token": refresh_token(&attacker) }),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&body), "key-generation-superseded");
    let (st, _) = post(
        &state,
        "/account/keys/reset/start",
        Some(&token(&attacker)),
        &json!({ "fresh_grant": grant(&attacker), "recovery_request": reg_start(b"x").1 }),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // The login that was in flight cannot finish.
    let (st, _) = post(
        &state,
        "/login/finish",
        None,
        &json!({
            "session_id": in_flight_start["session_id"],
            "finalize": in_flight_finalize,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    for dead in [&b"attacker password"[..], b"the old password"] {
        assert!(
            login(&state, &account.handle, dead, &ATTACKER_DEVICE)
                .await
                .is_none(),
            "a displaced password still signs in"
        );
    }
    assert!(!recovery_login(&state, &account.handle, &seed_a).await);
    assert!(!recovery_login(&state, &account.handle, &seed0).await);
    for table in [
        "totp_secrets",
        "webauthn_credentials",
        "device_session_floors",
        "recall_states",
    ] {
        assert_eq!(count(&pool, table, &account.id).await, 0, "{table}");
    }
    // The attacker's planted device grant is gone; the one row is the grant
    // the owner minted a moment ago.
    assert_eq!(count(&pool, "device_grants", &account.id).await, 1);
    // The login handle survives: the owner signs in by name.
    assert_eq!(count(&pool, "login_handles", &account.id).await, 1);
    assert!(login(
        &state,
        &account.handle,
        b"the owner's new password",
        &NEW_DEVICE
    )
    .await
    .is_some());
    assert!(recovery_login(&state, &account.handle, &seed1).await);

    // No standing is left to anybody: the owner's anchor was consumed, the
    // attacker's was above it, and the recall captured none.
    assert!(anchors(&pool, &account.id).await.is_empty());
    for key in [&k0, &ka, &k1] {
        assert_eq!(anchor_seq(&state, key, &account.id).await, None);
    }
    match recall(
        &state,
        &ka,
        &account.id,
        b"the old password",
        b"again",
        &fresh_seed(),
        &fresh_key(),
    )
    .await
    {
        Recall::Refused(StatusCode::UNAUTHORIZED, body) => assert_eq!(body, REFUSED),
        other => panic!("the displaced party recalled back: {other:?}"),
    }
}

/// The party a recall displaced cannot answer it, an attacker who resets
/// twice cannot make their own key the winning one, and an older standing
/// outranks a newer one (ADR-0074 D1).
#[tokio::test]
async fn a_double_resetting_attacker_cannot_lock_the_owner_out() {
    let pool = db_or_skip!("a_double_resetting_attacker_cannot_lock_the_owner_out");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"pw zero", &fresh_seed(), &k0).await;

    // Two resets by the attacker: KA1, then KA2.
    let ka1 = fresh_key();
    let ka2 = fresh_key();
    takeover(&state, &account, b"pw zero", &fresh_seed(), &ka1).await;
    takeover(&state, &account, b"pw zero", &fresh_seed(), &ka2).await;
    let rows = anchors(&pool, &account.id).await;
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].0, &rows[0].1), (1, &pubkey(&k0).to_vec()));
    assert_eq!((rows[1].0, &rows[1].1), (2, &pubkey(&ka1).to_vec()));
    // The current key has no anchor: it changed nothing yet.
    assert_eq!(anchor_seq(&state, &ka2, &account.id).await, None);

    // The attacker recalls through their own first key. That is allowed and
    // is NOT final: it removes their own anchor and leaves the owner's.
    let ka3 = fresh_key();
    let v = expect_recalled(
        recall(
            &state,
            &ka1,
            &account.id,
            b"pw zero",
            b"attacker pw",
            &fresh_seed(),
            &ka3,
        )
        .await,
    );
    assert_eq!(v["key_generation"], 4);
    let rows = anchors(&pool, &account.id).await;
    assert_eq!(rows.len(), 1, "the recall captured no anchor");
    assert_eq!((rows[0].0, &rows[0].1), (1, &pubkey(&k0).to_vec()));

    // The owner still recalls, with the old key and the password from before
    // the first change, although the live password is now the attacker's.
    let k1 = fresh_key();
    let v = expect_recalled(
        recall(
            &state,
            &k0,
            &account.id,
            b"pw zero",
            b"owner pw",
            &fresh_seed(),
            &k1,
        )
        .await,
    );
    assert_eq!(v["key_generation"], 5);
    assert_eq!(identity_of(&pool, &account.id).await.1, pubkey(&k1));
    assert!(anchors(&pool, &account.id).await.is_empty());
    for key in [&ka1, &ka2, &ka3, &k0] {
        assert_eq!(anchor_seq(&state, key, &account.id).await, None);
    }
    assert!(login(&state, &account.handle, b"owner pw", &NEW_DEVICE)
        .await
        .is_some());
    assert!(
        login(&state, &account.handle, b"attacker pw", &ATTACKER_DEVICE)
            .await
            .is_none()
    );

    // A change the owner makes afterwards opens a new window for the
    // owner's key, numbered from 1 again: nothing older is left to outrank.
    let again = login(&state, &account.handle, b"owner pw", &NEW_DEVICE)
        .await
        .unwrap();
    let (st, _) = reset_keys(&state, &again, &fresh_seed(), &fresh_key()).await;
    assert_eq!(st, StatusCode::OK);
    let rows = anchors(&pool, &account.id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].0, &rows[0].1), (1, &pubkey(&k1).to_vec()));
}

/// The honest sequence of the ADR: the owner resets because the phone was
/// stolen; later someone who knows the password resets again. The owner
/// recalls through the anchor of their own reset identity. The thief of the
/// old phone, who holds the first anchor's key and not the password, gets
/// nowhere before or after.
#[tokio::test]
async fn an_honest_reset_stays_recallable_and_the_old_phone_alone_is_useless() {
    let pool = db_or_skip!("an_honest_reset_stays_recallable_and_the_old_phone_alone_is_useless");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key(); // on the stolen phone
    let account = new_account(&state, b"shared pw", &fresh_seed(), &k0).await;

    // The owner, on a new phone, resets.
    let k1 = fresh_key();
    let seed1 = fresh_seed();
    takeover(&state, &account, b"shared pw", &seed1, &k1).await;

    // The thief of the old phone: the statement verifies (they hold K0), the
    // password does not.
    let seq = anchor_seq(&state, &k0, &account.id)
        .await
        .expect("anchor for K0");
    assert_eq!(seq, 1);
    let attempt = recall_start(
        &state,
        &k0,
        &account.id,
        seq,
        b"a guess",
        b"n",
        &fresh_seed(),
    )
    .await
    .expect("the start is answered to the key holder");
    assert!(
        finish_body(
            attempt,
            &k0,
            &account.id,
            seq,
            b"a guess",
            b"n",
            &fresh_seed(),
            &fresh_key()
        )
        .is_none(),
        "a wrong password cannot finish the login"
    );
    assert_eq!(identity_of(&pool, &account.id).await.1, pubkey(&k1));

    // Ten days later someone who knows the password resets again.
    let k2 = fresh_key();
    takeover(&state, &account, b"shared pw", &fresh_seed(), &k2).await;
    let rows = anchors(&pool, &account.id).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].1, pubkey(&k1));

    // The owner recalls through the anchor of their reset identity.
    let k3 = fresh_key();
    let v = expect_recalled(
        recall(
            &state,
            &k1,
            &account.id,
            b"shared pw",
            b"owner pw",
            &fresh_seed(),
            &k3,
        )
        .await,
    );
    assert_eq!(v["key_generation"], 4);
    assert_eq!(identity_of(&pool, &account.id).await.1, pubkey(&k3));

    // The older standing (the stolen phone's key) is still on record, as the
    // ordering rule says, and is still useless without the password.
    let rows = anchors(&pool, &account.id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].0, &rows[0].1), (1, &pubkey(&k0).to_vec()));
    let attempt = recall_start(
        &state,
        &k0,
        &account.id,
        1,
        b"another guess",
        b"n",
        &fresh_seed(),
    )
    .await
    .unwrap();
    assert!(finish_body(
        attempt,
        &k0,
        &account.id,
        1,
        b"another guess",
        b"n",
        &fresh_seed(),
        &fresh_key()
    )
    .is_none());
    assert_eq!(identity_of(&pool, &account.id).await.1, pubkey(&k3));
}

// ----------------------------------------------------------------
// Refusals, budgets, replay
// ----------------------------------------------------------------

/// Without a statement by a live anchor's key every route answers one body,
/// whatever is wrong, and nothing is counted: not the per-anchor budget, not
/// the anchor's attempts. A stranger who knows the account id cannot lock
/// the owner out.
#[tokio::test]
async fn every_refusal_without_the_anchor_key_is_the_same_and_spends_nothing() {
    let pool = db_or_skip!("every_refusal_without_the_anchor_key_is_the_same_and_spends_nothing");
    // The per-anchor budget is ONE attempt: if anything below charged it, the
    // owner's start at the end would be a 429.
    let state = build_state(
        pool.clone(),
        roomy().with_name_window(Scope::Recall, Window::new(1, 900)),
    );
    let k0 = fresh_key();
    let stranger = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &k0).await;
    let untouched = new_account(&state, b"pw", &fresh_seed(), &fresh_key()).await;
    let unknown = *Uuid::now_v7().as_bytes();
    takeover(&state, &account, b"pw", &fresh_seed(), &fresh_key()).await;
    assert_eq!(anchor_seq(&state, &k0, &account.id).await, Some(1));

    let mut answers: Vec<(String, StatusCode, String)> = Vec::new();
    let mut record = |what: &str, (st, body): (StatusCode, Bytes)| {
        answers.push((what.to_string(), st, text(&body)));
    };

    // --- status
    let status_with = |statement_b64: String| {
        let state = state.clone();
        async move {
            post(
                &state,
                "/account/recall/status",
                None,
                &json!({ "statement_b64": statement_b64 }),
            )
            .await
        }
    };
    let st_kind = AccountStatementKind::RecallStatus;
    record(
        "status: unknown account",
        status_with(statement(&k0, st_kind, &unknown, &[], now_ms())).await,
    );
    record(
        "status: account with no anchor",
        status_with(statement(&k0, st_kind, &untouched.id, &[], now_ms())).await,
    );
    record(
        "status: another key",
        status_with(statement(&stranger, st_kind, &account.id, &[], now_ms())).await,
    );
    // A statement with a wrong clock by someone who does NOT hold the
    // anchor's key is refused like any other. (By the key's holder it is
    // told the server's time: `key_rotate_tests`, sprint T5.)
    record(
        "status: stale, another key",
        status_with(statement(
            &stranger,
            st_kind,
            &account.id,
            &[],
            now_ms() - 200_000,
        ))
        .await,
    );
    record(
        "status: from the future, another key",
        status_with(statement(
            &stranger,
            st_kind,
            &account.id,
            &[],
            now_ms() + 200_000,
        ))
        .await,
    );
    record(
        "status: another kind",
        status_with(statement(
            &k0,
            AccountStatementKind::DeviceRevoke,
            &account.id,
            &[],
            now_ms(),
        ))
        .await,
    );
    record(
        "status: a body it should not cover",
        status_with(statement(&k0, st_kind, &account.id, b"x", now_ms())).await,
    );
    record("status: not base64", status_with("!!!".into()).await);
    record("status: too short", status_with(b64u(&[1u8; 137])).await);
    record("status: all zero", status_with(b64u(&[0u8; 138])).await);
    record("status: empty", status_with(String::new()).await);

    // --- start
    let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, b"pw").unwrap();
    let login_request = cl.message.serialize().to_vec();
    let other_request = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, b"pw")
        .unwrap()
        .message
        .serialize()
        .to_vec();
    let start_with = |account_id: [u8; 16], seq: u64, statement_b64: String| {
        let state = state.clone();
        let login_request = login_request.clone();
        async move {
            post(
                &state,
                "/account/recall/start",
                None,
                &json!({
                    "account_id_b64": b64u(&account_id),
                    "anchor_seq": seq,
                    "statement_b64": statement_b64,
                    "login_request": B64.encode(&login_request),
                    "password_request": reg_start(b"n").1,
                    "recovery_request": reg_start(b"s").1,
                }),
            )
            .await
        }
    };
    let start_kind = AccountStatementKind::RecallStart;
    let good_body = recall_start_body(1, &login_request);
    for round in 0..3 {
        record(
            &format!("start: another key ({round})"),
            start_with(
                account.id,
                1,
                statement(&stranger, start_kind, &account.id, &good_body, now_ms()),
            )
            .await,
        );
    }
    record(
        "start: unknown account",
        start_with(
            unknown,
            1,
            statement(&k0, start_kind, &unknown, &good_body, now_ms()),
        )
        .await,
    );
    record(
        "start: account with no anchor",
        start_with(
            untouched.id,
            1,
            statement(&k0, start_kind, &untouched.id, &good_body, now_ms()),
        )
        .await,
    );
    record(
        "start: an anchor that does not exist",
        start_with(
            account.id,
            2,
            statement(
                &k0,
                start_kind,
                &account.id,
                &recall_start_body(2, &login_request),
                now_ms(),
            ),
        )
        .await,
    );
    record(
        "start: statement for another anchor",
        start_with(
            account.id,
            1,
            statement(
                &k0,
                start_kind,
                &account.id,
                &recall_start_body(2, &login_request),
                now_ms(),
            ),
        )
        .await,
    );
    record(
        "start: statement over another login request",
        start_with(
            account.id,
            1,
            statement(
                &k0,
                start_kind,
                &account.id,
                &recall_start_body(1, &other_request),
                now_ms(),
            ),
        )
        .await,
    );
    record(
        "start: a status statement",
        start_with(
            account.id,
            1,
            statement(&k0, st_kind, &account.id, &[], now_ms()),
        )
        .await,
    );
    record(
        "start: stale, another key",
        start_with(
            account.id,
            1,
            statement(
                &stranger,
                start_kind,
                &account.id,
                &good_body,
                now_ms() - 200_000,
            ),
        )
        .await,
    );
    record(
        "start: statement for another account",
        start_with(
            account.id,
            1,
            statement(&k0, start_kind, &untouched.id, &good_body, now_ms()),
        )
        .await,
    );
    record(
        "start: not base64",
        start_with(account.id, 1, "!!!".into()).await,
    );

    // --- finish, with no attempt opened by the caller
    let no_session = Uuid::now_v7().to_string();
    record(
        "finish: unknown session, the anchor's key",
        finish_without_attempt(&state, &no_session, &k0, account.id).await,
    );
    record(
        "finish: unknown session, another key",
        finish_without_attempt(&state, &no_session, &stranger, account.id).await,
    );
    record(
        "finish: unknown account",
        finish_without_attempt(&state, &no_session, &k0, unknown).await,
    );
    record(
        "finish: account with no anchor",
        finish_without_attempt(&state, &no_session, &k0, untouched.id).await,
    );

    for (what, st, body) in &answers {
        assert_eq!(*st, StatusCode::UNAUTHORIZED, "{what}");
        assert_eq!(body, REFUSED, "{what}");
    }
    assert_eq!(answers.len(), 27);

    // Nothing was counted.
    let rows = anchors(&pool, &account.id).await;
    assert_eq!(rows[0].4, 0, "no attempt was opened");
    assert_eq!(count(&pool, "recall_states", &account.id).await, 0);
    let (st, body) = status(&state, &k0, &account.id).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body)["attempts_remaining"], MAX_ATTEMPTS);

    // The owner's one attempt is still there (the budget is 1 per window),
    // and the second is the limiter's answer, not a refusal.
    let attempt = recall_start(&state, &k0, &account.id, 1, b"pw", b"new", &fresh_seed()).await;
    assert!(attempt.is_ok(), "a stranger spent the owner's budget");
    match recall_start(&state, &k0, &account.id, 1, b"pw", b"new", &fresh_seed()).await {
        Err((st, body)) => {
            assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(code(&body), "too-many-attempts");
            assert!(json_of(&body)["retry_after_secs"].as_u64().unwrap() >= 1);
        },
        Ok(_) => panic!("the per-anchor budget did not hold"),
    }
    // The limited start was not counted against the anchor's attempts.
    assert_eq!(anchors(&pool, &account.id).await[0].4, 1);

    // Malformed OPAQUE messages are a 400 and name no account.
    let (st, _) = post(
        &state,
        "/account/recall/start",
        None,
        &json!({
            "account_id_b64": b64u(&unknown),
            "anchor_seq": 1,
            "statement_b64": "x",
            "login_request": "AAAA",
            "password_request": reg_start(b"n").1,
            "recovery_request": reg_start(b"s").1,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

/// A well-formed `recall/finish` for a session nobody opened.
async fn finish_without_attempt(
    state: &AppState,
    session_id: &str,
    signer: &SigningKey,
    account_id: [u8; 16],
) -> (StatusCode, Bytes) {
    let password_upload = registration_upload(state, &account_id, KIND_PASSWORD, b"n");
    let recovery_upload = registration_upload(state, &account_id, KIND_RECOVERY, b"s");
    let new_key = fresh_key();
    let body = recall_body(
        &[0u8; 32],
        &pubkey(&new_key),
        &B64.decode(&password_upload).unwrap(),
        &B64.decode(&recovery_upload).unwrap(),
    );
    post(
        state,
        "/account/recall/finish",
        None,
        &json!({
            "session_id": session_id,
            "account_id_b64": b64u(&account_id),
            "anchor_seq": 1,
            "login_finish": unrelated_finalization(),
            "password_upload": password_upload,
            "recovery_upload": recovery_upload,
            "new_signature_pubkey_b64": b64u(&pubkey(&new_key)),
            "statement_b64": statement(
                signer,
                AccountStatementKind::Recall,
                &account_id,
                &body,
                now_ms(),
            ),
        }),
    )
    .await
}

/// A base64 `RegistrationUpload` for `secret`, made against the server's own
/// setup (so no route is called).
fn registration_upload(state: &AppState, account: &[u8; 16], kind: u8, secret: &[u8]) -> String {
    let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut OsRng, secret).unwrap();
    let response = ServerRegistration::<LakkoCipherSuite>::start(
        &state.opaque_setup,
        cs.message,
        &build_credential_id(account, kind),
    )
    .unwrap();
    let fin = cs
        .state
        .finish(
            &mut OsRng,
            secret,
            response.message,
            ClientRegistrationFinishParameters::default(),
        )
        .unwrap();
    B64.encode(fin.message.serialize())
}

/// A found phone: the anchor's key, not the password. Every attempt is
/// answered (it has to be: the key holder may be the owner mistyping) and
/// counted; below the cap the owner still recalls; at the cap the anchor is
/// destroyed.
#[tokio::test]
async fn a_phone_finder_spends_attempts_and_the_owner_recalls_below_the_cap() {
    let pool = db_or_skip!("a_phone_finder_spends_attempts_and_the_owner_recalls_below_the_cap");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;

    // A wrong password, forced all the way to the server: `wrong-password`,
    // and the attempt is consumed (a second finish for it is refused).
    let attempt = recall_start(&state, &k0, &account.id, 1, b"wrong", b"n", &fresh_seed())
        .await
        .unwrap();
    let a = attempt.answer.clone();
    let password_upload = reg_finish(
        attempt.password_reg,
        b"n",
        a["password_response"].as_str().unwrap(),
    );
    let recovery_upload = reg_finish(
        attempt.recovery_reg,
        b"s",
        a["recovery_response"].as_str().unwrap(),
    );
    let nonce: [u8; 32] = URL_SAFE_NO_PAD
        .decode(a["server_nonce_b64"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let new_key = fresh_key();
    let forced = json!({
        "session_id": a["session_id"],
        "account_id_b64": b64u(&account.id),
        "anchor_seq": 1,
        "login_finish": unrelated_finalization(),
        "password_upload": password_upload,
        "recovery_upload": recovery_upload,
        "new_signature_pubkey_b64": b64u(&pubkey(&new_key)),
        "statement_b64": statement(
            &k0,
            AccountStatementKind::Recall,
            &account.id,
            &recall_body(
                &nonce,
                &pubkey(&new_key),
                &B64.decode(&password_upload).unwrap(),
                &B64.decode(&recovery_upload).unwrap(),
            ),
            now_ms(),
        ),
    });
    let (st, body) = post(&state, "/account/recall/finish", None, &forced).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&body), "wrong-password");
    let (st, body) = post(&state, "/account/recall/finish", None, &forced).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(
        text(&body),
        REFUSED,
        "a consumed attempt answers like no attempt"
    );
    assert_eq!(
        identity_of(&pool, &account.id).await.0,
        2,
        "nothing changed"
    );

    // Up to one below the cap, every start is answered and counted.
    for used in 2..MAX_ATTEMPTS {
        recall_start(&state, &k0, &account.id, 1, b"wrong", b"n", &fresh_seed())
            .await
            .expect("an attempt below the cap is answered");
        let (_, body) = status(&state, &k0, &account.id).await;
        assert_eq!(json_of(&body)["attempts_remaining"], MAX_ATTEMPTS - used);
    }
    assert_eq!(anchors(&pool, &account.id).await[0].4, MAX_ATTEMPTS - 1);

    // The owner, with the last attempt, recalls.
    let k1 = fresh_key();
    let v = expect_recalled(
        recall(
            &state,
            &k0,
            &account.id,
            b"right",
            b"owner pw",
            &fresh_seed(),
            &k1,
        )
        .await,
    );
    assert_eq!(v["key_generation"], 3);
    assert_eq!(identity_of(&pool, &account.id).await.1, pubkey(&k1));
}

/// At the cap the anchor is destroyed: the next start is the uniform
/// refusal, and so is everything after it.
#[tokio::test]
async fn twenty_wrong_attempts_destroy_the_anchor() {
    let pool = db_or_skip!("twenty_wrong_attempts_destroy_the_anchor");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;

    for _ in 0..MAX_ATTEMPTS {
        recall_start(&state, &k0, &account.id, 1, b"wrong", b"n", &fresh_seed())
            .await
            .expect("answered");
    }
    let (_, body) = status(&state, &k0, &account.id).await;
    assert_eq!(json_of(&body)["attempts_remaining"], 0);
    match recall_start(&state, &k0, &account.id, 1, b"right", b"n", &fresh_seed()).await {
        Err((st, body)) => {
            assert_eq!(st, StatusCode::UNAUTHORIZED);
            assert_eq!(text(&body), REFUSED);
        },
        Ok(_) => panic!("an attempt past the cap was answered"),
    }
    assert!(
        anchors(&pool, &account.id).await.is_empty(),
        "the anchor is destroyed"
    );
    assert_eq!(anchor_seq(&state, &k0, &account.id).await, None);
}

/// The default budget: five attempts per fifteen minutes per anchor.
#[tokio::test]
async fn the_default_budget_is_five_attempts_per_window_per_anchor() {
    let pool = db_or_skip!("the_default_budget_is_five_attempts_per_window_per_anchor");
    let defaults = Limits::defaults();
    assert_eq!(defaults.name_window(Scope::Recall), Window::new(5, 900));
    let state = build_state(
        pool.clone(),
        defaults.with_ip_window(Category::Attempt, Window::new(100_000, 60)),
    );
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;
    for _ in 0..5 {
        assert!(
            recall_start(&state, &k0, &account.id, 1, b"wrong", b"n", &fresh_seed())
                .await
                .is_ok()
        );
    }
    let Err((st, body)) =
        recall_start(&state, &k0, &account.id, 1, b"right", b"n", &fresh_seed()).await
    else {
        panic!("a sixth attempt inside the window was answered");
    };
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&body), "too-many-attempts");
    assert_eq!(anchors(&pool, &account.id).await[0].4, 5);
}

/// The finish's statement is bound to the attempt's server nonce, the new
/// key and both uploads. A finish that changes any of them is refused and
/// consumes nothing; the honest finish then succeeds; replayed, it is
/// refused, and so is a new attempt, because the anchor is gone.
#[tokio::test]
async fn a_tampered_finish_is_refused_before_and_after_the_recall() {
    let pool = db_or_skip!("a_tampered_finish_is_refused_before_and_after_the_recall");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;

    let seed1 = fresh_seed();
    let k1 = fresh_key();
    let attempt = recall_start(&state, &k0, &account.id, 1, b"right", b"new pw", &seed1)
        .await
        .unwrap();
    let honest = finish_body(
        attempt,
        &k0,
        &account.id,
        1,
        b"right",
        b"new pw",
        &seed1,
        &k1,
    )
    .expect("the password is right");

    let refused = |body: Value| {
        let state = state.clone();
        async move {
            let (st, bytes) = post(&state, "/account/recall/finish", None, &body).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED);
            assert_eq!(text(&bytes), REFUSED);
        }
    };
    // Another new key (the attacker swapping in their own).
    let mut swapped = honest.clone();
    swapped["new_signature_pubkey_b64"] = json!(b64u(&pubkey(&fresh_key())));
    refused(swapped).await;
    // Another password upload, another recovery upload.
    let mut other_password = honest.clone();
    other_password["password_upload"] = json!(registration_upload(
        &state,
        &account.id,
        KIND_PASSWORD,
        b"attacker"
    ));
    refused(other_password).await;
    let mut other_recovery = honest.clone();
    other_recovery["recovery_upload"] = json!(registration_upload(
        &state,
        &account.id,
        KIND_RECOVERY,
        b"attacker"
    ));
    refused(other_recovery).await;
    // Another anchor number, another session.
    let mut other_anchor = honest.clone();
    other_anchor["anchor_seq"] = json!(2);
    refused(other_anchor).await;
    let mut other_session = honest.clone();
    other_session["session_id"] = json!(Uuid::now_v7().to_string());
    refused(other_session).await;
    // The new key must be new: the old identity does not return.
    assert_eq!(identity_of(&pool, &account.id).await.0, 2);
    assert_eq!(
        count(&pool, "recall_states", &account.id).await,
        1,
        "nothing was consumed"
    );

    // The honest finish. Afterwards the same attempt cannot be used to do
    // anything ELSE: a finish for it that differs from the one that
    // succeeded is refused. (The identical request is answered again, see
    // `key_rotate_tests::a_lost_recall_answer_can_be_asked_again`.)
    let (st, bytes) = post(&state, "/account/recall/finish", None, &honest).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&bytes));
    assert_eq!(json_of(&bytes)["key_generation"], 3);
    let mut another_device = honest.clone();
    another_device["device_id_b64"] = json!(b64u(&ATTACKER_DEVICE));
    refused(another_device).await;
    let mut another_key = honest.clone();
    another_key["new_signature_pubkey_b64"] = json!(b64u(&pubkey(&fresh_key())));
    refused(another_key).await;
    assert_eq!(
        identity_of(&pool, &account.id).await.0,
        3,
        "the refused replays changed nothing"
    );
    match recall_start(&state, &k0, &account.id, 1, b"right", b"x", &fresh_seed()).await {
        Err((st, body)) => {
            assert_eq!(st, StatusCode::UNAUTHORIZED);
            assert_eq!(text(&body), REFUSED);
        },
        Ok(_) => panic!("a consumed anchor opened another attempt"),
    }
}

/// The old identity does not return: a recall that names the anchor's own
/// key as the new key is refused (and only its holder can even ask).
#[tokio::test]
async fn the_recalled_identity_must_be_a_new_key() {
    let pool = db_or_skip!("the_recalled_identity_must_be_a_new_key");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;
    match recall(
        &state,
        &k0,
        &account.id,
        b"right",
        b"new",
        &fresh_seed(),
        &k0,
    )
    .await
    {
        Recall::Finished(StatusCode::BAD_REQUEST, _) => {},
        other => panic!("the old key came back: {other:?}"),
    }
    assert_eq!(identity_of(&pool, &account.id).await.0, 2);
}

// ----------------------------------------------------------------
// The window
// ----------------------------------------------------------------

/// After the window the anchor is dead: status, start and finish are the
/// uniform refusal, for the right key and the right password.
#[tokio::test]
async fn an_expired_anchor_recalls_nothing() {
    let pool = db_or_skip!("an_expired_anchor_recalls_nothing");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;

    // An attempt opened inside the window does not outlive it.
    let seed1 = fresh_seed();
    let k1 = fresh_key();
    let attempt = recall_start(&state, &k0, &account.id, 1, b"right", b"new", &seed1)
        .await
        .unwrap();
    let body = finish_body(attempt, &k0, &account.id, 1, b"right", b"new", &seed1, &k1).unwrap();
    expire_anchors(&pool, &account.id).await;

    assert_eq!(anchor_seq(&state, &k0, &account.id).await, None);
    let Err((st, bytes)) =
        recall_start(&state, &k0, &account.id, 1, b"right", b"new", &fresh_seed()).await
    else {
        panic!("an expired anchor opened an attempt");
    };
    assert_eq!(
        (st, text(&bytes).as_str()),
        (StatusCode::UNAUTHORIZED, REFUSED)
    );
    let (st, bytes) = post(&state, "/account/recall/finish", None, &body).await;
    assert_eq!(
        (st, text(&bytes).as_str()),
        (StatusCode::UNAUTHORIZED, REFUSED)
    );
    assert_eq!(identity_of(&pool, &account.id).await.0, 2);

    // A new first change under the current key gets a fresh anchor in place
    // of nothing: the expired row of ANOTHER key is simply left to the
    // pruner, and does not count toward the cap.
    let v = login(&state, &account.handle, b"right", &ATTACKER_DEVICE)
        .await
        .unwrap();
    let (st, _) = reset_keys(&state, &v, &fresh_seed(), &fresh_key()).await;
    assert_eq!(st, StatusCode::OK);
    let live: Vec<_> = anchors(&pool, &account.id)
        .await
        .into_iter()
        .filter(|a| a.3 > now_ms())
        .collect();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].0, 2);
}

/// `DELETE /account` is refused while a recall window is open, with the
/// time it closes, and works once it has closed.
#[tokio::test]
async fn account_deletion_is_refused_while_a_recall_window_is_open() {
    let pool = db_or_skip!("account_deletion_is_refused_while_a_recall_window_is_open");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    let delete = |session: Value| {
        let state = state.clone();
        async move {
            call(
                &state,
                Method::DELETE,
                "/account",
                Some(&token(&session)),
                Some(("x-lakko-fresh-grant", &grant(&session))),
                None,
            )
            .await
        }
    };

    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;
    let expiry = anchors(&pool, &account.id).await[0].3;

    // The party who reset signs in again (the reset's own answer carries no
    // grant) and tries to delete.
    let attacker = login(&state, &account.handle, b"right", &ATTACKER_DEVICE)
        .await
        .unwrap();
    let (st, body) = delete(attacker.clone()).await;
    assert_eq!(st, StatusCode::CONFLICT, "{}", text(&body));
    assert_eq!(code(&body), "recall-window-open");
    assert_eq!(json_of(&body)["until_ms"].as_i64().unwrap(), expiry);
    assert_eq!(
        identity_of(&pool, &account.id).await.0,
        2,
        "the account is still there"
    );
    // Without the grant it is the grant's refusal, as before: the window is
    // not an oracle for a bare session.
    let (st, body) = call(
        &state,
        Method::DELETE,
        "/account",
        Some(&token(&attacker)),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "fresh-auth-required");

    // The owner can still recall.
    assert_eq!(anchor_seq(&state, &k0, &account.id).await, Some(1));

    // After the window: deleted.
    expire_anchors(&pool, &account.id).await;
    let (st, body) = delete(attacker).await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{}", text(&body));
    let gone: Option<u64> =
        sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ?")
            .bind(&account.id[..])
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(gone.is_none());
    assert!(
        anchors(&pool, &account.id).await.is_empty(),
        "the anchors go with the account"
    );
}

// ----------------------------------------------------------------
// The generation fence
// ----------------------------------------------------------------

/// A bearer route's write that must not survive a key change is refused once
/// the generation has moved, even though the bearer passed the middleware
/// before it moved.
#[tokio::test]
async fn the_generation_fence_refuses_a_write_from_a_displaced_generation() {
    let pool = db_or_skip!("the_generation_fence_refuses_a_write_from_a_displaced_generation");
    let state = build_state(pool.clone(), roomy());
    let account = new_account(&state, b"pw", &fresh_seed(), &fresh_key()).await;

    let tx = crate::session::begin_at_generation(&pool, &account.id, 1)
        .await
        .expect("the current generation passes");
    tx.rollback().await.unwrap();

    sqlx::query("UPDATE accounts SET identity_version = 2 WHERE id = ?")
        .bind(&account.id[..])
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        crate::session::begin_at_generation(&pool, &account.id, 1).await,
        Err(crate::webauthn::ApiError::KeyGenerationSuperseded)
    ));
    assert!(matches!(
        crate::session::begin_at_generation(&pool, &account.id, 3).await,
        Err(crate::webauthn::ApiError::InvalidCredential)
    ));
    assert!(matches!(
        crate::session::begin_at_generation(&pool, Uuid::now_v7().as_bytes(), 1).await,
        Err(crate::webauthn::ApiError::InvalidCredential)
    ));
}

/// Sprint T5: the key rotation, the session upgrade and the two recall
/// follow-ups. A child module so that it shares this file's harness.
#[path = "key_rotate_tests.rs"]
mod key_rotate_tests;
