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
//! REALITY, Shadowsocks-2022 and WebTransport listeners run side by side; the
//! relay is transport-agnostic (it only needs a `Box<dyn Connection>`).
//!
//! MASQUE is the deliberate omission. `lakko_transport::MasqueListener` exists
//! and its handshake works, but CONNECT-UDP carries unreliable, unordered
//! datagrams — a response datagram is dropped on close, never retransmitted and
//! never flushed ahead of `CONNECTION_CLOSE` — so relaying a *reliable* internal
//! HTTP service over it does not work. The ignored
//! `masque_tunnel_relays_http_request_to_internal_origin` in
//! `tests/loopback_e2e.rs` records that finding. MASQUE is for tunnelling UDP;
//! wiring it here would need a reliability layer this proxy should not add.

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use lakko_svc_obfs_proxy::metrics::{self, ActiveTunnel, Transport};
use lakko_svc_obfs_proxy::{config, relay};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
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

    // /metrics on its own localhost port, off when unset (ADR-0096).
    lakko_metrics::start_from_env(
        "obfs-proxy",
        env!("CARGO_PKG_VERSION"),
        "LAKKO_OBFS_PROXY_METRICS_BIND",
    )
    .await?;
    let cfg = config::ProxyConfig::from_env()?;
    info!(internal_target = %cfg.internal_target, "obfs-proxy starting");

    if let Some(reality) = cfg.reality {
        spawn_reality_listener(reality, cfg.internal_target).await?;
    } else {
        warn!("REALITY listener not configured (LAKKO_OBFS_PROXY_REALITY_BIND unset)");
    }

    if let Some(shadowsocks) = cfg.shadowsocks {
        spawn_shadowsocks_listener(shadowsocks, cfg.internal_target).await?;
    } else {
        warn!("Shadowsocks-2022 listener not configured (LAKKO_OBFS_PROXY_SS_BIND unset)");
    }

    if let Some(webtransport) = cfg.webtransport {
        spawn_webtransport_listener(webtransport, cfg.internal_target)?;
    } else {
        warn!("WebTransport listener not configured (LAKKO_OBFS_PROXY_WT_BIND unset)");
    }

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ready" }))
        .route("/version", get(|| async { env!("CARGO_PKG_VERSION") }))
        // Requests by route template, method and status class (ADR-0096).
        .layer(lakko_metrics::red_layer("obfs-proxy"));
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
                    metrics::tunnel_accepted(Transport::Reality);
                    info!("accepted REALITY tunnel; relaying to internal target");
                    tokio::spawn(async move {
                        let _active = ActiveTunnel::start(Transport::Reality);
                        if let Err(error) = relay::relay(tunnel, internal_target).await {
                            warn!(%error, "relay ended with error");
                        }
                    });
                },
                Err(error) => {
                    metrics::tunnel_refused(Transport::Reality);
                    warn!(%error, "REALITY accept failed");
                },
            }
        }
    });
    Ok(())
}

/// Bind the Shadowsocks-2022 listener + spawn its accept loop. A single
/// `ReplayWindow` is shared across accepts (the SS-2022 anti-replay state); the
/// handshake holds the lock briefly, then each tunnel relays independently.
async fn spawn_shadowsocks_listener(
    shadowsocks: config::ShadowsocksProxyConfig,
    internal_target: std::net::SocketAddr,
) -> anyhow::Result<()> {
    let tcp = TcpListener::bind(shadowsocks.bind).await?;
    info!(bind = %shadowsocks.bind, "Shadowsocks-2022 listener up");
    let psk = shadowsocks.psk;
    let window = Arc::new(Mutex::new(lakko_transport::ShadowsocksReplayWindow::new()));
    tokio::spawn(async move {
        loop {
            match tcp.accept().await {
                Ok((stream, _peer)) => {
                    let window = window.clone();
                    tokio::spawn(async move {
                        let tunnel = {
                            let mut w = window.lock().await;
                            lakko_transport::shadowsocks_accept_inbound(stream, &psk, &mut w).await
                        };
                        match tunnel {
                            Ok(conn) => {
                                metrics::tunnel_accepted(Transport::Shadowsocks);
                                let _active = ActiveTunnel::start(Transport::Shadowsocks);
                                info!("accepted SS-2022 tunnel; relaying to internal target");
                                if let Err(error) = relay::relay(conn, internal_target).await {
                                    warn!(%error, "relay ended with error");
                                }
                            },
                            Err(error) => {
                                metrics::tunnel_refused(Transport::Shadowsocks);
                                warn!(%error, "SS-2022 handshake failed");
                            },
                        }
                    });
                },
                Err(error) => {
                    metrics::tunnel_refused(Transport::Shadowsocks);
                    warn!(%error, "SS-2022 accept failed");
                },
            }
        }
    });
    Ok(())
}

/// Bind the WebTransport (QUIC) listener + spawn its accept loop. A self-signed
/// cover cert is fine — the client pins the cert hash via the bootstrap config.
fn spawn_webtransport_listener(
    webtransport: config::WebTransportProxyConfig,
    internal_target: std::net::SocketAddr,
) -> anyhow::Result<()> {
    let (cert_chain, key) = lakko_transport::make_self_signed_cert(&webtransport.cover_sni)
        .map_err(|e| anyhow::anyhow!("build WT cover cert: {e}"))?;
    let mut listener =
        lakko_transport::WebTransportListener::bind(webtransport.bind, cert_chain, key)
            .map_err(|e| anyhow::anyhow!("bind WebTransport listener: {e}"))?;
    info!(bind = %webtransport.bind, sni = %webtransport.cover_sni, "WebTransport listener up");
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok(tunnel) => {
                    metrics::tunnel_accepted(Transport::WebTransport);
                    info!("accepted WebTransport tunnel; relaying to internal target");
                    tokio::spawn(async move {
                        let _active = ActiveTunnel::start(Transport::WebTransport);
                        if let Err(error) = relay::relay(tunnel, internal_target).await {
                            warn!(%error, "relay ended with error");
                        }
                    });
                },
                Err(error) => {
                    metrics::tunnel_refused(Transport::WebTransport);
                    warn!(%error, "WebTransport accept failed");
                },
            }
        }
    });
    Ok(())
}
