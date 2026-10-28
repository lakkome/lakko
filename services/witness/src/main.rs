// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-svc-witness` — the independent KT-log witness co-signer (ADR-0020).
//!
//! It co-signs an operator root ONLY after verifying the root is operator-signed
//! (against a PINNED operator key) AND that a Merkle consistency proof shows the
//! root append-only-extends its last-co-signed root. Two divergent forks cannot
//! both extend one root, so the operator cannot obtain a co-signature on a fork
//! — the split-view defence (G-30 #1). For the guarantee to hold the witness
//! MUST run in a different trust domain from the operator (a deploy property).
//!
//! Env:
//! - `LAKKO_BIND` — `host:port` (default `0.0.0.0:8088`).
//! - `LAKKO_WITNESS_DATABASE_URL` / `DATABASE_URL` — MariaDB.
//! - `LAKKO_WITNESS_OPERATOR_PUBKEY` — base64 of the transparency operator's
//!   Ed25519 verifying key (the PIN; required — the witness refuses to boot
//!   without it, since it could otherwise co-sign any root).
//! - `LAKKO_WITNESS_COSIGN_SECRET` — shared secret gating `POST /witness/cosign`.

#![forbid(unsafe_code)]

mod handlers;
mod service_auth;
mod store;
mod witness_key;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_crypto::signing::VerifyingKey;
use serde_json::json;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tokio::signal;
use tower_http::trace::TraceLayer;

use store::WitnessStore;

/// Embedded migrations (compile-time; the build needs no DB).
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Default bind. Distinct from auth (8081), ds (8082), storage (8083),
/// discovery (8084), push-gw (8085), channels (8086), transparency (8087).
const DEFAULT_BIND: &str = "0.0.0.0:8088";

/// Max request body — a root + a small consistency proof is tiny.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Shared application state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: MySqlPool,
    pub(crate) store: Arc<WitnessStore>,
    /// The cosign shared secret, or `None` (the endpoint then serves 503).
    pub(crate) cosign_secret: Option<Arc<Vec<u8>>>,
    /// The PINNED transparency operator verifying key — the witness only
    /// co-signs roots signed by it.
    pub(crate) operator_pubkey: VerifyingKey,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    let _ = dotenvy::dotenv();
    init_tracing();

    let bind: SocketAddr = std::env::var("LAKKO_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_owned())
        .parse()
        .context("parse LAKKO_BIND as host:port")?;

    let db_url = std::env::var("LAKKO_WITNESS_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_WITNESS_DATABASE_URL or DATABASE_URL env var must be set")?;

    // The PINNED operator key — required; the witness refuses to boot without
    // it (a witness that co-signs unauthenticated roots is worse than none).
    let operator_pubkey = load_operator_pin().context("load LAKKO_WITNESS_OPERATOR_PUBKEY")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    let witness = witness_key::load_or_init_witness_key(&pool)
        .await
        .context("load witness signing key")?;
    let store = WitnessStore::load(&pool, witness)
        .await
        .context("load witness cosign state")?;

    let cosign_secret = service_auth::load_cosign_secret()
        .map_err(anyhow::Error::msg)
        .context("load LAKKO_WITNESS_COSIGN_SECRET")?;
    if cosign_secret.is_some() {
        tracing::info!("witness cosign secret configured — /witness/cosign armed");
    } else {
        tracing::warn!(
            "LAKKO_WITNESS_COSIGN_SECRET unset — POST /witness/cosign disabled (serves 503)"
        );
    }

    let state = AppState {
        db: pool.clone(),
        store: Arc::new(store),
        cosign_secret,
        operator_pubkey,
    };

    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "witness service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    pool.close().await;
    tracing::info!("witness service stopped cleanly");
    Ok(())
}

/// Parse the pinned operator verifying key from `LAKKO_WITNESS_OPERATOR_PUBKEY`
/// (base64 of a 32-byte Ed25519 key).
fn load_operator_pin() -> anyhow::Result<VerifyingKey> {
    let raw = std::env::var("LAKKO_WITNESS_OPERATOR_PUBKEY")
        .context("LAKKO_WITNESS_OPERATOR_PUBKEY must be set (the operator key the witness pins)")?;
    let bytes = B64
        .decode(raw.trim().as_bytes())
        .context("LAKKO_WITNESS_OPERATOR_PUBKEY is not valid base64")?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| anyhow::anyhow!("LAKKO_WITNESS_OPERATOR_PUBKEY is not a valid Ed25519 key"))
}

/// Build the router. `POST /witness/cosign` sits behind the cosign-secret
/// middleware; the read paths + probes stay open (public artefacts only).
pub(crate) fn build_app(state: AppState) -> Router {
    use axum::middleware;

    let protected = Router::new()
        .route("/witness/cosign", post(handlers::cosign))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            service_auth::require_cosign_secret,
        ));

    let open = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/witness/key", get(handlers::witness_key));

    open.merge(protected)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
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
        "service": "lakko-svc-witness",
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
