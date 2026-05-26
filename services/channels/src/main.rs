// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-svc-channels` — production binary entry point.
//!
//! Thin shim over `src/lib.rs` (same lib/bin split as
//! `services/sfu`). Lives behind systemd's
//! `EnvironmentFile=/etc/lakko/lakko.env` in production.

#![forbid(unsafe_code)]

use std::sync::Arc;

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

    let store: Arc<dyn ChannelStore + Send + Sync> =
        if let Some(url) = &cfg.database_url {
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
