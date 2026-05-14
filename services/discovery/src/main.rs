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
//! ## Phase 1.5b — auth-token gating
//!
//! `services/auth` issues HMAC-SHA256 bearer tokens on every
//! `*/finish` endpoint; this service verifies them on every
//! mutating write via `auth::require_session` middleware. Token
//! payload carries the authenticated `account_id`; handlers that
//! bind state to a specific account additionally check the body's
//! `account_id` matches what the token claims (anti-impersonation
//! when one account tries to register state under another's id).
//!
//! Read paths (`/discovery/oprf`, `/discovery/lookup`,
//! `/discovery/identity/{}`, `/discovery/keypackage/{}`,
//! `/discovery/invite/{}`) stay open — they take no account-bound
//! input and a leaked token would not let the attacker do
//! anything they couldn't do by knowing the public account_id or
//! blob_id anyway.
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
//! - `LAKKO_SESSION_TOKEN_SECRET` — base64-encoded HMAC-SHA256 key
//!   shared with `services/auth`. **Required** — middleware
//!   fails fast at startup if missing or shorter than 32 bytes
//!   after decode.
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
//! - `POST   /discovery/keypackage`         — upsert public MLS KeyPackage (Phase 1.16e1)
//! - `GET    /discovery/keypackage/{acct}`  — fetch public MLS KeyPackage (Phase 1.16e1)
//! - `POST   /discovery/invite`             — upload InviteCapability blob (Phase 1.18d1)
//! - `GET    /discovery/invite/{blob_id}`   — fetch InviteCapability blob (Phase 1.18d1)
//! - `POST   /discovery/invite/{blob_id}/consume` — atomic max_uses bump (Phase 1.18d2a)
//! - `DELETE /discovery/invite/{blob_id}`           — owner-signed revocation (Phase 1.18d2b)

#![forbid(unsafe_code)]

pub(crate) mod auth;
mod identity_bundle;
mod invite;
mod keypackage;
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
use lakko_auth_token::TokenSecret;
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
    /// Phase 1.5b: shared HMAC secret for session-token verification.
    /// Same value as `services/auth`'s `state.session_token_secret`;
    /// the auth-token crate's `verify` consumes this on every
    /// protected request via the `auth::require_session` middleware.
    pub(crate) session_token_secret: Arc<TokenSecret>,
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

    // Phase 1.5b: session-token verifier. Same env var name +
    // shape as `services/auth` — operator-side parity matters so
    // a deploy step can write the same value to /etc/lakko/lakko.env
    // once. Failure here is fail-fast at startup; an unconfigured
    // discovery service would otherwise reject every authenticated
    // write with a cryptic 401.
    let session_token_secret_raw = std::env::var("LAKKO_SESSION_TOKEN_SECRET")
        .context("LAKKO_SESSION_TOKEN_SECRET env var must be set (base64-encoded HMAC-SHA256 key, ≥ 32 bytes after decode)")?;
    let session_token_secret = TokenSecret::from_base64(&session_token_secret_raw)
        .context("parse LAKKO_SESSION_TOKEN_SECRET as base64")?;

    let state = AppState {
        db: pool.clone(),
        oprf: Arc::new(oprf_server),
        session_token_secret: Arc::new(session_token_secret),
    };

    let _pow_pruner = pow::spawn_pruner(pool.clone());
    // Phase 1.18d2c — hourly sweep of expired invite blobs.
    // The `idx_expiry` index from migration 0004 keeps the
    // recurring DELETE cheap; `expiry_ms = 0` (NEVER_EXPIRES)
    // rows are preserved by design — the codec layer surfaces
    // the stalking-risk warning, the storage layer doesn't
    // second-guess.
    let _invite_pruner = invite::spawn_pruner(pool.clone());

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
///
/// Phase 1.5b: protected routes (state-mutating writes that bind
/// to a specific account_id) sit behind the `require_session`
/// middleware. Read paths + `/pow/challenge` + `/discovery/oprf`
/// (which takes no account_id and is cheap-by-PoW) stay open.
/// `invite::consume` is also open — anyone holding the URL can
/// already exercise the consume counter, and the joiner doesn't
/// necessarily have an auth token yet (a fresh-install joiner
/// taps a Universal Link before they've signed up).
pub(crate) fn build_app(state: AppState) -> Router {
    use axum::middleware;

    // Protected routes — every write that binds to a specific
    // account_id requires a valid session token. The handler is
    // additionally responsible for checking the body's account_id
    // matches the token's (anti-impersonation).
    let protected = Router::new()
        .route("/discovery/register", post(registry::register))
        .route("/discovery/identity", post(identity_bundle::upload))
        .route("/discovery/keypackage", post(keypackage::upload))
        .route("/discovery/invite", post(invite::upload))
        // `DELETE /discovery/invite/{blob_id}` is owner-signed via
        // its body, but we ALSO require a session token: the
        // signature proves "owner of the capability", the token
        // proves "logged-in Lakko user", and the union is strictly
        // stronger than the signature alone (rate-limits a stolen
        // signing seed to one account).
        .route(
            "/discovery/invite/{blob_id}",
            axum::routing::delete(invite::revoke),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));

    // Open routes — no auth required.
    let open = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/pow/challenge", post(pow::issue_challenge))
        .route("/discovery/oprf", post(oprf::evaluate))
        .route("/discovery/lookup", post(registry::lookup))
        .route(
            "/discovery/identity/{account_id}",
            get(identity_bundle::fetch),
        )
        .route("/discovery/keypackage/{account_id}", get(keypackage::fetch))
        // Phase 1.18d1 — capability-based invite-link blob fetch +
        // consume. Both are unauthenticated by design: a joiner
        // tapping a Universal Link on a fresh install may not be
        // signed up yet.
        .route("/discovery/invite/{blob_id}", get(invite::fetch))
        .route(
            "/discovery/invite/{blob_id}/consume",
            post(invite::consume),
        );

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
