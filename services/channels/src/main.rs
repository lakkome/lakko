// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-svc-channels` — production binary entry point.
//!
//! Thin shim over `src/lib.rs` (same lib/bin split as
//! `services/sfu`). Lives behind systemd's
//! `EnvironmentFile=/etc/lakko/lakko.env` in production.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::mysql::MySqlPoolOptions;
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use lakko_svc_channels::{
    router, AppState, ChannelStore, Config, InMemoryChannelStore, SqlChannelStore, VERSION,
};

/// Migrations directory (relative to crate root). `sqlx::migrate!`
/// reads it at compile time, so a missing file fails the build
/// instead of erroring at boot.
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    dotenv_in_debug();

    let cfg = Config::from_env()?;
    info!(bind = %cfg.bind, "lakko-svc-channels starting (v{})", VERSION);

    let store: Arc<dyn ChannelStore + Send + Sync> = if let Some(url) = &cfg.database_url {
        info!("storage: MariaDB (LAKKO_CHANNELS_DATABASE_URL set) — running migrations");
        let pool = MySqlPoolOptions::new()
            .max_connections(16)
            .connect(url)
            .await?;
        MIGRATIONS.run(&pool).await?;
        Arc::new(SqlChannelStore::new(pool))
    } else {
        warn!(
            "storage: in-memory (no LAKKO_CHANNELS_DATABASE_URL) — \
                 state is lost on restart, set the env var for production"
        );
        Arc::new(InMemoryChannelStore::new())
    };
    // Spawn the background prune loop before handing
    // `store` into AppState — both halves share the same
    // `Arc<dyn ChannelStore + Send + Sync>` clone. The
    // task survives until the process exits; it's
    // cooperatively unaware of shutdown because the
    // single-bulk-DELETE work unit takes milliseconds and
    // tokio drops the task on runtime drop.
    let prune_interval = cfg.prune_interval_secs;
    let retention_secs = cfg.retention_secs;
    let subscriber_retention_secs = cfg.subscriber_retention_secs;
    if prune_interval > 0 {
        let prune_store = Arc::clone(&store);
        tokio::spawn(async move {
            run_prune_loop(
                prune_store,
                retention_secs,
                subscriber_retention_secs,
                prune_interval,
            )
            .await;
        });
        info!(
            retention_secs,
            subscriber_retention_secs,
            prune_interval_secs = prune_interval,
            "prune: background loop spawned",
        );
    } else {
        info!("prune: background loop disabled (LAKKO_CHANNELS_PRUNE_INTERVAL_SECS=0)");
    }

    let state = AppState::new(cfg.clone(), store);
    let app = router(state);

    let listener = TcpListener::bind(cfg.bind).await?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    tokio::select! {
        () = shutdown_signal() => info!("shutdown: signal received"),
        res = server => {
            if let Err(e) = res {
                warn!(error = %e, "server: task panicked");
            }
        },
    }

    info!("lakko-svc-channels shutting down");
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,lakko_svc_channels=info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).compact())
        .init();
}

fn dotenv_in_debug() {
    #[cfg(debug_assertions)]
    {
        let _ = dotenvy::dotenv();
    }
}

/// Background prune loop — sleeps for `interval_secs`,
/// then issues both retention prunes (posts + stale
/// subscribers) against the store. Runs until the tokio
/// runtime is dropped.
///
/// Either `*_retention_secs == 0` short-circuits its
/// own prune call (the loop still sleeps and logs, so
/// operators can flip either retention on/off live
/// without restarting the service).
async fn run_prune_loop(
    store: Arc<dyn ChannelStore + Send + Sync>,
    posts_retention_secs: u64,
    subscriber_retention_secs: u64,
    interval_secs: u64,
) {
    let interval = Duration::from_secs(interval_secs);
    loop {
        tokio::time::sleep(interval).await;
        let now_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => u64::try_from(d.as_millis()).unwrap_or(u64::MAX),
            Err(e) => {
                warn!(error = %e, "prune: clock before UNIX epoch — skipping tick");
                continue;
            },
        };

        // Posts retention — bulk DELETE per the cutoff.
        if posts_retention_secs == 0 {
            info!("prune: posts retention disabled — no work");
        } else {
            let cutoff_ms = now_ms.saturating_sub(posts_retention_secs.saturating_mul(1000));
            match store.prune_posts_older_than(cutoff_ms).await {
                Ok(removed) => {
                    if removed > 0 {
                        info!(removed, posts_retention_secs, "prune: posts tick complete");
                    }
                },
                Err(e) => warn!(error = %e, "prune: posts tick failed"),
            }
        }

        // Stale-subscriber retention — bulk DELETE +
        // envelope FK cascade.
        if subscriber_retention_secs == 0 {
            info!("prune: subscriber retention disabled — no work");
        } else {
            let cutoff_ms = now_ms.saturating_sub(subscriber_retention_secs.saturating_mul(1000));
            match store.prune_subscribers_older_than(cutoff_ms).await {
                Ok(removed) => {
                    if removed > 0 {
                        info!(
                            removed,
                            subscriber_retention_secs, "prune: subscribers tick complete"
                        );
                    }
                },
                Err(e) => warn!(error = %e, "prune: subscribers tick failed"),
            }
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!(error = %e, "shutdown: ctrl_c handler failed");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
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
