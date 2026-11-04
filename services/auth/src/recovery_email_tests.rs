// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests for ADR-0075: the recovery email (enrol, replace, remove,
//! recover by mailed code in both modes), the account notices, and the
//! recall's phrase arm.
//!
//! Every test drives the real router; the mailed code is read from a
//! recording [`crate::mail::MailSender`], the way a person reads it from a
//! mailbox. The OPAQUE and recall client helpers are those of
//! [`crate::recall_tests`].
//!
//! Gated on `LAKKO_AUTH_TEST_DATABASE_URL` (each test skips when it is
//! unset):
//!
//! ```text
//! LAKKO_AUTH_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_auth_test \
//!   cargo test -p lakko-svc-auth recovery_email_tests
//! ```

#![allow(clippy::many_single_char_names, clippy::too_many_lines)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_attest::{recall_start_body, AccountStatementKind};
use lakko_crypto::signing::SigningKey;
use opaque_ke::ClientLogin;
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde_json::{json, Value};
use sqlx::MySqlPool;

use crate::limits::{Category, Limits, Scope, Window};
use crate::mail::RecordingSender;
use crate::opaque::LakkoCipherSuite;
use crate::recall_tests::{
    b64u, build_state, call, code, finish_body, fresh_key, fresh_seed, grant, json_of, login,
    new_account, password_registration, pool, post, pubkey, recovery_login, reg_finish, reg_start,
    reset_keys, roomy, statement, takeover, text, token, Account, Attempt, OWNER_DEVICE,
};
use crate::session::now_ms;
use crate::AppState;

const DISABLED: &str = r#"{"error":"recovery-email-disabled"}"#;
const CODE_REFUSED: &str = r#"{"error":"code-refused"}"#;
const TICKET_REFUSED: &str = r#"{"error":"ticket-refused"}"#;
const RECALL_REFUSED: &str = r#"{"error":"recall-refused"}"#;

/// `(identity_pubkey, password_registration, recovery_registration,
/// mail_protected)` of a `recall_anchors` row.
type AnchorProbe = (Vec<u8>, Vec<u8>, Option<Vec<u8>>, bool);

const VAULT_KEY: [u8; 32] = [0x44; 32];
const TOTP_KEY: [u8; 32] = [0x55; 32];

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
fn wide() -> Limits {
    roomy()
        .with_ip_window(Category::Email, Window::new(100_000, 60))
        .with_ip_window(Category::HandleBind, Window::new(100_000, 60))
        .with_name_window(Scope::EmailLetterHandle, Window::new(1000, 900))
        .with_name_window(Scope::EmailLetterAccount, Window::new(1000, 900))
        .with_name_window(Scope::EmailVerify, Window::new(1000, 900))
        .with_name_window(Scope::EmailVaultOpen, Window::new(1000, 900))
        .with_name_window(Scope::TotpRecover, Window::new(1000, 900))
        .with_name_window(Scope::HandleBind, Window::new(1000, 900))
        .with_name_window(Scope::LoginRecovery, Window::new(1000, 900))
        .with_name_window(Scope::KeyRotate, Window::new(1000, 900))
        .with_name_window(Scope::EmailVaultStore, Window::new(1000, 900))
        .with_handle_change_interval(0)
}

/// A state with the recovery email on and a mailbox the test can read.
struct Rig {
    state: AppState,
    mail: Arc<RecordingSender>,
}

fn rig_with(pool: MySqlPool, limits: Limits) -> Rig {
    let mut state = build_state(pool, limits);
    let mail = Arc::new(RecordingSender::new());
    state.recovery_email = Some(Arc::new(crate::recovery_email::Config::new(
        [0x3C; 32],
        mail.clone(),
    )));
    state.vault_enc_key = Some(Arc::new(VAULT_KEY));
    state.totp_enc_key = Some(Arc::new(TOTP_KEY));
    Rig { state, mail }
}

fn rig(pool: MySqlPool) -> Rig {
    rig_with(pool, wide())
}

/// A fresh address nobody else in the test database uses.
fn fresh_address() -> String {
    let mut tag = [0u8; 8];
    OsRng.fill_bytes(&mut tag);
    format!("Person.{}@Example.com", b64u(&tag))
}

/// The 44-byte blob a client would seal (under a fixed secret: what it is
/// sealed under is not the server's business).
fn sealed_vault() -> Vec<u8> {
    let phrase = lakko_mnemonic::Mnemonic::generate();
    lakko_mnemonic::vault::seal_entropy_under_secret(&phrase, &[7u8; 32]).unwrap()
}

async fn get(state: &AppState, uri: &str, bearer: &str) -> (StatusCode, Bytes) {
    call(state, Method::GET, uri, Some(bearer), None, None).await
}

async fn delete(state: &AppState, uri: &str, bearer: &str, body: &Value) -> (StatusCode, Bytes) {
    call(state, Method::DELETE, uri, Some(bearer), None, Some(body)).await
}

/// An account with a password, a recovery seed, a handle and an identity key,
/// and a signed-in session that holds a fresh-auth grant.
struct Person {
    account: Account,
    password: Vec<u8>,
    seed: Vec<u8>,
    key: SigningKey,
    session: Value,
}

async fn person(state: &AppState) -> Person {
    let password = b"the first password".to_vec();
    let seed = fresh_seed();
    let key = fresh_key();
    let account = new_account(state, &password, &seed, &key).await;
    let session = login(state, &account.handle, &password, &OWNER_DEVICE)
        .await
        .expect("the password is right");
    Person {
        account,
        password,
        seed,
        key,
        session,
    }
}

/// Add (or replace) the person's recovery email through the routes; returns
/// the salt the start minted.
async fn enrol(rig: &Rig, who: &Person, address: &str, sealed: Option<&[u8]>) -> Vec<u8> {
    let bearer = token(&who.session);
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/start",
        Some(&bearer),
        &json!({
            "fresh_grant": grant(&who.session),
            "handle": who.account.handle,
            "address": address,
            "lang": "en",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let started = json_of(&body);
    assert_eq!(started["resend_after_secs"], 45);
    assert_eq!(started["expires_in_secs"], 600);
    let salt = B64.decode(started["vault_salt"].as_str().unwrap()).unwrap();
    assert_eq!(salt.len(), 16);
    let (to, mailed) = rig.mail.sent().last().cloned().expect("a letter was sent");
    assert_eq!(to, address.trim().to_lowercase());
    let mut finish = json!({ "challenge_id": started["challenge_id"], "code": mailed });
    if let Some(sealed) = sealed {
        finish["sealed_entropy"] = json!(B64.encode(sealed));
    }
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/finish",
        Some(&bearer),
        &finish,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(
        json_of(&body),
        json!({ "enabled": true, "vault": sealed.is_some() })
    );
    salt
}

async fn recover_start(rig: &Rig, handle: &str, address: &str) -> (StatusCode, Bytes) {
    post(
        &rig.state,
        "/recovery-email/recover/start",
        None,
        &json!({ "handle": handle, "address": address }),
    )
    .await
}

/// Wait for the letter that the spawned send delivers after the answer.
async fn letter_after(rig: &Rig, before: usize) -> Option<String> {
    for _ in 0..200 {
        let sent = rig.mail.sent();
        if sent.len() > before {
            return Some(sent[before].1.clone());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

/// Start a recovery for a matching pair and read the mailed code.
async fn start_and_read_code(rig: &Rig, handle: &str, address: &str) -> (String, String) {
    let before = rig.mail.sent().len();
    let (st, body) = recover_start(rig, handle, address).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let challenge = json_of(&body)["challenge_id"].as_str().unwrap().to_string();
    let mailed = letter_after(rig, before)
        .await
        .expect("a letter for a match");
    (challenge, mailed)
}

async fn verify(rig: &Rig, challenge: &str, typed: &str) -> (StatusCode, Bytes) {
    post(
        &rig.state,
        "/recovery-email/recover/verify",
        None,
        &json!({ "challenge_id": challenge, "code": typed }),
    )
    .await
}

/// A code that is certainly not `right`.
fn another_code(right: &str) -> String {
    let n: u32 = right.parse().unwrap();
    format!("{:06}", (n + 1) % 1_000_000)
}

/// Mailed code to ticket, for a matching pair.
async fn ticket_for(rig: &Rig, handle: &str, address: &str) -> Value {
    let (challenge, mailed) = start_and_read_code(rig, handle, address).await;
    let (st, body) = verify(rig, &challenge, &mailed).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    json_of(&body)
}

async fn notices(state: &AppState, bearer: &str) -> Vec<String> {
    let (st, body) = get(state, "/account/notices", bearer).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    json_of(&body)["notices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap().to_string())
        .collect()
}

async fn rows(pool: &MySqlPool, table: &str, account: &[u8; 16]) -> i64 {
    // `table` is a literal chosen by the tests below, never input.
    let sql = match table {
        "recovery_emails" => "SELECT COUNT(*) FROM recovery_emails WHERE account_id = ?",
        "email_vaults" => "SELECT COUNT(*) FROM email_vaults WHERE account_id = ?",
        "email_challenges" => "SELECT COUNT(*) FROM email_challenges WHERE account_id = ?",
        "email_tickets" => "SELECT COUNT(*) FROM email_tickets WHERE account_id = ?",
        "recall_anchors" => "SELECT COUNT(*) FROM recall_anchors WHERE account_id = ?",
        other => panic!("unknown table {other}"),
    };
    sqlx::query_scalar(sql)
        .bind(&account[..])
        .fetch_one(pool)
        .await
        .expect("count")
}

// ----------------------------------------------------------------
// The feature switch
// ----------------------------------------------------------------

#[tokio::test]
async fn without_the_pepper_every_route_is_503_and_available_is_false() {
    let pool = db_or_skip!("without_the_pepper_every_route_is_503_and_available_is_false");
    // The pepper is unset: `build_state` leaves `recovery_email` empty.
    let mut state = build_state(pool.clone(), wide());
    state.vault_enc_key = Some(Arc::new(VAULT_KEY));
    let who = person(&state).await;
    let bearer = token(&who.session);

    let (st, body) = call(
        &state,
        Method::GET,
        "/recovery-email/available",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body), json!({ "available": false }));

    let ticket = b64u(&[9u8; 32]);
    let anonymous = [
        (
            "/recovery-email/recover/start",
            json!({ "handle": who.account.handle, "address": "a@b.example" }),
        ),
        (
            "/recovery-email/recover/verify",
            json!({ "challenge_id": b64u(&[1u8; 16]), "code": "123456" }),
        ),
        (
            "/recovery-email/recover/totp",
            json!({ "ticket": ticket, "code": "123456" }),
        ),
        ("/recovery-email/recover/open", json!({ "ticket": ticket })),
        (
            "/recovery-email/recover/password/start",
            json!({ "ticket": ticket, "new_password_request": reg_start(b"x").1 }),
        ),
        (
            "/recovery-email/recover/password/finish",
            json!({ "ticket": ticket, "new_password_upload": "AAAA" }),
        ),
    ];
    for (uri, body) in &anonymous {
        let (st, answer) = post(&state, uri, None, body).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(text(&answer), DISABLED, "{uri}");
    }
    let (st, answer) = post(
        &state,
        "/recovery-email/enrol/start",
        Some(&bearer),
        &json!({
            "fresh_grant": grant(&who.session),
            "handle": who.account.handle,
            "address": "a@b.example",
        }),
    )
    .await;
    assert_eq!(
        (st, text(&answer).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, DISABLED)
    );
    let (st, answer) = post(
        &state,
        "/recovery-email/enrol/finish",
        Some(&bearer),
        &json!({ "challenge_id": b64u(&[1u8; 16]), "code": "123456" }),
    )
    .await;
    assert_eq!(
        (st, text(&answer).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, DISABLED)
    );
    let (st, answer) = delete(
        &state,
        "/recovery-email",
        &bearer,
        &json!({ "fresh_grant": grant(&who.session) }),
    )
    .await;
    assert_eq!(
        (st, text(&answer).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, DISABLED)
    );

    // The account's own state is still readable, and says mail is off.
    let (st, answer) = get(&state, "/recovery-email", &bearer).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        json_of(&answer),
        json!({ "available": false, "enabled": false, "vault": false, "phrase_saved": false, "totp": false })
    );

    // The pepper alone is not enough: the vault key is part of the feature.
    let mut half = rig(pool).state;
    half.vault_enc_key = None;
    let (st, answer) = recover_start_state(&half, &who.account.handle).await;
    assert_eq!(
        (st, text(&answer).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, DISABLED)
    );
}

async fn recover_start_state(state: &AppState, handle: &str) -> (StatusCode, Bytes) {
    post(
        state,
        "/recovery-email/recover/start",
        None,
        &json!({ "handle": handle, "address": "a@b.example" }),
    )
    .await
}

// ----------------------------------------------------------------
// Enrol, replace, remove
// ----------------------------------------------------------------

#[tokio::test]
async fn enrol_needs_a_fresh_grant_and_the_accounts_own_handle() {
    let pool = db_or_skip!("enrol_needs_a_fresh_grant_and_the_accounts_own_handle");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let other = person(&rig.state).await;
    let bearer = token(&who.session);
    let address = fresh_address();

    // No grant, a malformed grant, another account's grant: one answer, and
    // no letter.
    for fresh in [
        Value::Null,
        json!("not-a-grant"),
        json!(grant(&other.session)),
    ] {
        let (st, body) = post(
            &rig.state,
            "/recovery-email/enrol/start",
            Some(&bearer),
            &json!({ "fresh_grant": fresh, "handle": who.account.handle, "address": address }),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{}", text(&body));
        assert_eq!(code(&body), "fresh-auth-required");
    }
    assert!(rig.mail.sent().is_empty());

    // Another account's handle.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/start",
        Some(&bearer),
        &json!({
            "fresh_grant": grant(&who.session),
            "handle": other.account.handle,
            "address": address,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&body), "handle-mismatch");

    // An address the rule refuses.
    for bad in ["no-at-sign", "two@at@signs", "a b@c.example", "@b"] {
        let (st, body) = post(
            &rig.state,
            "/recovery-email/enrol/start",
            Some(&bearer),
            &json!({
                "fresh_grant": grant(&who.session),
                "handle": who.account.handle,
                "address": bad,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(code(&body), "address-invalid", "{bad}");
    }
    assert!(rig.mail.sent().is_empty());
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 0);

    // The real thing, with a vault.
    let sealed = sealed_vault();
    enrol(&rig, &who, &address, Some(&sealed)).await;
    let (_, body) = get(&rig.state, "/recovery-email", &bearer).await;
    assert_eq!(
        json_of(&body),
        json!({ "available": true, "enabled": true, "vault": true, "phrase_saved": false, "totp": false })
    );
    // The address is nowhere in the database: only the 32-byte key.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT email_key FROM recovery_emails WHERE account_id = ?")
            .bind(&who.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.len(), 32);

    // Replacing without a vault drops the old one (it was sealed under the
    // old address).
    enrol(&rig, &who, &fresh_address(), None).await;
    assert_eq!(rows(&pool, "email_vaults", &who.account.id).await, 0);

    // Removing needs the grant too.
    let (st, body) = delete(&rig.state, "/recovery-email", &bearer, &json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "fresh-auth-required");
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);
    let (st, body) = delete(
        &rig.state,
        "/recovery-email",
        &bearer,
        &json!({ "fresh_grant": grant(&who.session) }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(json_of(&body), json!({ "removed": true }));
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 0);

    assert_eq!(
        notices(&rig.state, &bearer).await,
        [
            "recovery-email-added",
            "recovery-email-replaced",
            "recovery-email-removed"
        ]
    );
}

#[tokio::test]
async fn an_enrol_code_has_five_tries_and_a_wrong_sized_vault_is_refused() {
    let pool = db_or_skip!("an_enrol_code_has_five_tries_and_a_wrong_sized_vault_is_refused");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let start = json!({
        "fresh_grant": grant(&who.session),
        "handle": who.account.handle,
        "address": fresh_address(),
    });
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/start",
        Some(&bearer),
        &start,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let challenge = json_of(&body)["challenge_id"].clone();
    let mailed = rig.mail.sent().last().unwrap().1.clone();

    // A vault that is not 44 bytes: refused before the code is looked at.
    let (st, _) = post(
        &rig.state,
        "/recovery-email/enrol/finish",
        Some(&bearer),
        &json!({
            "challenge_id": challenge,
            "code": mailed,
            "sealed_entropy": B64.encode([0u8; 60]),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Another account cannot finish this challenge, and spends none of it.
    let other = person(&rig.state).await;
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/finish",
        Some(&token(&other.session)),
        &json!({ "challenge_id": challenge, "code": mailed }),
    )
    .await;
    assert_eq!(st, StatusCode::GONE);
    assert_eq!(code(&body), "code-expired");

    for _ in 0..5 {
        let (st, body) = post(
            &rig.state,
            "/recovery-email/enrol/finish",
            Some(&bearer),
            &json!({ "challenge_id": challenge, "code": another_code(&mailed) }),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(text(&body), CODE_REFUSED);
    }
    // The fifth wrong code took the challenge: the right one is too late.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/finish",
        Some(&bearer),
        &json!({ "challenge_id": challenge, "code": mailed }),
    )
    .await;
    assert_eq!(st, StatusCode::GONE);
    assert_eq!(code(&body), "code-expired");
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 0);

    // A relay that refuses the letter: 502, and no challenge is left behind.
    rig.mail.fail_from_now();
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/start",
        Some(&bearer),
        &start,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
    assert_eq!(code(&body), "mail-send-failed");
    assert_eq!(rows(&pool, "email_challenges", &who.account.id).await, 0);
}

#[tokio::test]
async fn a_change_of_login_handle_drops_the_email_and_the_vault() {
    let pool = db_or_skip!("a_change_of_login_handle_drops_the_email_and_the_vault");
    let rig = rig(pool.clone());

    // Re-binding to another handle.
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    enrol(&rig, &who, &fresh_address(), Some(&sealed_vault())).await;
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);
    assert_eq!(rows(&pool, "email_vaults", &who.account.id).await, 1);
    // Re-binding the SAME handle changes nothing.
    let (st, _) = call(
        &rig.state,
        Method::PUT,
        "/auth/login-handle",
        Some(&bearer),
        None,
        Some(&json!({ "handle": who.account.handle })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);
    let (_, new_handle) = crate::login_handle::random_handle();
    // A change of handle needs the fresh-auth grant (ADR-0076); with it and
    // without the address, the recovery email is dropped.
    let (st, body) = call(
        &rig.state,
        Method::PUT,
        "/auth/login-handle",
        Some(&bearer),
        None,
        Some(&json!({ "handle": new_handle, "fresh_grant": grant(&who.session) })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 0);
    assert_eq!(rows(&pool, "email_vaults", &who.account.id).await, 0);
    assert_eq!(
        notices(&rig.state, &bearer).await,
        ["recovery-email-added", "recovery-email-removed"]
    );

    // Removing the handle.
    let gone = person(&rig.state).await;
    let bearer = token(&gone.session);
    enrol(&rig, &gone, &fresh_address(), Some(&sealed_vault())).await;
    let (st, _) = call(
        &rig.state,
        Method::DELETE,
        "/auth/login-handle",
        Some(&bearer),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(rows(&pool, "recovery_emails", &gone.account.id).await, 0);
    assert_eq!(rows(&pool, "email_vaults", &gone.account.id).await, 0);

    // Without a handle the account cannot enrol at all.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/start",
        Some(&bearer),
        &json!({
            "fresh_grant": grant(&gone.session),
            "handle": gone.account.handle,
            "address": fresh_address(),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&body), "username-required");
}

// ----------------------------------------------------------------
// Recover: a match and a miss
// ----------------------------------------------------------------

#[tokio::test]
async fn a_match_and_a_miss_answer_alike_and_only_a_match_gets_a_letter() {
    let pool = db_or_skip!("a_match_and_a_miss_answer_alike_and_only_a_match_gets_a_letter");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let address = fresh_address();
    enrol(&rig, &who, &address, Some(&sealed_vault())).await;
    let without_email = person(&rig.state).await;
    let (_, unknown_handle) = crate::login_handle::random_handle();
    let sent_before = rig.mail.sent().len();

    // The four cases of UX 19.3: a matching pair, the right name with
    // another address, a name nobody has, an account with no email.
    let cases = [
        (who.account.handle.clone(), address.clone()),
        (who.account.handle.clone(), fresh_address()),
        (unknown_handle, address.clone()),
        (without_email.account.handle.clone(), address.clone()),
    ];
    let mut challenges = Vec::new();
    for (handle, typed) in &cases {
        let (st, body) = recover_start(&rig, handle, typed).await;
        assert_eq!(st, StatusCode::OK, "{}", text(&body));
        let mut answer = json_of(&body);
        let challenge = answer["challenge_id"].as_str().unwrap().to_string();
        assert_eq!(URL_SAFE_NO_PAD.decode(&challenge).unwrap().len(), 16);
        // Everything but the random id is identical.
        answer["challenge_id"] = Value::Null;
        assert_eq!(
            answer,
            json!({ "challenge_id": null, "resend_after_secs": 45, "expires_in_secs": 600 })
        );
        challenges.push(challenge);
    }

    // Exactly one letter left, for the match, to the normalised address.
    let mailed = letter_after(&rig, sent_before).await.expect("the match");
    tokio::time::sleep(Duration::from_millis(150)).await;
    let sent = rig.mail.sent();
    assert_eq!(sent.len(), sent_before + 1, "a miss sends nothing");
    assert_eq!(sent[sent_before].0, address.to_lowercase());

    // A stand-in challenge never verifies: not with the code the match was
    // mailed, not with any other. The answer is the one a wrong code for the
    // real challenge gets, and the one an unknown challenge gets.
    for decoy in &challenges[1..] {
        for typed in [mailed.as_str(), "000000", "999999"] {
            let (st, body) = verify(&rig, decoy, typed).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED);
            assert_eq!(text(&body), CODE_REFUSED);
        }
    }
    let (st, body) = verify(&rig, &b64u(&[0xEE; 16]), &mailed).await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, CODE_REFUSED)
    );
    let (st, body) = verify(&rig, "not base64 at all!", &mailed).await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, CODE_REFUSED)
    );
    let (st, body) = verify(&rig, &challenges[0], &another_code(&mailed)).await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, CODE_REFUSED)
    );

    // The misses left the real account's live challenge alone: the mailed
    // code still works.
    let (st, body) = verify(&rig, &challenges[0], &mailed).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(json_of(&body)["totp_required"], false);
    // And it worked once.
    let (st, body) = verify(&rig, &challenges[0], &mailed).await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, CODE_REFUSED)
    );
}

#[tokio::test]
async fn five_wrong_codes_kill_the_challenge() {
    let pool = db_or_skip!("five_wrong_codes_kill_the_challenge");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let address = fresh_address();
    enrol(&rig, &who, &address, None).await;
    let (challenge, mailed) = start_and_read_code(&rig, &who.account.handle, &address).await;
    assert_eq!(rows(&pool, "email_challenges", &who.account.id).await, 1);

    for _ in 0..5 {
        let (st, body) = verify(&rig, &challenge, &another_code(&mailed)).await;
        assert_eq!(
            (st, text(&body).as_str()),
            (StatusCode::UNAUTHORIZED, CODE_REFUSED)
        );
    }
    assert_eq!(rows(&pool, "email_challenges", &who.account.id).await, 0);
    // The right code, a sixth try: the same refusal, and no ticket.
    let (st, body) = verify(&rig, &challenge, &mailed).await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, CODE_REFUSED)
    );
    assert_eq!(rows(&pool, "email_tickets", &who.account.id).await, 0);

    // A new start replaces the challenge: one live challenge per account,
    // and the earlier code is dead.
    let (first, first_code) = start_and_read_code(&rig, &who.account.handle, &address).await;
    let (second, second_code) = start_and_read_code(&rig, &who.account.handle, &address).await;
    assert_eq!(rows(&pool, "email_challenges", &who.account.id).await, 1);
    let (st, _) = verify(&rig, &first, &first_code).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = verify(&rig, &second, &second_code).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn the_letter_limit_refuses_a_known_and_an_unknown_name_alike() {
    let pool = db_or_skip!("the_letter_limit_refuses_a_known_and_an_unknown_name_alike");
    let rig = rig_with(
        pool,
        wide().with_name_window(Scope::EmailLetterHandle, Window::new(3, 900)),
    );
    let who = person(&rig.state).await;
    let address = fresh_address();
    // The enrol letter is the handle's first.
    enrol(&rig, &who, &address, None).await;
    let (_, unknown) = crate::login_handle::random_handle();
    for (handle, admitted) in [(who.account.handle.as_str(), 2), (unknown.as_str(), 3)] {
        for _ in 0..admitted {
            let (st, _) = recover_start(&rig, handle, &address).await;
            assert_eq!(st, StatusCode::OK);
        }
        let (st, body) = recover_start(&rig, handle, &address).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(code(&body), "email-rate-limited");
        assert!(json_of(&body)["retry_after_secs"].as_u64().unwrap() >= 1);
    }
}

#[tokio::test]
async fn a_spent_per_account_letter_budget_sends_nothing_and_answers_as_always() {
    let pool = db_or_skip!("a_spent_per_account_letter_budget_sends_nothing");
    let rig = rig_with(
        pool.clone(),
        wide().with_name_window(Scope::EmailLetterAccount, Window::new(2, 900)),
    );
    let who = person(&rig.state).await;
    let address = fresh_address();
    enrol(&rig, &who, &address, None).await; // the account's first letter
    let (challenge, mailed) = start_and_read_code(&rig, &who.account.handle, &address).await;
    // The third: over the budget. Same answer, no letter, and the live
    // challenge the second letter carried is untouched.
    let before = rig.mail.sent().len();
    let (st, body) = recover_start(&rig, &who.account.handle, &address).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(rig.mail.sent().len(), before);
    let (st, _) = verify(&rig, &challenge, &mailed).await;
    assert_eq!(st, StatusCode::OK);
}

// ----------------------------------------------------------------
// Recover: the vault
// ----------------------------------------------------------------

#[tokio::test]
async fn the_vault_leaves_only_after_the_code_and_the_authenticator() {
    let pool = db_or_skip!("the_vault_leaves_only_after_the_code_and_the_authenticator");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let address = fresh_address();
    let sealed = sealed_vault();
    let salt = enrol(&rig, &who, &address, Some(&sealed)).await;
    let secret = crate::totp::enroll_for_test(&pool, &TOTP_KEY, &who.account.id).await;
    // The account's own state says an authenticator will be asked for.
    let (_, body) = get(&rig.state, "/recovery-email", &bearer).await;
    assert_eq!(
        json_of(&body),
        json!({
            "available": true, "enabled": true, "vault": true, "phrase_saved": false,
            "totp": true,
        })
    );

    // Without a ticket: nothing.
    for bogus in [b64u(&[1u8; 32]), "junk".to_string()] {
        let (st, body) = post(
            &rig.state,
            "/recovery-email/recover/open",
            None,
            &json!({ "ticket": bogus }),
        )
        .await;
        assert_eq!(
            (st, text(&body).as_str()),
            (StatusCode::UNAUTHORIZED, TICKET_REFUSED)
        );
    }

    let verified = ticket_for(&rig, &who.account.handle, &address).await;
    assert_eq!(verified["totp_required"], true);
    assert_eq!(verified["expires_in_secs"], 600);
    let ticket = verified["ticket"].as_str().unwrap().to_string();
    assert_eq!(URL_SAFE_NO_PAD.decode(&ticket).unwrap().len(), 32);
    // The ticket is stored hashed.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT ticket_key FROM email_tickets WHERE account_id = ?")
            .bind(&who.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(stored, URL_SAFE_NO_PAD.decode(&ticket).unwrap());

    // The code alone does not open the vault, nor reach the password routes.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "totp-required");
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/password/start",
        None,
        &json!({ "ticket": ticket, "new_password_request": reg_start(b"new").1 }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "totp-required");

    // A wrong authenticator code.
    let right = crate::totp::current_code_for_test(&secret);
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/totp",
        None,
        &json!({ "ticket": ticket, "code": "12" }),
    )
    .await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, CODE_REFUSED)
    );
    let (st, _) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // The right one.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/totp",
        None,
        &json!({ "ticket": ticket, "code": right }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(json_of(&body), json!({ "ok": true }));

    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let opened = json_of(&body);
    assert_eq!(opened["mode"], "vault");
    assert_eq!(
        B64.decode(opened["sealed_entropy"].as_str().unwrap())
            .unwrap(),
        sealed
    );
    assert_eq!(
        B64.decode(opened["vault_salt"].as_str().unwrap()).unwrap(),
        salt
    );

    // The ticket is spent, and the vault is still wrapped at rest.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, TICKET_REFUSED)
    );
    let at_rest: Vec<u8> =
        sqlx::query_scalar("SELECT body_enc FROM email_vaults WHERE account_id = ?")
            .bind(&who.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(at_rest, sealed);
    assert_eq!(
        lakko_crypto::aead::open_with_key(&VAULT_KEY, &at_rest).unwrap(),
        sealed
    );

    // The account's devices are told.
    assert_eq!(
        notices(&rig.state, &bearer).await,
        ["recovery-email-added", "account-opened-by-email"]
    );

    // An authenticator code is accepted once: the same step does not pass a
    // second ticket.
    let again = ticket_for(&rig, &who.account.handle, &address).await;
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/totp",
        None,
        &json!({ "ticket": again["ticket"], "code": right }),
    )
    .await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, CODE_REFUSED)
    );
}

#[tokio::test]
async fn five_wrong_authenticator_codes_destroy_the_ticket() {
    let pool = db_or_skip!("five_wrong_authenticator_codes_destroy_the_ticket");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let address = fresh_address();
    enrol(&rig, &who, &address, Some(&sealed_vault())).await;
    let secret = crate::totp::enroll_for_test(&pool, &TOTP_KEY, &who.account.id).await;
    let ticket = ticket_for(&rig, &who.account.handle, &address).await["ticket"].clone();
    for _ in 0..5 {
        let (st, body) = post(
            &rig.state,
            "/recovery-email/recover/totp",
            None,
            &json!({ "ticket": ticket, "code": "not-a-code" }),
        )
        .await;
        assert_eq!(
            (st, text(&body).as_str()),
            (StatusCode::UNAUTHORIZED, CODE_REFUSED)
        );
    }
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/totp",
        None,
        &json!({ "ticket": ticket, "code": crate::totp::current_code_for_test(&secret) }),
    )
    .await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, TICKET_REFUSED)
    );
    assert_eq!(rows(&pool, "email_tickets", &who.account.id).await, 0);
}

#[tokio::test]
async fn the_vault_open_budget_is_per_account() {
    let pool = db_or_skip!("the_vault_open_budget_is_per_account");
    let rig = rig_with(
        pool,
        wide().with_name_window(Scope::EmailVaultOpen, Window::new(1, 900)),
    );
    let who = person(&rig.state).await;
    let address = fresh_address();
    enrol(&rig, &who, &address, Some(&sealed_vault())).await;
    let first = ticket_for(&rig, &who.account.handle, &address).await["ticket"].clone();
    let (st, _) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": first }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let second = ticket_for(&rig, &who.account.handle, &address).await["ticket"].clone();
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": second }),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&body), "too-many-attempts");
}

// ----------------------------------------------------------------
// Recover: the password
// ----------------------------------------------------------------

#[tokio::test]
async fn the_password_mode_captures_the_anchor_and_mints_no_grant() {
    let pool = db_or_skip!("the_password_mode_captures_the_anchor_and_mints_no_grant");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let owner_bearer = token(&who.session);
    let address = fresh_address();
    // No vault: a phrase-protected account, or one that never stored one.
    enrol(&rig, &who, &address, None).await;
    let old_envelope = password_registration(&pool, &who.account.id).await;
    assert_eq!(rows(&pool, "recall_anchors", &who.account.id).await, 0);

    let ticket = ticket_for(&rig, &who.account.handle, &address).await["ticket"].clone();
    let new_password = b"chosen by mail";
    let (reg, request) = reg_start(new_password);

    // Before `open` answered the password mode, the password routes refuse.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/password/start",
        None,
        &json!({ "ticket": ticket, "new_password_request": request }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&body), "wrong-mode");

    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(json_of(&body), json!({ "mode": "password" }));

    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/password/start",
        None,
        &json!({ "ticket": ticket, "new_password_request": request }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let upload = reg_finish(
        reg,
        new_password,
        json_of(&body)["new_password_response"].as_str().unwrap(),
    );
    let finish = json!({
        "ticket": ticket,
        "new_password_upload": upload,
        "device_id_b64": b64u(&[0xC3; 32]),
    });
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/password/finish",
        None,
        &finish,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let landed = json_of(&body);
    assert_eq!(landed["account_id_b64"], b64u(&who.account.id));
    assert!(landed["session_token"].is_string());
    assert!(landed["refresh_token"].is_string());
    // No fresh-auth grant, under any name.
    let keys: Vec<&String> = landed.as_object().unwrap().keys().collect();
    assert!(
        keys.iter().all(|k| !k.contains("grant")),
        "the answer carries a grant: {keys:?}"
    );

    // The anchor holds the account as it was BEFORE this change: the old
    // password envelope, the recovery envelope, and no email vault.
    let anchors: Vec<AnchorProbe> = sqlx::query_as(
        "SELECT identity_pubkey, password_registration, recovery_registration, mail_protected \
         FROM recall_anchors WHERE account_id = ?",
    )
    .bind(&who.account.id[..])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].0, pubkey(&who.key));
    assert_eq!(anchors[0].1, old_envelope);
    assert!(anchors[0].2.is_some());
    assert!(!anchors[0].3);
    assert_ne!(
        password_registration(&pool, &who.account.id).await,
        old_envelope
    );

    // The new password signs in; the old one does not.
    assert!(
        login(&rig.state, &who.account.handle, new_password, &OWNER_DEVICE)
            .await
            .is_some()
    );
    assert!(login(
        &rig.state,
        &who.account.handle,
        &who.password,
        &OWNER_DEVICE
    )
    .await
    .is_none());
    // The recovery phrase is untouched.
    assert!(recovery_login(&rig.state, &who.account.handle, &who.seed).await);

    // The landed session is a session, and it is not a fresh proof: it can
    // read the account's state and cannot change the email.
    let landed_bearer = token(&landed);
    let (st, _) = get(&rig.state, "/recovery-email", &landed_bearer).await;
    assert_eq!(st, StatusCode::OK);
    let (st, body) = delete(&rig.state, "/recovery-email", &landed_bearer, &json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "fresh-auth-required");

    // The ticket is spent.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/password/finish",
        None,
        &finish,
    )
    .await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, TICKET_REFUSED)
    );

    // The owner's other device stays signed in and is told.
    assert_eq!(
        notices(&rig.state, &owner_bearer).await,
        ["recovery-email-added", "password-changed-by-email"]
    );
}

// ----------------------------------------------------------------
// Phrase-protected accounts
// ----------------------------------------------------------------

#[tokio::test]
async fn saving_a_phrase_removes_the_vault_and_forbids_a_new_one() {
    let pool = db_or_skip!("saving_a_phrase_removes_the_vault_and_forbids_a_new_one");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let address = fresh_address();
    enrol(&rig, &who, &address, Some(&sealed_vault())).await;

    let (st, body) = delete(&rig.state, "/recovery-email/vault", &bearer, &json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "fresh-auth-required");
    assert_eq!(rows(&pool, "email_vaults", &who.account.id).await, 1);

    let with_grant = json!({ "fresh_grant": grant(&who.session) });
    let (st, body) = delete(&rig.state, "/recovery-email/vault", &bearer, &with_grant).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(
        json_of(&body),
        json!({ "vault_removed": true, "phrase_saved": true })
    );
    // Again: nothing more to remove, and no second notice.
    let (st, body) = delete(&rig.state, "/recovery-email/vault", &bearer, &with_grant).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body)["vault_removed"], false);

    let (_, body) = get(&rig.state, "/recovery-email", &bearer).await;
    assert_eq!(
        json_of(&body),
        json!({ "available": true, "enabled": true, "vault": false, "phrase_saved": true, "totp": false })
    );
    assert_eq!(
        notices(&rig.state, &bearer).await,
        ["recovery-email-added", "recovery-phrase-saved"]
    );

    // A mailed code now changes the password only.
    let ticket = ticket_for(&rig, &who.account.handle, &address).await["ticket"].clone();
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body), json!({ "mode": "password" }));

    // And the account never gets a vault again.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/start",
        Some(&bearer),
        &json!({
            "fresh_grant": grant(&who.session),
            "handle": who.account.handle,
            "address": address,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let mailed = rig.mail.sent().last().unwrap().1.clone();
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/finish",
        Some(&bearer),
        &json!({
            "challenge_id": json_of(&body)["challenge_id"],
            "code": mailed,
            "sealed_entropy": B64.encode(sealed_vault()),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&body), "phrase-saved");
    assert_eq!(rows(&pool, "email_vaults", &who.account.id).await, 0);
}

/// `accounts.phrase_saved` as stored.
async fn phrase_saved(pool: &MySqlPool, account: &[u8; 16]) -> bool {
    sqlx::query_scalar("SELECT phrase_saved FROM accounts WHERE id = ?")
        .bind(&account[..])
        .fetch_one(pool)
        .await
        .expect("account row")
}

/// A key rotation from the person's session: a new phrase and identity,
/// signed by the old key. `flag` is the body's `phrase_saved` (absent when
/// `None`). Returns the rotation's answer.
async fn rotate(
    rig: &Rig,
    who: &Person,
    new_seed: &[u8],
    new_key: &SigningKey,
    flag: Option<bool>,
) -> Value {
    let bearer = token(&who.session);
    let now_s = now_ms() / 1000;
    let proof = lakko_transparency::SupersessionProof::new(
        pubkey(&who.key),
        pubkey(new_key),
        now_s,
        now_s + 14 * 86_400,
    )
    .sign(&who.key);
    let (reg, request) = reg_start(new_seed);
    let (st, body) = post(
        &rig.state,
        "/account/keys/rotate/start",
        Some(&bearer),
        &json!({ "fresh_grant": grant(&who.session), "recovery_request": request }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let upload = reg_finish(
        reg,
        new_seed,
        json_of(&body)["recovery_response"].as_str().unwrap(),
    );
    let mut finish = json!({
        "fresh_grant": grant(&who.session),
        "recovery_upload": upload,
        "new_signature_pubkey_b64": b64u(&pubkey(new_key)),
        "supersession_proof_b64": b64u(&proof),
    });
    if let Some(flag) = flag {
        finish["phrase_saved"] = json!(flag);
    }
    let (st, body) = post(
        &rig.state,
        "/account/keys/rotate/finish",
        Some(&bearer),
        &finish,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    json_of(&body)
}

/// The phrase-arm recall of `account` by the holder of `old_key` and
/// `old_seed`; asserts that it succeeds.
async fn recall_with_phrase(rig: &Rig, account: &Account, old_key: &SigningKey, old_seed: &[u8]) {
    let (st, body) = status_by_handle(&rig.state, old_key, &account.handle).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let seq = json_of(&body)["anchor_seq"].as_u64().unwrap();
    let new_seed = fresh_seed();
    let attempt = phrase_start(
        &rig.state,
        old_key,
        &account.id,
        seq,
        old_seed,
        b"after the recall",
        &new_seed,
    )
    .await
    .unwrap_or_else(|(st, body)| panic!("start refused: {st} {}", text(&body)));
    let finish = finish_body(
        attempt,
        old_key,
        &account.id,
        seq,
        old_seed,
        b"after the recall",
        &new_seed,
        &fresh_key(),
    )
    .unwrap();
    let (st, body) = post(&rig.state, "/account/recall/finish", None, &finish).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
}

#[tokio::test]
async fn a_reset_and_a_recall_leave_phrase_saved_as_it_was() {
    let pool = db_or_skip!("a_reset_and_a_recall_leave_phrase_saved_as_it_was");
    let rig = rig(pool.clone());

    // An account that saved a phrase stays phrase-protected through both.
    let saved = person(&rig.state).await;
    let (st, _) = delete(
        &rig.state,
        "/recovery-email/vault",
        &token(&saved.session),
        &json!({ "fresh_grant": grant(&saved.session) }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(phrase_saved(&pool, &saved.account.id).await);
    takeover(
        &rig.state,
        &saved.account,
        &saved.password,
        &fresh_seed(),
        &fresh_key(),
    )
    .await;
    assert!(
        phrase_saved(&pool, &saved.account.id).await,
        "a key reset must not clear the flag"
    );
    recall_with_phrase(&rig, &saved.account, &saved.key, &saved.seed).await;
    assert!(
        phrase_saved(&pool, &saved.account.id).await,
        "a recall must not clear the flag"
    );

    // An account that never saved one is not marked by either.
    let unsaved = person(&rig.state).await;
    assert!(!phrase_saved(&pool, &unsaved.account.id).await);
    takeover(
        &rig.state,
        &unsaved.account,
        &unsaved.password,
        &fresh_seed(),
        &fresh_key(),
    )
    .await;
    assert!(
        !phrase_saved(&pool, &unsaved.account.id).await,
        "a key reset must not set the flag"
    );
    recall_with_phrase(&rig, &unsaved.account, &unsaved.key, &unsaved.seed).await;
    assert!(
        !phrase_saved(&pool, &unsaved.account.id).await,
        "a recall must not set the flag"
    );
}

#[tokio::test]
async fn a_rotation_sets_phrase_saved_only_when_it_says_so() {
    let pool = db_or_skip!("a_rotation_sets_phrase_saved_only_when_it_says_so");
    let rig = rig(pool.clone());

    // Absent, and explicitly false: unchanged, and no notice.
    for flag in [None, Some(false)] {
        let who = person(&rig.state).await;
        let rotated = rotate(&rig, &who, &fresh_seed(), &fresh_key(), flag).await;
        assert!(!phrase_saved(&pool, &who.account.id).await, "{flag:?}");
        assert!(notices(&rig.state, &token(&rotated)).await.is_empty());
    }

    // True: set, with the notice.
    let who = person(&rig.state).await;
    let rotated = rotate(&rig, &who, &fresh_seed(), &fresh_key(), Some(true)).await;
    assert!(phrase_saved(&pool, &who.account.id).await);
    assert_eq!(
        notices(&rig.state, &token(&rotated)).await,
        ["recovery-phrase-saved"]
    );

    // A later rotation without the flag does not clear it. The rotated
    // session carries no grant, so the person signs in again first.
    let new_key = fresh_key();
    let who = person(&rig.state).await;
    rotate(&rig, &who, &fresh_seed(), &new_key, Some(true)).await;
    let again = Person {
        session: login(
            &rig.state,
            &who.account.handle,
            &who.password,
            &OWNER_DEVICE,
        )
        .await
        .unwrap(),
        account: Account {
            id: who.account.id,
            handle: who.account.handle.clone(),
        },
        password: who.password.clone(),
        seed: fresh_seed(),
        key: new_key,
    };
    rotate(&rig, &again, &fresh_seed(), &fresh_key(), None).await;
    assert!(phrase_saved(&pool, &who.account.id).await);
}

#[tokio::test]
async fn a_rotation_that_saves_a_phrase_ends_the_mail_protected_mode() {
    let pool = db_or_skip!("a_rotation_that_saves_a_phrase_ends_the_mail_protected_mode");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let address = fresh_address();
    enrol(&rig, &who, &address, Some(&sealed_vault())).await;

    // "Save a recovery phrase": a key rotation (new phrase, new identity,
    // signed by the old key) that says so.
    let rotated = token(&rotate(&rig, &who, &fresh_seed(), &fresh_key(), Some(true)).await);

    // The vault of the old phrase is gone, the address is kept, and the
    // account is phrase-protected.
    let (_, body) = get(&rig.state, "/recovery-email", &rotated).await;
    assert_eq!(
        json_of(&body),
        json!({ "available": true, "enabled": true, "vault": false, "phrase_saved": true, "totp": false })
    );
    assert_eq!(
        notices(&rig.state, &rotated).await,
        ["recovery-email-added", "recovery-phrase-saved"]
    );
    // The anchor of the old key remembers that the old phrase sat in a vault:
    // the old phrase recalls nothing.
    let mail_protected: bool =
        sqlx::query_scalar("SELECT mail_protected FROM recall_anchors WHERE account_id = ?")
            .bind(&who.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(mail_protected);

    // A mailed code now gives a new password and no keys.
    let ticket = ticket_for(&rig, &who.account.handle, &address).await["ticket"].clone();
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body), json!({ "mode": "password" }));
}

// ----------------------------------------------------------------
// PUT /recovery-email/vault (amended 2026-10-13)
// ----------------------------------------------------------------

async fn put_vault(
    state: &AppState,
    bearer: &str,
    handle: &str,
    address: &str,
    salt: &[u8],
    sealed: &[u8],
) -> (StatusCode, Bytes) {
    call(
        state,
        Method::PUT,
        "/recovery-email/vault",
        Some(bearer),
        None,
        Some(&json!({
            "handle": handle,
            "address": address,
            "vault_salt": B64.encode(salt),
            "sealed_entropy": B64.encode(sealed),
        })),
    )
    .await
}

#[tokio::test]
async fn a_device_that_kept_the_address_stores_a_vault_again_after_a_reset() {
    let pool = db_or_skip!("a_device_that_kept_the_address_stores_a_vault_again_after_a_reset");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let address = fresh_address();
    let first_vault = sealed_vault();
    enrol(&rig, &who, &address, Some(&first_vault)).await;
    let letters = rig.mail.sent().len();

    // While a vault exists nothing can be put over it, even with the right
    // address.
    let (st, body) = put_vault(
        &rig.state,
        &token(&who.session),
        &who.account.handle,
        &address,
        &[9u8; 16],
        &sealed_vault(),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{}", text(&body));
    assert_eq!(code(&body), "vault-exists");
    let at_rest: Vec<u8> =
        sqlx::query_scalar("SELECT body_enc FROM email_vaults WHERE account_id = ?")
            .bind(&who.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        lakko_crypto::aead::open_with_key(&VAULT_KEY, &at_rest).unwrap(),
        first_vault
    );

    // The owner resets the account keys: the vault of the old phrase goes,
    // the confirmed address stays.
    let (st, body) = reset_keys(&rig.state, &who.session, &fresh_seed(), &fresh_key()).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let after_reset = token(&json_of(&body));
    assert_eq!(rows(&pool, "email_vaults", &who.account.id).await, 0);
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);

    // Malformed bodies cost nothing and store nothing.
    let good_salt = [0x5A; 16];
    let new_vault = sealed_vault();
    for (salt, sealed) in [
        (&[0u8; 15][..], &new_vault[..]),
        (&good_salt[..], &[0u8; 60][..]),
    ] {
        let (st, _) = put_vault(
            &rig.state,
            &after_reset,
            &who.account.handle,
            &address,
            salt,
            sealed,
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }
    let (st, body) = put_vault(
        &rig.state,
        &after_reset,
        &who.account.handle,
        "not an address",
        &good_salt,
        &new_vault,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(code(&body), "address-invalid");

    // Another address, and another account's handle with the right address.
    let (st, body) = put_vault(
        &rig.state,
        &after_reset,
        &who.account.handle,
        &fresh_address(),
        &good_salt,
        &new_vault,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{}", text(&body));
    assert_eq!(code(&body), "address-mismatch");
    let other = person(&rig.state).await;
    let (st, body) = put_vault(
        &rig.state,
        &after_reset,
        &other.account.handle,
        &address,
        &good_salt,
        &new_vault,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&body), "handle-mismatch");
    assert_eq!(rows(&pool, "email_vaults", &who.account.id).await, 0);

    // The device that kept the address: no grant, no letter, the vault is
    // back. The address is normalised as at enrolment.
    let (st, body) = put_vault(
        &rig.state,
        &after_reset,
        &who.account.handle,
        &format!("  {}  ", address.to_uppercase()),
        &good_salt,
        &new_vault,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(json_of(&body), json!({ "vault": true }));
    assert_eq!(rig.mail.sent().len(), letters, "no letter is sent");
    let (_, body) = get(&rig.state, "/recovery-email", &after_reset).await;
    assert_eq!(
        json_of(&body),
        json!({
            "available": true, "enabled": true, "vault": true, "phrase_saved": false,
            "totp": false,
        })
    );
    // No notice for it.
    assert_eq!(
        notices(&rig.state, &after_reset).await,
        ["recovery-email-added"]
    );

    // A mailed code releases exactly what was stored, under the client's salt.
    let ticket = ticket_for(&rig, &who.account.handle, &address).await["ticket"].clone();
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let opened = json_of(&body);
    assert_eq!(opened["mode"], "vault");
    assert_eq!(
        B64.decode(opened["vault_salt"].as_str().unwrap()).unwrap(),
        good_salt
    );
    assert_eq!(
        B64.decode(opened["sealed_entropy"].as_str().unwrap())
            .unwrap(),
        new_vault
    );

    // And a second put is refused: it never replaces.
    let (st, body) = put_vault(
        &rig.state,
        &after_reset,
        &who.account.handle,
        &address,
        &good_salt,
        &sealed_vault(),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&body), "vault-exists");
}

#[tokio::test]
async fn a_vault_is_not_stored_without_a_confirmed_address_or_once_a_phrase_is_saved() {
    let pool = db_or_skip!("a_vault_is_not_stored_without_a_confirmed_address");
    let rig = rig(pool.clone());

    // No recovery email at all.
    let bare = person(&rig.state).await;
    let (st, body) = put_vault(
        &rig.state,
        &token(&bare.session),
        &bare.account.handle,
        &fresh_address(),
        &[1u8; 16],
        &sealed_vault(),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{}", text(&body));
    assert_eq!(code(&body), "no-recovery-email");

    // A phrase-protected account with a confirmed address and no vault.
    let saved = person(&rig.state).await;
    let bearer = token(&saved.session);
    let address = fresh_address();
    enrol(&rig, &saved, &address, None).await;
    let (st, _) = delete(
        &rig.state,
        "/recovery-email/vault",
        &bearer,
        &json!({ "fresh_grant": grant(&saved.session) }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, body) = put_vault(
        &rig.state,
        &bearer,
        &saved.account.handle,
        &address,
        &[1u8; 16],
        &sealed_vault(),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{}", text(&body));
    assert_eq!(code(&body), "phrase-saved");
    // The wrong address is told so before anything about the phrase.
    let (st, body) = put_vault(
        &rig.state,
        &bearer,
        &saved.account.handle,
        &fresh_address(),
        &[1u8; 16],
        &sealed_vault(),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "address-mismatch");
    assert_eq!(rows(&pool, "email_vaults", &saved.account.id).await, 0);
    assert_eq!(rows(&pool, "email_vaults", &bare.account.id).await, 0);

    // No session: nothing.
    let (st, _) = call(
        &rig.state,
        Method::PUT,
        "/recovery-email/vault",
        None,
        None,
        Some(&json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // The feature off: the usual 503, before the body is read for meaning.
    let mut off = build_state(pool.clone(), wide());
    off.vault_enc_key = Some(Arc::new(VAULT_KEY));
    let (st, body) = put_vault(
        &off,
        &bearer,
        &saved.account.handle,
        &address,
        &[1u8; 16],
        &sealed_vault(),
    )
    .await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, DISABLED)
    );
}

#[tokio::test]
async fn address_guesses_against_the_stored_key_are_rationed_per_account() {
    let pool = db_or_skip!("address_guesses_against_the_stored_key_are_rationed_per_account");
    let rig = rig_with(
        pool,
        wide().with_name_window(Scope::EmailVaultStore, Window::new(3, 900)),
    );
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    enrol(&rig, &who, &fresh_address(), None).await;
    for _ in 0..3 {
        let (st, body) = put_vault(
            &rig.state,
            &bearer,
            &who.account.handle,
            &fresh_address(),
            &[1u8; 16],
            &sealed_vault(),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert_eq!(code(&body), "address-mismatch");
    }
    let (st, body) = put_vault(
        &rig.state,
        &bearer,
        &who.account.handle,
        &fresh_address(),
        &[1u8; 16],
        &sealed_vault(),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&body), "too-many-attempts");
    assert!(json_of(&body)["retry_after_secs"].as_u64().unwrap() >= 1);
}

// ----------------------------------------------------------------
// ADR-0076 section 2: a change of login handle
// ----------------------------------------------------------------

async fn put_handle(state: &AppState, bearer: &str, body: &Value) -> (StatusCode, Bytes) {
    call(
        state,
        Method::PUT,
        "/auth/login-handle",
        Some(bearer),
        None,
        Some(body),
    )
    .await
}

/// `POST /auth/login-handle/check`: the `state` it answers.
async fn handle_state(state: &AppState, bearer: &str, handle: &str) -> String {
    let (st, body) = post(
        state,
        "/auth/login-handle/check",
        Some(bearer),
        &json!({ "handle": handle }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    json_of(&body)["state"].as_str().unwrap().to_string()
}

/// Whether a recover start for the pair sends a letter.
async fn mail_reaches(rig: &Rig, handle: &str, address: &str) -> bool {
    let before = rig.mail.sent().len();
    let (st, _) = recover_start(rig, handle, address).await;
    assert_eq!(st, StatusCode::OK);
    for _ in 0..30 {
        if rig.mail.sent().len() > before {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test]
async fn a_rename_with_the_address_moves_the_recovery_email_to_the_new_handle() {
    let pool = db_or_skip!("a_rename_with_the_address_moves_the_recovery_email");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let old_handle = who.account.handle.clone();
    let address = fresh_address();
    let old_vault = sealed_vault();
    enrol(&rig, &who, &address, Some(&old_vault)).await;
    let letters = rig.mail.sent().len();
    let (_, new_handle) = crate::login_handle::random_handle();
    let new_vault = sealed_vault();
    let new_salt = [0x6B; 16];
    let rekey = |address: &str| {
        json!({
            "handle": new_handle,
            "fresh_grant": grant(&who.session),
            "previous_handle": old_handle,
            "recovery_email": {
                "address": address,
                "vault_salt": B64.encode(new_salt),
                "sealed_entropy": B64.encode(&new_vault),
            },
        })
    };

    // Another address: the whole bind is refused and nothing changes, the
    // handle included.
    let (st, body) = put_handle(&rig.state, &bearer, &rekey(&fresh_address())).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{}", text(&body));
    assert_eq!(code(&body), "address-mismatch");
    assert_eq!(handle_state(&rig.state, &bearer, &old_handle).await, "same");
    // A previous handle that is not the bound one.
    let mut wrong_previous = rekey(&address);
    wrong_previous["previous_handle"] = json!(crate::login_handle::random_handle().1);
    let (st, body) = put_handle(&rig.state, &bearer, &wrong_previous).await;
    assert_eq!(st, StatusCode::CONFLICT, "{}", text(&body));
    assert_eq!(code(&body), "handle-mismatch");
    // No previous handle at all: the stored key cannot be checked.
    let mut no_previous = rekey(&address);
    no_previous
        .as_object_mut()
        .unwrap()
        .remove("previous_handle");
    let (st, body) = put_handle(&rig.state, &bearer, &no_previous).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&body), "handle-mismatch");
    // Malformed: a salt without a blob, an address the rule refuses.
    let mut half = rekey(&address);
    half["recovery_email"]
        .as_object_mut()
        .unwrap()
        .remove("sealed_entropy");
    assert_eq!(
        put_handle(&rig.state, &bearer, &half).await.0,
        StatusCode::BAD_REQUEST
    );
    let (st, body) = put_handle(&rig.state, &bearer, &rekey("not an address")).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(code(&body), "address-invalid");
    // Everything is as it was: the old pair still gets a letter.
    assert_eq!(handle_state(&rig.state, &bearer, &old_handle).await, "same");
    assert!(mail_reaches(&rig, &old_handle, &address).await);
    let letters = letters + 1;

    // The rename with the right address: no letter, no notice, and the
    // recovery email follows the account to the new name.
    let (st, body) = put_handle(&rig.state, &bearer, &rekey(&address)).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(rig.mail.sent().len(), letters);
    assert_eq!(handle_state(&rig.state, &bearer, &new_handle).await, "same");
    assert_eq!(
        handle_state(&rig.state, &bearer, &old_handle).await,
        "other"
    );
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);
    assert_eq!(rows(&pool, "email_vaults", &who.account.id).await, 1);
    assert_eq!(notices(&rig.state, &bearer).await, ["recovery-email-added"]);

    // By mail under the OLD handle: nothing any more.
    assert!(!mail_reaches(&rig, &old_handle, &address).await);
    // Under the NEW handle: the code, and the vault re-sealed for it.
    let ticket = ticket_for(&rig, &new_handle, &address).await["ticket"].clone();
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let opened = json_of(&body);
    assert_eq!(opened["mode"], "vault");
    assert_eq!(
        B64.decode(opened["vault_salt"].as_str().unwrap()).unwrap(),
        new_salt
    );
    assert_eq!(
        B64.decode(opened["sealed_entropy"].as_str().unwrap())
            .unwrap(),
        new_vault
    );

    // A re-key without a vault keeps the address and removes the vault that
    // was sealed under the handle being left.
    let (_, third_handle) = crate::login_handle::random_handle();
    let (st, body) = put_handle(
        &rig.state,
        &bearer,
        &json!({
            "handle": third_handle,
            "fresh_grant": grant(&who.session),
            "previous_handle": new_handle,
            "recovery_email": { "address": address },
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);
    assert_eq!(rows(&pool, "email_vaults", &who.account.id).await, 0);
    assert!(mail_reaches(&rig, &third_handle, &address).await);

    // A phrase-protected account: the key moves, a vault sent with it is
    // not stored.
    let saved = person(&rig.state).await;
    let saved_bearer = token(&saved.session);
    let saved_address = fresh_address();
    enrol(&rig, &saved, &saved_address, None).await;
    let (st, _) = delete(
        &rig.state,
        "/recovery-email/vault",
        &saved_bearer,
        &json!({ "fresh_grant": grant(&saved.session) }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, saved_new) = crate::login_handle::random_handle();
    let (st, body) = put_handle(
        &rig.state,
        &saved_bearer,
        &json!({
            "handle": saved_new,
            "fresh_grant": grant(&saved.session),
            "previous_handle": saved.account.handle,
            "recovery_email": {
                "address": saved_address,
                "vault_salt": B64.encode([1u8; 16]),
                "sealed_entropy": B64.encode(sealed_vault()),
            },
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(rows(&pool, "email_vaults", &saved.account.id).await, 0);
    assert!(mail_reaches(&rig, &saved_new, &saved_address).await);

    // An account without a recovery email: the object changes nothing and
    // refuses nothing.
    let bare = person(&rig.state).await;
    let (_, bare_new) = crate::login_handle::random_handle();
    let (st, body) = put_handle(
        &rig.state,
        &token(&bare.session),
        &json!({
            "handle": bare_new,
            "fresh_grant": grant(&bare.session),
            "previous_handle": bare.account.handle,
            "recovery_email": { "address": fresh_address() },
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(rows(&pool, "recovery_emails", &bare.account.id).await, 0);
}

#[tokio::test]
async fn a_change_of_handle_needs_a_fresh_grant_and_a_first_bind_does_not() {
    let pool = db_or_skip!("a_change_of_handle_needs_a_fresh_grant");
    // Two binds an hour, and the default minute between two changes.
    let rig = rig_with(
        pool.clone(),
        wide()
            .with_name_window(Scope::HandleBind, Window::new(4, 3600))
            .with_handle_change_interval(60),
    );
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let fresh = grant(&who.session);
    let first = who.account.handle.clone();
    let (_, second) = crate::login_handle::random_handle();
    let (_, third) = crate::login_handle::random_handle();

    // A session alone cannot move the name, however often it asks: the
    // refusal comes before the bind counter, so it spends none of it.
    for bad in [Value::Null, json!("not-a-grant")] {
        for _ in 0..6 {
            let (st, body) = put_handle(
                &rig.state,
                &bearer,
                &json!({ "handle": second, "fresh_grant": bad }),
            )
            .await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{}", text(&body));
            assert_eq!(code(&body), "fresh-auth-required");
        }
    }
    assert_eq!(handle_state(&rig.state, &bearer, &first).await, "same");

    // Re-binding the handle the account holds is not a change: no grant.
    let (st, body) = put_handle(&rig.state, &bearer, &json!({ "handle": first })).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));

    // With the grant the name moves. (The fixture bound the first handle a
    // moment ago; the minute between two changes is over for it.)
    sqlx::query(
        "UPDATE login_handle_changes SET last_changed_at = NOW(3) - INTERVAL 1 HOUR \
         WHERE account_id = ?",
    )
    .bind(&who.account.id[..])
    .execute(&pool)
    .await
    .unwrap();
    let (st, body) = put_handle(
        &rig.state,
        &bearer,
        &json!({ "handle": second, "fresh_grant": fresh }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(handle_state(&rig.state, &bearer, &second).await, "same");

    // A stale device of the account still holds the old name. It is told
    // `other`, and its bind of the old name, a change with no grant, is
    // refused: it cannot revert the rename.
    assert_eq!(handle_state(&rig.state, &bearer, &first).await, "other");
    let (st, body) = put_handle(&rig.state, &bearer, &json!({ "handle": first })).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "fresh-auth-required");
    assert_eq!(handle_state(&rig.state, &bearer, &second).await, "same");

    // Inside the minute another change is refused by the interval even with
    // the grant; restoring the handle held just before is exempt from the
    // interval and still needs the grant (refused just above without it).
    let (st, body) = put_handle(
        &rig.state,
        &bearer,
        &json!({ "handle": third, "fresh_grant": fresh }),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{}", text(&body));
    assert_eq!(code(&body), "handle-bind-limited");
    let (st, body) = put_handle(
        &rig.state,
        &bearer,
        &json!({ "handle": first, "fresh_grant": fresh }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(handle_state(&rig.state, &bearer, &first).await, "same");

    // An account with no handle binds its first one with the session alone.
    let guest = person(&rig.state).await;
    let guest_bearer = token(&guest.session);
    let (st, _) = call(
        &rig.state,
        Method::DELETE,
        "/auth/login-handle",
        Some(&guest_bearer),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        handle_state(&rig.state, &guest_bearer, &guest.account.handle).await,
        "none"
    );
    // (The handle just released is the one a bind may restore at once.)
    let (st, body) = put_handle(
        &rig.state,
        &guest_bearer,
        &json!({ "handle": guest.account.handle }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(
        handle_state(&rig.state, &guest_bearer, &guest.account.handle).await,
        "same"
    );
}

#[tokio::test]
async fn the_handle_check_answers_for_the_sessions_account_only_and_is_counted() {
    let pool = db_or_skip!("the_handle_check_answers_for_the_sessions_account_only");
    let rig = rig_with(
        pool,
        wide().with_name_window(Scope::HandleCheck, Window::new(4, 3600)),
    );
    let who = person(&rig.state).await;
    let other = person(&rig.state).await;
    let bearer = token(&who.session);

    assert_eq!(
        handle_state(&rig.state, &bearer, &who.account.handle).await,
        "same"
    );
    // Another account's handle, and a handle nobody holds, read the same:
    // the route says nothing about anyone else.
    assert_eq!(
        handle_state(&rig.state, &bearer, &other.account.handle).await,
        "other"
    );
    assert_eq!(
        handle_state(&rig.state, &bearer, &crate::login_handle::random_handle().1).await,
        "other"
    );
    // Malformed, and no session.
    let (st, _) = post(
        &rig.state,
        "/auth/login-handle/check",
        Some(&bearer),
        &json!({ "handle": "AAAA" }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = post(
        &rig.state,
        "/auth/login-handle/check",
        None,
        &json!({ "handle": who.account.handle }),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Its own per-account counter (three spent above; the malformed one
    // spent none).
    assert_eq!(
        handle_state(&rig.state, &bearer, &who.account.handle).await,
        "same"
    );
    let (st, body) = post(
        &rig.state,
        "/auth/login-handle/check",
        Some(&bearer),
        &json!({ "handle": who.account.handle }),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&body), "too-many-attempts");
    // The other account's counter is its own.
    assert_eq!(
        handle_state(&rig.state, &token(&other.session), &other.account.handle).await,
        "same"
    );
}

// ----------------------------------------------------------------
// The recall's phrase arm
// ----------------------------------------------------------------

/// `recall/status` by login handle: the statement carries the handle's first
/// 16 bytes where the account id goes.
async fn status_by_handle(state: &AppState, key: &SigningKey, handle: &str) -> (StatusCode, Bytes) {
    let handle_bytes = URL_SAFE_NO_PAD.decode(handle).unwrap();
    let stated: [u8; 16] = handle_bytes[..16].try_into().unwrap();
    post(
        state,
        "/account/recall/status",
        None,
        &json!({
            "handle": handle,
            "statement_b64": statement(
                key,
                AccountStatementKind::RecallStatus,
                &stated,
                &[],
                now_ms(),
            ),
        }),
    )
    .await
}

/// `recall/start` with `proof: "phrase"`: the login is made from the OLD
/// recovery seed.
async fn phrase_start(
    state: &AppState,
    key: &SigningKey,
    account: &[u8; 16],
    seq: u64,
    old_seed: &[u8],
    new_password: &[u8],
    new_seed: &[u8],
) -> Result<Attempt, (StatusCode, Bytes)> {
    let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, old_seed).unwrap();
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
            "proof": "phrase",
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

#[tokio::test]
async fn the_phrase_arm_recalls_a_phrase_protected_account_and_not_a_mail_protected_one() {
    let pool = db_or_skip!("the_phrase_arm_recalls_a_phrase_protected_account");
    let rig = rig(pool.clone());

    // --- A phrase-protected account: no email vault when the keys change.
    let owner = person(&rig.state).await;
    let attacker_seed = fresh_seed();
    let attacker_key = fresh_key();
    takeover(
        &rig.state,
        &owner.account,
        &owner.password,
        &attacker_seed,
        &attacker_key,
    )
    .await;

    // The owner holds the phrase (the old seed and the key derived from it),
    // no device, no account id, and does not use the old password.
    let (st, body) = status_by_handle(&rig.state, &owner.key, &owner.account.handle).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let status = json_of(&body);
    assert_eq!(status["account_id_b64"], b64u(&owner.account.id));
    assert_eq!(status["phrase_arm"], true);
    let seq = status["anchor_seq"].as_u64().unwrap();

    // By handle, a key that is not the anchor's, and a name nobody has, get
    // the one refusal.
    let (st, body) = status_by_handle(&rig.state, &fresh_key(), &owner.account.handle).await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, RECALL_REFUSED)
    );
    let (_, unknown) = crate::login_handle::random_handle();
    let (st, body) = status_by_handle(&rig.state, &owner.key, &unknown).await;
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, RECALL_REFUSED)
    );

    let new_password = b"taken back";
    let new_seed = fresh_seed();
    let new_key = fresh_key();
    let attempt = phrase_start(
        &rig.state,
        &owner.key,
        &owner.account.id,
        seq,
        &owner.seed,
        new_password,
        &new_seed,
    )
    .await
    .unwrap_or_else(|(st, body)| panic!("phrase start refused: {st} {}", text(&body)));
    let finish = finish_body(
        attempt,
        &owner.key,
        &owner.account.id,
        seq,
        &owner.seed,
        new_password,
        &new_seed,
        &new_key,
    )
    .expect("the old seed opens the anchored recovery envelope");
    let (st, body) = post(&rig.state, "/account/recall/finish", None, &finish).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let recalled = json_of(&body);
    assert!(recalled["key_reset_attest_b64"].is_string());
    // The account is the owner's again: the new password signs in, the
    // attacker's phrase and session do not.
    assert!(login(
        &rig.state,
        &owner.account.handle,
        new_password,
        &OWNER_DEVICE
    )
    .await
    .is_some());
    assert!(!recovery_login(&rig.state, &owner.account.handle, &attacker_seed).await);
    assert!(recovery_login(&rig.state, &owner.account.handle, &new_seed).await);

    // A wrong phrase with the right key cannot finish: the client cannot
    // complete the login, as with a wrong password.
    let second = person(&rig.state).await;
    takeover(
        &rig.state,
        &second.account,
        &second.password,
        &fresh_seed(),
        &fresh_key(),
    )
    .await;
    let (_, body) = status_by_handle(&rig.state, &second.key, &second.account.handle).await;
    let seq = json_of(&body)["anchor_seq"].as_u64().unwrap();
    let wrong_seed = fresh_seed();
    let attempt = phrase_start(
        &rig.state,
        &second.key,
        &second.account.id,
        seq,
        &wrong_seed,
        b"x",
        &fresh_seed(),
    )
    .await
    .unwrap_or_else(|(st, body)| panic!("start refused: {st} {}", text(&body)));
    assert!(finish_body(
        attempt,
        &second.key,
        &second.account.id,
        seq,
        &wrong_seed,
        b"x",
        &fresh_seed(),
        &fresh_key(),
    )
    .is_none());

    // --- A mail-protected account: it had an email vault at the change, so
    // whoever came in by mail holds the same phrase. The phrase arm is
    // refused, with the uniform refusal and nothing counted.
    let mailed = person(&rig.state).await;
    enrol(&rig, &mailed, &fresh_address(), Some(&sealed_vault())).await;
    takeover(
        &rig.state,
        &mailed.account,
        &mailed.password,
        &fresh_seed(),
        &fresh_key(),
    )
    .await;
    // The reset deleted the vault of the old phrase...
    assert_eq!(rows(&pool, "email_vaults", &mailed.account.id).await, 0);
    // ...and the anchor remembers that there was one.
    let (st, body) = status_by_handle(&rig.state, &mailed.key, &mailed.account.handle).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let status = json_of(&body);
    assert_eq!(status["phrase_arm"], false);
    let seq = status["anchor_seq"].as_u64().unwrap();
    let Err((st, body)) = phrase_start(
        &rig.state,
        &mailed.key,
        &mailed.account.id,
        seq,
        &mailed.seed,
        b"x",
        &fresh_seed(),
    )
    .await
    else {
        panic!("the phrase arm must be refused for a mail-protected account");
    };
    assert_eq!(
        (st, text(&body).as_str()),
        (StatusCode::UNAUTHORIZED, RECALL_REFUSED)
    );
    let attempts: u32 =
        sqlx::query_scalar("SELECT attempts FROM recall_anchors WHERE account_id = ?")
            .bind(&mailed.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attempts, 0, "a refused arm spends no attempt");

    // An unknown proof name is a malformed request.
    let (st, _) = post(
        &rig.state,
        "/account/recall/start",
        None,
        &json!({
            "account_id_b64": b64u(&mailed.account.id),
            "anchor_seq": seq,
            "proof": "email",
            "statement_b64": "AA",
            "login_request": B64.encode(
                ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, b"x")
                    .unwrap()
                    .message
                    .serialize()
            ),
            "password_request": reg_start(b"x").1,
            "recovery_request": reg_start(b"x").1,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_recall_removes_the_displaced_partys_recovery_email() {
    let pool = db_or_skip!("a_recall_removes_the_displaced_partys_recovery_email");
    let rig = rig(pool.clone());
    let owner = person(&rig.state).await;
    takeover(
        &rig.state,
        &owner.account,
        &owner.password,
        &fresh_seed(),
        &fresh_key(),
    )
    .await;
    // The attacker signs in again (the reset's answer carries no grant) and
    // adds a mailbox of their own.
    let attacker = Person {
        session: login(
            &rig.state,
            &owner.account.handle,
            &owner.password,
            &OWNER_DEVICE,
        )
        .await
        .unwrap(),
        account: Account {
            id: owner.account.id,
            handle: owner.account.handle.clone(),
        },
        password: owner.password.clone(),
        seed: fresh_seed(),
        key: fresh_key(),
    };
    enrol(&rig, &attacker, &fresh_address(), None).await;
    assert_eq!(rows(&pool, "recovery_emails", &owner.account.id).await, 1);

    let (_, body) = status_by_handle(&rig.state, &owner.key, &owner.account.handle).await;
    let seq = json_of(&body)["anchor_seq"].as_u64().unwrap();
    let new_seed = fresh_seed();
    let attempt = phrase_start(
        &rig.state,
        &owner.key,
        &owner.account.id,
        seq,
        &owner.seed,
        b"mine again",
        &new_seed,
    )
    .await
    .unwrap_or_else(|(st, body)| panic!("start refused: {st} {}", text(&body)));
    let finish = finish_body(
        attempt,
        &owner.key,
        &owner.account.id,
        seq,
        &owner.seed,
        b"mine again",
        &new_seed,
        &fresh_key(),
    )
    .unwrap();
    let (st, body) = post(&rig.state, "/account/recall/finish", None, &finish).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(rows(&pool, "recovery_emails", &owner.account.id).await, 0);
}

// ----------------------------------------------------------------
// Notices
// ----------------------------------------------------------------

#[tokio::test]
async fn notices_are_paged_by_cursor_scoped_to_the_account_and_pruned() {
    let pool = db_or_skip!("notices_are_paged_by_cursor_scoped_to_the_account_and_pruned");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let other = person(&rig.state).await;
    let bearer = token(&who.session);
    let now = now_ms();
    for i in 0..150_i64 {
        sqlx::query("INSERT INTO account_notices (account_id, kind, at_ms) VALUES (?, ?, ?)")
            .bind(&who.account.id[..])
            .bind(1 + u8::try_from(i % 6).unwrap())
            .bind(now + i)
            .execute(&pool)
            .await
            .unwrap();
    }
    // A kind a newer build wrote: skipped, and the cursor still passes it.
    sqlx::query("INSERT INTO account_notices (account_id, kind, at_ms) VALUES (?, 200, ?)")
        .bind(&who.account.id[..])
        .bind(now + 150)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO account_notices (account_id, kind, at_ms) VALUES (?, 1, ?)")
        .bind(&other.account.id[..])
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();

    let (st, body) = get(&rig.state, "/account/notices", &bearer).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let first = json_of(&body);
    let page = first["notices"].as_array().unwrap();
    assert_eq!(page.len(), 100);
    assert_eq!(first["more"], true);
    assert_eq!(page[0]["kind"], "password-changed-by-email");
    assert_eq!(page[0]["at_ms"], now);
    let ids: Vec<u64> = page.iter().map(|n| n["id"].as_u64().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "oldest first");
    let cursor = first["next_after"].as_u64().unwrap();
    assert_eq!(cursor, *ids.last().unwrap());

    let (_, body) = get(
        &rig.state,
        &format!("/account/notices?after={cursor}"),
        &bearer,
    )
    .await;
    let second = json_of(&body);
    assert_eq!(second["notices"].as_array().unwrap().len(), 50);
    assert_eq!(second["more"], false);
    let end = second["next_after"].as_u64().unwrap();
    assert!(
        end > second["notices"][49]["id"].as_u64().unwrap(),
        "the cursor passed the unknown kind"
    );
    let (_, body) = get(
        &rig.state,
        &format!("/account/notices?after={end}"),
        &bearer,
    )
    .await;
    let third = json_of(&body);
    assert!(third["notices"].as_array().unwrap().is_empty());
    assert_eq!(third["next_after"], end);

    // `since_ms` with no cursor: only what happened at or after that time
    // (rows are at now + 0..=150; the last is the unknown kind), paged the
    // same way.
    let since = now + 140;
    let (st, body) = get(
        &rig.state,
        &format!("/account/notices?since_ms={since}"),
        &bearer,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let recent = json_of(&body);
    let recent_page = recent["notices"].as_array().unwrap();
    assert_eq!(recent_page.len(), 10);
    assert!(recent_page
        .iter()
        .all(|n| n["at_ms"].as_i64().unwrap() >= since));
    assert_eq!(recent["more"], false);
    assert_eq!(recent["next_after"], end);
    // With a cursor as well: both bounds hold.
    let (_, body) = get(
        &rig.state,
        &format!("/account/notices?after={cursor}&since_ms={since}"),
        &bearer,
    )
    .await;
    assert_eq!(json_of(&body)["notices"].as_array().unwrap().len(), 10);
    let (_, body) = get(
        &rig.state,
        &format!("/account/notices?since_ms={}", now + 10_000),
        &bearer,
    )
    .await;
    let none = json_of(&body);
    assert!(none["notices"].as_array().unwrap().is_empty());
    assert_eq!(none["next_after"], 0);
    // A long window pages like the plain request.
    let (_, body) = get(
        &rig.state,
        &format!("/account/notices?since_ms={}", now - 30 * 86_400_000),
        &bearer,
    )
    .await;
    let wide_window = json_of(&body);
    assert_eq!(wide_window["notices"].as_array().unwrap().len(), 100);
    assert_eq!(wide_window["more"], true);
    assert_eq!(wide_window["next_after"], cursor);

    // The other account sees its own and nothing else.
    assert_eq!(
        notices(&rig.state, &token(&other.session)).await,
        ["password-changed-by-email"]
    );
    // No session, no notices.
    let (st, _) = call(
        &rig.state,
        Method::GET,
        "/account/notices",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // The pruner, at a clock where every row here is fresh, deletes none of
    // this account's rows (other tests' notices share the table, so only
    // this account is counted)...
    let count = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM account_notices WHERE account_id = ?")
            .bind(&who.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    crate::account_notices::prune(&pool, now).await.unwrap();
    assert_eq!(count().await, 151);
    // ...and deletes a row older than sixty days.
    sqlx::query("INSERT INTO account_notices (account_id, kind, at_ms) VALUES (?, 1, ?)")
        .bind(&who.account.id[..])
        .bind(now - crate::account_notices::RETENTION_MS - 1)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count().await, 152);
    crate::account_notices::prune(&pool, now).await.unwrap();
    assert_eq!(count().await, 151);
}

/// Whether the account row exists.
async fn account_exists(pool: &MySqlPool, id: &[u8; 16]) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM accounts WHERE id = ?")
        .bind(&id[..])
        .fetch_one(pool)
        .await
        .unwrap()
        == 1
}

/// `DELETE /auth/login-handle`, with or without the fresh-auth header.
async fn release_handle(
    state: &AppState,
    bearer: &str,
    fresh: Option<&str>,
) -> (StatusCode, Bytes) {
    call(
        state,
        Method::DELETE,
        "/auth/login-handle",
        Some(bearer),
        fresh.map(|g| ("x-lakko-fresh-grant", g)),
        None,
    )
    .await
}

/// `DELETE /account` on the session alone.
async fn delete_account_bare(state: &AppState, bearer: &str) -> (StatusCode, Bytes) {
    call(state, Method::DELETE, "/account", Some(bearer), None, None).await
}

/// ADR-0032 guest lifecycle meeting the fresh-auth rule: an account nobody
/// named is deleted on its session alone; a named one is not; and a named
/// account past the unclaimed lifetime cannot shed its name on a session
/// alone to become deletable.
#[tokio::test]
async fn an_unnamed_account_is_deleted_on_its_session_and_an_old_one_keeps_its_name() {
    let pool = db_or_skip!("an_unnamed_account_is_deleted_on_its_session");
    let rig = rig_with(pool.clone(), wide());

    // A named account: the session alone deletes nothing.
    let named = person(&rig.state).await;
    let bearer = token(&named.session);
    let (st, body) = delete_account_bare(&rig.state, &bearer).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{}", text(&body));
    assert_eq!(code(&body), "fresh-auth-required");
    assert!(account_exists(&pool, &named.account.id).await);

    // Young (a sign-up or a guest claim being undone): the name is released
    // freely, and the account, now unnamed, goes on its session alone.
    let (st, body) = release_handle(&rig.state, &bearer, None).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let (st, body) = delete_account_bare(&rig.state, &bearer).await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{}", text(&body));
    assert!(!account_exists(&pool, &named.account.id).await);

    // Past the unclaimed lifetime the release needs the grant, so a stolen
    // session cannot turn a named account into a deletable one.
    let old = person(&rig.state).await;
    let bearer = token(&old.session);
    sqlx::query("UPDATE accounts SET created_at = NOW() - INTERVAL 2 DAY WHERE id = ?")
        .bind(&old.account.id[..])
        .execute(&pool)
        .await
        .unwrap();
    let (st, body) = release_handle(&rig.state, &bearer, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{}", text(&body));
    assert_eq!(code(&body), "fresh-auth-required");
    assert_eq!(
        handle_state(&rig.state, &bearer, &old.account.handle).await,
        "same"
    );
    let (st, _) = delete_account_bare(&rig.state, &bearer).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(account_exists(&pool, &old.account.id).await);
    // With the grant the owner may still release it.
    let (st, body) = release_handle(&rig.state, &bearer, Some(&grant(&old.session))).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
}

// ----------------------------------------------------------------
// Logs (ADR-0078 section 11, ADR-0075)
// ----------------------------------------------------------------

/// Everything the service logs while it runs one test, at info and above.
#[derive(Clone, Default)]
struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl CapturedLog {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("log buffer")).into_owned()
    }
}

/// The address and the account never reach the log: enrol with a vault, a
/// recovery by mailed code, the vault released. The operator's relay sees
/// the address when it delivers a letter; a log line that put the account
/// beside that moment would join the two, and the email vault is keyed by
/// the address (ADR-0075).
#[tokio::test(flavor = "current_thread")]
async fn the_recovery_flow_logs_neither_the_address_nor_the_account() {
    let pool = db_or_skip!("the_recovery_flow_logs_neither_the_address_nor_the_account");
    let captured = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let rig = rig(pool);
    let who = person(&rig.state).await;
    let address = fresh_address();
    enrol(&rig, &who, &address, Some(&sealed_vault())).await;
    let ticket = ticket_for(&rig, &who.account.handle, &address).await;
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": ticket["ticket"] }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));

    let log = captured.text();
    assert!(
        log.contains("recovery email: vault released"),
        "the capture saw the flow: {log}"
    );
    let id = who.account.id;
    for needle in [
        address.clone(),
        address.trim().to_lowercase(),
        URL_SAFE_NO_PAD.encode(id),
        B64.encode(id),
        id.iter().fold(String::new(), |mut out, b| {
            use std::fmt::Write as _;
            let _ = write!(out, "{b:02x}");
            out
        }),
    ] {
        assert!(!log.contains(&needle), "the log carries {needle}: {log}");
    }
}

// ---- saving a phrase without a rotation (ADR-0075 section 3, amended) ------

async fn phrase_status(state: &AppState, bearer: &str) -> Value {
    let (st, body) = get(state, "/account/phrase", bearer).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    json_of(&body)
}

#[tokio::test]
async fn a_phrase_never_in_a_vault_is_saved_with_no_key_change() {
    let pool = db_or_skip!("a_phrase_never_in_a_vault_is_saved_with_no_key_change");
    let rig = rig(pool);
    let who = person(&rig.state).await;
    let bearer = token(&who.session);

    assert_eq!(
        phrase_status(&rig.state, &bearer).await,
        json!({ "saved": false, "rotation_needed": false })
    );

    // The proof of section 17.1 is needed.
    let (st, _) = post(
        &rig.state,
        "/account/phrase/saved",
        Some(&bearer),
        &json!({}),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(
        phrase_status(&rig.state, &bearer).await["saved"],
        json!(false)
    );

    let body = json!({ "fresh_grant": grant(&who.session) });
    let (st, answer) = post(&rig.state, "/account/phrase/saved", Some(&bearer), &body).await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{}", text(&answer));
    assert_eq!(
        phrase_status(&rig.state, &bearer).await,
        json!({ "saved": true, "rotation_needed": false })
    );

    // Saying it twice writes one notice, and no key moved.
    let (st, _) = post(&rig.state, "/account/phrase/saved", Some(&bearer), &body).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(
        notices(&rig.state, &bearer).await,
        vec!["recovery-phrase-saved".to_string()]
    );
    let generation: u64 = sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ?")
        .bind(&who.account.id[..])
        .fetch_one(&rig.state.db)
        .await
        .unwrap();
    assert_eq!(generation, 1);
}

#[tokio::test]
async fn a_phrase_that_was_in_a_vault_takes_a_rotation_even_after_the_email_is_gone() {
    let pool =
        db_or_skip!("a_phrase_that_was_in_a_vault_takes_a_rotation_even_after_the_email_is_gone");
    let rig = rig(pool);
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    enrol(&rig, &who, &fresh_address(), Some(&sealed_vault())).await;

    assert_eq!(
        phrase_status(&rig.state, &bearer).await["rotation_needed"],
        json!(true)
    );
    let (st, answer) = post(
        &rig.state,
        "/account/phrase/saved",
        Some(&bearer),
        &json!({ "fresh_grant": grant(&who.session) }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(
        json_of(&answer)["error"],
        json!("phrase-save-needs-rotation")
    );

    // A copy taken while the vault existed still opens this phrase.
    let (st, _) = delete(
        &rig.state,
        "/recovery-email",
        &bearer,
        &json!({ "fresh_grant": grant(&who.session) }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        phrase_status(&rig.state, &bearer).await,
        json!({ "saved": false, "rotation_needed": true })
    );
}

#[tokio::test]
async fn an_address_with_no_vault_does_not_call_for_a_rotation() {
    let pool = db_or_skip!("an_address_with_no_vault_does_not_call_for_a_rotation");
    let rig = rig(pool);
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    enrol(&rig, &who, &fresh_address(), None).await;
    assert_eq!(
        phrase_status(&rig.state, &bearer).await["rotation_needed"],
        json!(false)
    );
}
