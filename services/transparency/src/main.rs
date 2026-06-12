// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Transparency service for Lakko.
//!
//! Phase 2 — the persistent CONIKS-style key-transparency log, the
//! trust anchor for identity. The append-only Merkle log, signed
//! roots, and inclusion proofs all live in the `lakko-transparency`
//! crate; this service WRAPS that with:
//!
//! - **Persistence** — every leaf is durably stored in MariaDB
//!   (`log_entries`); the in-memory Merkle tree is rebuilt by
//!   replaying those rows in `idx` order at boot, so the log survives
//!   a restart (see `store.rs`).
//! - **An operator signing key** — a single Ed25519 seed, generated on
//!   first boot and persisted (`transparency_operator_key`). The
//!   derived verifying key is the trust anchor clients pin out-of-band
//!   (see `operator_key.rs`).
//! - **HTTP** — append (shared-service-secret-authed; KT-log S2),
//!   current signed root, inclusion proof, operator-key publication
//!   (see `handlers.rs`).
//!
//! ## Configuration
//!
//! Loaded from `/etc/lakko/lakko.env` via systemd `EnvironmentFile=`
//! in production; from `.env` via `dotenvy` in debug builds.
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8087`).
//! - `LAKKO_TRANSPARENCY_DATABASE_URL` (or `DATABASE_URL` fallback) —
//!   sqlx-style MariaDB DSN; the database must be exclusive to this
//!   service per the per-service DB isolation rule.
//! - `LAKKO_TRANSPARENCY_APPEND_SECRET` — base64-encoded shared service
//!   secret (`>= 16` bytes after decode) that gates the append
//!   endpoint (KT-log S2). Only the discovery service holds it. When
//!   UNSET the append endpoint serves **503** (loud — there is no
//!   anonymous-append fallback); a SET-but-malformed value fails fast
//!   at startup. The read paths stay anonymous regardless.
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec.
//!
//! ## Endpoints
//!
//! - `GET  /health`                      — liveness, 200 OK
//! - `GET  /ready`                       — readiness (DB ping), 200 / 503
//! - `GET  /version`
//! - `POST /transparency/append`         — service-secret-authed leaf append
//! - `GET  /transparency/root`           — anon current signed root
//! - `GET  /transparency/proof/{idx}`    — anon inclusion proof
//! - `GET  /transparency/latest/{username_hash_b64}` — anon latest entry
//! - `GET  /transparency/operator-key`   — anon operator pubkey to pin

#![forbid(unsafe_code)]

pub(crate) mod handlers;
pub(crate) mod operator_key;
pub(crate) mod service_auth;
pub(crate) mod store;

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

use crate::store::LogStore;

/// Default bind address if `LAKKO_BIND` is not set. Distinct from auth
/// (8081), ds (8082), storage (8083), discovery (8084), push-gw (8085),
/// channels (8086).
const DEFAULT_BIND: &str = "127.0.0.1:8087";

/// Migrations directory (relative to crate root). `sqlx::migrate!`
/// reads it at compile time and embeds the SQL in the binary.
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Per-request body limit. Append carries a couple of base64 fields +
/// a version, so 64 KB is plenty.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Shared application state.
#[derive(Clone)]
pub(crate) struct AppState {
    /// MariaDB pool — the durable log + operator-key store.
    pub(crate) db: MySqlPool,
    /// The persistent log: an in-memory Merkle tree (rebuilt from the DB
    /// at boot) behind a mutex, plus the operator signing key. `Arc` so
    /// cloning the state for axum is free.
    pub(crate) store: Arc<LogStore>,
    /// Shared service secret gating `POST /transparency/append` (KT-log
    /// S2). Decoded bytes from `LAKKO_TRANSPARENCY_APPEND_SECRET`; `None`
    /// when the env var is unset, in which case the append endpoint
    /// serves 503 (loud — no anonymous-append fallback). Consumed by the
    /// `service_auth::require_service_secret` middleware on the append
    /// route via a constant-time compare.
    pub(crate) append_secret: Option<Arc<Vec<u8>>>,
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
    let db_url = std::env::var("LAKKO_TRANSPARENCY_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_TRANSPARENCY_DATABASE_URL or DATABASE_URL env var must be set")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    // Operator signing key — generated on first boot, persisted, rebuilt
    // from the seed at every start. The derived verifying key is the
    // trust anchor.
    let operator = operator_key::load_or_init_operator_key(&pool)
        .await
        .context("load operator signing key")?;

    // Rebuild the Merkle tree from the durable rows. Fails loud on a
    // corrupt (non-contiguous) `idx` sequence rather than serving a
    // forked root.
    let store = LogStore::load(&pool, operator)
        .await
        .context("rebuild transparency log from DB")?;

    // Append service secret (KT-log S2). Only the discovery service
    // holds it. A SET-but-malformed value fails fast at startup; UNSET
    // is allowed here (the append endpoint then serves 503 loud — see
    // `service_auth::require_service_secret`) so a fresh deploy can boot
    // the read paths before the shared secret is provisioned.
    let append_secret = service_auth::load_append_secret()
        .map_err(anyhow::Error::msg)
        .context("load LAKKO_TRANSPARENCY_APPEND_SECRET")?;
    if append_secret.is_some() {
        tracing::info!("transparency append secret configured — append endpoint armed");
    } else {
        tracing::warn!(
            "LAKKO_TRANSPARENCY_APPEND_SECRET unset — POST /transparency/append disabled \
             (serves 503); read paths remain open"
        );
    }

    let state = AppState {
        db: pool.clone(),
        store: Arc::new(store),
        append_secret,
    };

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "transparency service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    pool.close().await;
    tracing::info!("transparency service stopped cleanly");
    Ok(())
}

/// Build the public router. Extracted so tests can drive it directly
/// without binding a TCP listener.
///
/// `POST /transparency/append` sits behind the
/// `require_service_secret` middleware (KT-log S2 gate — only the
/// discovery service holds the shared secret). The read paths
/// (`/root`, `/proof`, `/operator-key`) plus the probes stay open —
/// they take no account-bound input and serve only public artefacts.
pub(crate) fn build_app(state: AppState) -> Router {
    use axum::middleware;

    let protected = Router::new()
        .route("/transparency/append", post(handlers::append))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            service_auth::require_service_secret,
        ));

    let open = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/transparency/root", get(handlers::root))
        .route("/transparency/proof/{idx}", get(handlers::proof))
        .route(
            "/transparency/latest/{username_hash_b64}",
            get(handlers::latest),
        )
        .route("/transparency/operator-key", get(handlers::operator_key));

    open.merge(protected)
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
        "service": "lakko-svc-transparency",
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
        assert_eq!(parsed["service"], "lakko-svc-transparency");
    }
}
