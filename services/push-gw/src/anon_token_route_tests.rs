// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0054 S3 - the four registration routes spend one `push-register`
//! anonymous token each.
//!
//! The migration pin and the dormant (503) case run without a database; the
//! spend cases are gated on `LAKKO_PUSH_GW_TEST_DATABASE_URL` and skip (do not
//! fail) when it is unset.

use std::collections::HashMap;
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
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tower::ServiceExt as _;

use crate::{build_app, AppState};

const MASTER_HEX: &str = "7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c";

fn master() -> MasterSecret {
    MasterSecret::from_hex(MASTER_HEX).unwrap()
}

fn state(db: MySqlPool, enabled: bool) -> AppState {
    let config = enabled.then(|| {
        AnonTokenServerConfig::from_values(Some(MASTER_HEX), None)
            .unwrap()
            .unwrap()
    });
    AppState {
        anon_tokens: TokenVerifier::new(db.clone(), config),
        db,
        providers: Arc::new(HashMap::new()),
        internal_secret: None,
    }
}

/// A fresh random 32-byte pseudonym, lowercase hex, so reruns never collide.
fn fresh_hex() -> String {
    use rand::RngCore as _;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

fn post(uri: &str, token: Option<&str>, body: String) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
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

/// Each route with a well-formed body for a fresh device (and group).
fn routes() -> Vec<(&'static str, String)> {
    let device = fresh_hex();
    let group = fresh_hex();
    vec![
        (
            "/push/register",
            serde_json::json!({
                "device_pseudonym": device,
                "provider": "fcm",
                "token": B64.encode([7u8; 32]),
            })
            .to_string(),
        ),
        (
            "/push/unregister",
            serde_json::json!({ "device_pseudonym": device, "provider": "fcm" }).to_string(),
        ),
        (
            "/push/group/subscribe",
            serde_json::json!({ "device_pseudonym": device, "group_pseudonym": group }).to_string(),
        ),
        (
            "/push/group/unsubscribe",
            serde_json::json!({ "device_pseudonym": device, "group_pseudonym": group }).to_string(),
        ),
    ]
}

async fn test_pool(name: &str) -> Option<MySqlPool> {
    let Ok(url) = std::env::var("LAKKO_PUSH_GW_TEST_DATABASE_URL") else {
        eprintln!("skipping {name}: LAKKO_PUSH_GW_TEST_DATABASE_URL unset");
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

#[test]
fn the_spent_set_migration_is_the_shared_crate_schema_byte_for_byte() {
    assert_eq!(
        include_str!("../migrations/0004_anon_token_spent.sql"),
        SPENT_SET_MIGRATION_SQL
    );
}

#[tokio::test]
async fn every_registration_route_answers_503_when_tokens_are_disabled() {
    let db = MySqlPoolOptions::new()
        .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/none")
        .unwrap();
    let app = build_app(state(db, false));
    let token = mint_header(&master(), TokenContext::PushRegister, 1);
    for (uri, body) in routes() {
        let (status, resp) = send(&app, post(uri, Some(&token), body)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(error_code(&resp), "anon-tokens-disabled", "{uri}");
    }
}

#[tokio::test]
async fn each_registration_route_spends_one_push_register_token() {
    let Some(pool) = test_pool("each_registration_route_spends_one_push_register_token").await
    else {
        return;
    };
    let app = build_app(state(pool, true));
    let m = master();
    let now = current_epoch(DEFAULT_WINDOW_SECS);

    for (uri, body) in routes() {
        let (status, resp) = send(&app, post(uri, None, body.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(error_code(&resp), "anon-token-required", "{uri}");

        let wrong = mint_header(&m, TokenContext::DsKnock, now);
        let (status, resp) = send(&app, post(uri, Some(&wrong), body.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(error_code(&resp), "anon-token-wrong-context", "{uri}");

        let t = mint_header(&m, TokenContext::PushRegister, now);
        let (status, resp) = send(&app, post(uri, Some(&t), body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {resp}");
        let (status, resp) = send(&app, post(uri, Some(&t), body.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(error_code(&resp), "anon-token-spent", "{uri}");

        let previous = mint_header(&m, TokenContext::PushRegister, now - 1);
        let (status, _) = send(&app, post(uri, Some(&previous), body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{uri}");

        let older = mint_header(&m, TokenContext::PushRegister, now - 2);
        let (status, resp) = send(&app, post(uri, Some(&older), body)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(error_code(&resp), "anon-token-expired", "{uri}");
    }
}
