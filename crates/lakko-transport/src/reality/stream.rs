// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Post-handshake `Connection` wrapper for REALITY.
//!
//! After [`crate::reality::driver::drive_handshake`]
//! finishes, the caller owns:
//!
//! - an async socket already carrying TLS-encrypted
//!   bytes,
//! - a `rustls::ClientConnection` in the post-handshake
//!   "transport-mode" state (no more handshake messages
//!   expected, just app data).
//!
//! [`RealityConnection`] bundles those two halves and
//! implements the Lakko [`crate::transport::Connection`]
//! trait by shuttling length-prefixed frames through
//! rustls's encrypt + decrypt pipeline.
//!
//! ## Wire layout of app data (inside the TLS tunnel)
//!
//! ```text
//!     +-----------+----------------------+
//!     | u32-BE len |  payload (len bytes)|
//!     +-----------+----------------------+
//! ```
//!
//! Length cap is [`MAX_FRAME_BYTES`] (16 MiB) — same as
//! every other Lakko transport. The cap is enforced on
//! both `send` and `recv`, so a peer cannot trick us into
//! buffering more than 16 MiB by claiming a giant
//! length.

use std::io::{self, Cursor, Read as _, Write as _};

use async_trait::async_trait;
use bytes::Bytes;
use rustls::ClientConnection;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::error::{Error, Result};
use crate::transport::{Connection, MAX_FRAME_BYTES};

/// Length of the per-frame u32-BE length prefix.
const FRAME_LEN_PREFIX: usize = 4;

/// Per-call socket read buffer.
const SOCKET_READ_CHUNK: usize = 8 * 1024;

/// Post-handshake REALITY `Connection`.
///
/// Generic over the underlying socket type so tests can
/// swap in `tokio::io::duplex` mocks; the production
/// instantiation is `RealityConnection<tokio::net::TcpStream>`.
///
/// Send + 'static so the wrapper survives the
/// `Box<dyn Connection>` contract in
/// [`crate::transport::Transport::connect`].
pub struct RealityConnection<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    /// Async socket carrying TLS-encrypted bytes.
    socket: S,
    /// rustls state machine in post-handshake mode.
    conn: ClientConnection,
    /// Plaintext bytes that have been decrypted but not
    /// yet drained by `recv`. We buffer here because a
    /// single `read_tls + process_new_packets` cycle may
    /// surface several frames at once.
    rx_plaintext: Vec<u8>,
    /// `true` once `close` has been called. Subsequent
    /// calls return Ok immediately (idempotent close).
    closed: bool,
}

impl<S> RealityConnection<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    /// Wrap a post-handshake `ClientConnection` + socket
    /// into a Lakko `Connection`.
    ///
    /// The caller is responsible for confirming the
    /// handshake actually completed (typically: just
    /// returned from `drive_handshake`). This constructor
    /// does not re-verify.
    #[must_use]
    pub fn new(socket: S, conn: ClientConnection) -> Self {
        Self {
            socket,
            conn,
            rx_plaintext: Vec::with_capacity(8 * 1024),
            closed: false,
        }
    }

    /// Drain rustls's encrypted-write queue to the
    /// socket. Called after every plaintext write and
    /// once at close-notify time.
    async fn flush_writes(&mut self) -> Result<()> {
        // rustls's write_tls drains as many records as
        // are pending into a sync io::Write. We use a
        // Vec<u8> and then push it through the async
        // socket in one go.
        let mut buf = Vec::with_capacity(SOCKET_READ_CHUNK);
        while self.conn.wants_write() {
            self.conn
                .write_tls(&mut buf)
                .map_err(|e| Error::Io(format!("write_tls: {e}")))?;
        }
        if !buf.is_empty() {
            self.socket
                .write_all(&buf)
                .await
                .map_err(|e| Error::Io(format!("socket write_all: {e}")))?;
            self.socket
                .flush()
                .await
                .map_err(|e| Error::Io(format!("socket flush: {e}")))?;
        }
        Ok(())
    }

    /// Read until at least `needed` bytes of plaintext
    /// are buffered. Cycles socket reads + rustls
    /// decrypt until the buffer is big enough or peer
    /// closes.
    async fn read_until(&mut self, needed: usize) -> Result<()> {
        // Drain any plaintext that rustls has already
        // decrypted but not yet pumped into rx_plaintext.
        // This can happen when the handshake driver
        // (drive_handshake) fed records that contained
        // application data alongside the ServerFinished —
        // e.g. when the server bundles NewSessionTicket
        // and application-data in the same TCP burst.
        // Without this, read_until would block on
        // socket.read() even though the data is ready.
        self.pump_decrypt()?;
        while self.rx_plaintext.len() < needed {
            let mut chunk = [0u8; SOCKET_READ_CHUNK];
            let n = self
                .socket
                .read(&mut chunk)
                .await
                .map_err(|e| Error::Io(format!("socket read: {e}")))?;
            if n == 0 {
                // EOF on the socket. If we also have
                // decrypted plaintext sitting in our
                // buffer that satisfies the caller, the
                // outer loop catches that on next
                // iteration. Otherwise the peer closed
                // without sending the full frame.
                return Err(Error::PeerClosed);
            }
            // Feed the freshly-read bytes to rustls. The
            // sync Cursor adapter is the canonical
            // bridge from async I/O to rustls's
            // read_tls(&mut dyn io::Read).
            let mut cur = Cursor::new(&chunk[..n]);
            self.conn
                .read_tls(&mut cur)
                .map_err(|e| Error::Io(format!("read_tls: {e}")))?;
            self.conn
                .process_new_packets()
                .map_err(|e| Error::Io(format!("process_new_packets: {e}")))?;
            // Drain whatever plaintext rustls decoded.
            self.pump_decrypt()?;
            // rustls may also have new pending writes
            // (e.g. an ack / alert in response). Flush
            // them so we don't deadlock the peer.
            self.flush_writes().await?;
        }
        Ok(())
    }

    /// Pull every byte of currently-available decrypted
    /// plaintext out of rustls into `rx_plaintext`.
    fn pump_decrypt(&mut self) -> Result<()> {
        let mut buf = [0u8; SOCKET_READ_CHUNK];
        loop {
            match self.conn.reader().read(&mut buf) {
                Ok(0) => {
                    // Either no more plaintext right now
                    // OR peer sent close_notify. The
                    // outer flow distinguishes the two
                    // by checking socket state later;
                    // here we just stop pumping.
                    break;
                },
                Ok(n) => self.rx_plaintext.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(Error::Io(format!("rustls reader: {e}"))),
            }
        }
        Ok(())
    }
}

#[async_trait]
impl<S> Connection for RealityConnection<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    async fn send(&mut self, frame: Bytes) -> Result<()> {
        if self.closed {
            return Err(Error::LocalClosed);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(Error::FrameTooLarge {
                actual: frame.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        // u32 ≥ MAX_FRAME_BYTES (16 MiB) so the cast is
        // lossless under the cap.
        let len_be = u32::try_from(frame.len())
            .expect("bounded by MAX_FRAME_BYTES check above")
            .to_be_bytes();
        // Write into rustls's plaintext input. The
        // `writer()` handle implements io::Write; rustls
        // does the AEAD encryption internally + queues
        // ready-to-send TLS records.
        self.conn
            .writer()
            .write_all(&len_be)
            .map_err(|e| Error::Io(format!("rustls writer (len): {e}")))?;
        self.conn
            .writer()
            .write_all(&frame)
            .map_err(|e| Error::Io(format!("rustls writer (payload): {e}")))?;
        self.flush_writes().await
    }

    async fn recv(&mut self) -> Result<Bytes> {
        if self.closed {
            return Err(Error::LocalClosed);
        }
        // Wait for the 4-byte length prefix.
        self.read_until(FRAME_LEN_PREFIX).await?;
        let mut len_bytes = [0u8; FRAME_LEN_PREFIX];
        len_bytes.copy_from_slice(&self.rx_plaintext[..FRAME_LEN_PREFIX]);
        let len = u32::from_be_bytes(len_bytes) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(Error::FrameTooLarge {
                actual: len,
                limit: MAX_FRAME_BYTES,
            });
        }
        // Wait for the whole payload.
        self.read_until(FRAME_LEN_PREFIX + len).await?;
        // Peel off prefix + payload. Drain leaves the
        // tail for the next recv call.
        self.rx_plaintext.drain(..FRAME_LEN_PREFIX);
        let frame: Vec<u8> = self.rx_plaintext.drain(..len).collect();
        Ok(Bytes::from(frame))
    }

    async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        // Send TLS close_notify alert. rustls queues it
        // as an encrypted record; flush pushes it to the
        // peer.
        self.conn.send_close_notify();
        // Best-effort flush — if the socket is already
        // dead, we still mark ourselves closed.
        let _ = self.flush_writes().await;
        // Then shut down the socket's write half.
        let _ = self.socket.shutdown().await;
        self.closed = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// Spawn a fake post-handshake state for round-trip
    /// testing of the frame-codec math WITHOUT going
    /// through a real TLS session. We use a mock socket
    /// plus a default-constructed `ClientConnection` that
    /// will refuse `write_tls` without a handshake, so
    /// these tests only exercise the cheap-edge cases
    /// (frame-too-large, closed-after-close) where
    /// rustls is never called.
    fn empty_conn() -> ClientConnection {
        use std::sync::Arc;
        let provider = rustls::crypto::ring::default_provider();
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(
                crate::reality::cert::AcceptAnyCertVerifier::new(
                    &rustls::crypto::ring::default_provider(),
                )
                .into_arc(),
            )
            .with_no_client_auth();
        let server_name = "www.microsoft.com".try_into().unwrap();
        ClientConnection::new(Arc::new(cfg), server_name).unwrap()
    }

    #[tokio::test]
    async fn send_rejects_oversize_frame() {
        let (left, _right) = duplex(4096);
        let mut rc = RealityConnection::new(left, empty_conn());
        let huge = Bytes::from(vec![0u8; MAX_FRAME_BYTES + 1]);
        match rc.send(huge).await {
            Err(Error::FrameTooLarge { actual, limit }) => {
                assert!(actual > limit);
                assert_eq!(limit, MAX_FRAME_BYTES);
            },
            other => panic!("expected FrameTooLarge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn double_close_is_idempotent() {
        let (left, _right) = duplex(4096);
        let mut rc = RealityConnection::new(left, empty_conn());
        rc.close().await.expect("first close");
        rc.close().await.expect("second close is no-op");
    }

    #[tokio::test]
    async fn send_after_close_returns_local_closed() {
        let (left, _right) = duplex(4096);
        let mut rc = RealityConnection::new(left, empty_conn());
        rc.close().await.unwrap();
        match rc.send(Bytes::from_static(b"hi")).await {
            Err(Error::LocalClosed) => {},
            other => panic!("expected LocalClosed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_after_close_returns_local_closed() {
        let (left, _right) = duplex(4096);
        let mut rc = RealityConnection::new(left, empty_conn());
        rc.close().await.unwrap();
        match rc.recv().await {
            Err(Error::LocalClosed) => {},
            other => panic!("expected LocalClosed, got {other:?}"),
        }
    }
}
