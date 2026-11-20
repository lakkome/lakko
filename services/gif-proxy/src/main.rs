// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! GIF search and media proxy for Lakko (ADR-0047 §4).
//!
//! A stateless service between signed-in clients and GIPHY. Clients search and
//! browse trending GIFs through it and load every preview and full-size file
//! through it, so GIPHY learns only that Lakko's server asked, never a user's
//! IP. A picked GIF is then encrypted and uploaded by the client like any other
//! attachment; recipients never contact this service.
//!
//! What this service learns and keeps:
//!
//! - It sees the search terms and timing of a signed-in account. That is the
//!   stated cost of the feature (ADR-0047, UX spec §5).
//! - It keeps nothing on disk. In memory it holds a per-account token bucket
//!   and a short-lived per-account map of the renditions GIPHY returned; both
//!   are bounded.
//! - It logs method, route template, status and latency only — never a query,
//!   a GIF id, an account id, or a GIPHY URL (every API URL carries the key).
//!
//! Configuration (systemd `EnvironmentFile=/etc/lakko/lakko.env`):
//!
//! - `LAKKO_GIF_PROXY_BIND` — listen address (default `127.0.0.1:8091`)
//! - `LAKKO_SESSION_TOKEN_SECRET` — required; the same base64 HMAC secret as
//!   `services/auth`
//! - `LAKKO_GIF_GIPHY_API_KEY` — optional; unset or blank means every `/gifs`
//!   route answers `503 {"error":"gif-provider-not-configured"}`
//! - `LAKKO_GIF_SEARCH_RATE_PER_MINUTE` / `LAKKO_GIF_SEARCH_BURST` — per-account
//!   limit shared by search and trending (defaults 30 / 10)
//! - `LAKKO_GIF_MEDIA_RATE_PER_MINUTE` / `LAKKO_GIF_MEDIA_BURST` — per-account
//!   limit for media (defaults 600 / 120)
//! - `RUST_LOG` — tracing filter
//!
//! Endpoints:
//!
//! - `GET /health`, `GET /ready`, `GET /version` — unauthenticated
//! - `GET /gifs/trending?offset=N`
//! - `GET /gifs/search?q=...&offset=N`
//! - `GET /gifs/media/{id}/{preview|full}`
//!
//! The `/gifs` routes require `Authorization: Bearer <session token>`.

#![forbid(unsafe_code)]
#![warn(clippy::nursery)]
// `redundant_pub_crate` (nursery) and the workspace `unreachable_pub` lint
// demand opposite spellings for items in a private module of a binary; the
// workspace lint wins, so items stay `pub(crate)`.
#![allow(clippy::redundant_pub_crate)]

mod access_log;
mod auth;
mod config;
mod giphy;
mod handlers;
mod metrics;
mod rate_limit;
mod rendition_cache;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use lakko_auth_token::TokenSecret;
use serde_json::json;
use tokio::signal;

use crate::config::Config;
use crate::handlers::GifService;
use crate::rate_limit::AccountRateLimiter;
use crate::rendition_cache::RenditionCache;

/// Shared per-request state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) session_token_secret: Arc<TokenSecret>,
    /// `None` when no GIPHY key is configured.
    pub(crate) gif: Option<Arc<GifService>>,
}

impl AppState {
    fn from_config(config: Config) -> Result<Self> {
        let gif = match config.giphy_api_key {
            Some(key) => {
                let client = giphy::GiphyClient::new(&config.upstream, key)
                    .context("build the GIPHY HTTP client")?;
                Some(Arc::new(GifService {
                    client,
                    search_limiter: AccountRateLimiter::new(
                        config.search_rate,
                        rate_limit::DEFAULT_MAX_ENTRIES,
                    ),
                    media_limiter: AccountRateLimiter::new(
                        config.media_rate,
                        rate_limit::DEFAULT_MAX_ENTRIES,
                    ),
                    cache: RenditionCache::new(
                        rendition_cache::DEFAULT_TTL,
                        rendition_cache::DEFAULT_MAX_ENTRIES,
                    ),
                }))
            },
            None => None,
        };
        Ok(Self {
            session_token_secret: Arc::new(config.session_token_secret),
            gif,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load `.env` from the workspace root in debug builds only. Production
    // reads the systemd `EnvironmentFile=`.
    #[cfg(debug_assertions)]
    let dotenv_loaded = dotenvy::dotenv().is_ok();

    init_tracing();
    // /metrics on its own localhost port, off when unset (ADR-0096).
    lakko_metrics::start_from_env(
        "gif-proxy",
        env!("CARGO_PKG_VERSION"),
        "LAKKO_GIF_PROXY_METRICS_BIND",
    )
    .await?;

    #[cfg(debug_assertions)]
    if dotenv_loaded {
        tracing::debug!("dotenv loaded");
    }

    let config = Config::from_env().context("read gif-proxy configuration")?;
    let bind = config.bind;
    if config.giphy_api_key.is_none() {
        tracing::warn!(
            "GIF provider not configured: LAKKO_GIF_GIPHY_API_KEY is unset, every /gifs route answers 503"
        );
    }

    let state = AppState::from_config(config)?;
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "gif-proxy service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    tracing::info!("gif-proxy service stopped cleanly");
    Ok(())
}

/// Build the router. Extracted so tests drive it without a socket.
pub(crate) fn build_app(state: AppState) -> Router {
    let gif_routes = Router::new()
        .route("/gifs/trending", get(handlers::trending))
        .route("/gifs/search", get(handlers::search))
        .route("/gifs/media/{id}/{variant}", get(handlers::media))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .merge(gif_routes)
        .layer(middleware::from_fn(access_log::log_request))
        // Requests by route template, method and status class (ADR-0096).
        .layer(lakko_metrics::red_layer("gif-proxy"))
        .with_state(state)
}

/// Liveness.
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// Readiness. The service holds no store to probe, so a running process is
/// ready; an unconfigured provider is a supported state, reported by `/gifs`.
async fn ready() -> impl IntoResponse {
    (StatusCode::OK, "ready\n")
}

/// Service version metadata.
async fn version() -> impl IntoResponse {
    Json(json!({
        "service": "lakko-svc-gif-proxy",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

/// Initialise tracing from `RUST_LOG`, falling back to `info`.
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
