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
//!   Ed25519 verifying key (the PIN for the KT log; required — the witness
//!   refuses to boot without it, since it could otherwise co-sign any root).
//! - `LAKKO_WITNESS_MODERATION_OPERATOR_PUBKEY` — base64 of the moderation
//!   log's operator key (ADR-0098 section 5; optional — unset, a moderation
//!   cosign answers 404 `unknown-log`).
//! - `LAKKO_WITNESS_COSIGN_SECRET` — shared secret gating `POST /witness/cosign`.

#![forbid(unsafe_code)]

mod handlers;
mod metrics;
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
    /// co-signs KT roots signed by it.
    pub(crate) operator_pubkey: VerifyingKey,
    /// The PINNED moderation-log operator key, or `None` (the witness then
    /// co-signs no moderation root).
    pub(crate) moderation_operator_pubkey: Option<VerifyingKey>,
}

impl AppState {
    /// The pinned operator key for `log`, or `None` when this witness does not
    /// witness that log.
    pub(crate) fn operator_for(&self, log: lakko_transparency::LogId) -> Option<VerifyingKey> {
        match log {
            lakko_transparency::LogId::Kt => Some(self.operator_pubkey),
            lakko_transparency::LogId::Moderation => self.moderation_operator_pubkey,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    let _ = dotenvy::dotenv();
    init_tracing();
    // /metrics on its own localhost port, off when unset (ADR-0096).
    lakko_metrics::start_from_env(
        "witness",
        env!("CARGO_PKG_VERSION"),
        "LAKKO_WITNESS_METRICS_BIND",
    )
    .await?;

    let bind: SocketAddr = std::env::var("LAKKO_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_owned())
        .parse()
        .context("parse LAKKO_BIND as host:port")?;

    let db_url = std::env::var("LAKKO_WITNESS_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_WITNESS_DATABASE_URL or DATABASE_URL env var must be set")?;

    // The PINNED operator key — required; the witness refuses to boot without
    // it (a witness that co-signs unauthenticated roots is worse than none).
    let operator_pubkey = load_operator_pin("LAKKO_WITNESS_OPERATOR_PUBKEY")?
        .context("LAKKO_WITNESS_OPERATOR_PUBKEY must be set (the operator key the witness pins)")?;
    let moderation_operator_pubkey = load_operator_pin("LAKKO_WITNESS_MODERATION_OPERATOR_PUBKEY")?;
    if moderation_operator_pubkey.is_some() {
        tracing::info!("moderation operator key pinned — the moderation log is witnessed");
    } else {
        tracing::warn!(
            "LAKKO_WITNESS_MODERATION_OPERATOR_PUBKEY unset — moderation cosigns answer 404"
        );
    }

    let pool = MySqlPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;
    // Connections in use and idle (ADR-0096).
    let pool_for_gauge = pool.clone();
    let _pool_gauge =
        lakko_metrics::spawn_pool_gauge(move || (pool_for_gauge.size(), pool_for_gauge.num_idle()));

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
        moderation_operator_pubkey,
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

/// Parse a pinned operator verifying key (base64 of a 32-byte Ed25519 key)
/// from `var`; `Ok(None)` when the variable is unset or empty.
fn load_operator_pin(var: &str) -> anyhow::Result<Option<VerifyingKey>> {
    let Ok(raw) = std::env::var(var) else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let bytes = B64
        .decode(raw.as_bytes())
        .with_context(|| format!("{var} is not valid base64"))?;
    VerifyingKey::from_bytes(&bytes)
        .map(Some)
        .map_err(|_| anyhow::anyhow!("{var} is not a valid Ed25519 key"))
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
        // Requests by route template, method and status class (ADR-0096).
        .layer(lakko_metrics::red_layer("witness"))
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

#[cfg(test)]
mod route_tests {
    //! Request-shape tests for `POST /witness/cosign` that never reach the
    //! database: the `log` field is checked before any storage access.

    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use lakko_crypto::signing::SigningKey;
    use tower::ServiceExt as _;

    const SECRET: &[u8] = b"0123456789abcdef0123";

    fn app(moderation_operator: bool) -> Router {
        // A lazy pool never connects; these requests are refused before any
        // storage access.
        let db = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://nobody@127.0.0.1:1/none")
            .expect("lazy pool");
        let operator = SigningKey::from_seed(&[0x77; 32]).verifying_key();
        build_app(AppState {
            db,
            store: Arc::new(WitnessStore::empty_for_tests(SigningKey::from_seed(
                &[0x88; 32],
            ))),
            cosign_secret: Some(Arc::new(SECRET.to_vec())),
            operator_pubkey: operator,
            moderation_operator_pubkey: moderation_operator.then_some(operator),
        })
    }

    async fn post_with_body(app: Router, body: serde_json::Value) -> (StatusCode, String) {
        use http_body_util::BodyExt as _;
        let req = Request::post("/witness/cosign")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", B64.encode(SECRET)))
            .body(Body::from(body.to_string()))
            .expect("request");
        let res = app.oneshot(req).await.expect("response");
        let status = res.status();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn post(app: Router, body: serde_json::Value) -> StatusCode {
        post_with_body(app, body).await.0
    }

    fn root() -> serde_json::Value {
        json!({
            "tree_root_b64": B64.encode([0u8; 32]),
            "log_size": 0,
            "timestamp_unix": 0,
            "signature_b64": B64.encode([0u8; 64]),
        })
    }

    #[tokio::test]
    async fn missing_log_is_400() {
        let status = post(app(true), json!({ "new_root": root() })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_log_is_400() {
        for log in ["", "KT", "tree", "moderation-log"] {
            let status = post(app(true), json!({ "log": log, "new_root": root() })).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "log {log:?}");
        }
    }

    #[tokio::test]
    async fn moderation_without_an_operator_key_is_404() {
        let status = post(
            app(false),
            json!({ "log": "moderation", "new_root": root() }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_known_log_reaches_the_operator_check() {
        // An all-zero signature fails the operator check (400 with a different
        // body) before any storage access, showing the log was accepted.
        for log in ["kt", "moderation"] {
            let (status, body) =
                post_with_body(app(true), json!({ "log": log, "new_root": root() })).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "log {log:?}");
            assert_eq!(body, "operator signature is invalid", "log {log:?}");
        }
    }
}
