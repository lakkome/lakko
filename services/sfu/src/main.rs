// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Lakko SFU — Selective Forwarding Unit for group calls.
//!
//! This is the skeleton sprint (`S-3-B-skeleton`) of
//! [ADR-0011](../../docs/adr/0011-rust-sfu.md). It boots the
//! HTTP / WebSocket host, the metrics exporter, the JWKS
//! cache + verifier, and the peer registry. Real media
//! routing (track fanout, simulcast layer selection, TWCC)
//! lands in subsequent sprints on top of this scaffold.
//!
//! Configuration is read from environment variables — see
//! [`config::Config`] for the full list. In debug builds a
//! local `.env` file is honoured.
//!
//! Endpoints:
//!
//! | Path | Purpose |
//! |---|---|
//! | `GET /health` | Liveness — always 200 once boot is past the JWKS fetch |
//! | `GET /ready` | Readiness — 200 if JWKS cache is fresh, 503 otherwise |
//! | `GET /version` | `{service,version,git_sha}` |
//! | `GET /v1/connect?token=…` | WebSocket upgrade — see [`signaling`] |
//!
//! The metrics endpoint lives on a separate socket bound by
//! `LAKKO_SFU_METRICS_BIND` so nginx can mTLS-gate the
//! Prometheus scrape independently of the signaling path.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod jwks;
mod peer;
mod prom;
mod protocol;
mod router;
mod signaling;

use std::time::Duration;

use axum::{response::IntoResponse, routing::get, Json, Router};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::config::Config;
use crate::jwks::JwksCache;
use crate::peer::PeerRegistry;
use crate::prom::MetricsExporter;
use crate::signaling::AppState;

/// Service version pulled from the crate manifest at compile
/// time. Surfaces via `/version`.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Threshold past which `/ready` flips from 200 to 503 — the
/// JWKS cache is considered stale if it has not refreshed in
/// twice the configured cadence. The cadence default is 5 min
/// (see [`config`]); this means a hard 10 min before readiness
/// drops, which is well below any realistic external
/// monitoring sensitivity.
const JWKS_STALE_THRESHOLD_MULTIPLIER: u32 = 2;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    dotenv_in_debug();

    let cfg = Config::from_env()?;
    info!(
        signaling_bind = %cfg.signaling_bind,
        metrics_bind = %cfg.metrics_bind,
        jwks_url = %cfg.jwks_url,
        "lakko-svc-sfu starting (v{})",
        VERSION
    );

    // Install Prometheus recorder BEFORE anything else can
    // emit a metric — `metrics::counter!` calls before
    // recorder install are silently lost.
    let exporter = MetricsExporter::install()?;

    // Fail-fast on the first JWKS fetch.
    let jwks = JwksCache::fetch_initial(&cfg).await?;
    let _refresher_handle = jwks.clone().spawn_refresher(cfg.clone());

    let peers = PeerRegistry::new();
    let state = AppState::new(jwks.clone(), peers.clone());

    // Background task: keep `lakko_sfu_jwks_cache_age_seconds`
    // gauge fresh so a stale auth service surfaces in
    // Prometheus before /ready degrades.
    {
        let jwks = jwks.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                let age = jwks.age().await.as_secs_f64();
                metrics::gauge!(crate::prom::names::JWKS_CACHE_AGE_SECS).set(age);
            }
        });
    }

    // Signaling-side HTTP server. Two-socket layout — the
    // metrics exporter binds separately below.
    let app = Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route(
            "/ready",
            get({
                let cfg = cfg.clone();
                let jwks = jwks.clone();
                move || async move { ready(cfg, jwks).await }
            }),
        )
        .route("/v1/connect", get(signaling::ws_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let signaling_listener = tokio::net::TcpListener::bind(cfg.signaling_bind).await?;
    let signaling = tokio::spawn(async move {
        axum::serve(signaling_listener, app).await
    });

    // Metrics exporter — separate socket.
    let metrics_exporter = exporter.clone();
    let metrics_bind = cfg.metrics_bind;
    let metrics_task = tokio::spawn(async move {
        if let Err(e) = crate::prom::serve(metrics_exporter, metrics_bind).await {
            warn!(error = %e, "metrics: server exited with error");
        }
    });

    // Graceful shutdown — any of (Ctrl-C, SIGTERM, child task
    // exit) triggers a cooperative drain. tokio::select! is
    // the canonical pattern.
    tokio::select! {
        () = shutdown_signal() => info!("shutdown: signal received"),
        res = signaling => {
            if let Err(e) = res {
                warn!(error = %e, "signaling: task panicked");
            }
        },
        res = metrics_task => {
            if let Err(e) = res {
                warn!(error = %e, "metrics: task panicked");
            }
        },
    }

    info!("lakko-svc-sfu shutting down");
    Ok(())
}

/// `GET /health` — liveness. Always returns 200 once boot
/// has reached the router (which means the JWKS fetch
/// succeeded). The handler does not touch the registry or
/// the cache; it is the canonical "the process is alive"
/// probe systemd + nginx use.
async fn health() -> &'static str {
    "ok"
}

/// `GET /version` — service identity. Returned as JSON for
/// consistency with the other services' `/version` shape.
async fn version() -> impl IntoResponse {
    Json(serde_json::json!({
        "service": "lakko-svc-sfu",
        "version": VERSION,
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

/// `GET /ready` — JWKS cache must be fresh.
async fn ready(cfg: Config, jwks: JwksCache) -> impl IntoResponse {
    let age = jwks.age().await;
    let threshold = cfg.jwks_refresh * JWKS_STALE_THRESHOLD_MULTIPLIER;
    if age > threshold {
        warn!(
            age_secs = age.as_secs(),
            threshold_secs = threshold.as_secs(),
            "ready: JWKS cache stale"
        );
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "jwks stale");
    }
    (axum::http::StatusCode::OK, "ready")
}

/// Initialise the global `tracing` subscriber. The filter
/// honours the standard `RUST_LOG` env var; if unset, the
/// SFU defaults to `info` so production logs are useful by
/// default but not noisy.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,lakko_svc_sfu=info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).compact())
        .init();
}

/// Honour a local `.env` file in debug builds only. Production
/// uses systemd `EnvironmentFile=/etc/lakko/lakko.env`; running
/// `dotenvy` in release would silently mask production
/// configuration with stale developer values.
fn dotenv_in_debug() {
    #[cfg(debug_assertions)]
    {
        let _ = dotenvy::dotenv();
    }
}

/// Cooperative shutdown trigger — Ctrl-C on every platform
/// plus SIGTERM on Unix (the signal systemd sends).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!(error = %e, "shutdown: ctrl_c handler failed");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        let mut term = match tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "shutdown: SIGTERM handler init failed");
                return;
            },
        };
        let _ = term.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
