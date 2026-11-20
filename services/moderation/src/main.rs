// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-svc-moderation`: report intake and receipts (ADR-0099).
//!
//! ## Configuration
//!
//! Loaded from `/etc/lakko/lakko.env` via systemd `EnvironmentFile=` in
//! production; from `.env` via `dotenvy` in debug builds.
//!
//! - `LAKKO_BIND` — `host:port` (default `127.0.0.1:8100`).
//! - `LAKKO_MODERATION_DATABASE_URL` (or `DATABASE_URL`) — this service's
//!   own MariaDB. Unset: reports are kept in memory and lost on restart
//!   (dev only; logged loudly).
//! - `LAKKO_MODERATION_LIST_PATH` + `LAKKO_MODERATION_ROOT_KEY` — the signed
//!   moderator list and the root key it must verify under. Neither set:
//!   reporting answers 503 `reporting-unavailable`. One set, or a list that
//!   does not verify: startup fails.
//! - `LAKKO_ANON_TOKEN_MASTER_SECRET` / `LAKKO_ANON_TOKEN_WINDOW_SECS` —
//!   the values auth issues under. Unset: the report route answers 503
//!   `anon-tokens-disabled`.
//! - `LAKKO_MODERATION_WITNESS_URLS` + `LAKKO_MODERATION_WITNESS_COSIGN_SECRETS`
//!   (+ `LAKKO_MODERATION_WITNESS_TIMEOUT_SECS`) — the witnesses of the
//!   moderation log, comma lists one to one. Unset: roots carry no
//!   co-signature.
//! - `LAKKO_MODERATION_INTERNAL_BIND` (default `127.0.0.1:8096`) +
//!   `LAKKO_MODERATION_INTERNAL_SECRET` (base64, >= 16 bytes) — the internal
//!   listener taking moderator statements. Secret unset: not started.
//! - `LAKKO_MODERATION_{AUTH,DISCOVERY,CHANNELS,STORAGE}_INTERNAL_URL` +
//!   `..._INTERNAL_SECRET` — where each service's actions are applied
//!   (ADR-0099 step 5): Suspend and Lift at auth; names, search and group
//!   handles at discovery; channel actions at channels; a blob delete at
//!   storage. Both or neither per service. Unset: such a statement is logged
//!   and answered `{"applied": false, "forwarded": false}`. At startup the
//!   moderator list is pushed to every configured service (its list floor),
//!   retried in the background until each answers.
//! - `LAKKO_MODERATION_PANEL_BIND` (default `127.0.0.1:8099`) — the
//!   moderators' panel (ADR-0099 step 5): sign-in with a security key and
//!   the panel API at `/panel/api/*`. Started only with a moderator list.
//! - `LAKKO_MODERATION_METRICS_BIND` — `/metrics` on its own localhost port
//!   (ADR-0096; e.g. `127.0.0.1:9663`). Unset: no metrics listener.
//! - `LAKKO_MODERATION_PANEL_DIR` — LOCAL DEVELOPMENT ONLY: the built panel
//!   app (`apps/moderation` `dist`), served at `/` beside the API. In
//!   production nginx serves the app from its own static root
//!   (`/var/www/lakko-moderation-panel`) and proxies only `/panel/api/`, so
//!   this process never serves the code that drives a moderator's security
//!   key; a production unit leaves it unset.
//! - `LAKKO_MODERATION_SNAPSHOT_PERIOD_SECS` (default 86400, at least 60) —
//!   how often a suspension snapshot is built (ADR-0099 step 4).
//! - `RUST_LOG`.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use lakko_anon_token_server::{AnonTokenServerConfig, TokenVerifier};
use lakko_svc_moderation::internal::{
    build_internal_app, parse_secret, spawn_list_push, Forwarders, InternalState,
    DEFAULT_INTERNAL_BIND,
};
use lakko_svc_moderation::log::{list_key, ModerationLog, LEAF_MODERATOR_LIST};
use lakko_svc_moderation::log_store::{LogStore, MemoryLogStore, SqlLogStore};
use lakko_svc_moderation::panel::{
    blob_slots, build_panel_app, PanelState, Sessions, DEFAULT_PANEL_BIND,
};
use lakko_svc_moderation::snapshots::{parse_period_secs, Snapshots};
use lakko_svc_moderation::store::{MemoryStore, ReportStore, SqlStore};
use lakko_svc_moderation::witness_client::WitnessClient;
use lakko_svc_moderation::{build_app, ip_limit, list, AppState};
use sqlx::mysql::MySqlPoolOptions;

const DEFAULT_BIND: &str = "127.0.0.1:8100";

const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// `/metrics` on its own localhost port, off when unset (ADR-0096).
async fn start_metrics() -> Result<()> {
    lakko_metrics::start_from_env(
        "moderation",
        env!("CARGO_PKG_VERSION"),
        "LAKKO_MODERATION_METRICS_BIND",
    )
    .await?;
    Ok(())
}

#[tokio::main]
#[allow(clippy::single_match_else)]
async fn main() -> Result<()> {
    #[cfg(debug_assertions)]
    {
        if let Ok(path) = dotenvy::dotenv() {
            eprintln!("dotenv loaded from {}", path.display());
        }
    }
    init_tracing();
    start_metrics().await?;

    let bind: SocketAddr = std::env::var("LAKKO_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_string())
        .parse()
        .context("parse LAKKO_BIND as host:port")?;

    let loaded = list::load_from_env().context("moderator list")?;
    match &loaded {
        Some(l) => tracing::info!(
            list_version = l.list.list_version,
            moderators = l.list.entries.len(),
            "moderator list verified"
        ),
        None => tracing::warn!(
            "LAKKO_MODERATION_LIST_PATH / LAKKO_MODERATION_ROOT_KEY unset - reporting is not \
             available on this server"
        ),
    }

    let db_url = std::env::var("LAKKO_MODERATION_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|v| !v.trim().is_empty());
    let token_config =
        AnonTokenServerConfig::from_env().context("anonymous-token configuration")?;
    #[allow(clippy::type_complexity)]
    let (store, log_store, tokens, pool): (
        Arc<dyn ReportStore>,
        Arc<dyn LogStore>,
        TokenVerifier,
        _,
    ) = match db_url {
        Some(url) => {
            let pool = MySqlPoolOptions::new()
                .max_connections(8)
                .acquire_timeout(Duration::from_secs(5))
                .connect(&url)
                .await
                .context("connect to MariaDB")?;
            MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;
            (
                Arc::new(SqlStore::new(pool.clone())),
                Arc::new(SqlLogStore::new(pool.clone())),
                TokenVerifier::new(pool.clone(), token_config),
                Some(pool),
            )
        },
        None => {
            tracing::warn!(
                "LAKKO_MODERATION_DATABASE_URL unset - reports and the moderation log are kept \
                 in memory and lost on restart"
            );
            (
                Arc::new(MemoryStore::new()),
                Arc::new(MemoryLogStore::new()),
                TokenVerifier::in_memory(token_config),
                None,
            )
        },
    };
    tokens.log_startup("moderation");
    let _pruner = tokens.spawn_pruner();

    let loaded = loaded.map(Arc::new);
    let log = open_log(log_store, loaded.as_deref()).await?;
    let forwarders = Arc::new(Forwarders::from_env().map_err(anyhow::Error::msg)?);
    if let Some(l) = &loaded {
        // Each push runs until its service answers; the tasks are not joined.
        drop(spawn_list_push(&forwarders, l));
    }
    let internal = start_internal_listener(&log, loaded.clone(), forwarders.clone()).await?;
    let panel = start_panel_listener(&log, loaded.clone(), forwarders, store.clone()).await?;
    let snapshots = start_snapshots(&log).await?;

    let state = AppState {
        store,
        list: loaded,
        tokens,
        limits: Arc::new(ip_limit::IpLimiter::default_limits()),
        log,
        snapshots,
    };
    let app = build_app(state).into_make_service_with_connect_info::<SocketAddr>();
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "moderation service listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;
    join_listener("internal", internal).await;
    join_listener("panel", panel).await;
    if let Some(pool) = pool {
        pool.close().await;
    }
    tracing::info!("moderation service stopped cleanly");
    Ok(())
}

/// Open the moderation log and append the loaded moderator list if it is
/// not in it yet.
async fn open_log(
    log_store: Arc<dyn LogStore>,
    loaded: Option<&list::LoadedList>,
) -> Result<Arc<ModerationLog>> {
    let witnesses = WitnessClient::list_from_env().context("moderation witness configuration")?;
    let log = Arc::new(
        ModerationLog::open(log_store, witnesses)
            .await
            .context("open the moderation log")?,
    );
    tracing::info!(
        operator_key = %base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            log.operator_public()
        ),
        log_size = log.size().await,
        witnesses = log.witness_count(),
        "moderation log open"
    );
    if let Some(l) = loaded {
        let appended = log
            .append(LEAF_MODERATOR_LIST, list_key(&l.bytes), &l.bytes, now_ms())
            .await
            .context("append the moderator list to the log")?;
        if appended.new {
            tracing::info!(
                log_index = appended.index,
                "moderator list appended to the log"
            );
        }
    }
    Ok(log)
}

/// Reload the stored suspension snapshots and start the builder.
async fn start_snapshots(log: &Arc<ModerationLog>) -> Result<Arc<Snapshots>> {
    let period_secs = parse_period_secs(
        std::env::var("LAKKO_MODERATION_SNAPSHOT_PERIOD_SECS")
            .ok()
            .as_deref(),
    )
    .map_err(anyhow::Error::msg)?;
    let snapshots = Arc::new(Snapshots::new(period_secs));
    let reloaded = snapshots.reload(log).await;
    tracing::info!(period_secs, reloaded, "moderation snapshots");
    // The builder runs for the life of the process.
    drop(snapshots.clone().spawn(log.clone()));
    Ok(snapshots)
}

/// The handle of the internal listener task.
type InternalHandle = tokio::task::JoinHandle<std::io::Result<()>>;

/// Start the internal listener when its secret is configured.
#[allow(clippy::single_match_else)]
async fn start_internal_listener(
    log: &Arc<ModerationLog>,
    loaded: Option<Arc<list::LoadedList>>,
    forwarders: Arc<Forwarders>,
) -> Result<Option<InternalHandle>> {
    let internal_secret = parse_secret(
        "LAKKO_MODERATION_INTERNAL_SECRET",
        std::env::var("LAKKO_MODERATION_INTERNAL_SECRET")
            .ok()
            .as_deref(),
    )
    .map_err(anyhow::Error::msg)?;
    let handle = match internal_secret {
        Some(secret) => {
            for service in forwarders.missing() {
                tracing::warn!(
                    service,
                    "forwarding unset - this service's moderator actions are logged but not applied"
                );
            }
            let bind: SocketAddr = std::env::var("LAKKO_MODERATION_INTERNAL_BIND")
                .unwrap_or_else(|_| DEFAULT_INTERNAL_BIND.to_string())
                .parse()
                .context("parse LAKKO_MODERATION_INTERNAL_BIND as host:port")?;
            let app = build_internal_app(InternalState {
                log: log.clone(),
                list: loaded.clone(),
                forwarders,
                secret,
            });
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .with_context(|| format!("bind {bind}"))?;
            tracing::info!(addr = %bind, "moderation internal listener");
            Some(tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown_signal())
                    .await
            }))
        },
        None => {
            tracing::warn!(
                "LAKKO_MODERATION_INTERNAL_SECRET unset - the internal listener is not started; \
                 no moderator statement can be logged"
            );
            None
        },
    };
    Ok(handle)
}

/// Wait for a side listener to stop and log how it ended.
async fn join_listener(name: &'static str, handle: Option<InternalHandle>) {
    if let Some(handle) = handle {
        match handle.await {
            Ok(Ok(())) => {},
            Ok(Err(e)) => tracing::error!(listener = name, error = %e, "listener failed"),
            Err(e) => tracing::error!(listener = name, error = %e, "listener task failed"),
        }
    }
}

/// Start the moderators' panel when a moderator list is configured.
async fn start_panel_listener(
    log: &Arc<ModerationLog>,
    loaded: Option<Arc<list::LoadedList>>,
    forwarders: Arc<Forwarders>,
    store: Arc<dyn ReportStore>,
) -> Result<Option<InternalHandle>> {
    let Some(list) = loaded else {
        tracing::warn!("no moderator list - the moderators' panel is not started");
        return Ok(None);
    };
    let bind: SocketAddr = std::env::var("LAKKO_MODERATION_PANEL_BIND")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PANEL_BIND.to_string())
        .parse()
        .context("parse LAKKO_MODERATION_PANEL_BIND as host:port")?;
    let dir = std::env::var("LAKKO_MODERATION_PANEL_DIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from);
    match &dir {
        Some(d) if !d.join("index.html").is_file() => tracing::warn!(
            "LAKKO_MODERATION_PANEL_DIR has no index.html - the panel app will not load"
        ),
        Some(_) => tracing::warn!(
            "LAKKO_MODERATION_PANEL_DIR set - the panel app is served by this process; \
             local development only, nginx serves it in production"
        ),
        None => tracing::info!("LAKKO_MODERATION_PANEL_DIR unset - the panel serves its API only"),
    }
    let app = build_panel_app(
        PanelState {
            store,
            list,
            log: log.clone(),
            forwarders,
            sessions: Arc::new(Sessions::new()),
            blob_slots: blob_slots(),
        },
        dir,
    );
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "moderators' panel listening");
    Ok(Some(tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
    })))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
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
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
