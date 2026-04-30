// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! In-memory transport for tests and local IPC.
//!
//! Provides:
//!
//! - [`pair()`] — directly connected `(Connection, Connection)` for the
//!   simplest case (no transport / listener at all).
//! - [`switch()`] — a connected `(Transport, Listener)` pair where every
//!   `Transport::connect` produces a fresh `Connection` on both sides.
//!
//! No networking, no serialization — frames are passed between tokio
//! channels. Behaviour matches a perfect bidirectional message link.

use std::net::SocketAddr;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::transport::{Connection, Listener, Transport, MAX_FRAME_BYTES};

/// Directly connected `(Connection, Connection)` pair.
#[must_use]
pub fn pair() -> (Box<dyn Connection>, Box<dyn Connection>) {
    let (a_to_b_tx, a_to_b_rx) = mpsc::channel::<Bytes>(64);
    let (b_to_a_tx, b_to_a_rx) = mpsc::channel::<Bytes>(64);

    let alice = InMemoryConnection {
        tx: Some(a_to_b_tx),
        rx: b_to_a_rx,
    };
    let bob = InMemoryConnection {
        tx: Some(b_to_a_tx),
        rx: a_to_b_rx,
    };
    (Box::new(alice), Box::new(bob))
}

/// Build a connected `(Transport, Listener)` pair.
///
/// Every `Transport::connect` call hands a fresh `Connection` to the
/// listener via `Listener::accept`. The pair has a synthetic local address
/// so tests can compare endpoints meaningfully.
#[must_use]
pub fn switch() -> (InMemoryTransport, InMemoryListener) {
    let local: SocketAddr = "127.0.0.1:0".parse().expect("static addr parses");
    let (accept_tx, accept_rx) = mpsc::channel::<Box<dyn Connection>>(64);
    (
        InMemoryTransport {
            accept_tx,
            local_endpoint: Endpoint::direct(local),
        },
        InMemoryListener {
            accept_rx,
            local_endpoint: Endpoint::direct(local),
        },
    )
}

/// In-memory transport (client side of a [`switch`]).
#[derive(Clone)]
pub struct InMemoryTransport {
    accept_tx: mpsc::Sender<Box<dyn Connection>>,
    local_endpoint: Endpoint,
}

impl InMemoryTransport {
    /// Endpoint reported as "remote" / "local" for tests.
    #[must_use]
    pub fn local_endpoint(&self) -> &Endpoint {
        &self.local_endpoint
    }
}

#[async_trait]
impl Transport for InMemoryTransport {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        let (client, server) = pair();
        self.accept_tx
            .send(server)
            .await
            .map_err(|_| Error::Setup("listener dropped".into()))?;
        Ok(client)
    }
}

/// In-memory listener (server side of a [`switch`]).
pub struct InMemoryListener {
    accept_rx: mpsc::Receiver<Box<dyn Connection>>,
    local_endpoint: Endpoint,
}

#[async_trait]
impl Listener for InMemoryListener {
    async fn accept(&mut self) -> Result<Box<dyn Connection>> {
        self.accept_rx
            .recv()
            .await
            .ok_or(Error::Setup("transport dropped".into()))
    }

    fn local_endpoint(&self) -> Endpoint {
        self.local_endpoint.clone()
    }
}

/// Concrete connection type. Bidirectional, mpsc-backed.
struct InMemoryConnection {
    tx: Option<mpsc::Sender<Bytes>>,
    rx: mpsc::Receiver<Bytes>,
}

#[async_trait]
impl Connection for InMemoryConnection {
    async fn send(&mut self, frame: Bytes) -> Result<()> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err(Error::FrameTooLarge {
                actual: frame.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        let tx = self.tx.as_ref().ok_or(Error::LocalClosed)?;
        tx.send(frame).await.map_err(|_| Error::PeerClosed)
    }

    async fn recv(&mut self) -> Result<Bytes> {
        self.rx.recv().await.ok_or(Error::PeerClosed)
    }

    async fn close(&mut self) -> Result<()> {
        self.tx = None;
        self.rx.close();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pair_roundtrips_frames() {
        let (mut alice, mut bob) = pair();

        alice.send(Bytes::from_static(b"hello bob")).await.unwrap();
        bob.send(Bytes::from_static(b"hi alice")).await.unwrap();

        assert_eq!(bob.recv().await.unwrap(), Bytes::from_static(b"hello bob"));
        assert_eq!(alice.recv().await.unwrap(), Bytes::from_static(b"hi alice"));
    }

    #[tokio::test]
    async fn close_then_recv_signals_peer_closed() {
        let (mut alice, mut bob) = pair();
        alice.close().await.unwrap();
        let result = bob.recv().await;
        assert!(matches!(result, Err(Error::PeerClosed)));
    }

    #[tokio::test]
    async fn frame_too_large_is_rejected_before_send() {
        let (mut alice, _bob) = pair();
        let oversize = Bytes::from(vec![0u8; MAX_FRAME_BYTES + 1]);
        let result = alice.send(oversize).await;
        assert!(matches!(result, Err(Error::FrameTooLarge { .. })));
    }

    #[tokio::test]
    async fn switch_accepts_multiple_connections() {
        let (transport, mut listener) = switch();

        let mut clients = Vec::new();
        for _ in 0..3 {
            let conn = transport
                .connect(&Endpoint::direct("127.0.0.1:0".parse().unwrap()))
                .await
                .unwrap();
            clients.push(conn);
        }

        for (i, mut client) in clients.into_iter().enumerate() {
            let mut server = listener.accept().await.unwrap();

            let payload = Bytes::from(format!("conn-{i}"));
            client.send(payload.clone()).await.unwrap();
            assert_eq!(server.recv().await.unwrap(), payload);

            server.send(Bytes::from(format!("ack-{i}"))).await.unwrap();
            assert_eq!(
                client.recv().await.unwrap(),
                Bytes::from(format!("ack-{i}"))
            );
        }
    }
}
