// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-svc-obfs-proxy` — the obfs-terminating proxy (ADR-0026, Phase 1).
//!
//! Runs the REALITY listener on a public TCP port, terminates each tunnel into
//! a framed [`lakko_transport::Connection`], and byte-relays it to a fixed
//! internal HTTP target (see [`relay`]). A small control-plane HTTP surface
//! (`/health` `/ready` `/version`) serves ops + load-balancer probes; the obfs
//! port itself is raw, not HTTP.
//!
//! Shadowsocks-2022, MASQUE, and WebTransport listeners (the QUIC/UDP ones)
//! slot in beside the REALITY accept loop in a follow-up — the relay is
//! transport-agnostic (it only needs a `Box<dyn Connection>`).

mod config;
mod relay;

use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    {
        let _ = dotenvy::dotenv();
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = config::ProxyConfig::from_env()?;
    info!(internal_target = %cfg.internal_target, "obfs-proxy starting");

    if let Some(reality) = cfg.reality {
        spawn_reality_listener(reality, cfg.internal_target).await?;
    } else {
        warn!("REALITY listener not configured (LAKKO_OBFS_PROXY_REALITY_BIND unset) — control-plane only");
    }

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ready" }))
        .route("/version", get(|| async { env!("CARGO_PKG_VERSION") }));
    let control = TcpListener::bind(cfg.control_bind).await?;
    info!(bind = %cfg.control_bind, "control plane up");
    axum::serve(control, app).await?;
    Ok(())
}

/// Bind the REALITY listener + spawn its accept loop. Each accepted tunnel
/// gets its own relay task to the internal target.
async fn spawn_reality_listener(
    reality: config::RealityProxyConfig,
    internal_target: std::net::SocketAddr,
) -> anyhow::Result<()> {
    let tcp = TcpListener::bind(reality.bind).await?;
    info!(bind = %reality.bind, pairs = reality.allowed_pairs.len(), sni = %reality.cover_sni, "REALITY listener up");
    // A self-signed cover cert for the configured SNI — REALITY's security
    // comes from the session_id/ServerHello authenticator chain, not PKI.
    let (cert_chain, key) = lakko_transport::make_self_signed_cert(&reality.cover_sni)
        .map_err(|e| anyhow::anyhow!("build cover cert: {e}"))?;
    let tls = lakko_transport::build_reality_server_config(cert_chain, key)
        .map_err(|e| anyhow::anyhow!("build REALITY server config: {e}"))?;
    let mut listener = lakko_transport::reality::listener::RealityListener::new(
        tcp,
        std::sync::Arc::new(tls),
        reality.allowed_pairs,
        reality.tolerance_secs,
    );
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((tunnel, _accepted)) => {
                    info!("accepted REALITY tunnel; relaying to internal target");
                    tokio::spawn(async move {
                        if let Err(error) = relay::relay(tunnel, internal_target).await {
                            warn!(%error, "relay ended with error");
                        }
                    });
                },
                Err(error) => {
                    warn!(%error, "REALITY accept failed");
                },
            }
        }
    });
    Ok(())
}
