// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tests for the channel recovery evaluation (ADR-0101 section 7). The
//! derivation and the refusals before the store run without a database; the
//! route tests are gated on `LAKKO_AUTH_TEST_DATABASE_URL` and skip when it
//! is unset.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use lakko_channels::{channel_recovery_oprf_input, ChannelRecoveryKey};
use rand::rngs::OsRng;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tower::ServiceExt as _;
use voprf::{EvaluationElement, OprfClient};

use super::{ChannelRecoveryMaster, Suite, CODE_DISABLED};
use crate::limits::{Limits, Scope, Window};

const MASTER: [u8; 32] = [0x5C; 32];
const DEVICE: [u8; 32] = [0xC7; 32];
const CHANNEL: [u8; 16] = [0x0C; 16];

fn build_state(pool: MySqlPool, master: Option<[u8; 32]>, limits: Limits) -> crate::AppState {
    use opaque_ke::ServerSetup;
    let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
    crate::AppState {
        db: pool,
        opaque_setup: Arc::new(ServerSetup::<crate::opaque::LakkoCipherSuite>::new(
            &mut OsRng,
        )),
        session_token_secret: Arc::new(token_secret),
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
        anon_cred: None,
        handle_pepper: None,
        recovery_email: None,
        channel_recovery: master.map(|m| Arc::new(ChannelRecoveryMaster::from_bytes(m))),
        limits: Arc::new(limits),
    }
}

fn lazy_pool() -> MySqlPool {
    MySqlPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(200))
        .connect_lazy("mysql://nobody@127.0.0.1:1/nodb")
        .expect("lazy pool (never connects)")
}

async fn db() -> Option<MySqlPool> {
    let url = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL").ok()?;
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect");
    crate::MIGRATIONS.run(&pool).await.expect("migrate");
    Some(pool)
}

macro_rules! db_or_skip {
    ($name:literal) => {{
        let Some(pool) = db().await else {
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

/// A blinded request for `channel` the way a client makes one: the client
/// state to finalise with, and the base64url element.
fn blind(channel: &[u8; 16]) -> (OprfClient<Suite>, String) {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let result =
        OprfClient::<Suite>::blind(&channel_recovery_oprf_input(channel), &mut OsRng).unwrap();
    (
        result.state,
        URL_SAFE_NO_PAD.encode(result.message.serialize()),
    )
}

/// Finalise an evaluation and derive the channel's recovery public key.
fn recovery_key(client: &OprfClient<Suite>, channel: &[u8; 16], evaluation_b64: &str) -> [u8; 32] {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let bytes = URL_SAFE_NO_PAD.decode(evaluation_b64).unwrap();
    let evaluation = EvaluationElement::<Suite>::deserialize(&bytes).unwrap();
    let output = client
        .finalize(&channel_recovery_oprf_input(channel), &evaluation)
        .unwrap();
    ChannelRecoveryKey::from_oprf_output(&output)
        .verifying_key()
        .to_bytes()
}

fn evaluate_req(bearer: Option<&str>, body: &serde_json::Value) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/auth/channel-recovery/evaluate")
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn send(state: &crate::AppState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = crate::build_app(state.clone())
        .oneshot(req)
        .await
        .expect("router oneshot");
    let status = resp.status();
    let bytes: Bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// The key is a function of the master and the account id alone: the same
/// account gets the same recovery key for a channel on every evaluation,
/// another account or another master a different one, another channel a
/// different one.
#[test]
fn the_recovery_key_depends_on_the_master_the_account_and_the_channel_only() {
    let master = ChannelRecoveryMaster::from_bytes(MASTER);
    let account = [0xA1; 16];
    let derive = |m: &ChannelRecoveryMaster, account: &[u8; 16], channel: &[u8; 16]| {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let (client, blinded) = blind(channel);
        let blinded = URL_SAFE_NO_PAD.decode(blinded).unwrap();
        let evaluation = m.evaluate(account, &blinded).unwrap();
        recovery_key(&client, channel, &URL_SAFE_NO_PAD.encode(evaluation))
    };
    let first = derive(&master, &account, &CHANNEL);
    assert_eq!(first, derive(&master, &account, &CHANNEL));
    assert_ne!(first, derive(&master, &[0xA2; 16], &CHANNEL));
    assert_ne!(first, derive(&master, &account, &[0x0D; 16]));
    assert_ne!(
        first,
        derive(
            &ChannelRecoveryMaster::from_bytes([0x5D; 32]),
            &account,
            &CHANNEL
        )
    );
    assert_eq!(format!("{master:?}"), "ChannelRecoveryMaster(..)");
}

#[tokio::test]
async fn the_route_is_disabled_without_a_secret_and_needs_a_session() {
    let state = build_state(lazy_pool(), None, Limits::defaults());
    let (_, blinded) = blind(&CHANNEL);
    // No session: the middleware refuses before anything else.
    let (status, _) = send(
        &state,
        evaluate_req(None, &serde_json::json!({ "blinded_element": blinded })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(CODE_DISABLED, "channel-recovery-disabled");
}

#[tokio::test]
async fn the_route_evaluates_refuses_bad_input_and_counts() {
    let pool = db_or_skip!("the_route_evaluates_refuses_bad_input_and_counts");
    let limits = Limits::defaults().with_name_window(Scope::ChannelRecovery, Window::new(3, 3600));
    let state = build_state(pool.clone(), Some(MASTER), limits);
    let account = *uuid::Uuid::now_v7().as_bytes();
    sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&account[..])
        .execute(&pool)
        .await
        .unwrap();
    let bearer = crate::session::issue_pair_at(&state, &account, 1, &DEVICE)
        .unwrap()
        .session_token;

    // Disabled: 503 with the code, before anything is charged.
    let disabled = build_state(pool.clone(), None, Limits::defaults());
    let (_, blinded) = blind(&CHANNEL);
    let (status, body) = send(
        &disabled,
        evaluate_req(
            Some(&bearer),
            &serde_json::json!({ "blinded_element": blinded }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], CODE_DISABLED);

    // Bad input is refused and spends no budget.
    for bad in [
        serde_json::json!({ "blinded_element": "!!" }),
        serde_json::json!({ "blinded_element": "AAAA" }),
        // 32 bytes that are not a canonical Ristretto encoding.
        serde_json::json!({ "blinded_element": "_____________________________________________w" }),
        // The identity element.
        serde_json::json!({ "blinded_element": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" }),
        serde_json::json!({ "blinded_element": "A".repeat(100) }),
    ] {
        let (status, _) = send(&state, evaluate_req(Some(&bearer), &bad)).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{status} for {bad}"
        );
    }

    // Three evaluations pass, a fourth is limited.
    let mut keys = Vec::new();
    for _ in 0..3 {
        let (client, blinded) = blind(&CHANNEL);
        let (status, body) = send(
            &state,
            evaluate_req(
                Some(&bearer),
                &serde_json::json!({ "blinded_element": blinded }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        keys.push(recovery_key(
            &client,
            &CHANNEL,
            body["evaluation"].as_str().unwrap(),
        ));
    }
    assert!(keys.windows(2).all(|w| w[0] == w[1]));
    let (_, blinded) = blind(&CHANNEL);
    let (status, body) = send(
        &state,
        evaluate_req(
            Some(&bearer),
            &serde_json::json!({ "blinded_element": blinded }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "too-many-attempts");
}

/// ADR-0101: the recovery key is the same before and after an ADR-0073 key
/// reset (the generation moves, the account id does not), and the old
/// generation's session is refused; a suspended account is refused.
#[tokio::test]
async fn the_recovery_key_survives_a_key_reset() {
    let pool = db_or_skip!("the_recovery_key_survives_a_key_reset");
    let state = build_state(pool.clone(), Some(MASTER), Limits::defaults());
    let account = *uuid::Uuid::now_v7().as_bytes();
    sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&account[..])
        .execute(&pool)
        .await
        .unwrap();

    let key_with = |bearer: String| {
        let state = state.clone();
        async move {
            let (client, blinded) = blind(&CHANNEL);
            let (status, body) = send(
                &state,
                evaluate_req(
                    Some(&bearer),
                    &serde_json::json!({ "blinded_element": blinded }),
                ),
            )
            .await;
            (
                status,
                body["evaluation"]
                    .as_str()
                    .map(|e| recovery_key(&client, &CHANNEL, e)),
                body,
            )
        }
    };

    let before_bearer = crate::session::issue_pair_at(&state, &account, 1, &DEVICE)
        .unwrap()
        .session_token;
    let (status, before, _) = key_with(before_bearer.clone()).await;
    assert_eq!(status, StatusCode::OK);

    // The reset: the account's key generation moves to 2 (what
    // `POST /account/keys/reset/finish` does to the row).
    sqlx::query("UPDATE accounts SET identity_version = 2 WHERE id = ?")
        .bind(&account[..])
        .execute(&pool)
        .await
        .unwrap();
    let (status, _, body) = key_with(before_bearer).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "key-generation-superseded");

    let after_bearer = crate::session::issue_pair_at(&state, &account, 2, &[0xC8; 32])
        .unwrap()
        .session_token;
    let (status, after, _) = key_with(after_bearer.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(before.is_some());
    assert_eq!(before, after);

    // A suspended account is refused by the session middleware.
    sqlx::query(
        "INSERT INTO account_suspensions \
         (account_id, until_ms, reason, reason_salt, statement_id, log_index, suspended_at_ms) \
         VALUES (?, NULL, 1, ?, ?, 0, 0)",
    )
    .bind(&account[..])
    .bind(&[0u8; 16][..])
    .bind(&uuid::Uuid::now_v7().as_bytes()[..])
    .execute(&pool)
    .await
    .unwrap();
    let (status, evaluation, _) = key_with(after_bearer).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(evaluation, None);
}
