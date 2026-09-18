// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Library entry point for the SFU.
//!
//! This crate ships as both:
//!
//! - a binary (`src/main.rs`) — the production service
//!   `lakko-svc-sfu` that systemd starts on the SFU VPS;
//! - a library (this file) — re-exports the modules so the
//!   integration tests under `tests/` can construct the
//!   router + test JWKS without going through the full
//!   `main` entry point.
//!
//! The split is the standard Rust pattern for a binary
//! crate that also needs to be testable end-to-end. The
//! `main.rs` is now a thin shim around
//! [`run_service`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod frame_descriptor;
pub mod jwks;
pub mod media;
pub mod peer;
pub mod prom;
pub mod protocol;
pub mod router;
pub mod signaling;

use std::sync::Arc;

use axum::{response::IntoResponse, routing::get, Json, Router};
use tower_http::trace::TraceLayer;
use tracing::warn;

use crate::config::Config;
use crate::jwks::JwksCache;
use crate::media::MediaTaskHandle;
use crate::peer::PeerRegistry;
use crate::signaling::AppState;

/// Service version pulled from the crate manifest at compile
/// time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build the signaling-side axum router. Used by both the
/// production binary and the integration tests; the only
/// difference between the two callers is how they obtain
/// the [`JwksCache`] (production: HTTP fetch from
/// `auth.lakko.me`; tests: in-memory constructor).
pub fn build_router(state: Arc<AppState>, cfg: Config, jwks: JwksCache) -> Router {
    Router::new()
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
        .with_state(state)
}

/// `GET /health` — liveness probe.
pub async fn health() -> &'static str {
    "ok"
}

/// `GET /version` — service identity as JSON.
pub async fn version() -> impl IntoResponse {
    Json(serde_json::json!({
        "service": "lakko-svc-sfu",
        "version": VERSION,
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

/// `GET /ready` — JWKS cache must be fresher than
/// `2 * jwks_refresh`.
pub async fn ready(cfg: Config, jwks: JwksCache) -> impl IntoResponse {
    let age = jwks.age().await;
    let threshold = cfg.jwks_refresh * 2;
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

/// Convenience — build everything needed for a fresh SFU
/// instance: registry, app state, router. Returns the
/// router ready to be passed to `axum::serve`. Runs the
/// SFU in signaling-only mode; SDP / ICE frames surface
/// as `RouterError::MediaNotWired` until the caller
/// switches to [`build_app_with_media`].
pub fn build_app(cfg: Config, jwks: JwksCache) -> (Router, PeerRegistry) {
    let peers = PeerRegistry::new();
    let state = AppState::new(jwks.clone(), peers.clone());
    let router = build_router(state, cfg, jwks);
    (router, peers)
}

/// Same as [`build_app`] but with a [`MediaTaskHandle`]
/// already wired into [`AppState`]. Production path —
/// called from `main.rs` when `LAKKO_SFU_MEDIA_BIND` is
/// set.
pub fn build_app_with_media(
    cfg: Config,
    jwks: JwksCache,
    media: MediaTaskHandle,
) -> (Router, PeerRegistry) {
    let peers = PeerRegistry::new();
    let state = AppState::new_with_media(jwks.clone(), peers.clone(), media);
    let router = build_router(state, cfg, jwks);
    (router, peers)
}

/// Same as [`build_app`] but with a caller-supplied
/// [`PeerRegistry`]. The media task needs to share the
/// registry with the signaling layer so SFU-initiated
/// [`crate::protocol::ServerFrame::SdpOffer`] frames can
/// reach the right peer's outbound mpsc; `main.rs`
/// constructs the registry once and threads it through
/// both halves.
pub fn build_app_without_media(cfg: Config, jwks: JwksCache, peers: PeerRegistry) -> Router {
    let state = AppState::new(jwks.clone(), peers);
    build_router(state, cfg, jwks)
}

/// Same as [`build_app_with_media`] but with a caller-
/// supplied [`PeerRegistry`] — the production path that
/// shares the registry with `media::spawn_media_task`.
pub fn build_app_with_media_and_peers(
    cfg: Config,
    jwks: JwksCache,
    media: MediaTaskHandle,
    peers: PeerRegistry,
) -> Router {
    let state = AppState::new_with_media(jwks.clone(), peers, media);
    build_router(state, cfg, jwks)
}
