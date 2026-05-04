// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Push gateway for Lakko.
//!
//! Phase 1.6 — token registry + dispatch scaffold.
//!
//! The server bridges between an internal "wake this device" event and
//! the external platform push services (APNs, FCM, Web Push,
//! UnifiedPush). Payload carried to providers is strictly metadata
//! pointers — `(group_pseudonym, envelope_pointer)`. Apple, Google
//! and Mozilla never see plaintext: the recipient client wakes up,
//! fetches ciphertext from `services/ds`, decrypts locally and builds
//! the OS notification on-device.
//!
//! ## Phase 1.6 scope
//!
//! Real platform integrations need .p8 / service-account / VAPID keys
//! that are operational artifacts, not code. So this sprint ships a
//! provider abstraction (`src/provider.rs`) plus a single
//! [`provider::LogOnlyProvider`] that records intent into `tracing`
//! instead of contacting a remote service.
//! Phase 1.6b drops in real `ApnsProvider` / `FcmProvider` /
//! `WebPushProvider` impls behind the same trait without registry /
//! handler changes.
//!
//! ## Configuration
//!
//! Loaded from `/etc/lakko/lakko.env` via systemd `EnvironmentFile=`
//! in production; from `.env` via `dotenvy` in debug builds.
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8085`)
//! - `LAKKO_PUSH_GW_DATABASE_URL` (or `DATABASE_URL` fallback) —
//!   sqlx-style MariaDB DSN; the database must be exclusive to this
//!   service per the per-service DB isolation rule.
//! - `LAKKO_PUSH_GW_POW_EFFORT_BITS` — PoW difficulty (default 8;
//!   0 in tests).
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec.
//!
//! ## Endpoints
//!
//! - `GET    /health`         — liveness, 200 OK
//! - `GET    /ready`          — readiness (DB ping), 200 / 503
//! - `GET    /version`
//! - `POST   /pow/challenge`  — Equix PoW challenge (1.1e-style)
//! - `POST   /push/register`  — bind `(device_pseudonym, provider) → token`
//! - `POST   /push/unregister`— drop a token
//! - `POST   /push/notify`    — wake every registered endpoint for a device

#![forbid(unsafe_code)]

mod notify;
mod pow;
mod provider;
mod registry;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tokio::signal;
use tower_http::trace::TraceLayer;

use crate::provider::{LogOnlyProvider, Provider, ProviderKind};

/// Default bind address if `LAKKO_BIND` is not set. Distinct from
/// auth (8081), ds (8082), storage (8083), discovery (8084).
const DEFAULT_BIND: &str = "127.0.0.1:8085";

/// Migrations directory (relative to crate root).
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Per-request body limit. Push-gw payloads are small JSON (PoW proof
/// + a couple of base64 fields + maybe a 2 KiB token), 64 KiB is plenty.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Shared application state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: MySqlPool,
    /// One provider per `ProviderKind`. Phase 1.6 always wires every
    /// kind to a [`LogOnlyProvider`]; Phase 1.6b will swap in real
    /// `ApnsProvider` / `FcmProvider` / `WebPushProvider` instances
    /// based on env config.
    pub(crate) providers: Arc<HashMap<ProviderKind, Box<dyn Provider>>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(debug_assertions)]
    {
        if let Ok(path) = dotenvy::dotenv() {
            eprintln!("dotenv loaded from {}", path.display());
        }
    }

    init_tracing();

    let bind: SocketAddr = std::env::var("LAKKO_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_string())
        .parse()
        .context("parse LAKKO_BIND as host:port")?;

    let db_url = std::env::var("LAKKO_PUSH_GW_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_PUSH_GW_DATABASE_URL or DATABASE_URL env var must be set")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    let providers = resolve_providers();
    let state = AppState {
        db: pool.clone(),
        providers: Arc::new(providers),
    };

    let _pow_pruner = pow::spawn_pruner(pool.clone());

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "push-gw service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    pool.close().await;
    tracing::info!("push-gw service stopped cleanly");
    Ok(())
}

/// Pick a provider implementation per `ProviderKind`. Phase 1.6 wires
/// every kind to [`LogOnlyProvider`]. Phase 1.6b will read env (paths
/// to .p8 / service-account.json / VAPID keys) and replace LogOnly
/// entries with real impls.
fn resolve_providers() -> HashMap<ProviderKind, Box<dyn Provider>> {
    let mut map: HashMap<ProviderKind, Box<dyn Provider>> = HashMap::new();
    for kind in [
        ProviderKind::Apns,
        ProviderKind::Fcm,
        ProviderKind::WebPush,
        ProviderKind::UnifiedPush,
    ] {
        map.insert(kind, Box::new(LogOnlyProvider { kind }));
    }
    tracing::info!(
        providers = ?map.keys().map(|k| k.as_str()).collect::<Vec<_>>(),
        "Phase 1.6: every provider kind wired to LogOnlyProvider (no real keys yet)",
    );
    map
}

/// Build the public router. Extracted so tests can drive it directly
/// without binding a TCP listener.
pub(crate) fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/pow/challenge", post(pow::issue_challenge))
        .route("/push/register", post(registry::register))
        .route("/push/unregister", post(registry::unregister))
        .route("/push/notify", post(notify::notify))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.db)
        .await
    {
        Ok(_) => (StatusCode::OK, "ready\n").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "readiness probe: db ping failed");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready\n").into_response()
        },
    }
}

async fn version() -> impl IntoResponse {
    Json(json!({
        "service": "lakko-svc-push-gw",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_level(true)
        .compact()
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received SIGINT, shutting down"),
        () = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    fn test_app_no_db() -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/version", get(version))
    }

    #[tokio::test]
    async fn health_returns_200_ok() {
        let response = test_app_no_db()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok\n");
    }

    #[tokio::test]
    async fn version_returns_json_with_service_name() {
        let response = test_app_no_db()
            .oneshot(
                Request::builder()
                    .uri("/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["service"], "lakko-svc-push-gw");
    }

    #[test]
    fn resolve_providers_covers_every_kind() {
        let map = resolve_providers();
        for kind in [
            ProviderKind::Apns,
            ProviderKind::Fcm,
            ProviderKind::WebPush,
            ProviderKind::UnifiedPush,
        ] {
            let provider = map.get(&kind).expect("provider missing");
            assert_eq!(provider.kind(), kind);
        }
    }
}
