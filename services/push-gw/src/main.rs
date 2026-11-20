// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Push gateway for Lakko.
//!
//! Phase 1.6 — token registry + dispatch scaffold.
//!
//! The server bridges between an internal "wake this group" event and
//! the external platform push services (APNs, FCM, Web Push,
//! UnifiedPush). Payload carried to providers is strictly metadata
//! pointers — `(subscription_id, push_gid, envelope_pointer)`. Apple,
//! Google and Mozilla never see plaintext: the recipient client wakes up,
//! fetches ciphertext from `services/ds`, decrypts locally and builds
//! the OS notification on-device.
//!
//! Since ADR-0078 section 3 this service holds no device pseudonym and no
//! token in the clear: a subscription row is `(sub_id, push_gid, wake_pk,
//! sealed tokens)`, the tokens sealed under a group key that arrives only
//! inside each sealed wake (`src/registry.rs`, `src/notify.rs`).
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
//! - `LAKKO_ANON_TOKEN_MASTER_SECRET` — 64 hex characters, the SAME value
//!   `services/auth` issues under (ADR-0054). Unset: every registration
//!   route answers 503 `anon-tokens-disabled` (loud startup warning).
//!   Malformed: startup fails.
//! - `LAKKO_ANON_TOKEN_WINDOW_SECS` — the token epoch window (default 86400);
//!   must match auth.
//! - `LAKKO_PUSH_GW_INTERNAL_BIND` — the internal listener (default
//!   `127.0.0.1:8092`). Not proxied by nginx; bind it to loopback or a
//!   private interface only.
//! - `LAKKO_PUSH_GW_INTERNAL_SECRET` — base64, >= 16 bytes; the shared
//!   secret the DS presents (its `LAKKO_DS_PUSH_GW_SECRET`). Unset: the
//!   internal listener is not started and no group wake is delivered.
//! - `LAKKO_PUSH_GW_WAKE_BURST` / `LAKKO_PUSH_GW_WAKE_PER_MINUTE` — the
//!   per-group wake cap (defaults 30 / 60; chosen, not measured). Moved here
//!   from the DS, which no longer sees a `push_gid` (ADR-0078).
//!
//! The X25519 wake key is generated at first boot and stored in this
//! service's database (`src/wake_key.rs`), not taken from the environment.
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec.
//!
//! ## Endpoints
//!
//! Public listener (`LAKKO_BIND`, proxied by nginx):
//!
//! - `GET    /health`                — liveness, 200 OK
//! - `GET    /ready`                 — readiness (DB ping), 200 / 503
//! - `GET    /version`
//! - `GET    /push/wake-key`         — the X25519 key wakes are sealed to
//! - `POST   /push/subscribe`        — store (or replace) a sealed subscription
//! - `POST   /push/unsubscribe`      — drop one by its `sub_id`
//!
//! The two subscription routes each spend one `push-register` anonymous
//! token (ADR-0054 S3, header `x-lakko-anon-token`). The per-device
//! `/push/register` / `/push/unregister` and the `/push/group/*` routes are
//! gone with the tables they wrote (ADR-0078).
//!
//! Internal listener (`LAKKO_PUSH_GW_INTERNAL_BIND`, shared-secret gated,
//! ADR-0054 decision 1 — see `src/internal.rs`):
//!
//! - `POST   /push/notify-group`     — the DS forwards a sealed wake, unread,
//!   after a signed publish
//!
//! The per-device `/push/notify` is gone (ADR-0054: no caller on any
//! client), and no route on the public listener can wake a device.

#![forbid(unsafe_code)]

mod internal;
mod metrics;
mod notify;
mod provider;
mod providers;
mod registry;
mod wake_guard;
mod wake_key;

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
use lakko_anon_token::TokenContext;
use lakko_anon_token_server::{require_token, AnonTokenServerConfig, TokenVerifier};
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

/// Per-request body limit. Push-gw payloads are small JSON (a couple of
/// base64 fields + maybe a 2 KiB token), 64 KiB is plenty.
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
    /// ADR-0054 - the secret the internal listener checks. `None` when
    /// `LAKKO_PUSH_GW_INTERNAL_SECRET` is unset (the listener is then off).
    pub(crate) internal_secret: Option<Arc<Vec<u8>>>,
    /// ADR-0054 S3 - the anonymous-token verifier the registration routes
    /// spend through, over this service's spent-set (`anon_token_spent`).
    /// Dormant (those routes answer 503) when
    /// `LAKKO_ANON_TOKEN_MASTER_SECRET` is unset.
    pub(crate) anon_tokens: TokenVerifier,
    /// ADR-0078 - the X25519 key every wake is sealed to.
    pub(crate) wake_key: Arc<lakko_contacts::push_wake::PushGwKey>,
    /// ADR-0078 - freshness, nonce dedup and the per-group cap.
    pub(crate) wake_guard: Arc<wake_guard::WakeGuard>,
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    #[cfg(debug_assertions)]
    {
        if let Ok(path) = dotenvy::dotenv() {
            eprintln!("dotenv loaded from {}", path.display());
        }
    }

    init_tracing();
    // /metrics on its own localhost port, off when unset (ADR-0096).
    lakko_metrics::start_from_env(
        "push-gw",
        env!("CARGO_PKG_VERSION"),
        "LAKKO_PUSH_GW_METRICS_BIND",
    )
    .await?;

    let bind: SocketAddr = std::env::var("LAKKO_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_string())
        .parse()
        .context("parse LAKKO_BIND as host:port")?;

    let internal_bind: SocketAddr = std::env::var("LAKKO_PUSH_GW_INTERNAL_BIND")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| internal::DEFAULT_INTERNAL_BIND.to_string())
        .parse()
        .context("parse LAKKO_PUSH_GW_INTERNAL_BIND as host:port")?;
    let internal_secret = internal::load_internal_secret()
        .map_err(anyhow::Error::msg)
        .context("internal secret")?;

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
    // Connections in use and idle (ADR-0096).
    let pool_for_gauge = pool.clone();
    let _pool_gauge =
        lakko_metrics::spawn_pool_gauge(move || (pool_for_gauge.size(), pool_for_gauge.num_idle()));

    // ADR-0054 S3 - a malformed secret or window refuses startup; an unset
    // secret leaves the registration routes answering 503.
    let anon_tokens = TokenVerifier::new(
        pool.clone(),
        AnonTokenServerConfig::from_env().context("anonymous-token configuration")?,
    );
    anon_tokens.log_startup("push-gw");

    let (wake_key, wake_guard) = load_wake(&pool).await?;

    let providers = resolve_providers();
    let state = AppState {
        db: pool.clone(),
        providers: Arc::new(providers),
        internal_secret,
        anon_tokens,
        wake_key: Arc::new(wake_key),
        wake_guard: Arc::new(wake_guard),
    };

    let _spent_pruner = state.anon_tokens.spawn_pruner();

    // One shutdown signal, observed by both listeners.
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = stop_tx.send(true);
    });
    let stopped = |mut rx: tokio::sync::watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|stop| *stop).await;
    };

    let internal = if state.internal_secret.is_some() {
        if !internal_bind.ip().is_loopback() {
            tracing::warn!(
                addr = %internal_bind,
                "internal listener is not on loopback; keep it off any public interface"
            );
        }
        let listener = tokio::net::TcpListener::bind(internal_bind)
            .await
            .with_context(|| format!("bind internal {internal_bind}"))?;
        tracing::info!(addr = %internal_bind, "push-gw internal listener up");
        let app = internal::build_internal_app(state.clone());
        let stop = stopped(stop_rx.clone());
        Some(tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stop)
                .await
        }))
    } else {
        tracing::warn!(
            "LAKKO_PUSH_GW_INTERNAL_SECRET unset - the internal listener is OFF: the DS \
             cannot forward wakes and no group wake is delivered"
        );
        None
    };

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "push-gw service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(stopped(stop_rx))
        .await
        .context("axum serve")?;
    if let Some(internal) = internal {
        internal
            .await
            .context("internal listener task")?
            .context("axum serve internal")?;
    }

    pool.close().await;
    tracing::info!("push-gw service stopped cleanly");
    Ok(())
}

/// ADR-0078: the X25519 wake key (generated at first boot) and the guard
/// with its per-group cap from the environment.
async fn load_wake(
    pool: &MySqlPool,
) -> Result<(lakko_contacts::push_wake::PushGwKey, wake_guard::WakeGuard)> {
    let key = wake_key::load_or_init_wake_key(pool)
        .await
        .context("wake key")?;
    let guard = wake_guard::WakeGuard::new(
        wake_guard::knob("LAKKO_PUSH_GW_WAKE_BURST", wake_guard::DEFAULT_BURST)?,
        wake_guard::knob(
            "LAKKO_PUSH_GW_WAKE_PER_MINUTE",
            wake_guard::DEFAULT_PER_MINUTE,
        )?,
    );
    Ok((key, guard))
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
    // ADR-0054 S3 - every registration route spends one `push-register`
    // anonymous token before its handler runs. The wake itself is NOT here:
    // since ADR-0054 it is the DS's forward to the internal listener
    // (`src/internal.rs`), never a public route.
    let registration = Router::new()
        .route("/push/subscribe", post(registry::subscribe))
        .route("/push/unsubscribe", post(registry::unsubscribe))
        .route_layer(axum::middleware::from_fn_with_state(
            state.anon_tokens.gate(TokenContext::PushRegister),
            require_token,
        ));
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/push/wake-key", get(wake_key::wake_key_handler))
        .merge(registration)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        // Requests by route template, method and status class (ADR-0096).
        .layer(lakko_metrics::red_layer("push-gw"))
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
mod anon_token_route_tests;

#[cfg(test)]
mod sealed_wake_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    /// sqlx's migrator does not tolerate two concurrent runs on one
    /// database, so the DB-gated tests migrate once per process.
    pub(crate) static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

    /// An `AppState` whose pool never connects - for routes and gates that
    /// must answer before any query runs.
    pub(crate) fn state_without_db(internal_secret: Option<Arc<Vec<u8>>>) -> AppState {
        let db = MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/none")
            .expect("lazy pool");
        AppState {
            anon_tokens: TokenVerifier::new(db.clone(), None),
            db,
            providers: Arc::new(HashMap::new()),
            internal_secret,
            wake_key: Arc::new(lakko_contacts::push_wake::PushGwKey::from_secret_bytes(
                [0x77; 32],
            )),
            wake_guard: Arc::new(wake_guard::WakeGuard::new(30, 60)),
        }
    }

    #[tokio::test]
    async fn the_public_router_wakes_nobody() {
        // ADR-0054: `/push/notify` is deleted and `/push/notify-group` lives
        // on the internal listener only. The public router must not match
        // either, whatever is presented.
        for path in ["/push/notify", "/push/notify-group"] {
            let response = build_app(state_without_db(None))
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("content-type", "application/json")
                        .header(
                            internal::SECRET_HEADER,
                            "c2hhcmVkLXdha2Utc2VjcmV0LWZvci10ZXN0cy0zMmI=",
                        )
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

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
