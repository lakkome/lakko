// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Ciphertext blob storage proxy for Lakko.
//!
//! Phase 1.2: filesystem-backed content-addressable store. Clients PUT
//! ciphertext keyed by `SHA-256(ciphertext)`; on GET the same hash is
//! used to look the blob back up. The server is **blind** — it cannot
//! decrypt blobs and cannot tell who can. The hash is over the
//! ciphertext, not the plaintext, so the server cannot deduplicate
//! across users who encrypt the same plaintext with different keys
//! (which is the desired property — ciphertext deduplication only).
//!
//! Backend: local filesystem rooted at `LAKKO_BLOB_DIR`. Phase 4+
//! migrates to SeaweedFS / S3-compatible object storage. The
//! filesystem layout (`<root>/<hex[0:2]>/<hex[2:4]>/<hex>`) is chosen
//! so a future migration can rsync the tree directly into a bucket
//! prefix scheme.
//!
//! No authentication and no quota in Phase 1 — the server is a
//! "public CDN for E2EE blobs". Anti-abuse relies on:
//!
//!   * Hash verification on PUT (random spam cannot poison existing
//!     blobs because SHA-256 collisions are infeasible)
//!   * nginx-layer `limit_req` zones for per-IP rate limit
//!   * 50 MB per-blob ceiling enforced both here and at nginx
//!
//! Owner tracking, quota, DELETE, and PoW gating land in Phase 2
//! hardening. See `~/.claude/plans/1-nested-wreath.md` §4 (Object
//! storage) and ADR-0001.
//!
//! Configuration via environment variables (loaded by systemd from
//! `/etc/lakko/lakko.env`):
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8083`)
//! - `LAKKO_BLOB_DIR` — root directory for blob storage (default
//!   `./target/dev-blobs/`; production unit sets `/var/lib/lakko/blobs/`)
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec
//!
//! Endpoints:
//!
//! - `GET    /health`             — liveness, 200 OK
//! - `GET    /ready`              — readiness (probes the blob root), 200 / 503
//! - `GET    /version`            — `{"service", "version", "git_sha"}`
//! - `PUT    /blob/{sha256_hex}`  — upload, body=ciphertext, idempotent
//! - `GET    /blob/{sha256_hex}`  — download, streamed
//! - `HEAD   /blob/{sha256_hex}`  — exists check

#![forbid(unsafe_code)]

mod blob;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, put};
use axum::Router;
use serde_json::json;
use tokio::signal;
use tower_http::trace::TraceLayer;

/// Default bind address if `LAKKO_BIND` is not set. Distinct port from
/// auth (8081) and ds (8082) so all three can run on the same VPS
/// behind nginx without collision.
const DEFAULT_BIND: &str = "127.0.0.1:8083";

/// Default blob root for local development. The systemd unit overrides
/// this to `/var/lib/lakko/blobs/` in production.
const DEFAULT_BLOB_DIR: &str = "./target/dev-blobs";

/// Per-blob ceiling. Mirrored in nginx `client_max_body_size`. Going
/// beyond this for an MLS-app message attachment is unlikely; large
/// files would be chunked client-side first.
const MAX_BLOB_BYTES: usize = 50 * 1024 * 1024;

/// Shared application state.
///
/// Holds the resolved blob root path. Wrapped in `Arc` so cloning into
/// per-request state is free.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) blob_root: Arc<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load `.env` from the workspace root in debug builds. Production
    // picks up env vars from the systemd `EnvironmentFile=` directive.
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

    let blob_root = PathBuf::from(
        std::env::var("LAKKO_BLOB_DIR").unwrap_or_else(|_| DEFAULT_BLOB_DIR.to_string()),
    );

    // Ensure the root exists and is writable on startup. Fail fast if
    // not — the service is useless without a usable blob directory.
    tokio::fs::create_dir_all(&blob_root)
        .await
        .with_context(|| format!("create LAKKO_BLOB_DIR at {}", blob_root.display()))?;
    blob::probe_writable(&blob_root)
        .await
        .with_context(|| format!("LAKKO_BLOB_DIR not writable: {}", blob_root.display()))?;

    let state = AppState {
        blob_root: Arc::new(blob_root.clone()),
    };

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, blob_root = %blob_root.display(), "storage service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    tracing::info!("storage service stopped cleanly");
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
            "/blob/{hex}",
            put(blob::put_blob)
                .get(blob::get_blob)
                .head(blob::head_blob),
        )
        // Per-route body limit (axum default is 2 MB which is too small).
        .layer(DefaultBodyLimit::max(MAX_BLOB_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Liveness probe — process is alive, no I/O.
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// Readiness probe — confirms the blob root is writable. Touches a
/// temp file so a missing-mount or permissions regression after start
/// is detected by ops monitoring.
async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    match blob::probe_writable(&state.blob_root).await {
        Ok(()) => (StatusCode::OK, "ready\n").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "readiness probe: blob root not writable");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready\n").into_response()
        },
    }
}

/// Service version metadata.
async fn version() -> impl IntoResponse {
    Json(json!({
        "service": "lakko-svc-storage",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

/// Initialize tracing-subscriber from `RUST_LOG`. Falls back to `info`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    fn test_state() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppState {
            blob_root: Arc::new(tmp.path().to_path_buf()),
        };
        (state, tmp)
    }

    #[tokio::test]
    async fn health_returns_200_ok() {
        let (state, _tmp) = test_state();
        let app = build_app(state);
        let response = app
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
    async fn ready_returns_200_when_blob_root_writable() {
        let (state, _tmp) = test_state();
        let app = build_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn version_returns_json() {
        let (state, _tmp) = test_state();
        let app = build_app(state);
        let response = app
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
        assert_eq!(parsed["service"], "lakko-svc-storage");
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let (state, _tmp) = test_state();
        let app = build_app(state);
        let response = app
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
