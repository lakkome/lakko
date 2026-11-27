// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests for the second factor of a key reset and what hangs on it
//! (ADR-0073, amended 2026-11-26; ADR-0101 section 7): the four reset factor
//! routes, the reset routes on the factor grant, the owned-channels secret,
//! and the last-factor refusal of a passkey delete.
//!
//! Gated on `LAKKO_AUTH_TEST_DATABASE_URL` (each test skips when it is
//! unset):
//!
//! ```text
//! LAKKO_AUTH_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_auth_test \
//!   cargo test -p lakko-svc-auth reset_factor_tests
//! ```

#![allow(clippy::too_many_lines)]

use std::time::Duration;

use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_auth_token::{GrantMethod, ResetFactor};
use serde_json::{json, Value};
use sqlx::MySqlPool;

use crate::limits::{Scope, Window};
use crate::recall_tests::{
    account_of_token, b64u, call, code, fresh_key, fresh_seed, grant, identity_proof, json_of,
    pool, post, pubkey, reg_finish, reg_start, text, token,
};
use crate::recovery_email_tests::{
    enrol, fresh_address, letter_after, person, rig, rig_with, second_factor, Person, Rig,
};
use crate::session::now_ms;
use crate::AppState;

const FACTOR_REFUSED: &str = r#"{"error":"factor-refused"}"#;
const RESET_FACTOR_REQUIRED: &str = r#"{"error":"reset-factor-required"}"#;
const FRESH_AUTH_REQUIRED: &str = r#"{"error":"fresh-auth-required"}"#;

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

/// A passkey-minted fresh-auth grant for `who`'s account at generation 1.
fn passkey_grant(state: &AppState, who: &Person) -> String {
    state
        .session_token_secret
        .issue_fresh_grant(&who.account.id, 1, GrantMethod::Passkey, now_ms() + 60_000)
        .unwrap()
}

/// Both reset calls with `reset_grant` (as given) for a new seed and key.
async fn reset_with(
    state: &AppState,
    bearer: &str,
    body_grant: &Value,
) -> ((StatusCode, Bytes), (StatusCode, Bytes)) {
    let seed = fresh_seed();
    let key = fresh_key();
    let (cs, request) = reg_start(&seed);
    let mut start = json!({ "recovery_request": request });
    let mut finish_extra = serde_json::Map::new();
    if let Value::Object(map) = body_grant {
        for (k, v) in map {
            start[k] = v.clone();
            finish_extra.insert(k.clone(), v.clone());
        }
    }
    let started = post(state, "/account/keys/reset/start", Some(bearer), &start).await;
    // The finish is driven with an upload made against the server's setup
    // whatever the start answered, so both gates are seen.
    let response = if started.0 == StatusCode::OK {
        json_of(&started.1)["recovery_response"]
            .as_str()
            .unwrap()
            .to_string()
    } else {
        let server = opaque_ke::ServerRegistration::<crate::opaque::LakkoCipherSuite>::start(
            &state.opaque_setup,
            opaque_ke::ClientRegistration::<crate::opaque::LakkoCipherSuite>::start(
                &mut rand::rngs::OsRng,
                &seed,
            )
            .unwrap()
            .message,
            &crate::opaque::build_credential_id(
                &account_of_token(bearer),
                crate::opaque::KIND_RECOVERY,
            ),
        )
        .unwrap();
        base64::engine::general_purpose::STANDARD.encode(server.message.serialize())
    };
    let upload = reg_finish(cs, &seed, &response);
    let mut finish = json!({
        "recovery_upload": upload,
        "new_signature_pubkey_b64": b64u(&pubkey(&key)),
        "identity_proof_b64": identity_proof(&account_of_token(bearer), &key),
    });
    for (k, v) in finish_extra {
        finish[k] = v;
    }
    let finished = post(state, "/account/keys/reset/finish", Some(bearer), &finish).await;
    (started, finished)
}

async fn generation(pool: &MySqlPool, account: &[u8; 16]) -> u64 {
    crate::session::key_generation_of(pool, account)
        .await
        .unwrap()
        .unwrap()
}

async fn totp_factor(
    state: &AppState,
    bearer: &str,
    fresh: &str,
    code: &str,
) -> (StatusCode, Bytes) {
    post(
        state,
        "/account/keys/reset/factor/totp",
        Some(bearer),
        &json!({ "fresh_grant": fresh, "code": code }),
    )
    .await
}

fn reset_grant_of(body: &Bytes) -> String {
    let v = json_of(body);
    let grant = v["reset_grant"].as_str().unwrap().to_string();
    assert!(grant.starts_with("f1."), "{grant}");
    let ttl = v["reset_grant_expires_at_ms"].as_i64().unwrap() - now_ms();
    assert!(ttl > 0 && ttl <= 600_000, "{ttl}");
    grant
}

// ----------------------------------------------------------------
// The reset routes take the factor grant and nothing else
// ----------------------------------------------------------------

#[tokio::test]
async fn a_fresh_auth_grant_alone_resets_nothing() {
    let pool = db_or_skip!("a_fresh_auth_grant_alone_resets_nothing");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let other = person(&rig.state).await;
    let bearer = token(&who.session);
    let fresh = grant(&who.session);
    let secret = &rig.state.session_token_secret;
    let now = now_ms();
    let other_account = secret
        .issue_factor_grant(&other.account.id, 1, ResetFactor::Email, now + 60_000)
        .unwrap();
    let other_generation = secret
        .issue_factor_grant(&who.account.id, 2, ResetFactor::Email, now + 60_000)
        .unwrap();
    let expired = secret
        .issue_factor_grant(&who.account.id, 1, ResetFactor::Email, now - 1)
        .unwrap();
    for (what, body) in [
        ("no grant", json!({})),
        (
            "the fresh-auth grant as fresh_grant",
            json!({ "fresh_grant": fresh }),
        ),
        (
            "the fresh-auth grant as reset_grant",
            json!({ "reset_grant": fresh }),
        ),
        ("another account's", json!({ "reset_grant": other_account })),
        (
            "another generation's",
            json!({ "reset_grant": other_generation }),
        ),
        ("an expired one", json!({ "reset_grant": expired })),
        ("the bearer", json!({ "reset_grant": bearer })),
    ] {
        let ((st, b), (st2, b2)) = reset_with(&rig.state, &bearer, &body).await;
        assert_eq!(
            (st, text(&b).as_str()),
            (StatusCode::FORBIDDEN, RESET_FACTOR_REQUIRED),
            "start with {what}"
        );
        assert_eq!(
            (st2, text(&b2).as_str()),
            (StatusCode::FORBIDDEN, RESET_FACTOR_REQUIRED),
            "finish with {what}"
        );
    }
    assert_eq!(generation(&pool, &who.account.id).await, 1);
    // The owned-channels secret is refused the same way.
    for body in [json!({}), json!({ "reset_grant": fresh })] {
        let (st, b) = post(
            &rig.state,
            "/account/owned-channels-secret",
            Some(&bearer),
            &body,
        )
        .await;
        assert_eq!(
            (st, text(&b).as_str()),
            (StatusCode::FORBIDDEN, RESET_FACTOR_REQUIRED)
        );
    }
}

#[tokio::test]
async fn every_factor_route_needs_a_password_grant_and_refuses_a_missing_factor() {
    let pool = db_or_skip!("every_factor_route_needs_a_password_grant");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let by_passkey = passkey_grant(&rig.state, &who);
    let routes = [
        (
            "/account/keys/reset/factor/totp",
            json!({ "code": "123456" }),
        ),
        (
            "/account/keys/reset/factor/email/start",
            json!({ "handle": who.account.handle, "address": "a@b.example" }),
        ),
        (
            "/account/keys/reset/factor/email/finish",
            json!({ "challenge_id": b64u(&[1u8; 16]), "code": "123456" }),
        ),
        ("/account/keys/reset/factor/passkey", json!({})),
    ];
    for (uri, base) in &routes {
        for fresh in [None, Some(by_passkey.as_str()), Some("k1.junk")] {
            let mut body = base.clone();
            if let Some(f) = fresh {
                body["fresh_grant"] = json!(f);
            }
            let (st, b) = post(&rig.state, uri, Some(&bearer), &body).await;
            assert_eq!(
                (st, text(&b).as_str()),
                (StatusCode::FORBIDDEN, FRESH_AUTH_REQUIRED),
                "{uri} with {fresh:?}"
            );
        }
    }
    // Nothing was charged and no letter went out.
    assert!(rig.mail.sent().is_empty());

    // With the password's grant and no such factor: the one refusal.
    let fresh = grant(&who.session);
    let (st, b) = totp_factor(&rig.state, &bearer, &fresh, "123456").await;
    assert_eq!(
        (st, text(&b).as_str()),
        (StatusCode::UNAUTHORIZED, FACTOR_REFUSED)
    );
    let (st, b) = post(
        &rig.state,
        "/account/keys/reset/factor/passkey",
        Some(&bearer),
        &json!({ "fresh_grant": fresh }),
    )
    .await;
    assert_eq!(
        (st, text(&b).as_str()),
        (StatusCode::UNAUTHORIZED, FACTOR_REFUSED)
    );
    let (st, b) = post(
        &rig.state,
        "/account/keys/reset/factor/email/finish",
        Some(&bearer),
        &json!({ "fresh_grant": fresh, "challenge_id": b64u(&[1u8; 16]), "code": "123456" }),
    )
    .await;
    assert_eq!(
        (st, text(&b).as_str()),
        (StatusCode::UNAUTHORIZED, FACTOR_REFUSED)
    );
}

// ----------------------------------------------------------------
// The authenticator
// ----------------------------------------------------------------

#[tokio::test]
async fn the_authenticator_factor_mints_a_grant_the_reset_spends() {
    let pool = db_or_skip!("the_authenticator_factor_mints_a_grant_the_reset_spends");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let fresh = grant(&who.session);
    let secret = second_factor(&pool, &who).await;
    let right = crate::totp::current_code_for_test(&secret);
    let wrong = format!("{:06}", (right.parse::<u32>().unwrap() + 1) % 1_000_000);

    let (st, b) = totp_factor(&rig.state, &bearer, &fresh, &wrong).await;
    assert_eq!(
        (st, text(&b).as_str()),
        (StatusCode::UNAUTHORIZED, FACTOR_REFUSED)
    );
    let (st, b) = totp_factor(&rig.state, &bearer, &fresh, &right).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&b));
    let reset_grant = reset_grant_of(&b);
    let verified = rig
        .state
        .session_token_secret
        .verify_factor_grant(&reset_grant, now_ms())
        .unwrap();
    assert_eq!(verified.account_id, who.account.id);
    assert_eq!(verified.key_generation, 1);
    assert_eq!(verified.factor, ResetFactor::Authenticator);
    // The step is consumed: the same code mints nothing again.
    let (st, _) = totp_factor(&rig.state, &bearer, &fresh, &right).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // The grant releases `S` without being spent, then resets.
    let (st, b) = post(
        &rig.state,
        "/account/owned-channels-secret",
        Some(&bearer),
        &json!({ "reset_grant": reset_grant }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&b));
    let s = json_of(&b)["owned_channels_secret"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(URL_SAFE_NO_PAD.decode(&s).unwrap().len(), 32);
    assert_eq!(
        s,
        rig.state
            .owned_channels
            .as_ref()
            .unwrap()
            .server_half_b64(&who.account.id)
    );
    let ((st, b), (st2, b2)) =
        reset_with(&rig.state, &bearer, &json!({ "reset_grant": reset_grant })).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&b));
    assert_eq!(st2, StatusCode::OK, "{}", text(&b2));
    assert_eq!(generation(&pool, &who.account.id).await, 2);
    // The authenticator survives the reset (ADR-0073: kept, deliberately).
    assert!(crate::totp::is_enabled(&pool, &who.account.id)
        .await
        .unwrap());

    // The grant is spent: under the new session it names an old generation.
    let new_bearer = token(&json_of(&b2));
    let ((st, b), _) = reset_with(
        &rig.state,
        &new_bearer,
        &json!({ "reset_grant": reset_grant }),
    )
    .await;
    assert_eq!(
        (st, text(&b).as_str()),
        (StatusCode::FORBIDDEN, RESET_FACTOR_REQUIRED)
    );
    let (st, _) = post(
        &rig.state,
        "/account/owned-channels-secret",
        Some(&new_bearer),
        &json!({ "reset_grant": reset_grant }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_authenticator_factor_and_the_secret_are_counted_per_account() {
    let pool = db_or_skip!("the_authenticator_factor_and_the_secret_are_counted_per_account");
    let rig = rig_with(
        pool.clone(),
        crate::recall_tests::roomy()
            .with_name_window(Scope::ResetFactor, Window::new(2, 900))
            .with_name_window(Scope::OwnedChannelsSecret, Window::new(1, 86_400)),
    );
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let fresh = grant(&who.session);
    let secret = second_factor(&pool, &who).await;
    let right = crate::totp::current_code_for_test(&secret);
    let (st, _) = totp_factor(&rig.state, &bearer, &fresh, "000001").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, b) = totp_factor(&rig.state, &bearer, &fresh, &right).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&b));
    let reset_grant = reset_grant_of(&b);
    let (st, b) = totp_factor(&rig.state, &bearer, &fresh, &right).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&b), "too-many-attempts");

    let ask = json!({ "reset_grant": reset_grant });
    let (st, _) = post(
        &rig.state,
        "/account/owned-channels-secret",
        Some(&bearer),
        &ask,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, b) = post(
        &rig.state,
        "/account/owned-channels-secret",
        Some(&bearer),
        &ask,
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&b), "too-many-attempts");
    // A refused grant charged nothing of another account's budget.
    let other = person(&rig.state).await;
    let (st, _) = post(
        &rig.state,
        "/account/owned-channels-secret",
        Some(&token(&other.session)),
        &json!({}),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

// ----------------------------------------------------------------
// The mailed code
// ----------------------------------------------------------------

async fn email_start(rig: &Rig, who: &Person, address: &str) -> String {
    let (st, b) = post(
        &rig.state,
        "/account/keys/reset/factor/email/start",
        Some(&token(&who.session)),
        &json!({
            "fresh_grant": grant(&who.session),
            "handle": who.account.handle,
            "address": address,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&b));
    let v = json_of(&b);
    assert_eq!(v["resend_after_secs"], 45);
    assert_eq!(v["expires_in_secs"], 600);
    v["challenge_id"].as_str().unwrap().to_string()
}

async fn email_finish(
    rig: &Rig,
    who: &Person,
    challenge: &str,
    typed: &str,
) -> (StatusCode, Bytes) {
    post(
        &rig.state,
        "/account/keys/reset/factor/email/finish",
        Some(&token(&who.session)),
        &json!({
            "fresh_grant": grant(&who.session),
            "challenge_id": challenge,
            "code": typed,
        }),
    )
    .await
}

#[tokio::test]
async fn the_email_factor_mails_only_a_matching_pair_and_mints_on_the_code() {
    let pool = db_or_skip!("the_email_factor_mails_only_a_matching_pair");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let address = fresh_address();
    enrol(&rig, &who, &address).await;

    // Another address: the same answer, no letter, and its challenge takes
    // no code.
    let before = rig.mail.sent().len();
    let decoy = email_start(&rig, &who, &fresh_address()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.mail.sent().len(), before, "no letter for a miss");
    for typed in ["000000", "123456"] {
        let (st, b) = email_finish(&rig, &who, &decoy, typed).await;
        assert_eq!(
            (st, text(&b).as_str()),
            (StatusCode::UNAUTHORIZED, FACTOR_REFUSED)
        );
    }

    // The right pair: a letter, and the code mints a grant once.
    let before = rig.mail.sent().len();
    let challenge = email_start(&rig, &who, &address).await;
    let mailed = letter_after(&rig, before)
        .await
        .expect("a letter for a match");
    // Another account cannot finish it.
    let other = person(&rig.state).await;
    let (st, b) = email_finish(&rig, &other, &challenge, &mailed).await;
    assert_eq!(
        (st, text(&b).as_str()),
        (StatusCode::UNAUTHORIZED, FACTOR_REFUSED)
    );
    let wrong = format!("{:06}", (mailed.parse::<u32>().unwrap() + 1) % 1_000_000);
    let (st, _) = email_finish(&rig, &who, &challenge, &wrong).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, b) = email_finish(&rig, &who, &challenge, &mailed).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&b));
    let reset_grant = reset_grant_of(&b);
    assert_eq!(
        rig.state
            .session_token_secret
            .verify_factor_grant(&reset_grant, now_ms())
            .unwrap()
            .factor,
        ResetFactor::Email
    );
    let (st, _) = email_finish(&rig, &who, &challenge, &mailed).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "the challenge is spent");

    // A recover-purpose challenge is not a reset factor.
    let before = rig.mail.sent().len();
    let (st, b) = post(
        &rig.state,
        "/recovery-email/recover/start",
        None,
        &json!({ "handle": who.account.handle, "address": address }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let recover = json_of(&b)["challenge_id"].as_str().unwrap().to_string();
    let recover_code = letter_after(&rig, before).await.unwrap();
    let (st, _) = email_finish(&rig, &who, &recover, &recover_code).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // An account without a recovery email gets the same answer and no letter.
    let bare = person(&rig.state).await;
    let before = rig.mail.sent().len();
    let _ = email_start(&rig, &bare, &fresh_address()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.mail.sent().len(), before);

    // The reset with the email grant.
    let ((st, _), (st2, b2)) = reset_with(
        &rig.state,
        &token(&who.session),
        &json!({ "reset_grant": reset_grant }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(st2, StatusCode::OK, "{}", text(&b2));
    // The recovery email survives a reset.
    let emails: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM recovery_emails WHERE account_id = ?")
            .bind(&who.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(emails, 1);
}

// ----------------------------------------------------------------
// A passkey
// ----------------------------------------------------------------

async fn write_action_grant(pool: &MySqlPool, account: &[u8; 16], hash: &[u8; 32]) {
    sqlx::query(
        "INSERT INTO webauthn_action_grants (account_id, action_hash, expires_at) \
         VALUES (?, ?, NOW(3) + INTERVAL 120 SECOND)",
    )
    .bind(&account[..])
    .bind(&hash[..])
    .execute(pool)
    .await
    .unwrap();
}

async fn add_passkey_row(pool: &MySqlPool, account: &[u8; 16], credential_id: &[u8]) {
    sqlx::query(
        "INSERT INTO webauthn_credentials (credential_id, account_id, passkey_bytes) \
         VALUES (?, ?, ?)",
    )
    .bind(credential_id)
    .bind(&account[..])
    .bind(&b"opaque"[..])
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn the_passkey_factor_consumes_a_reset_keys_action_grant() {
    let pool = db_or_skip!("the_passkey_factor_consumes_a_reset_keys_action_grant");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let fresh = grant(&who.session);
    let ask = json!({ "fresh_grant": fresh });

    // A delete-account grant is not a reset grant.
    write_action_grant(
        &pool,
        &who.account.id,
        &crate::webauthn_action::delete_account_action_hash(&who.account.id),
    )
    .await;
    let (st, b) = post(
        &rig.state,
        "/account/keys/reset/factor/passkey",
        Some(&bearer),
        &ask,
    )
    .await;
    assert_eq!(
        (st, text(&b).as_str()),
        (StatusCode::UNAUTHORIZED, FACTOR_REFUSED)
    );

    write_action_grant(
        &pool,
        &who.account.id,
        &crate::reset_factor::reset_keys_action_hash(&who.account.id),
    )
    .await;
    let (st, b) = post(
        &rig.state,
        "/account/keys/reset/factor/passkey",
        Some(&bearer),
        &ask,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&b));
    assert_eq!(
        rig.state
            .session_token_secret
            .verify_factor_grant(&reset_grant_of(&b), now_ms())
            .unwrap()
            .factor,
        ResetFactor::Passkey
    );
    // Single use.
    let (st, _) = post(
        &rig.state,
        "/account/keys/reset/factor/passkey",
        Some(&bearer),
        &ask,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

// ----------------------------------------------------------------
// The last factor stays
// ----------------------------------------------------------------

#[tokio::test]
async fn the_last_passkey_is_not_deleted_and_the_action_grant_is_kept() {
    let pool = db_or_skip!("the_last_passkey_is_not_deleted");
    let rig = rig(pool.clone());
    let who = person(&rig.state).await;
    let bearer = token(&who.session);
    let mut credential = b"last-factor-passkey-".to_vec();
    credential.extend_from_slice(&who.account.id);
    add_passkey_row(&pool, &who.account.id, &credential).await;
    let hash = crate::webauthn_action::delete_credential_action_hash(&credential);
    write_action_grant(&pool, &who.account.id, &hash).await;
    let uri = format!(
        "/webauthn/credentials/{}",
        URL_SAFE_NO_PAD.encode(&credential)
    );

    let (st, b) = call(&rig.state, Method::DELETE, &uri, Some(&bearer), None, None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{}", text(&b));
    assert_eq!(code(&b), "last-factor");
    let grants: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_action_grants WHERE account_id = ?")
            .bind(&who.account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(grants, 1, "a refused delete spends no action grant");

    // A second passkey beside it: the first goes.
    let mut second = b"second-passkey-".to_vec();
    second.extend_from_slice(&who.account.id);
    add_passkey_row(&pool, &who.account.id, &second).await;
    let (st, b) = call(&rig.state, Method::DELETE, &uri, Some(&bearer), None, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{}", text(&b));

    // A recovery email beside the last one: it goes too, and then the email
    // is the last factor.
    let address = fresh_address();
    enrol(&rig, &who, &address).await;
    let hash = crate::webauthn_action::delete_credential_action_hash(&second);
    write_action_grant(&pool, &who.account.id, &hash).await;
    let uri = format!("/webauthn/credentials/{}", URL_SAFE_NO_PAD.encode(&second));
    let (st, _) = call(&rig.state, Method::DELETE, &uri, Some(&bearer), None, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, b) = call(
        &rig.state,
        Method::DELETE,
        "/recovery-email",
        Some(&bearer),
        None,
        Some(&json!({ "fresh_grant": grant(&who.session) })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(code(&b), "last-factor");

    // A passkey that does not exist is still the idempotent no-op.
    let gone = format!("/webauthn/credentials/{}", URL_SAFE_NO_PAD.encode(b"nope"));
    let (st, _) = call(&rig.state, Method::DELETE, &gone, Some(&bearer), None, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
}
