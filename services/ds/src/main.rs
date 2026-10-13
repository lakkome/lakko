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
//!   carries `x-lakko-wake` is forwarded there. Both unset: the header is
//!   still validated but every wake is dropped (one startup warning). Only
//!   one set: the service refuses to start.
//! - `LAKKO_ANON_TOKEN_MASTER_SECRET` — 64 hex characters, the SAME value
//!   `services/auth` issues under (ADR-0054). Unset: the knock answers 503
//!   `anon-tokens-disabled` (loud startup warning). Malformed: startup fails.
//! - `LAKKO_ANON_TOKEN_WINDOW_SECS` — the token epoch window (default 86400);
//!   must match auth.
//! - `LAKKO_DS_WAKE_BURST` / `LAKKO_DS_WAKE_PER_MINUTE` — the wake cap, per
//!   journal and per woken `push_gid` (defaults 30 and 60; see
//!   `src/wake.rs` for why). Over the cap the message is still appended, the
//!   wake is dropped.
//! - `LAKKO_DS_BINDING_PEPPER` — 64 hex characters, server-only (ADR-0057):
//!   push bindings are stored as keyed-BLAKE3 of the `push_gid` under it, and
//!   a wake goes out only for the `push_gid` bound to the publishing journal.
//!   Unset: `PUT .../push-binding` answers 503 `push-binding-disabled` and
//!   every wake is dropped (startup warning). Malformed: startup fails.
//! - `LAKKO_DS_ATTEST_KEY` — base64 of auth's 32-byte Ed25519 attest
//!   verifying key (ADR-0073): a key-reset attestation carried by
//!   `PUT .../reset` is verified under it, letting the device that holds an
//!   account's NEW seed re-root the DeviceGroup journal the old seed owned.
//!   Unset: that arm answers 503 `key-reset-disabled` (startup warning); the
//!   plain root reset is unaffected. Malformed: startup fails.
//!
//! Endpoints:
//!
//! - `GET    /health`                              — liveness, 200 OK
//! - `GET    /ready`                               — readiness (DB ping), 200 / 503
//! - `GET    /version`
//! - `POST   /ds/group/{group_id_hex}/messages`    — publish to group (1 MB cap;
//!   optional `x-lakko-wake: <push_gid b64url>;<message|call>`, ADR-0054)
//! - `GET    /ds/group/{group_id_hex}/messages`    — `?after=<id>&limit=<n>&wait=<secs>`
//! - `POST   /ds/mailbox/{device_id_hex}/messages` — publish to device mailbox
//!   (a claimed, closed mailbox takes a write only under a reach key or a
//!   delegation, ADR-0060; see `src/reach.rs`)
//! - `GET    /ds/mailbox/{device_id_hex}/messages` — `?after=<id>&limit=<n>&wait=<secs>`
//!   (personal: the identity proof, whether or not the mailbox is claimed,
//!   ADR-0060)
//! - `POST   /ds/rendezvous/{id_hex}/messages`     — append to a rendezvous
//!   (pairing, recovery, courier, state bundle): no proof, never claimable,
//!   its own table, so no rendezvous id names a mailbox's rows (ADR-0060)
//! - `GET    /ds/rendezvous/{id_hex}/messages`     — `?after=<id>&limit=<n>&wait=<secs>`, no proof
//! - `PUT    /ds/group/{group_id_hex}/claim`       — first claim, by the journal's root (ADR-0057)
//! - `PUT    /ds/group/{group_id_hex}/reset`       — the root replaces the epoch chain; with
//!   `key_reset_attest_b64`, any signer re-roots the journal under a verified key-reset
//!   attestation of a newer generation (ADR-0073)
//! - `PUT    /ds/group/{group_id_hex}/access`      — a registrar registers an epoch's access key
//! - `PUT    /ds/group/{group_id_hex}/registrars`  — the root manages registrars
//! - `PUT    /ds/group/{group_id_hex}/root`        — the root names a new root
//! - `PUT    /ds/group/{group_id_hex}/push-binding` — root or admin binds one `push_gid`
//! - `PUT    /ds/mailbox/{device_id_hex}/access`   — claim a mailbox under its identity key
//! - `PUT    /ds/mailbox/{device_id_hex}/reach`    — register / remove reach keys, set open (ADR-0060)
//! - `GET    /ds/mailbox/{device_id_hex}/reach`    — the owner reads its reach keys back (ADR-0060)
//! - `POST   /ds/mailbox/{device_id_hex}/reach/check` — is this reach key registered? (ADR-0060)
//! - `POST   /ds/group/{group_id_hex}/knock`       — a non-member's request to join (token)
//! - `GET    /ds/group/{group_id_hex}/knocks`      — the members read the knocks
//!
//! See `~/.claude/plans/1-nested-wreath.md` §4 (MLS Delivery Service).

#![forbid(unsafe_code)]

mod access;
mod journal;
mod key_reset;
mod ownership;
mod reach;
#[cfg(test)]
mod test_support;
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
    /// ADR-0057 - the pepper push bindings are keyed under
    /// (`LAKKO_DS_BINDING_PEPPER`). `None`: the binding route answers 503
    /// and every wake is dropped.
    pub(crate) binding_pepper: Option<[u8; 32]>,
    /// ADR-0073 - the auth attest VERIFYING key pinned from
    /// `LAKKO_DS_ATTEST_KEY`, which a key-reset attestation on
    /// `PUT .../reset` is verified under. `None`: that arm answers 503
    /// `key-reset-disabled`; the plain root reset is unaffected.
    pub(crate) attest_key: Option<lakko_ds_proof::VerifyingKey>,
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

    let binding_pepper = ownership::pepper_from_lookup(|name| std::env::var(name).ok())
        .map_err(anyhow::Error::msg)
        .context("push binding pepper")?;
    if binding_pepper.is_none() {
        tracing::warn!(
            "LAKKO_DS_BINDING_PEPPER unset - push bindings are refused (503) and every \
             x-lakko-wake is dropped; devices are not woken by a publish"
        );
    }

    let attest_key = key_reset::attest_key_from_lookup(|name| std::env::var(name).ok())
        .map_err(anyhow::Error::msg)
        .context("key-reset attest key")?;
    if attest_key.is_none() {
        tracing::warn!(
            "LAKKO_DS_ATTEST_KEY unset - an attested journal reset (ADR-0073) answers 503 \
             key-reset-disabled; an account that reset its keys cannot re-root its DeviceGroup"
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
        binding_pepper,
        attest_key,
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
            "/ds/group/{group_hex}/push-binding",
            put(ownership::put_push_binding),
        )
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
        .route("/ds/group/{group_hex}/knocks", get(access::get_knocks))
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
