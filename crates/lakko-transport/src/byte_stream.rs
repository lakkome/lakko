// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Byte-stream adapter over a framed [`Connection`] (ADR-0026 component 2).
//!
//! The obfuscated transports ([`crate::reality`], [`crate::shadowsocks`],
//! [`crate::webtransport_pt`]) expose a **message-oriented** tunnel — a
//! [`Connection`] with `async send(Bytes)` / `recv() -> Bytes` framed at
//! [`MAX_FRAME_BYTES`]. The HTTP-over-tunnel path ([`crate::tunnel_http`]) is
//! request/response, so it speaks frames directly. A **byte-oriented** protocol
//! that needs `AsyncRead + AsyncWrite` — chiefly a WebSocket client (the SFU
//! signaling channel) — cannot run over the framed tunnel directly.
//!
//! [`ByteStreamConnection`] bridges the two: it presents the framed tunnel as a
//! single `AsyncRead + AsyncWrite` byte stream so a `tokio`-based byte protocol
//! (e.g. `tokio_tungstenite::client_async`) can run over the obfuscated tunnel
//! unchanged. Writes are chunked into frames bounded by [`MAX_FRAME_BYTES`];
//! inbound frame payloads concatenate into the read stream. The transport's own
//! AEAD still authenticates + encrypts every byte; this adapter never inspects
//! the payload (the WebSocket / inner HTTP stays opaque to it).

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio::io::{
    duplex, split, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::transport::{Connection, MAX_FRAME_BYTES};

/// In-process buffer between the byte-stream consumer and the pump task.
const DUPLEX_BUFFER: usize = 64 * 1024;

/// Max bytes pulled from the consumer per outbound frame. Kept well under
/// [`MAX_FRAME_BYTES`] so a single read always fits one frame.
const OUTBOUND_CHUNK: usize = 32 * 1024;

/// Depth of the outbound frame channel feeding the single task that owns the
/// [`Connection`] (so it can `select!` between an inbound frame and an outbound
/// chunk without a second mutable borrow of the connection).
const OUTBOUND_DEPTH: usize = 32;

const _: () = assert!(OUTBOUND_CHUNK <= MAX_FRAME_BYTES);

/// A framed [`Connection`] presented as a single `AsyncRead + AsyncWrite` byte
/// stream (ADR-0026 component 2).
///
/// Construct with [`ByteStreamConnection::new`]; a background pump task owns the
/// underlying connection and shuttles bytes in both directions. Dropping the
/// stream aborts the pump and closes the tunnel.
pub struct ByteStreamConnection {
    inner: DuplexStream,
    pump: JoinHandle<()>,
}

impl ByteStreamConnection {
    /// Wrap a framed [`Connection`] as a byte stream. Spawns the pump task on
    /// the current `tokio` runtime.
    #[must_use]
    pub fn new(conn: Box<dyn Connection>) -> Self {
        let (near, far) = duplex(DUPLEX_BUFFER);
        let pump = tokio::spawn(pump(conn, far));
        Self { inner: near, pump }
    }
}

impl Drop for ByteStreamConnection {
    fn drop(&mut self) {
        // Tear down the pump; its final `conn.close()` runs on the next poll
        // before the abort lands, or the abort drops the connection (which the
        // transport treats as a close) — either way the tunnel is released.
        self.pump.abort();
    }
}

impl AsyncRead for ByteStreamConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for ByteStreamConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// The pump: owns the [`Connection`] and bridges it to the consumer's `far`
/// half of the duplex. Mirrors the `services/obfs-proxy` relay idiom — a reader
/// task funnels the consumer's writes into an mpsc channel so the single task
/// owning `conn` can `select!` between an inbound frame and an outbound chunk
/// without ever holding two mutable borrows of `conn`.
async fn pump(mut conn: Box<dyn Connection>, far: DuplexStream) {
    let (mut far_read, mut far_write) = split(far);
    let (tx, mut rx) = mpsc::channel::<Bytes>(OUTBOUND_DEPTH);

    // Consumer writes -> outbound channel.
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; OUTBOUND_CHUNK];
        loop {
            match far_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Bytes::copy_from_slice(&buf[..n])).await.is_err() {
                        break;
                    }
                },
            }
        }
    });

    loop {
        tokio::select! {
            inbound = conn.recv() => {
                match inbound {
                    // Tunnel -> consumer read stream.
                    Ok(frame) => {
                        if far_write.write_all(&frame).await.is_err() {
                            break;
                        }
                    },
                    // Tunnel closed (clean or error) — tear down.
                    Err(_) => break,
                }
            }
            outbound = rx.recv() => {
                match outbound {
                    // Consumer write -> tunnel frame.
                    Some(frame) => {
                        if conn.send(frame).await.is_err() {
                            break;
                        }
                    },
                    // Consumer closed the write side — tear down.
                    None => break,
                }
            }
        }
    }

    reader.abort();
    let _ = far_write.shutdown().await;
    let _ = conn.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::pair;

    #[tokio::test]
    async fn consumer_writes_surface_as_frames_on_the_peer() {
        let (a, mut b) = pair();
        let mut stream = ByteStreamConnection::new(a);

        stream.write_all(b"hello tunnel").await.unwrap();
        stream.flush().await.unwrap();

        // The peer connection receives the written bytes (as one or more
        // frames; a single small write is one frame).
        let frame = b.recv().await.unwrap();
        assert_eq!(&frame[..], b"hello tunnel");
    }

    #[tokio::test]
    async fn peer_frames_surface_as_bytes_on_the_consumer() {
        let (a, mut b) = pair();
        let mut stream = ByteStreamConnection::new(a);

        b.send(Bytes::from_static(b"server says hi")).await.unwrap();

        let mut buf = [0u8; 14];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"server says hi");
    }

    #[tokio::test]
    async fn full_duplex_round_trip() {
        let (a, mut b) = pair();
        let mut stream = ByteStreamConnection::new(a);

        // Consumer -> peer.
        stream.write_all(b"ping").await.unwrap();
        stream.flush().await.unwrap();
        let got = b.recv().await.unwrap();
        assert_eq!(&got[..], b"ping");

        // Peer -> consumer, concatenated across two frames.
        b.send(Bytes::from_static(b"po")).await.unwrap();
        b.send(Bytes::from_static(b"ng")).await.unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[tokio::test]
    async fn large_write_chunks_into_bounded_frames() {
        let (a, mut b) = pair();
        let mut stream = ByteStreamConnection::new(a);

        // Larger than one OUTBOUND_CHUNK so the pump emits multiple frames.
        let payload = vec![0xABu8; OUTBOUND_CHUNK + 1234];
        stream.write_all(&payload).await.unwrap();
        stream.flush().await.unwrap();

        // Reassemble frames until we have the whole payload; each frame is
        // bounded by OUTBOUND_CHUNK (well under MAX_FRAME_BYTES).
        let mut received = Vec::new();
        while received.len() < payload.len() {
            let frame = b.recv().await.unwrap();
            assert!(
                frame.len() <= OUTBOUND_CHUNK,
                "frame exceeded the chunk bound"
            );
            received.extend_from_slice(&frame);
        }
        assert_eq!(received, payload);
    }
}
