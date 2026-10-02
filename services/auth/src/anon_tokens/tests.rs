// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tests for anonymous-token issuance. The routing / validation tests run
//! without a database (every refusal they check happens before the store is
//! touched); the issuance round-trip and quota tests are gated on
//! `LAKKO_AUTH_TEST_DATABASE_URL` and skip when it is unset.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_anon_token::{
    verify_token, IssuerKey, MasterSecret, TokenContext, TokenRequest, MAX_BATCH,
};
use lakko_anon_token_server::{AnonTokenServerConfig, ISSUANCE_MIGRATION_SQL};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tower::ServiceExt as _;

use super::{quota_var, quotas_from, AnonTokenIssuer, DEFAULT_QUOTAS};

const MASTER_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

fn master() -> MasterSecret {
    MasterSecret::from_hex(MASTER_HEX).unwrap()
}

fn issuer_with(quotas: [(TokenContext, u32); super::CONTEXT_COUNT]) -> AnonTokenIssuer {
    let server = AnonTokenServerConfig::from_values(Some(MASTER_HEX), None)
        .unwrap()
        .unwrap();
    AnonTokenIssuer::new(server, quotas)
}

fn build_state(
    pool: MySqlPool,
    anon: Option<AnonTokenIssuer>,
) -> (crate::AppState, lakko_auth_token::TokenSecret) {
    use opaque_ke::ServerSetup;
    use rand::rngs::OsRng;
    let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
    let state = crate::AppState {
        db: pool,
        opaque_setup: Arc::new(ServerSetup::<crate::opaque::LakkoCipherSuite>::new(
            &mut OsRng,
        )),
        session_token_secret: Arc::new(token_secret.clone()),
        session_token_ttl_ms: 86_400_000,
        webauthn: None,
        webauthn_action_core: None,
        call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
        attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
        turn: None,
        totp_enc_key: None,
        recovery_contact_pepper: None,
        vault_enc_key: None,
        otp_sender: Arc::new(crate::otp_sender::LogOnlyOtpSender),
        throttle: crate::throttle::ThrottleConfig::disabled(),
        backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
        anon_tokens: anon.map(Arc::new),
        handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
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

fn bearer(secret: &lakko_auth_token::TokenSecret, account: [u8; 16]) -> String {
    let token = secret
        .issue(&account, now_ms() + 3_600_000)
        .expect("issue session token");
    format!("Bearer {token}")
}

async fn send(state: &crate::AppState, req: Request<Body>) -> (StatusCode, Option<String>, Bytes) {
    let resp = crate::build_app(state.clone())
        .oneshot(req)
        .await
        .expect("router oneshot");
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(axum::http::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, retry_after, bytes)
}

fn issue_req(auth: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/auth/tokens")
        .header("authorization", auth)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn key_req(context: &str, epoch: u64) -> Request<Body> {
    Request::builder()
        .uri(format!("/auth/tokens/key/{context}/{epoch}"))
        .body(Body::empty())
        .unwrap()
}

fn json(bytes: &Bytes) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("json body")
}

fn blinded_json(req: &TokenRequest) -> Vec<String> {
    req.blinded_elements()
        .iter()
        .map(|b| URL_SAFE_NO_PAD.encode(b))
        .collect()
}

fn decode32(v: &serde_json::Value) -> [u8; 32] {
    URL_SAFE_NO_PAD
        .decode(v.as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

// ---- DB-free -----------------------------------------------------------------

#[test]
fn the_migration_is_the_shared_crate_schema_byte_for_byte() {
    assert_eq!(
        include_str!("../../migrations/0019_anon_token_issuance.sql"),
        ISSUANCE_MIGRATION_SQL
    );
}

#[test]
fn quota_variables_are_named_after_the_contexts() {
    assert_eq!(
        quota_var(TokenContext::DiscoveryLookup),
        "LAKKO_ANON_TOKEN_QUOTA_DISCOVERY_LOOKUP"
    );
    assert_eq!(
        quota_var(TokenContext::DsKnock),
        "LAKKO_ANON_TOKEN_QUOTA_DS_KNOCK"
    );
    assert_eq!(
        quota_var(TokenContext::VaultClaim),
        "LAKKO_ANON_TOKEN_QUOTA_VAULT_CLAIM"
    );
    assert_eq!(
        quota_var(TokenContext::BlobUpload),
        "LAKKO_ANON_TOKEN_QUOTA_BLOB_UPLOAD"
    );
    assert_eq!(
        quota_var(TokenContext::BlobRead),
        "LAKKO_ANON_TOKEN_QUOTA_BLOB_READ"
    );
    // Every context has exactly one default, in wire order.
    let listed: Vec<TokenContext> = DEFAULT_QUOTAS.iter().map(|(c, _)| *c).collect();
    assert_eq!(listed, TokenContext::ALL);
}

fn default_quota(context: TokenContext) -> u32 {
    DEFAULT_QUOTAS
        .iter()
        .find(|(c, _)| *c == context)
        .map(|(_, q)| *q)
        .unwrap()
}

#[test]
fn the_default_quotas_are_the_documented_ones() {
    assert_eq!(default_quota(TokenContext::DiscoveryLookup), 100);
    assert_eq!(default_quota(TokenContext::DiscoveryRegister), 10);
    assert_eq!(default_quota(TokenContext::InviteConsume), 30);
    assert_eq!(default_quota(TokenContext::DsKnock), 200);
    assert_eq!(default_quota(TokenContext::PushRegister), 100);
    assert_eq!(default_quota(TokenContext::ChannelSubscribe), 20);
    assert_eq!(default_quota(TokenContext::DirectorySearch), 30);
    assert_eq!(default_quota(TokenContext::VaultClaim), 100);
    assert_eq!(default_quota(TokenContext::BlobUpload), 96);
    assert_eq!(default_quota(TokenContext::BlobRead), 96);
}

#[test]
fn a_quota_variable_overrides_its_context_and_no_other() {
    // Nothing set: the defaults.
    assert_eq!(quotas_from(|_| None).unwrap(), DEFAULT_QUOTAS);

    // Each context's variable moves that context alone, the new ones included.
    for target in TokenContext::ALL {
        let var = quota_var(target);
        let quotas = quotas_from(|name| (name == var).then(|| " 7 ".to_string())).unwrap();
        for (c, q) in quotas {
            let expected = if c == target { 7 } else { default_quota(c) };
            assert_eq!(q, expected, "{} under {var}", c.name());
        }
    }

    // Zero stops issuance for the context; a blank value keeps the default.
    let var = quota_var(TokenContext::BlobRead);
    let zero = quotas_from(|name| (name == var).then(|| "0".to_string())).unwrap();
    assert!(zero.contains(&(TokenContext::BlobRead, 0)));
    let blank = quotas_from(|name| (name == var).then(|| "  ".to_string())).unwrap();
    assert_eq!(blank, DEFAULT_QUOTAS);

    // A malformed value is an error naming the variable, never a default.
    for bad in ["many", "-1", "1.5", "4294967296"] {
        let var = quota_var(TokenContext::VaultClaim);
        let err = quotas_from(|name| (name == var).then(|| bad.to_string())).unwrap_err();
        assert!(err
            .to_string()
            .contains("LAKKO_ANON_TOKEN_QUOTA_VAULT_CLAIM"));
    }
}

#[tokio::test]
async fn dormant_without_a_master_secret() {
    let (state, secret) = build_state(lazy_pool(), None);
    let (status, _, body) = send(
        &state,
        issue_req(
            &bearer(&secret, [1; 16]),
            &serde_json::json!({"context": "ds-knock", "blinded_elements": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json(&body)["error"], "anon-tokens-disabled");

    let (status, _, _) = send(&state, key_req("ds-knock", 0)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn issuance_requires_a_session() {
    let (state, _) = build_state(lazy_pool(), Some(issuer_with(DEFAULT_QUOTAS)));
    let req = Request::builder()
        .method("POST")
        .uri("/auth/tokens")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"context": "ds-knock", "blinded_elements": []}).to_string(),
        ))
        .unwrap();
    let (status, _, _) = send(&state, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn malformed_requests_are_refused_before_the_store() {
    // The pool can never connect: reaching the quota store would fail as a
    // 500, so every 400 here proves the refusal happened before it.
    let (state, secret) = build_state(lazy_pool(), Some(issuer_with(DEFAULT_QUOTAS)));
    let auth = bearer(&secret, [2; 16]);
    let good = blinded_json(&TokenRequest::new(TokenContext::DsKnock, 0, 1).unwrap());

    let cases = [
        (
            serde_json::json!({"context": "nope", "blinded_elements": good}),
            "unknown-context",
        ),
        (
            serde_json::json!({"context": "ds-knock", "blinded_elements": []}),
            "bad-batch-size",
        ),
        (
            serde_json::json!({"context": "ds-knock", "blinded_elements": vec![good[0].clone(); MAX_BATCH + 1]}),
            "bad-batch-size",
        ),
        (
            serde_json::json!({"context": "ds-knock", "blinded_elements": ["!!"]}),
            "bad-blinded-element",
        ),
        (
            serde_json::json!({"context": "ds-knock", "blinded_elements": [URL_SAFE_NO_PAD.encode([0u8; 31])]}),
            "bad-blinded-element",
        ),
        (
            // The identity element.
            serde_json::json!({"context": "ds-knock", "blinded_elements": [URL_SAFE_NO_PAD.encode([0u8; 32])]}),
            "bad-blinded-element",
        ),
        (
            serde_json::json!({"context": "ds-knock", "epoch": 1, "blinded_elements": good}),
            "epoch-not-issuable",
        ),
    ];
    for (body, code) in cases {
        let (status, _, bytes) = send(&state, issue_req(&auth, &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{code}");
        assert_eq!(json(&bytes)["error"], code);
    }
}

#[tokio::test]
async fn the_key_route_serves_the_derived_key_near_the_current_epoch_only() {
    let issuer = issuer_with(DEFAULT_QUOTAS);
    let current = issuer.server.current_epoch();
    let (state, _) = build_state(lazy_pool(), Some(issuer));

    for epoch in [current - 1, current, current + 1] {
        let (status, _, body) = send(&state, key_req("invite-consume", epoch)).await;
        assert_eq!(status, StatusCode::OK);
        let v = json(&body);
        assert_eq!(v["context"], "invite-consume");
        assert_eq!(v["epoch"], epoch);
        let expected = IssuerKey::derive(&master(), TokenContext::InviteConsume, epoch)
            .unwrap()
            .public_key();
        assert_eq!(decode32(&v["public_key"]), expected);
    }
    for epoch in [current - 2, current + 2] {
        let (status, _, body) = send(&state, key_req("invite-consume", epoch)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json(&body)["error"], "epoch-out-of-range");
    }
    let (status, _, _) = send(&state, key_req("nope", current)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---- DB-backed ---------------------------------------------------------------

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

/// The whole flow a client will run: blind, issue, fetch the published key,
/// check the proof and unblind, then spend at a verifier.
#[tokio::test]
async fn issued_tokens_finalize_against_the_published_key_and_verify() {
    let Some(pool) = test_pool("issued_tokens_finalize_against_the_published_key_and_verify").await
    else {
        return;
    };
    let issuer = issuer_with(DEFAULT_QUOTAS);
    let window = issuer.server.window_secs;
    let (state, secret) = build_state(pool, Some(issuer));
    let account: [u8; 16] = uuid::Uuid::now_v7().into_bytes();

    let (status, _, body) = send(
        &state,
        issue_req(
            &bearer(&secret, account),
            &serde_json::json!({"context": "discovery-lookup", "blinded_elements": [URL_SAFE_NO_PAD.encode([9u8; 32])]}),
        ),
    )
    .await;
    // [9; 32] is not a valid Ristretto encoding: refused, quota untouched.
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", json(&body));

    let epoch = lakko_anon_token_server::current_epoch(window);
    let req = TokenRequest::new(TokenContext::DiscoveryLookup, epoch, 5).unwrap();
    let (status, _, body) = send(
        &state,
        issue_req(
            &bearer(&secret, account),
            &serde_json::json!({"context": "discovery-lookup", "blinded_elements": blinded_json(&req)}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", json(&body));
    let res = json(&body);
    assert_eq!(res["epoch"], epoch);
    assert_eq!(res["remaining"], 95);
    let evaluations: Vec<[u8; 32]> = res["evaluations"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode32)
        .collect();
    let proof: [u8; 64] = URL_SAFE_NO_PAD
        .decode(res["proof"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();

    let (status, _, key_body) = send(&state, key_req("discovery-lookup", epoch)).await;
    assert_eq!(status, StatusCode::OK);
    let published = decode32(&json(&key_body)["public_key"]);
    assert_eq!(published, decode32(&res["public_key"]));

    let tokens = req.finalize(&evaluations, &proof, &published).unwrap();
    assert_eq!(tokens.len(), 5);
    let now = lakko_anon_token_server::now_unix_secs();
    for t in tokens {
        let v = verify_token(
            &master(),
            &t.encode(),
            TokenContext::DiscoveryLookup,
            now,
            window,
        )
        .expect("an issued token verifies at a consumer");
        assert_eq!(v.epoch, epoch);
    }
}

#[tokio::test]
async fn over_quota_is_a_429_with_retry_after_and_counts_nothing() {
    let Some(pool) = test_pool("over_quota_is_a_429_with_retry_after_and_counts_nothing").await
    else {
        return;
    };
    let mut quotas = DEFAULT_QUOTAS;
    for (c, q) in &mut quotas {
        if *c == TokenContext::DsKnock {
            *q = 3;
        }
    }
    let issuer = issuer_with(quotas);
    let window = issuer.server.window_secs;
    let (state, secret) = build_state(pool, Some(issuer));
    let account: [u8; 16] = uuid::Uuid::now_v7().into_bytes();
    let auth = bearer(&secret, account);
    let epoch = lakko_anon_token_server::current_epoch(window);

    let ask = |n: usize| {
        let req = TokenRequest::new(TokenContext::DsKnock, epoch, n).unwrap();
        serde_json::json!({"context": "ds-knock", "blinded_elements": blinded_json(&req)})
    };

    let (status, _, body) = send(&state, issue_req(&auth, &ask(2))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["remaining"], 1);

    let (status, retry_after, body) = send(&state, issue_req(&auth, &ask(2))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let v = json(&body);
    assert_eq!(v["error"], "anon-token-quota-exhausted");
    assert_eq!(v["remaining"], 1);
    let retry: u64 = retry_after.expect("Retry-After").parse().unwrap();
    assert!(retry >= 1 && retry <= window);
    assert_eq!(v["retry_after_secs"], retry);

    // The refused batch counted nothing: the last token is still there.
    let (status, _, body) = send(&state, issue_req(&auth, &ask(1))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["remaining"], 0);

    // Another context is untouched by this one's quota.
    let req = TokenRequest::new(TokenContext::PushRegister, epoch, 1).unwrap();
    let (status, _, _) = send(
        &state,
        issue_req(
            &auth,
            &serde_json::json!({"context": "push-register", "blinded_elements": blinded_json(&req)}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
