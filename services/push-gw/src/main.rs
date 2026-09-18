// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Push gateway for Lakko.
//!
//! Phase 1.6 — token registry + dispatch scaffold.
//!
//! The server bridges between an internal "wake this device" event and
//! the external platform push services (APNs, FCM, Web Push,
//! UnifiedPush). Payload carried to providers is strictly metadata
//! pointers — `(group_pseudonym, envelope_pointer)`. Apple, Google
//! and Mozilla never see plaintext: the recipient client wakes up,
//! fetches ciphertext from `services/ds`, decrypts locally and builds
//! the OS notification on-device.
//!
//! ## Phase 1.6 scope
//!
//! Real platform integrations need .p8 / service-account / VAPID keys
//! that are operational artifacts, not code. So this sprint ships a
//! provider abstraction (`src/provider.rs`) plus a single
//! [`provider::LogOnlyProvider`] that records intent into `tracing`
//! instead of contacting a remote service.
//! Phase 1.6b drops in real `ApnsProvider` / `FcmProvider` /
//! `WebPushProvider` impls behind the same trait without registry /
//! handler changes.
//!
//! ## Configuration
//!
//! Loaded from `/etc/lakko/lakko.env` via systemd `EnvironmentFile=`
//! in production; from `.env` via `dotenvy` in debug builds.
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8085`)
//! - `LAKKO_PUSH_GW_DATABASE_URL` (or `DATABASE_URL` fallback) —
//!   sqlx-style MariaDB DSN; the database must be exclusive to this
//!   service per the per-service DB isolation rule.
//! - `LAKKO_PUSH_GW_POW_EFFORT_BITS` — PoW difficulty (default 8;
//!   0 in tests).
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec.
//!
//! ## Endpoints
//!
//! - `GET    /health`         — liveness, 200 OK
//! - `GET    /ready`          — readiness (DB ping), 200 / 503
//! - `GET    /version`
//! - `POST   /pow/challenge`  — Equix PoW challenge (1.1e-style)
//! - `POST   /push/register`  — bind `(device_pseudonym, provider) → token`
//! - `POST   /push/unregister`— drop a token
//! - `POST   /push/notify`    — wake every registered endpoint for a device

#![forbid(unsafe_code)]

mod notify;
mod pow;
mod provider;
mod providers;
mod registry;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tokio::signal;
use tower_http::trace::TraceLayer;

use crate::provider::{LogOnlyProvider, Provider, ProviderKind};
use crate::providers::{
    apns::{ApnsConfig, ApnsProvider},
    fcm::{FcmConfig, FcmProvider},
    unifiedpush::UnifiedPushProvider,
    webpush::{WebPushConfig, WebPushProvider},
};

/// Default bind address if `LAKKO_BIND` is not set. Distinct from
/// auth (8081), ds (8082), storage (8083), discovery (8084).
const DEFAULT_BIND: &str = "127.0.0.1:8085";

/// Migrations directory (relative to crate root).
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Per-request body limit. Push-gw payloads are small JSON (PoW proof
/// + a couple of base64 fields + maybe a 2 KiB token), 64 KiB is plenty.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Shared application state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: MySqlPool,
    /// One provider per `ProviderKind`. Phase 1.6 always wires every
    /// kind to a [`LogOnlyProvider`]; Phase 1.6b will swap in real
    /// `ApnsProvider` / `FcmProvider` / `WebPushProvider` instances
    /// based on env config.
    pub(crate) providers: Arc<HashMap<ProviderKind, Box<dyn Provider>>>,
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

    let db_url = std::env::var("LAKKO_PUSH_GW_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_PUSH_GW_DATABASE_URL or DATABASE_URL env var must be set")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    let providers = resolve_providers();
    let state = AppState {
        db: pool.clone(),
        providers: Arc::new(providers),
    };

    let _pow_pruner = pow::spawn_pruner(pool.clone());

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "push-gw service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    pool.close().await;
    tracing::info!("push-gw service stopped cleanly");
    Ok(())
}

/// Pick a provider implementation per [`ProviderKind`].
///
/// Phase 1.6b: probes env for real provider credentials and falls
/// back to [`LogOnlyProvider`] per-kind when they're missing. Each
/// `*Config::from_env` call logs the reason for any fallback so
/// operators see what's wrong without grepping for empty env vars.
///
/// Required env vars per kind:
///
/// - APNs alert token (`ProviderKind::Apns`):
///     - `LAKKO_APNS_KEY_PATH` — path to the .p8 file Apple issues.
///     - `LAKKO_APNS_KEY_ID` — 10-char key id (e.g. `ABCDE12345`).
///     - `LAKKO_APNS_TEAM_ID` — 10-char team id (e.g. `1A2B3C4D5E`).
///     - `LAKKO_APNS_TOPIC` — bundle id (e.g. `me.lakko.dev`).
///     - `LAKKO_APNS_ENDPOINT` — `sandbox` for TestFlight builds,
///       anything else (or unset) = production.
/// - APNs call token (`ProviderKind::ApnsVoip`) — a SEPARATE Apple
///   credential from the alert one, so a separate variable set. Without
///   it a terminated iPhone cannot be made to ring at all
///   (`docs/ux/calls.md` section 2.1); the service falls back to the
///   alert token for call wakes, which reaches a backgrounded app but
///   not a closed one.
///     - `LAKKO_APNS_VOIP_KEY_PATH` — path to the VoIP .p8 file.
///     - `LAKKO_APNS_VOIP_KEY_ID` — 10-char key id.
///     - `LAKKO_APNS_VOIP_TEAM_ID` — 10-char team id.
///     - `LAKKO_APNS_VOIP_TOPIC` — `<bundle id>.voip`; APNs refuses a
///       VoIP push to the bare bundle id.
///     - `LAKKO_APNS_VOIP_ENDPOINT` — `sandbox` / production, as above.
/// - FCM (`ProviderKind::Fcm`):
///     - `LAKKO_FCM_SA_PATH` — path to the service-account JSON
///       Google Cloud issues.
/// - Web Push (`ProviderKind::WebPush`):
///     - `LAKKO_WEBPUSH_VAPID_PRIVATE_KEY_PEM` — P-256 private key as a
///       PKCS#8 PEM (`BEGIN PRIVATE KEY`); a SEC1 `BEGIN EC PRIVATE KEY` is refused.
///     - `LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL` — base64url-no-pad
///       of the uncompressed P-256 point (65 bytes).
///     - `LAKKO_WEBPUSH_VAPID_SUBJECT` — `mailto:` or `https:` URI.
/// - UnifiedPush (`ProviderKind::UnifiedPush`): none — always
///   wired, the subscription endpoint URL itself is the capability.
fn resolve_providers() -> HashMap<ProviderKind, Box<dyn Provider>> {
    let mut map: HashMap<ProviderKind, Box<dyn Provider>> = HashMap::new();

    // APNs. Explicit `let p: Box<dyn Provider>` lets each arm
    // return a concrete `Box<ApnsProvider>` or `Box<LogOnlyProvider>`
    // without an `as` coercion — the let-type drives unification.
    let apns: Box<dyn Provider> = if let Some(cfg) = ApnsConfig::from_env() {
        tracing::info!("APNs provider configured from env");
        Box::new(ApnsProvider::new(cfg))
    } else {
        tracing::warn!("APNs provider falling back to LogOnly — LAKKO_APNS_* not configured",);
        Box::new(LogOnlyProvider {
            kind: ProviderKind::Apns,
        })
    };
    map.insert(ProviderKind::Apns, apns);

    // APNs call token (PushKit). Its own Apple credential, so its own
    // variable set — see `providers::apns` on why it is not the alert
    // one. Falling back to LogOnly here does not break calls outright:
    // `notify::delivers_to` routes a call wake to the alert token for
    // any device with no call token registered, and a device that DID
    // register one keeps sending its calls into this LogOnly provider
    // until the credential lands. So the warning says what it costs.
    let apns_voip: Box<dyn Provider> = if let Some(cfg) = ApnsConfig::voip_from_env() {
        tracing::info!("APNs VoIP provider configured from env");
        Box::new(ApnsProvider::new(cfg))
    } else {
        tracing::warn!(
            "APNs VoIP provider falling back to LogOnly — LAKKO_APNS_VOIP_* not configured; \
             a terminated iPhone will not ring",
        );
        Box::new(LogOnlyProvider {
            kind: ProviderKind::ApnsVoip,
        })
    };
    map.insert(ProviderKind::ApnsVoip, apns_voip);

    let fcm: Box<dyn Provider> = if let Some(cfg) = FcmConfig::from_env() {
        tracing::info!("FCM provider configured from env");
        Box::new(FcmProvider::new(cfg))
    } else {
        tracing::warn!("FCM provider falling back to LogOnly — LAKKO_FCM_SA_PATH not configured",);
        Box::new(LogOnlyProvider {
            kind: ProviderKind::Fcm,
        })
    };
    map.insert(ProviderKind::Fcm, fcm);

    let webpush: Box<dyn Provider> = if let Some(cfg) = WebPushConfig::from_env() {
        tracing::info!("Web Push provider configured from env");
        Box::new(WebPushProvider::new(cfg))
    } else {
        tracing::warn!(
            "Web Push provider falling back to LogOnly — LAKKO_WEBPUSH_VAPID_* not configured",
        );
        Box::new(LogOnlyProvider {
            kind: ProviderKind::WebPush,
        })
    };
    map.insert(ProviderKind::WebPush, webpush);

    // UnifiedPush — always real; no shared credentials.
    let unifiedpush: Box<dyn Provider> = Box::new(UnifiedPushProvider::new());
    map.insert(ProviderKind::UnifiedPush, unifiedpush);
    tracing::info!("UnifiedPush provider configured (no shared credentials needed)");

    map
}

/// Build the public router. Extracted so tests can drive it directly
/// without binding a TCP listener.
pub(crate) fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/pow/challenge", post(pow::issue_challenge))
        .route("/push/register", post(registry::register))
        .route("/push/unregister", post(registry::unregister))
        .route("/push/notify", post(notify::notify))
        // 1.20e — group fan-out. Devices subscribe to groups
        // they care about; publishers call `/push/notify-group`
        // once per send + the gateway fans out to every
        // subscribed device's tokens.
        .route("/push/group/subscribe", post(registry::group_subscribe))
        .route("/push/group/unsubscribe", post(registry::group_unsubscribe))
        .route("/push/notify-group", post(notify::notify_group))
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
        "service": "lakko-svc-push-gw",
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
        assert_eq!(parsed["service"], "lakko-svc-push-gw");
    }

    #[test]
    fn resolve_providers_covers_every_kind() {
        let map = resolve_providers();
        for kind in crate::provider::tests::ALL_KINDS {
            let provider = map.get(&kind).expect("provider missing");
            assert_eq!(provider.kind(), kind);
        }
        assert_eq!(map.len(), crate::provider::tests::ALL_KINDS.len());
    }
}
