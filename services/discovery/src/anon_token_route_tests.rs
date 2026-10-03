// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0054 S3 - the discovery routes that spend an anonymous token.
//!
//! The migration pin and the dormant (503) case run without a database; the
//! spend cases are gated on `LAKKO_DISCOVERY_TEST_DATABASE_URL` and skip (do
//! not fail) when it is unset:
//!
//! ```text
//! LAKKO_DISCOVERY_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_discovery_test \
//!   cargo test -p lakko-svc-discovery
//! ```

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_anon_token::{MasterSecret, TokenContext, DEFAULT_WINDOW_SECS, TOKEN_HEADER};
use lakko_anon_token_server::testing::mint_header;
use lakko_anon_token_server::{
    current_epoch, AnonTokenServerConfig, TokenVerifier, SPENT_SET_MIGRATION_SQL,
};
use lakko_auth_token::TokenSecret;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tower::ServiceExt as _;

use crate::{build_app, oprf, rate_limit, AppState};

const MASTER_HEX: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

fn master() -> MasterSecret {
    MasterSecret::from_hex(MASTER_HEX).unwrap()
}

pub(crate) fn state(db: MySqlPool, enabled: bool) -> AppState {
    let secret = Arc::new(TokenSecret::new(vec![7u8; 32]).unwrap());
    let limit = rate_limit::Limit {
        per_minute: 6_000.0,
        burst: 1_000.0,
    };
    let config = enabled.then(|| {
        AnonTokenServerConfig::from_values(Some(MASTER_HEX), None)
            .unwrap()
            .unwrap()
    });
    AppState {
        anon_tokens: TokenVerifier::new(db.clone(), config),
        db,
        oprf: Arc::new(oprf::LakkoOprfServer::new_from_seed(&[1u8; 32], b"lakko/oprf/v1").unwrap()),
        session_token_secret: Arc::clone(&secret),
        rate_limits: Arc::new(rate_limit::DiscoveryLimits::new(
            limit,
            limit,
            limit,
            Arc::clone(&secret),
        )),
        fresh_identity_cooldown_secs: 0,
        transparency: None,
        username_cert_key: Arc::new(lakko_crypto::signing::SigningKey::from_seed(&[3u8; 32])),
        key_reset_attest_key: None,
    }
}

fn blinded_body() -> String {
    use rand::rngs::OsRng;
    let blind = voprf::OprfClient::<oprf::LakkoOprfCs>::blind(b"alice", &mut OsRng).unwrap();
    serde_json::json!({ "blinded": B64.encode(blind.message.serialize()) }).to_string()
}

fn post(uri: &str, token: Option<&str>, body: String) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header("x-real-ip", "192.0.2.10")
        .header("content-type", "application/json");
    if let Some(t) = token {
        b = b.header(TOKEN_HEADER, t);
    }
    b.body(Body::from(body)).unwrap()
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn error_code(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub(crate) async fn test_pool(name: &str) -> Option<MySqlPool> {
    let Ok(url) = std::env::var("LAKKO_DISCOVERY_TEST_DATABASE_URL") else {
        eprintln!("skipping {name}: LAKKO_DISCOVERY_TEST_DATABASE_URL unset");
        return None;
    };
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect test DB");
    // Parallel tests share one database and sqlx's migrator does not tolerate
    // two concurrent runs on it (the first run on an empty database failed
    // several tests), so migrate once per process.
    migrate_once(&pool).await;
    Some(pool)
}

static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Run the migrations on the shared test database once per test process.
/// Every DB-backed test module calls this rather than the migrator.
pub(crate) async fn migrate_once(pool: &MySqlPool) {
    MIGRATED
        .get_or_init(|| async {
            crate::MIGRATIONS.run(pool).await.expect("run migrations");
        })
        .await;
}

#[test]
fn the_spent_set_migration_is_the_shared_crate_schema_byte_for_byte() {
    assert_eq!(
        include_str!("../migrations/0019_anon_token_spent.sql"),
        SPENT_SET_MIGRATION_SQL
    );
}

#[tokio::test]
async fn every_token_route_answers_503_when_tokens_are_disabled() {
    let db = MySqlPoolOptions::new()
        .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/none")
        .unwrap();
    let app = build_app(state(db, false));
    let token = mint_header(&master(), TokenContext::DiscoveryLookup, 1);
    for uri in [
        "/discovery/oprf",
        "/discovery/channel/register",
        "/discovery/invite/AAAAAAAA/consume",
        "/discovery/call-invite/AAAAAAAA/consume",
        "/discovery/group-invite/AAAAAAAA/consume",
    ] {
        let (status, body) = send(&app, post(uri, Some(&token), "{}".into())).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(error_code(&body), "anon-tokens-disabled", "{uri}");
    }
    // The routes that lost their proof of work and carry no token are not
    // affected: lookup reaches its handler (a bad body is a 4xx, not a 503).
    let (status, _) = send(&app, post("/discovery/lookup", None, "{}".into())).await;
    assert_ne!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn the_oprf_evaluation_spends_one_discovery_lookup_token() {
    let Some(pool) = test_pool("the_oprf_evaluation_spends_one_discovery_lookup_token").await
    else {
        return;
    };
    let app = build_app(state(pool, true));
    let m = master();
    let now = current_epoch(DEFAULT_WINDOW_SECS);

    // Missing header.
    let (status, body) = send(&app, post("/discovery/oprf", None, blinded_body())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-required");

    // Accepted once, refused the second time.
    let t = mint_header(&m, TokenContext::DiscoveryLookup, now);
    let (status, body) = send(&app, post("/discovery/oprf", Some(&t), blinded_body())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("evaluated"));
    let (status, body) = send(&app, post("/discovery/oprf", Some(&t), blinded_body())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-spent");

    // A token drawn for another action.
    let wrong = mint_header(&m, TokenContext::InviteConsume, now);
    let (status, body) = send(&app, post("/discovery/oprf", Some(&wrong), blinded_body())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-wrong-context");

    // The previous epoch is accepted, the one before it is not.
    let prev = mint_header(&m, TokenContext::DiscoveryLookup, now - 1);
    let (status, _) = send(&app, post("/discovery/oprf", Some(&prev), blinded_body())).await;
    assert_eq!(status, StatusCode::OK);
    let older = mint_header(&m, TokenContext::DiscoveryLookup, now - 2);
    let (status, body) = send(&app, post("/discovery/oprf", Some(&older), blinded_body())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-expired");

    // Garbage in the header.
    let (status, body) = send(
        &app,
        post("/discovery/oprf", Some("not-a-token"), blinded_body()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-malformed");
}

#[tokio::test]
async fn consumes_and_channel_register_spend_their_own_contexts() {
    let Some(pool) = test_pool("consumes_and_channel_register_spend_their_own_contexts").await
    else {
        return;
    };
    let app = build_app(state(pool, true));
    let m = master();
    let now = current_epoch(DEFAULT_WINDOW_SECS);

    let consume_body = serde_json::json!({ "joiner_tag": B64.encode([0u8; 32]) }).to_string();
    for uri in [
        "/discovery/invite/AAAAAAAA/consume",
        "/discovery/call-invite/AAAAAAAA/consume",
        "/discovery/group-invite/AAAAAAAA/consume",
    ] {
        let wrong = mint_header(&m, TokenContext::DiscoveryLookup, now);
        let (status, body) = send(&app, post(uri, Some(&wrong), consume_body.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(error_code(&body), "anon-token-wrong-context", "{uri}");

        let t = mint_header(&m, TokenContext::InviteConsume, now);
        // The token pays; the (absent) invite then answers 404.
        let (status, body) = send(&app, post(uri, Some(&t), consume_body.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
        let (status, body) = send(&app, post(uri, Some(&t), consume_body.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(error_code(&body), "anon-token-spent", "{uri}");
    }

    let mut hash = [0u8; 64];
    lakko_crypto::random::fill(&mut hash);
    let mut channel = [0u8; 16];
    lakko_crypto::random::fill(&mut channel);
    let register_body = serde_json::json!({
        "oprf_hash": B64.encode(hash),
        "channel_id": B64.encode(channel),
    })
    .to_string();
    let wrong = mint_header(&m, TokenContext::DiscoveryLookup, now);
    let (status, body) = send(
        &app,
        post(
            "/discovery/channel/register",
            Some(&wrong),
            register_body.clone(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-wrong-context");
    let t = mint_header(&m, TokenContext::DiscoveryRegister, now);
    let (status, body) = send(
        &app,
        post(
            "/discovery/channel/register",
            Some(&t),
            register_body.clone(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = send(
        &app,
        post("/discovery/channel/register", Some(&t), register_body),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "anon-token-spent");
}
