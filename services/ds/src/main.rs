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
//! has - asking to join - is a knock that spends one anonymous token
//! (ADR-0054 S3, context `ds-knock`, header `x-lakko-anon-token`). An
//! unclaimed journal keeps the old posture. Anti-DoS beyond that is the nginx-layer rate limit.
//!
//! Configuration via environment variables (loaded by systemd from
//! `/etc/lakko/lakko.env`):
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8082`)
//! - `DATABASE_URL` — sqlx-style MariaDB DSN
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec
//! - `LAKKO_DS_PUSH_GW_URL` + `LAKKO_DS_PUSH_GW_SECRET` — push-gw's internal
//!   listener (e.g. `http://127.0.0.1:8092`) and the shared secret it checks
//!   (base64, >= 16 bytes; the same value as push-gw's
//!   `LAKKO_PUSH_GW_INTERNAL_SECRET`). ADR-0054: a signed group publish that
//!   carries `x-lakko-wake` is forwarded there; since ADR-0078 the header is
//!   a wake sealed to push-gw, forwarded unread. Both unset: the header is
//!   still validated but every wake is dropped (one startup warning). Only
//!   one set: the service refuses to start.
//! - `LAKKO_ANON_TOKEN_MASTER_SECRET` — 64 hex characters, the SAME value
//!   `services/auth` issues under (ADR-0054). Unset: the knock answers 503
//!   `anon-tokens-disabled` (loud startup warning). Malformed: startup fails.
//! - `LAKKO_ANON_TOKEN_WINDOW_SECS` — the token epoch window (default 86400);
//!   must match auth.
//! - `LAKKO_DS_WAKE_BURST` / `LAKKO_DS_WAKE_PER_MINUTE` — the wake cap per
//!   journal (defaults 30 and 60; see `src/wake.rs` for why). Over the cap
//!   the message is still appended, the wake is dropped. The per-`push_gid`
//!   cap moved to push-gw with ADR-0078: the DS no longer sees a `push_gid`.
//!   `LAKKO_DS_BINDING_PEPPER` and the push bindings are gone with it.
//!
//! Endpoints:
//!
//! - `GET    /health`                              — liveness, 200 OK
//! - `GET    /ready`                               — readiness (DB ping), 200 / 503
//! - `GET    /version`
//! - `POST   /ds/group/{group_id_hex}/messages`    — publish to group (1 MB cap;
//!   optional `x-lakko-wake: <a wake sealed to push-gw, b64url>`, ADR-0054,
//!   ADR-0078)
//! - `GET    /ds/group/{group_id_hex}/messages`    — `?after=<id>&limit=<n>&wait=<secs>`
//! - `POST   /ds/mailbox/{mailbox_hex}/messages`   — append a sealed, bucket-padded row
//!   to a personal mailbox (ADR-0078; a claimed, closed mailbox takes a write
//!   only under a reach key or a delegation, ADR-0060; an open or unclaimed
//!   one spends a `mailbox-write` token; see `src/reach.rs`)
//! - `GET    /ds/mailbox/{mailbox_hex}/messages`   — `?after=<id>&limit=<n>&wait=<secs>`
//!   (personal: the mailbox-key proof, whether or not the mailbox is claimed,
//!   ADR-0060)
//! - `POST   /ds/rendezvous/{id_hex}/messages`     — append to a rendezvous
//!   (pairing, recovery, courier, state bundle): no proof, never claimable,
//!   its own table, so no rendezvous id names a mailbox's rows (ADR-0060)
//! - `GET    /ds/rendezvous/{id_hex}/messages`     — `?after=<id>&limit=<n>&wait=<secs>`, no proof
//! - `PUT    /ds/group/{group_id_hex}/claim`       — first claim, by the journal's root (ADR-0057)
//! - `PUT    /ds/group/{group_id_hex}/reset`       — the root replaces the epoch chain
//! - `PUT    /ds/group/{group_id_hex}/access`      — a registrar registers an epoch's access key
//! - `POST   /ds/group/{group_id_hex}/commit`      — register the next epoch and append the commit
//!   row that reaches it, in one transaction (ADR-0081; `src/commit.rs`)
//! - `PUT    /ds/group/{group_id_hex}/registrars`  — the root manages registrars
//! - `PUT    /ds/group/{group_id_hex}/root`        — the root names a new root
//! - `PUT    /ds/mailbox/{mailbox_hex}/access`     — claim a mailbox under its mailbox key (ADR-0078)
//! - `PUT    /ds/mailbox/{mailbox_hex}/reach`      — register / remove reach keys, set open (ADR-0060)
//! - `GET    /ds/mailbox/{mailbox_hex}/reach`      — the owner reads its reach keys back (ADR-0060)
//! - `POST   /ds/mailbox/{mailbox_hex}/reach/check` — is this reach key registered? (ADR-0060)
//! - `POST   /ds/group/{group_id_hex}/knock`       — a non-member's request to join (token)
//! - `GET    /ds/group/{group_id_hex}/knocks`      — the members read the knocks
//! - `DELETE /ds/group/{group_id_hex}/knocks`      — an admin clears the knocks, `?through=` (write
//!   tier, ADR-0087 section 5)
//! - `PUT    /ds/directory/{id_hex}/claim`         — claim a circle directory under its owner key
//!   (ADR-0086; `src/directory.rs`; spends a `circle-claim` token)
//! - `POST   /ds/directory/{id_hex}/write`         — the owner's one write: head, edges, chain,
//!   filter, rows, and the next read key when the epoch moves
//! - `GET    /ds/directory/{id_hex}/header`        — public: the head, `?nodes=` the path, `?chain_from=`
//! - `GET    /ds/directory/{id_hex}/filter`        — the filter, under the current read key
//! - `GET    /ds/directory/{id_hex}/rows/{index}`  — one row, under the current read key
//! - `DELETE /ds/directory/{id_hex}`               — the owner deletes the directory
//!
//! See `~/.claude/plans/1-nested-wreath.md` §4 (MLS Delivery Service).

#![forbid(unsafe_code)]

mod access;
mod commit;
mod directory;
mod journal;
mod ownership;
mod reach;
#[cfg(test)]
mod test_support;

#[cfg(test)]
mod log_capture_tests;
mod wake;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post, put};
use axum::Router;
use lakko_anon_token_server::{AnonTokenServerConfig, TokenVerifier};
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
    /// ADR-0054 - the wake caps and the forwarder to push-gw.
    pub(crate) waker: Arc<wake::Waker>,
    /// ADR-0054 S3 - the anonymous-token verifier the knock spends through,
    /// over this service's spent-set (`anon_token_spent`). Dormant (the knock
    /// answers 503) when `LAKKO_ANON_TOKEN_MASTER_SECRET` is unset.
    pub(crate) anon_tokens: TokenVerifier,
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

    // Read the wake configuration before touching the database, so a
    // half-configured forward fails the start loudly and at once.
    let wake_config = wake::WakeConfig::from_env()
        .map_err(anyhow::Error::msg)
        .context("wake configuration")?;
    let waker = wake::Waker::new(&wake_config)
        .map_err(anyhow::Error::msg)
        .context("wake forwarder")?;
    if waker.is_active() {
        tracing::info!(
            burst = wake_config.cap.burst,
            per_minute = wake_config.cap.per_minute,
            "wakes are forwarded to push-gw"
        );
    } else {
        tracing::warn!(
            "LAKKO_DS_PUSH_GW_URL / LAKKO_DS_PUSH_GW_SECRET unset - x-lakko-wake is \
             validated but no wake is forwarded; devices are not woken by a publish"
        );
    }

    let pool = MySqlPoolOptions::new()
        .max_connections(32)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    // ADR-0054 S3 - a malformed secret or window refuses startup; an unset
    // secret leaves the knock answering 503.
    let anon_tokens = TokenVerifier::new(
        pool.clone(),
        AnonTokenServerConfig::from_env().context("anonymous-token configuration")?,
    );
    anon_tokens.log_startup("ds");

    let state = AppState {
        db: pool.clone(),
        notifiers: journal::Notifiers::default(),
        waker: Arc::new(waker),
        anon_tokens,
    };

    let _pruner = journal::spawn_pruner(pool.clone());
    let _spent_pruner = state.anon_tokens.spawn_pruner();

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
        .route("/ds/group/{group_hex}/claim", put(ownership::put_claim))
        .route("/ds/group/{group_hex}/reset", put(ownership::put_reset))
        .route("/ds/group/{group_hex}/access", put(ownership::put_register))
        .route("/ds/group/{group_hex}/commit", post(commit::post_commit))
        .route(
            "/ds/group/{group_hex}/registrars",
            put(ownership::put_registrars),
        )
        .route(
            "/ds/group/{group_hex}/registrars-set",
            put(ownership::put_registrars_set),
        )
        .route("/ds/group/{group_hex}/root", put(ownership::put_root))
        .route(
            "/ds/rendezvous/{rendezvous_hex}/messages",
            post(journal::post_rendezvous).get(journal::get_rendezvous),
        )
        .route(
            "/ds/mailbox/{device_hex}/access",
            put(access::put_mailbox_access),
        )
        .route(
            "/ds/mailbox/{device_hex}/reach",
            put(reach::put_mailbox_reach).get(reach::get_mailbox_reach),
        )
        .route(
            "/ds/mailbox/{device_hex}/reach/check",
            post(reach::post_reach_check),
        )
        .route("/ds/group/{group_hex}/knock", post(access::post_knock))
        .route(
            "/ds/group/{group_hex}/knocks",
            get(access::get_knocks).delete(access::delete_knocks),
        )
        .route(
            "/ds/directory/{directory_hex}/claim",
            put(directory::put_claim).layer(DefaultBodyLimit::max(directory::MAX_WRITE_BODY)),
        )
        .route(
            "/ds/directory/{directory_hex}/write",
            post(directory::post_write).layer(DefaultBodyLimit::max(directory::MAX_WRITE_BODY)),
        )
        .route(
            "/ds/directory/{directory_hex}/header",
            get(directory::get_header),
        )
        .route(
            "/ds/directory/{directory_hex}/filter",
            get(directory::get_filter),
        )
        .route(
            "/ds/directory/{directory_hex}/rows/{index}",
            get(directory::get_row),
        )
        .route(
            "/ds/directory/{directory_hex}",
            axum::routing::delete(directory::delete_directory),
        )
        .layer(DefaultBodyLimit::max(journal::MAX_MESSAGE_BYTES))
        // The request span names the method and the route template, never
        // the URI: a path carries a mailbox address or a journal id, which a
        // log line must not (ADR-0078).
        .layer(TraceLayer::new_for_http().make_span_with(lakko_log_hygiene::request_span))
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
