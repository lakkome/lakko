// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The byte-relay at the heart of the obfs proxy (ADR-0026, Phase 1).
//!
//! Each accepted obfuscated tunnel is a framed [`Connection`]
//! (`send(frame)` / `recv() -> frame`). The relay shuttles its bytes to a
//! fixed **internal** HTTP target over a plain `TcpStream`, in both directions,
//! until either side closes:
//!
//! - tunnel → TCP: every frame received from the client is written verbatim to
//!   the internal socket.
//! - TCP → tunnel: every chunk read from the internal socket is wrapped in a
//!   frame and sent back to the client.
//!
//! The relay is a **dumb byte-forwarder** — it never parses the inner HTTP,
//! never inspects application data (which is MLS-E2EE), and only ever talks to
//! the one configured internal address (no open relay). The obfs layer already
//! provides the on-the-wire confidentiality; this just bridges the framed
//! tunnel to the byte-oriented service socket.

use std::net::SocketAddr;

use bytes::Bytes;
use lakko_transport::Connection;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::metrics::{relay_ended, RelayEnd};

/// Read-chunk size for the internal-socket → tunnel direction. Bounded well
/// under `lakko_transport::MAX_FRAME_BYTES` so each chunk maps to one frame.
const RELAY_READ_CHUNK: usize = 16 * 1024;

/// Back-pressure depth for the internal-socket → tunnel channel.
const RELAY_CHANNEL_DEPTH: usize = 32;

/// Relay `tunnel` ↔ a fresh TCP connection to `target` until either side
/// closes. Returns `Ok(())` on a clean close from either end.
///
/// # Errors
///
/// Returns an error only if the initial TCP connect to `target` fails; once
/// the relay loop is running, a peer/socket close is a normal `Ok(())`
/// termination (the caller does not distinguish which side hung up).
pub async fn relay(mut tunnel: Box<dyn Connection>, target: SocketAddr) -> anyhow::Result<()> {
    let tcp = match TcpStream::connect(target).await {
        Ok(tcp) => tcp,
        Err(e) => {
            relay_ended(RelayEnd::ConnectFailed);
            return Err(e.into());
        },
    };
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    // Internal-socket → tunnel frames flow through this channel so the single
    // task that owns `tunnel` can `select!` between an inbound frame and an
    // outbound chunk without a second mutable borrow of `tunnel`.
    let (tx, mut rx) = mpsc::channel::<Bytes>(RELAY_CHANNEL_DEPTH);

    // Pump: internal socket → channel.
    let pump = tokio::spawn(async move {
        let mut buf = vec![0u8; RELAY_READ_CHUNK];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Bytes::copy_from_slice(&buf[..n])).await.is_err() {
                        break;
                    }
                },
            }
        }
    });

    let end = loop {
        tokio::select! {
            inbound = tunnel.recv() => {
                match inbound {
                    // Client → internal socket.
                    Ok(frame) => {
                        if tcp_write.write_all(&frame).await.is_err() {
                            break RelayEnd::InternalWriteFailed;
                        }
                    },
                    // Tunnel closed (clean or error) — tear down.
                    Err(_) => break RelayEnd::Closed,
                }
            }
            outbound = rx.recv() => {
                match outbound {
                    // Internal socket → client.
                    Some(frame) => {
                        if tunnel.send(frame).await.is_err() {
                            break RelayEnd::TunnelSendFailed;
                        }
                    },
                    // Internal socket closed — tear down.
                    None => break RelayEnd::Closed,
                }
            }
        }
    };
    relay_ended(end);

    pump.abort();
    let _ = tcp_write.shutdown().await;
    let _ = tunnel.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_transport::in_memory;
    use tokio::net::TcpListener;

    /// Spawn a local TCP echo server; returns its bound address.
    async fn spawn_echo() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    loop {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if sock.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            },
                        }
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn relay_round_trips_through_internal_echo() {
        let echo = spawn_echo().await;
        // `client` is the obfs-client side; `server` is what the proxy holds.
        let (mut client, server) = in_memory::pair();
        tokio::spawn(async move {
            let _ = relay(server, echo).await;
        });

        // A frame sent by the client is relayed to the echo and comes back.
        client
            .send(Bytes::from_static(b"GET /health HTTP/1.0\r\n\r\n"))
            .await
            .unwrap();
        let echoed = client.recv().await.unwrap();
        assert_eq!(&echoed[..], b"GET /health HTTP/1.0\r\n\r\n");
    }

    #[tokio::test]
    async fn relay_forwards_multiple_frames_in_order() {
        let echo = spawn_echo().await;
        let (mut client, server) = in_memory::pair();
        tokio::spawn(async move {
            let _ = relay(server, echo).await;
        });

        client.send(Bytes::from_static(b"alpha")).await.unwrap();
        let a = client.recv().await.unwrap();
        assert_eq!(&a[..], b"alpha");

        client.send(Bytes::from_static(b"bravo")).await.unwrap();
        let b = client.recv().await.unwrap();
        assert_eq!(&b[..], b"bravo");
    }

    #[tokio::test]
    async fn relay_errors_on_unreachable_target() {
        // Port 1 on loopback is not listening — the initial connect fails.
        let (_, server) = in_memory::pair();
        let bad: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(relay(server, bad).await.is_err());
    }

    /// ADR-0096: a relay counts how it ended, and the exposition carries no
    /// address.
    #[tokio::test]
    async fn relay_counts_how_it_ended() {
        let metrics = lakko_metrics::install("obfs-proxy", "0.0.0").unwrap();
        let (_, server) = in_memory::pair();
        let bad: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(relay(server, bad).await.is_err());

        let echo = spawn_echo().await;
        let (client, server) = in_memory::pair();
        drop(client);
        relay(server, echo).await.unwrap();

        let text = metrics.render();
        lakko_metrics::exposition::assert_clean(&text, &["transport"], &[]);
        assert!(!text.contains("127.0.0.1"), "{text}");
        for line in [
            "lakko_obfs_proxy_relays_total{outcome=\"connect_failed\"}",
            "lakko_obfs_proxy_relays_total{outcome=\"closed\"}",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
    }
}
