// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests for ADR-0075: the recovery email (enrol, replace, remove,
//! recover by mailed code: a new password, never keys, since the email vault
//! was removed on 2026-11-26), the account notices, the recall's phrase arm,
//! and the last-factor refusals of the email (ADR-0075, amended 2026-11-26).
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
    roomy, statement, takeover, text, token, Account, Attempt, OWNER_DEVICE,
};
use crate::session::now_ms;
use crate::AppState;

const DISABLED: &str = r#"{"error":"recovery-email-disabled"}"#;
const CODE_REFUSED: &str = r#"{"error":"code-refused"}"#;
const TICKET_REFUSED: &str = r#"{"error":"ticket-refused"}"#;
const RECALL_REFUSED: &str = r#"{"error":"recall-refused"}"#;

/// `(identity_pubkey, password_registration, recovery_registration)` of a
/// `recall_anchors` row.
type AnchorProbe = (Vec<u8>, Vec<u8>, Option<Vec<u8>>);

pub(crate) const TOTP_KEY: [u8; 32] = [0x55; 32];

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
        .with_name_window(Scope::TotpRecover, Window::new(1000, 900))
        .with_name_window(Scope::HandleBind, Window::new(1000, 900))
        .with_name_window(Scope::LoginRecovery, Window::new(1000, 900))
        .with_name_window(Scope::KeyRotate, Window::new(1000, 900))
        .with_name_window(Scope::ResetFactor, Window::new(1000, 900))
        .with_name_window(Scope::OwnedChannelsSecret, Window::new(1000, 900))
        .with_handle_change_interval(0)
}

/// A state with the recovery email on and a mailbox the test can read.
pub(crate) struct Rig {
    pub(crate) state: AppState,
    pub(crate) mail: Arc<RecordingSender>,
}

pub(crate) fn rig_with(pool: MySqlPool, limits: Limits) -> Rig {
    let mut state = build_state(pool, limits);
    let mail = Arc::new(RecordingSender::new());
    state.recovery_email = Some(Arc::new(crate::recovery_email::Config::new(
        [0x3C; 32],
        mail.clone(),
    )));
    state.totp_enc_key = Some(Arc::new(TOTP_KEY));
    Rig { state, mail }
}

pub(crate) fn rig(pool: MySqlPool) -> Rig {
    rig_with(pool, wide())
}

/// A fresh address nobody else in the test database uses.
pub(crate) fn fresh_address() -> String {
    let mut tag = [0u8; 8];
    OsRng.fill_bytes(&mut tag);
    format!("Person.{}@Example.com", b64u(&tag))
}

async fn get(state: &AppState, uri: &str, bearer: &str) -> (StatusCode, Bytes) {
    call(state, Method::GET, uri, Some(bearer), None, None).await
}

async fn delete(state: &AppState, uri: &str, bearer: &str, body: &Value) -> (StatusCode, Bytes) {
    call(state, Method::DELETE, uri, Some(bearer), None, Some(body)).await
}

/// An account with a password, a recovery seed, a handle and an identity key,
/// and a signed-in session that holds a fresh-auth grant.
pub(crate) struct Person {
    pub(crate) account: Account,
    pub(crate) password: Vec<u8>,
    pub(crate) seed: Vec<u8>,
    pub(crate) key: SigningKey,
    pub(crate) session: Value,
}

pub(crate) async fn person(state: &AppState) -> Person {
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

/// Add (or replace) the person's recovery email through the routes.
pub(crate) async fn enrol(rig: &Rig, who: &Person, address: &str) {
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
    assert!(started.get("vault_salt").is_none());
    let (to, mailed) = rig.mail.sent().last().cloned().expect("a letter was sent");
    assert_eq!(to, address.trim().to_lowercase());
    let finish = json!({ "challenge_id": started["challenge_id"], "code": mailed });
    let (st, body) = post(
        &rig.state,
        "/recovery-email/enrol/finish",
        Some(&bearer),
        &finish,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(json_of(&body), json!({ "enabled": true }));
}

/// Give the person a confirmed authenticator, so removing the email leaves a
/// factor (ADR-0075, amended 2026-11-26).
pub(crate) async fn second_factor(pool: &MySqlPool, who: &Person) -> Vec<u8> {
    crate::totp::enroll_for_test(pool, &TOTP_KEY, &who.account.id).await
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
pub(crate) async fn letter_after(rig: &Rig, before: usize) -> Option<String> {
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
    let state = build_state(pool.clone(), wide());
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
    // The email reset factor too, once the password grant passed.
    for (uri, body) in [
        (
            "/account/keys/reset/factor/email/start",
            json!({
                "fresh_grant": grant(&who.session),
                "handle": who.account.handle,
                "address": "a@b.example",
            }),
        ),
        (
            "/account/keys/reset/factor/email/finish",
            json!({
                "fresh_grant": grant(&who.session),
                "challenge_id": b64u(&[1u8; 16]),
                "code": "123456",
            }),
        ),
    ] {
        let (st, answer) = post(&state, uri, Some(&bearer), &body).await;
        assert_eq!(
            (st, text(&answer).as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, DISABLED),
            "{uri}"
        );
    }

    // The account's own state is still readable, and says mail is off.
    let (st, answer) = get(&state, "/recovery-email", &bearer).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        json_of(&answer),
        json!({ "available": false, "enabled": false, "totp": false })
    );

    // The pepper alone is not enough: the login-handle pepper is part of the
    // feature (the vault key no longer is).
    let mut half = rig(pool).state;
    half.handle_pepper = None;
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

    // The real thing.
    enrol(&rig, &who, &address).await;
    let (_, body) = get(&rig.state, "/recovery-email", &bearer).await;
    assert_eq!(
        json_of(&body),
        json!({ "available": true, "enabled": true, "totp": false })
    );
    // The address is nowhere in the database: only the 32-byte key.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT email_key FROM recovery_emails WHERE account_id = ?")
            .bind(&who.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.len(), 32);

    // Replacing.
    enrol(&rig, &who, &fresh_address()).await;

    // Removing needs the grant too.
    let (st, body) = delete(&rig.state, "/recovery-email", &bearer, &json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "fresh-auth-required");
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);
    // ADR-0075 (amended 2026-11-26): the email is the account's only second
    // factor, so it stays.
    let (st, body) = delete(
        &rig.state,
        "/recovery-email",
        &bearer,
        &json!({ "fresh_grant": grant(&who.session) }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{}", text(&body));
    assert_eq!(code(&body), "last-factor");
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);
    // With an authenticator beside it, it goes.
    second_factor(&pool, &who).await;
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
async fn an_enrol_code_has_five_tries() {
    let pool = db_or_skip!("an_enrol_code_has_five_tries");
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
async fn a_change_of_login_handle_drops_the_email_unless_it_is_the_last_factor() {
    let pool = db_or_skip!("a_change_of_login_handle_drops_the_email_unless_it_is_the_last_factor");
    let rig = rig(pool.clone());

    // Re-binding to another handle.
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    enrol(&rig, &who, &fresh_address()).await;
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);
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
    // without the address the recovery email would be dropped, and it is the
    // account's last factor: refused, nothing changed (ADR-0075, amended
    // 2026-11-26).
    let change = json!({ "handle": new_handle, "fresh_grant": grant(&who.session) });
    let (st, body) = call(
        &rig.state,
        Method::PUT,
        "/auth/login-handle",
        Some(&bearer),
        None,
        Some(&change),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{}", text(&body));
    assert_eq!(code(&body), "last-factor");
    assert_eq!(rows(&pool, "recovery_emails", &who.account.id).await, 1);
    assert_eq!(
        handle_state(&rig.state, &bearer, &who.account.handle).await,
        "same"
    );
    // With an authenticator, the change drops the email.
    second_factor(&pool, &who).await;
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
    assert_eq!(
        notices(&rig.state, &bearer).await,
        ["recovery-email-added", "recovery-email-removed"]
    );

    // Removing the handle.
    let gone = person(&rig.state).await;
    let bearer = token(&gone.session);
    enrol(&rig, &gone, &fresh_address()).await;
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
    enrol(&rig, &who, &address).await;
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
    enrol(&rig, &who, &address).await;
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
    enrol(&rig, &who, &address).await;
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
    enrol(&rig, &who, &address).await; // the account's first letter
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
// Recover: the ticket and the authenticator
// ----------------------------------------------------------------

#[tokio::test]
async fn the_password_routes_open_only_after_the_code_and_the_authenticator() {
    let pool = db_or_skip!("the_password_routes_open_only_after_the_code_and_the_authenticator");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let address = fresh_address();
    enrol(&rig, &who, &address).await;
    let secret = crate::totp::enroll_for_test(&pool, &TOTP_KEY, &who.account.id).await;
    // The account's own state says an authenticator will be asked for.
    let (_, body) = get(&rig.state, "/recovery-email", &bearer).await;
    assert_eq!(
        json_of(&body),
        json!({ "available": true, "enabled": true, "totp": true })
    );
    let request = reg_start(b"new").1;

    // Without a ticket: nothing.
    for bogus in [b64u(&[1u8; 32]), "junk".to_string()] {
        let (st, body) = post(
            &rig.state,
            "/recovery-email/recover/password/start",
            None,
            &json!({ "ticket": bogus, "new_password_request": request }),
        )
        .await;
        assert_eq!(
            (st, text(&body).as_str()),
            (StatusCode::UNAUTHORIZED, TICKET_REFUSED)
        );
    }
    // The vault's route is gone.
    let (st, _) = post(
        &rig.state,
        "/recovery-email/recover/open",
        None,
        &json!({ "ticket": b64u(&[1u8; 32]) }),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

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

    // The code alone does not reach the password routes.
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/password/start",
        None,
        &json!({ "ticket": ticket, "new_password_request": request }),
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
        "/recovery-email/recover/password/start",
        None,
        &json!({ "ticket": ticket, "new_password_request": request }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // The right one, and the password route answers.
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
        "/recovery-email/recover/password/start",
        None,
        &json!({ "ticket": ticket, "new_password_request": request }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert!(json_of(&body)["new_password_response"].is_string());

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
    enrol(&rig, &who, &address).await;
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
    enrol(&rig, &who, &address).await;
    let old_envelope = password_registration(&pool, &who.account.id).await;
    assert_eq!(rows(&pool, "recall_anchors", &who.account.id).await, 0);

    let ticket = ticket_for(&rig, &who.account.handle, &address).await["ticket"].clone();
    let new_password = b"chosen by mail";
    let (reg, request) = reg_start(new_password);

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
    // password envelope and the recovery envelope.
    let anchors: Vec<AnchorProbe> = sqlx::query_as(
        "SELECT identity_pubkey, password_registration, recovery_registration \
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
    enrol(&rig, &who, &address).await;
    let letters = rig.mail.sent().len();
    let (_, new_handle) = crate::login_handle::random_handle();
    let rekey = |address: &str| {
        json!({
            "handle": new_handle,
            "fresh_grant": grant(&who.session),
            "previous_handle": old_handle,
            "recovery_email": { "address": address },
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
    // Malformed: an address the rule refuses.
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
    assert_eq!(notices(&rig.state, &bearer).await, ["recovery-email-added"]);

    // By mail under the OLD handle: nothing any more.
    assert!(!mail_reaches(&rig, &old_handle, &address).await);
    // Under the NEW handle: the code.
    assert!(mail_reaches(&rig, &new_handle, &address).await);

    // A rename with the address moves an email that is the account's last
    // factor: nothing is dropped, so nothing is refused.
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
    assert!(mail_reaches(&rig, &third_handle, &address).await);

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
async fn the_phrase_arm_recalls_an_account_with_or_without_a_recovery_email() {
    let pool = db_or_skip!("the_phrase_arm_recalls_an_account_with_or_without_a_recovery_email");
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

    // --- An account with a recovery email: since the email vault is gone
    // (ADR-0075, amended 2026-11-26) nobody gets the phrase by mail, so the
    // arm serves it too.
    let mailed = person(&rig.state).await;
    enrol(&rig, &mailed, &fresh_address()).await;
    takeover(
        &rig.state,
        &mailed.account,
        &mailed.password,
        &fresh_seed(),
        &fresh_key(),
    )
    .await;
    let (st, body) = status_by_handle(&rig.state, &mailed.key, &mailed.account.handle).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let status = json_of(&body);
    assert_eq!(status["phrase_arm"], true);
    let seq = status["anchor_seq"].as_u64().unwrap();

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
    enrol(&rig, &attacker, &fresh_address()).await;
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
            // The live kinds: 2 and 6 are retired (ADR-0075, amended
            // 2026-11-26).
            .bind([1u8, 3, 4, 5][usize::try_from(i % 4).unwrap()])
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

/// The address and the account never reach the log: enrol, a recovery by
/// mailed code, a new password. The operator's relay sees the address when
/// it delivers a letter; a log line that put the account beside that moment
/// would join the two (ADR-0075).
#[tokio::test(flavor = "current_thread")]
async fn the_recovery_flow_logs_neither_the_address_nor_the_account() {
    let pool = db_or_skip!("the_recovery_flow_logs_neither_the_address_nor_the_account");
    let captured = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    // tracing-core caches a callsite's interest from the thread that first
    // hits it when only ONE dispatcher is registered (`has_just_one`); the
    // tests running on other threads have none, so a callsite they hit first
    // would be cached as never-wanted. A second registered dispatcher keeps
    // the cache computed over both.
    let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _guard = tracing::subscriber::set_default(subscriber);
    tracing::callsite::rebuild_interest_cache();

    let rig = rig(pool);
    let who = person(&rig.state).await;
    let address = fresh_address();
    enrol(&rig, &who, &address).await;
    let ticket = ticket_for(&rig, &who.account.handle, &address).await;
    let new_password = b"by mail";
    let (reg, request) = reg_start(new_password);
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/password/start",
        None,
        &json!({ "ticket": ticket["ticket"], "new_password_request": request }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let upload = reg_finish(
        reg,
        new_password,
        json_of(&body)["new_password_response"].as_str().unwrap(),
    );
    let (st, body) = post(
        &rig.state,
        "/recovery-email/recover/password/finish",
        None,
        &json!({ "ticket": ticket["ticket"], "new_password_upload": upload }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));

    let log = captured.text();
    assert!(
        log.contains("recovery email: password replaced"),
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
