// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Discovery service for Lakko.
//!
//! Phase 1.5 — username table only. Resolves `@username` to
//! `account_id` via OPRF-blinded queries (RFC 9497, Ristretto255-SHA512
//! ciphersuite). The server never stores plaintext usernames or sees
//! them in queries — only blinded curve points and OPRF outputs. See
//! `oprf.rs` for the protocol details and `registry.rs` for the
//! storage layer.
//!
//! KeyPackage bundle delivery and Key Transparency log inclusion
//! proofs are deferred to Phase 1.5b / Phase 2.
//!
//! ## Known Phase-1 gap
//!
//! `/discovery/register` trusts the client's claim of `account_id`.
//! Phase 1.5b will gate it on an auth-issued session token (JWT signed
//! by `services/auth`); until then squatting is technically possible
//! but PoW-rate-limited and recoverable by table reset before beta.
//!
//! ## Configuration
//!
//! Loaded from `/etc/lakko/lakko.env` via systemd `EnvironmentFile=`
//! in production; from `.env` via `dotenvy` in debug builds.
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8084`)
//! - `LAKKO_DISCOVERY_DATABASE_URL` (or `DATABASE_URL` fallback) —
//!   sqlx-style MariaDB DSN; the database must be exclusive to this
//!   service per the per-service DB isolation rule.
//! - `LAKKO_DISCOVERY_POW_EFFORT_BITS` — PoW difficulty (default 8;
//!   0 in tests).
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec.
//!
//! ## Endpoints
//!
//! - `GET    /health`              — liveness, 200 OK
//! - `GET    /ready`               — readiness (DB ping), 200 / 503
//! - `GET    /version`
//! - `POST   /pow/challenge`       — Equix PoW challenge (1.1e-style)
//! - `POST   /discovery/oprf`      — apply server static `K` to a blinded element
//! - `POST   /discovery/register`  — bind OPRF-hashed username to account_id
//! - `POST   /discovery/lookup`    — resolve OPRF-hashed username → account_id
//! - `POST   /discovery/identity`           — upsert public identity bundle (Phase 1.16d3a)
//! - `GET    /discovery/identity/{acct}`    — fetch public identity bundle (Phase 1.16d3a)

#![forbid(unsafe_code)]

mod identity_bundle;
mod oprf;
mod pow;
mod registry;

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

/// Default bind address if `LAKKO_BIND` is not set. Distinct from
/// auth (8081), ds (8082), storage (8083).
const DEFAULT_BIND: &str = "127.0.0.1:8084";

/// Migrations directory (relative to crate root). `sqlx::migrate!`
/// reads it at compile time and embeds the SQL in the binary.
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Per-request body limit. Discovery endpoints carry small JSON
/// (PoW proof + a couple of base64 fields) so 64 KB is plenty.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Shared application state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: MySqlPool,
    /// Server-static OPRF state — loaded (or generated on first run)
    /// at bootstrap. `Arc` so cloning the state for axum is free.
    pub(crate) oprf: Arc<oprf::LakkoOprfServer>,
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

    // Per-service DB — see CLAUDE.md "Local dev". Falls back to the
    // generic `DATABASE_URL` for single-service runs in isolation.
    let db_url = std::env::var("LAKKO_DISCOVERY_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_DISCOVERY_DATABASE_URL or DATABASE_URL env var must be set")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    let oprf_server = oprf::load_or_init_server_setup(&pool)
        .await
        .context("load OPRF server setup")?;

    let state = AppState {
        db: pool.clone(),
        oprf: Arc::new(oprf_server),
    };

    let _pow_pruner = pow::spawn_pruner(pool.clone());

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "discovery service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    pool.close().await;
    tracing::info!("discovery service stopped cleanly");
    Ok(())
}

/// Build the public router. Extracted so tests can drive it directly
/// without binding a TCP listener.
pub(crate) fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/pow/challenge", post(pow::issue_challenge))
        .route("/discovery/oprf", post(oprf::evaluate))
        .route("/discovery/register", post(registry::register))
        .route("/discovery/lookup", post(registry::lookup))
        .route("/discovery/identity", post(identity_bundle::upload))
        .route(
            "/discovery/identity/{account_id}",
            get(identity_bundle::fetch),
        )
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
        "service": "lakko-svc-discovery",
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

    /// Build a router that doesn't touch the DB — for /health,
    /// /version, /not-found probes that work without a live pool.
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
        assert_eq!(parsed["service"], "lakko-svc-discovery");
    }
}
