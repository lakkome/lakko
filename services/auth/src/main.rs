// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Auth service for Lakko.
//!
//! Phase 1.1a: HTTP server skeleton (axum) + MariaDB pool (sqlx) +
//! liveness/readiness probes + graceful shutdown.
//! Phase 1.1b: OPAQUE-PAKE registration (`/signup/{start,finish}`).
//! Phase 1.1c: OPAQUE-PAKE login (`/login/{start,finish}`).
//! Phase 1.1d: BIP39 mnemonic recovery — a parallel OPAQUE registration
//! kind, bundled into signup and exposed for re-authentication via
//! `/recovery/login/{start,finish}`.
//! Phase 1.1e: Equix proof-of-work anti-spam (`/pow/challenge`) gating
//! the four expensive endpoints. Privacy Pass tokens (RFC 9576) are
//! deferred until we have App Attest / Play Integrity issuers; IP rate
//! limit with rotating salts lands in Phase 2 hardening.
//!
//! Configuration via environment variables (loaded by systemd from
//! `/etc/lakko/lakko.env`):
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8081`)
//! - `DATABASE_URL` — sqlx-style MariaDB DSN
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec
//! - `LAKKO_POW_EFFORT_BITS` — PoW difficulty (default 8; 0 in tests)
//!
//! Endpoints:
//!
//! - `GET  /health`                  — liveness (no DB), 200 OK
//! - `GET  /ready`                   — readiness (pings DB), 200 OK or 503
//! - `GET  /version`                 — `{"service", "version", "git_sha"}`
//! - `POST /pow/challenge`           — issue Equix PoW challenge (Phase 1.1e)
//! - `POST /signup/start`            — OPAQUE registration step 1 (Phase 1.1b/d/e)
//! - `POST /signup/finish`           — OPAQUE registration step 2 (Phase 1.1b/d/e)
//! - `POST /login/start`             — OPAQUE login step 1, password (Phase 1.1c/e)
//! - `POST /login/finish`            — OPAQUE login step 2, password (Phase 1.1c)
//! - `POST /recovery/login/start`    — OPAQUE login step 1, BIP39 (Phase 1.1d/e)
//! - `POST /recovery/login/finish`   — OPAQUE login step 2, BIP39 (Phase 1.1d)

#![forbid(unsafe_code)]

mod opaque;
mod pow;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use opaque_ke::ServerSetup;
use serde_json::json;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tokio::signal;
use tower_http::trace::TraceLayer;

use crate::opaque::LakkoCipherSuite;

/// Default bind address if `LAKKO_BIND` is not set.
const DEFAULT_BIND: &str = "127.0.0.1:8081";

/// Migrations directory (relative to crate root). `sqlx::migrate!` reads it
/// at compile time and embeds the SQL in the binary.
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Shared application state.
///
/// Every field must be `Clone`-able (primitive or `Arc<...>`); axum
/// requires `Clone + Send + Sync` for the `State<AppState>` extractor.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: MySqlPool,
    /// Server-static OPAQUE setup — one per service instance, loaded (or
    /// generated on first run) at bootstrap. Wrapped in `Arc` to avoid
    /// cloning the inner scalar / point material on every request.
    pub(crate) opaque_setup: Arc<ServerSetup<LakkoCipherSuite>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load `.env` from the workspace root (or nearest parent) — debug
    // builds only. Release builds skip dotenv entirely; production picks
    // up env vars from the systemd `EnvironmentFile=/etc/lakko/lakko.env`
    // directive instead.
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

    let db_url = std::env::var("DATABASE_URL").context("DATABASE_URL env var must be set")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    // Apply embedded migrations on startup. Idempotent.
    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    // Load (or initialize on first run) the server-static OPAQUE setup.
    // Runs AFTER migrations so opaque_server_setup table exists.
    let opaque_setup = opaque::load_or_init_server_setup(&pool)
        .await
        .context("load OPAQUE ServerSetup")?;

    let state = AppState {
        db: pool.clone(),
        opaque_setup: Arc::new(opaque_setup),
    };

    // Background tasks: periodically delete expired rows from the
    // `opaque_login_states` and `pow_challenges` tables. Aborted when
    // the service stops.
    let _login_pruner = opaque::spawn_login_state_pruner(pool.clone());
    let _pow_pruner = pow::spawn_pruner(pool.clone());

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "auth service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    pool.close().await;
    tracing::info!("auth service stopped cleanly");
    Ok(())
}

/// Build the public router. Extracted so tests can drive it directly without
/// binding a TCP listener.
fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/pow/challenge", post(pow::issue_challenge))
        .route("/signup/start", post(opaque::signup_start))
        .route("/signup/finish", post(opaque::signup_finish))
        .route("/login/start", post(opaque::login_start))
        .route("/login/finish", post(opaque::login_finish))
        .route("/recovery/login/start", post(opaque::recovery_login_start))
        .route(
            "/recovery/login/finish",
            post(opaque::recovery_login_finish),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Liveness — service process is alive. Does **not** check the DB; nginx /
/// systemd watchdog should poll this for restart decisions.
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// Readiness — service can serve traffic, including DB-backed endpoints.
/// Pings the connection pool with `SELECT 1`.
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

/// Service version metadata.
async fn version() -> impl IntoResponse {
    Json(json!({
        "service": "lakko-svc-auth",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

/// Initialize tracing-subscriber from `RUST_LOG`. Falls back to `info`.
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

/// Wait for SIGTERM (systemd stop) or SIGINT (Ctrl-C).
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

    /// Build a router suitable for testing endpoints that don't touch the DB.
    /// `/health` and `/version` work without a live connection; `/ready`
    /// requires a real pool and is covered by integration tests.
    fn test_app_no_db() -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/version", get(version))
    }

    #[tokio::test]
    async fn health_returns_200_ok() {
        let app = test_app_no_db();
        let response = app
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
        let app = test_app_no_db();
        let response = app
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
        assert_eq!(parsed["service"], "lakko-svc-auth");
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let app = test_app_no_db();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
