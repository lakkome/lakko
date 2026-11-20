// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tests for supporter codes (ADR-0097). The routing refusals that happen
//! before the store run without a database; the redeem and bind tests are
//! gated on `LAKKO_AUTH_TEST_DATABASE_URL` and skip when it is unset.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use lakko_anon_token::{
    IssuerKey, MasterSecret, SupporterCode, TokenContext, BETA_PLACE_WINDOW_SECS,
};
use lakko_anon_token_server::AnonTokenServerConfig;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tower::ServiceExt as _;
use uuid::Uuid;

use super::{code_hash, mint, place_from_headers, SupporterPepper, SupporterPlace};
use crate::anon_tokens::AnonTokenIssuer;
use crate::login_handle::{bind, random_handle, BindOutcome, ChangeTerms, TEST_PEPPER};

const MASTER_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";
const PEPPER: SupporterPepper = SupporterPepper([0x77; 32]);

fn issuer(pepper: Option<SupporterPepper>) -> AnonTokenIssuer {
    let server = AnonTokenServerConfig::from_values(Some(MASTER_HEX), None)
        .unwrap()
        .unwrap();
    AnonTokenIssuer::for_test(server).with_supporter_pepper(pepper)
}

fn build_state(pool: MySqlPool, anon: Option<AnonTokenIssuer>) -> crate::AppState {
    use opaque_ke::ServerSetup;
    use rand::rngs::OsRng;
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
        anon_tokens: anon.map(Arc::new),
        anon_cred: None,
        handle_pepper: Some(Arc::new(TEST_PEPPER)),
        recovery_email: None,
        limits: Arc::new(crate::limits::Limits::defaults()),
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

fn current_epoch() -> u64 {
    lakko_anon_token_server::now_unix_secs() / BETA_PLACE_WINDOW_SECS
}

fn redeem_req(code: &str, epoch: u64, blinded: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/auth/supporter-code/redeem")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "code": code, "epoch": epoch, "blinded_element": blinded })
                .to_string(),
        ))
        .unwrap()
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

fn blinded_of(code: &SupporterCode, epoch: u64) -> String {
    code.token_request(epoch).unwrap().blinded_elements_b64url()[0].clone()
}

#[tokio::test]
async fn redeem_is_disabled_without_a_pepper_or_master() {
    let code = SupporterCode::generate();
    let blinded = blinded_of(&code, current_epoch());
    for anon in [None, Some(issuer(None))] {
        let state = build_state(lazy_pool(), anon);
        let (status, body) = send(
            &state,
            redeem_req(&code.to_display(), current_epoch(), &blinded),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "supporter-codes-disabled");
    }
}

#[tokio::test]
async fn redeem_refuses_a_bad_shape_before_the_store() {
    let state = build_state(lazy_pool(), Some(issuer(Some(PEPPER))));
    let code = SupporterCode::generate();
    let epoch = current_epoch();
    let (status, body) = send(
        &state,
        redeem_req("ABCD-EFGH", epoch, &blinded_of(&code, epoch)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "supporter-code-malformed");
    let (status, body) = send(&state, redeem_req(&code.to_display(), epoch, "AAAA")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "bad-blinded-element");
    let (status, body) = send(
        &state,
        redeem_req(&code.to_display(), epoch + 1, &blinded_of(&code, epoch + 1)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "epoch-not-issuable");
    assert_eq!(body["current_epoch"], epoch);
}

#[tokio::test]
async fn the_key_route_counts_beta_place_epochs_in_thirty_days() {
    let state = build_state(lazy_pool(), Some(issuer(None)));
    let epoch = current_epoch();
    let resp = crate::build_app(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/auth/tokens/key/beta-place/{epoch}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // A daily epoch number is far outside the 30-day range.
    let daily = lakko_anon_token_server::now_unix_secs() / 86_400;
    let resp = crate::build_app(state)
        .oneshot(
            Request::builder()
                .uri(format!("/auth/tokens/key/beta-place/{daily}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[test]
fn a_bind_header_counts_only_a_valid_beta_place_token() {
    let anon = Arc::new(issuer(None));
    let master = MasterSecret::from_hex(MASTER_HEX).unwrap();
    let mut headers = axum::http::HeaderMap::new();
    assert_eq!(place_from_headers(Some(&anon), &headers), None);

    let other = lakko_anon_token_server::testing::mint_header(
        &master,
        TokenContext::DsKnock,
        lakko_anon_token_server::now_unix_secs() / 86_400,
    );
    headers.insert("x-lakko-anon-token", other.parse().unwrap());
    assert_eq!(place_from_headers(Some(&anon), &headers), None);

    let place =
        lakko_anon_token_server::testing::mint(&master, TokenContext::BetaPlace, current_epoch());
    headers.insert(
        "x-lakko-anon-token",
        place.to_header_value().parse().unwrap(),
    );
    let got = place_from_headers(Some(&anon), &headers).expect("verifies");
    assert_eq!(got.epoch, current_epoch());
    assert_eq!(got.spend_id, place.spend_id());
    // Tokens dormant: nothing verifies.
    assert_eq!(place_from_headers(None, &headers), None);
}

#[tokio::test]
async fn redeem_round_trip_retry_mismatch_unknown_and_expired() {
    let Some(pool) = db().await else {
        eprintln!("skipping supporter redeem test: LAKKO_AUTH_TEST_DATABASE_URL unset");
        return;
    };
    let state = build_state(pool.clone(), Some(issuer(Some(PEPPER))));
    let epoch = current_epoch();

    let unknown = SupporterCode::generate();
    let (status, body) = send(
        &state,
        redeem_req(&unknown.to_display(), epoch, &blinded_of(&unknown, epoch)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "supporter-code-unknown");

    let mut codes = mint(&pool, &PEPPER, 2).await.expect("mint");
    let code = codes.remove(0);
    let typed = code.to_display().to_lowercase().replace('-', " ");
    let blinded = blinded_of(&code, epoch);
    let (status, first) = send(&state, redeem_req(&typed, epoch, &blinded)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["epoch"], epoch);
    assert_eq!(first["context"], "beta-place");

    // A retry from the same code gets the stored answer back and finalizes.
    let (status, again) = send(&state, redeem_req(&code.to_display(), epoch, &blinded)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["evaluation"], first["evaluation"]);
    assert_eq!(again["proof"], first["proof"]);
    let master = MasterSecret::from_hex(MASTER_HEX).unwrap();
    let pk = IssuerKey::derive(&master, TokenContext::BetaPlace, epoch)
        .unwrap()
        .public_key();
    let tokens = code
        .token_request(epoch)
        .unwrap()
        .finalize_b64url(
            &[again["evaluation"].as_str().unwrap()],
            again["proof"].as_str().unwrap(),
            &lakko_anon_token::encode_b64url(&pk),
        )
        .expect("finalize against the stored answer");
    assert_eq!(tokens.len(), 1);

    // Another element for a redeemed code is refused.
    let stranger = blinded_of(&SupporterCode::generate(), epoch);
    let (status, body) = send(&state, redeem_req(&code.to_display(), epoch, &stranger)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "supporter-code-mismatch");

    // A code redeemed in an earlier window is spent for good.
    let old = codes.remove(0);
    sqlx::query("UPDATE supporter_codes SET epoch = ?, blinded_element = ? WHERE code_hash = ?")
        .bind(epoch - 1)
        .bind(&[1u8; 32][..])
        .bind(&code_hash(&PEPPER, &old)[..])
        .execute(&pool)
        .await
        .unwrap();
    let (status, body) = send(
        &state,
        redeem_req(&old.to_display(), epoch, &blinded_of(&old, epoch)),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(body["error"], "supporter-code-expired");

    for c in [&code, &old] {
        sqlx::query("DELETE FROM supporter_codes WHERE code_hash = ?")
            .bind(&code_hash(&PEPPER, c)[..])
            .execute(&pool)
            .await
            .unwrap();
    }
}

async fn new_account(pool: &MySqlPool) -> [u8; 16] {
    let id: [u8; 16] = *Uuid::now_v7().as_bytes();
    sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(id.as_slice())
        .execute(pool)
        .await
        .expect("insert account");
    id
}

async fn spent(pool: &MySqlPool, place: &SupporterPlace) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM anon_token_spent WHERE context = ? AND spend_id = ?",
    )
    .bind(TokenContext::BetaPlace.as_u8())
    .bind(&place.spend_id[..])
    .fetch_one(pool)
    .await
    .unwrap()
        > 0
}

fn place() -> SupporterPlace {
    let master = MasterSecret::from_hex(MASTER_HEX).unwrap();
    let t =
        lakko_anon_token_server::testing::mint(&master, TokenContext::BetaPlace, current_epoch());
    SupporterPlace {
        spend_id: t.spend_id(),
        epoch: t.epoch,
    }
}

/// The cap is set to 1 after this test bound one handle of its own, so the
/// count is at least 1 whatever else runs on the shared database.
#[tokio::test]
async fn a_supporter_place_admits_one_bind_past_the_cap() {
    let Some(pool) = db().await else {
        eprintln!("skipping supporter bind test: LAKKO_AUTH_TEST_DATABASE_URL unset");
        return;
    };
    let terms = |max, supporter| ChangeTerms {
        grant: true,
        max_accounts: max,
        supporter,
        ..ChangeTerms::default()
    };
    let first = new_account(&pool).await;
    let past_cap = new_account(&pool).await;
    let reuse = new_account(&pool).await;
    let refused = new_account(&pool).await;
    let held = place();

    // Under the cap: bound, and the place is not spent.
    let (handle, _) = random_handle();
    assert_eq!(
        bind(
            &pool,
            &TEST_PEPPER,
            &first,
            &handle,
            0,
            &terms(0, Some(held))
        )
        .await
        .unwrap(),
        BindOutcome::Bound
    );
    assert!(!spent(&pool, &held).await);

    // At the cap without a place: the ordinary refusal.
    let (handle, _) = random_handle();
    assert_eq!(
        bind(&pool, &TEST_PEPPER, &refused, &handle, 0, &terms(1, None))
            .await
            .unwrap(),
        BindOutcome::RegistrationClosed
    );

    // At the cap with a place: bound past it, and the place is spent.
    let (handle, _) = random_handle();
    assert_eq!(
        bind(
            &pool,
            &TEST_PEPPER,
            &past_cap,
            &handle,
            0,
            &terms(1, Some(held))
        )
        .await
        .unwrap(),
        BindOutcome::Bound
    );
    assert!(spent(&pool, &held).await);

    // The same place for another account: refused, nothing written.
    let (handle, _) = random_handle();
    assert_eq!(
        bind(
            &pool,
            &TEST_PEPPER,
            &reuse,
            &handle,
            0,
            &terms(1, Some(held))
        )
        .await
        .unwrap(),
        BindOutcome::SupporterCodeUsed
    );
    let c_handle: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT handle_key FROM login_handles WHERE account_id = ?")
            .bind(reuse.as_slice())
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert_eq!(c_handle, None);

    for id in [first, past_cap, reuse, refused] {
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(id.as_slice())
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM anon_token_spent WHERE context = ? AND spend_id = ?")
        .bind(TokenContext::BetaPlace.as_u8())
        .bind(&held.spend_id[..])
        .execute(&pool)
        .await
        .unwrap();
}
