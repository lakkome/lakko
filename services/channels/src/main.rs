// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-svc-channels` — production binary entry point.
//!
//! Thin shim over `src/lib.rs` (same lib/bin split as
//! `services/sfu`). Lives behind systemd's
//! `EnvironmentFile=/etc/lakko/lakko.env` in production.
//!
//! `LAKKO_ANON_CRED_MASTER_SECRET` (64 hex characters, the value auth issues
//! the channel-poll credential under; ADR-0090 section 4.4) arms the vote
//! routes; unset, they answer `503 anon-credential-disabled`.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::mysql::MySqlPoolOptions;
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use lakko_anon_token_server::{AnonTokenServerConfig, TokenVerifier};
use lakko_svc_channels::api::INVITE_KEY_GRACE_MS;
use lakko_svc_channels::internal::{internal_router, load_internal_secret};
use lakko_svc_channels::owner_auth::OWNER_NONCE_RETENTION_MS;
use lakko_svc_channels::{
    router, AppState, ChannelStore, Config, InMemoryChannelStore, SqlChannelStore, MIGRATIONS,
    VERSION,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    dotenv_in_debug();

    let cfg = Config::from_env()?;
    info!(bind = %cfg.bind, "lakko-svc-channels starting (v{})", VERSION);
    let anon_config = AnonTokenServerConfig::from_env()?;
    let internal_secret = load_internal_secret().map_err(anyhow::Error::msg)?;
    // ADR-0090 section 4.4 — the channel-poll credential master shared with
    // auth. Unset: the vote routes answer 503.
    let votes = match std::env::var("LAKKO_ANON_CRED_MASTER_SECRET") {
        Ok(v) if !v.trim().is_empty() => Some(lakko_anon_cred::CredentialVerifier::new(
            lakko_anon_cred::MasterSecret::from_hex(&v).map_err(|_| {
                anyhow::anyhow!("LAKKO_ANON_CRED_MASTER_SECRET must be 64 hex characters")
            })?,
        )),
        _ => {
            warn!(
                "channel poll votes DISABLED: LAKKO_ANON_CRED_MASTER_SECRET is not set; \
                 the vote routes answer 503"
            );
            None
        },
    };

    let (store, anon_tokens): (Arc<dyn ChannelStore + Send + Sync>, TokenVerifier) =
        if let Some(url) = &cfg.database_url {
            info!("storage: MariaDB (LAKKO_CHANNELS_DATABASE_URL set) — running migrations");
            let pool = MySqlPoolOptions::new()
                .max_connections(16)
                .connect(url)
                .await?;
            MIGRATIONS.run(&pool).await?;
            let tokens = TokenVerifier::new(pool.clone(), anon_config);
            (
                Arc::new(
                    SqlChannelStore::new(pool)
                        .with_envelope_retention(cfg.envelope_retention_epochs),
                ),
                tokens,
            )
        } else {
            warn!(
                "storage: in-memory (no LAKKO_CHANNELS_DATABASE_URL) — \
                 state is lost on restart, set the env var for production"
            );
            (
                Arc::new(
                    InMemoryChannelStore::new()
                        .with_envelope_retention(cfg.envelope_retention_epochs),
                ),
                TokenVerifier::in_memory(anon_config),
            )
        };
    anon_tokens.log_startup("channels");
    let _spent_pruner = anon_tokens.spawn_pruner();
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

    let state = AppState::new_with_votes(cfg.clone(), store, anon_tokens, votes);
    let app = router(Arc::clone(&state));

    if let Some(secret) = internal_secret {
        let internal = internal_router(state, secret);
        let internal_listener = TcpListener::bind(cfg.internal_bind).await?;
        info!(bind = %cfg.internal_bind, "internal moderation listener started");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(internal_listener, internal).await {
                warn!(error = %e, "internal listener stopped");
            }
        });
    } else {
        warn!(
            "internal moderation listener NOT started: LAKKO_CHANNELS_INTERNAL_SECRET is unset; \
             listings cannot be removed or blocked by the operator"
        );
    }

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
            // A public seed superseded before the post cutoff opens no post
            // that is left (ADR-0087 section 6).
            match store.prune_public_seeds_superseded_before(cutoff_ms).await {
                Ok(removed) if removed > 0 => info!(removed, "prune: public seeds tick complete"),
                Ok(_) => {},
                Err(e) => warn!(error = %e, "prune: public seeds tick failed"),
            }
        }

        // Invite keys a grace period past their expiry (ADR-0087 section 7).
        match store
            .prune_invite_keys_expired_before(now_ms.saturating_sub(INVITE_KEY_GRACE_MS))
            .await
        {
            Ok(removed) if removed > 0 => info!(removed, "prune: invite keys tick complete"),
            Ok(_) => {},
            Err(e) => warn!(error = %e, "prune: invite keys tick failed"),
        }

        // Owner request-proof nonces older than twice the proof window can
        // never be replayed (the window refuses them), so they go.
        match store
            .prune_owner_nonces_older_than(now_ms.saturating_sub(OWNER_NONCE_RETENTION_MS))
            .await
        {
            Ok(removed) if removed > 0 => info!(removed, "prune: owner nonces tick complete"),
            Ok(_) => {},
            Err(e) => warn!(error = %e, "prune: owner nonces tick failed"),
        }

        // Write-proof nonces (ADR-0087 section 13a), likewise.
        match store
            .prune_write_nonces_older_than(
                now_ms.saturating_sub(lakko_svc_channels::feedback_api::WRITE_NONCE_RETENTION_MS),
            )
            .await
        {
            Ok(removed) if removed > 0 => info!(removed, "prune: write nonces tick complete"),
            Ok(_) => {},
            Err(e) => warn!(error = %e, "prune: write nonces tick failed"),
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
