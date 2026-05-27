// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Lakko SFU — Selective Forwarding Unit for group calls.
//!
//! This is the binary entry point. The module surface lives
//! in `src/lib.rs` so the integration tests under `tests/`
//! can share it. See ADR-0011 for the architectural
//! rationale; see `services/sfu/README.md` for the
//! operational layout and pinned dependencies.

#![forbid(unsafe_code)]

use std::time::Duration;

use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use lakko_svc_sfu::{
    build_app,
    config::Config,
    jwks::JwksCache,
    prom::{self, MetricsExporter},
    VERSION,
};

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

    // Build the signaling-side router. The peer registry
    // lives inside it; we keep the handle around in case a
    // future admin endpoint needs to inspect it (currently
    // unused, hence the `_`).
    let (router, _peers) = build_app(cfg.clone(), jwks.clone());

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
                metrics::gauge!(prom::names::JWKS_CACHE_AGE_SECS).set(age);
            }
        });
    }

    let signaling_listener = TcpListener::bind(cfg.signaling_bind).await?;
    let signaling = tokio::spawn(async move { axum::serve(signaling_listener, router).await });

    // Metrics exporter — separate socket so nginx can mTLS-
    // gate it independently of the signaling path.
    let metrics_exporter = exporter.clone();
    let metrics_bind = cfg.metrics_bind;
    let metrics_task = tokio::spawn(async move {
        if let Err(e) = prom::serve(metrics_exporter, metrics_bind).await {
            warn!(error = %e, "metrics: server exited with error");
        }
    });

    // Graceful shutdown — any of (Ctrl-C, SIGTERM, child task
    // exit) triggers a cooperative drain.
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

/// Honour a local `.env` file in debug builds only.
/// Production uses systemd `EnvironmentFile=/etc/lakko/lakko.env`;
/// running `dotenvy` in release would silently mask
/// production configuration with stale developer values.
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
