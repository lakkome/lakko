// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS Delivery Service for Lakko.
//!
//! Phase 1.3 — skeleton: append-only MariaDB-backed journals for both
//! per-group fan-out and per-device direct mailboxes.
//! Phase 1.4 — long-poll fanout. GET endpoints accept `?wait=<secs>`;
//! POST handlers wake any in-flight long-polls on the same `(queue,
//! owner_id)` key via an in-process `tokio::sync::Notify` map. Real-time
//! delivery without WebSocket/SSE complexity. Multi-instance fanout
//! (NATS / Redis pub/sub) is Phase 2 horizontal-scaling work; QUIC
//! stream push is Phase 5.
//!
//! The server is **blind**:
//!
//!   * It sees only the group's / device's pseudonym (a 32-byte
//!     identifier the client picks; the server cannot link it to a
//!     real identity).
//!   * Payloads are opaque MLS ciphertext. The server does not parse
//!     them, does not interpret epoch / seq / sender — just stores the
//!     bytes and assigns a per-row monotonic id.
//!   * Fan-out is implicit: clients with the group's pseudonym poll
//!     the journal. The server doesn't know who is in the group.
//!
//! Phase 1.3 had no authentication — anyone with the right pseudonym could
//! publish or fetch, on the ground that a 32-byte random name cannot be
//! guessed. ADR-0053 (`src/access.rs`) ended that where a name travels: a
//! journal its owner has **claimed** answers only a request carrying a proof
//! signed with a key it was claimed under (a per-epoch key every member of a
//! group derives from the MLS exporter; the identity key a mailbox is named
//! after), a failed proof is one uniform 401, and the one act a non-member
//! has - asking to join - is a PoW-gated knock. An unclaimed journal keeps
//! the old posture. Anti-DoS beyond that is the nginx-layer rate limit.
//!
//! Configuration via environment variables (loaded by systemd from
//! `/etc/lakko/lakko.env`):
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8082`)
//! - `DATABASE_URL` — sqlx-style MariaDB DSN
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec
//!
//! Endpoints:
//!
//! - `GET    /health`                              — liveness, 200 OK
//! - `GET    /ready`                               — readiness (DB ping), 200 / 503
//! - `GET    /version`
//! - `POST   /ds/group/{group_id_hex}/messages`    — publish to group (1 MB cap)
//! - `GET    /ds/group/{group_id_hex}/messages`    — `?after=<id>&limit=<n>&wait=<secs>`
//! - `POST   /ds/mailbox/{device_id_hex}/messages` — publish to device mailbox
//! - `GET    /ds/mailbox/{device_id_hex}/messages` — `?after=<id>&limit=<n>&wait=<secs>`
//! - `PUT    /ds/group/{group_id_hex}/access`      — register an epoch's access key (ADR-0053)
//! - `PUT    /ds/mailbox/{device_id_hex}/access`   — claim a mailbox under its identity key
//! - `POST   /ds/group/{group_id_hex}/knock`       — a non-member's request to join (PoW)
//! - `GET    /ds/group/{group_id_hex}/knocks`      — the members read the knocks
//! - `POST   /pow/challenge`                       — a proof-of-work challenge for a knock
//!
//! See `~/.claude/plans/1-nested-wreath.md` §4 (MLS Delivery Service).

#![forbid(unsafe_code)]

mod access;
mod journal;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post, put};
use axum::Router;
use serde_json::json;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tokio::signal;
use tower_http::trace::TraceLayer;

/// Default bind address if `LAKKO_BIND` is not set. Distinct from auth
/// (8081) and storage (8083).
const DEFAULT_BIND: &str = "127.0.0.1:8082";

/// Migrations directory (relative to crate root). `sqlx::migrate!`
/// reads it at compile time and embeds the SQL in the binary.
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Shared application state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: MySqlPool,
    /// Per-pseudonym notifiers for long-poll fanout. POST handlers call
    /// `notify_waiters()` for the matching key; GET handlers with
    /// `?wait=<secs>` register on `Notify::notified()` and wake up.
    /// In-process only — multi-instance fanout (NATS / Redis pub/sub)
    /// is Phase 2 horizontal-scaling work.
    pub(crate) notifiers: journal::Notifiers,
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

    // Per-service database — see `services/auth/src/main.rs` for the
    // rationale. DS reads its own `LAKKO_DS_DATABASE_URL` first.
    let db_url = std::env::var("LAKKO_DS_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_DS_DATABASE_URL or DATABASE_URL env var must be set")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(32)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    let state = AppState {
        db: pool.clone(),
        notifiers: journal::Notifiers::default(),
    };

    let _pruner = journal::spawn_pruner(pool.clone());
    let _pow_pruner = lakko_pow_server::spawn_pruner(pool.clone());

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "ds service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    pool.close().await;
    tracing::info!("ds service stopped cleanly");
    Ok(())
}

/// Build the public router. Extracted so tests can drive it directly
/// without binding a TCP listener.
pub(crate) fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route(
            "/ds/group/{group_hex}/messages",
            post(journal::post_group).get(journal::get_group),
        )
        .route(
            "/ds/mailbox/{device_hex}/messages",
            post(journal::post_mailbox).get(journal::get_mailbox),
        )
        .route(
            "/ds/group/{group_hex}/access",
            put(access::put_group_access),
        )
        .route(
            "/ds/mailbox/{device_hex}/access",
            put(access::put_mailbox_access),
        )
        .route("/ds/group/{group_hex}/knock", post(access::post_knock))
        .route("/ds/group/{group_hex}/knocks", get(access::get_knocks))
        .route("/pow/challenge", post(access::issue_pow_challenge))
        .layer(DefaultBodyLimit::max(journal::MAX_MESSAGE_BYTES))
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
        "service": "lakko-svc-ds",
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
        assert_eq!(parsed["service"], "lakko-svc-ds");
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let response = test_app_no_db()
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
