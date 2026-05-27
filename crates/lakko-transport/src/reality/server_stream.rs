// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Post-handshake `Connection` wrapper for the REALITY
//! **server** side.
//!
//! Mirrors [`crate::reality::stream::RealityConnection`]
//! (client side): same u32-BE length-prefixed framing,
//! same `MAX_FRAME_BYTES` cap on both directions, same
//! idempotent close — the only difference is the rustls
//! state machine type (`ServerConnection` here vs
//! `ClientConnection` on the client).
//!
//! See [`crate::reality::stream`] for the wire-layout doc.

use std::io::{self, Cursor, Read as _, Write as _};

use async_trait::async_trait;
use bytes::Bytes;
use rustls::ServerConnection;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::error::{Error, Result};
use crate::transport::{Connection, MAX_FRAME_BYTES};

/// Length of the per-frame u32-BE length prefix.
const FRAME_LEN_PREFIX: usize = 4;

/// Per-call socket read buffer.
const SOCKET_READ_CHUNK: usize = 8 * 1024;

/// Post-handshake REALITY server `Connection`.
///
/// Generic over the underlying socket so tests can swap in
/// `tokio::io::duplex` mocks; the production instantiation
/// is `RealityServerConnection<tokio::net::TcpStream>`.
pub struct RealityServerConnection<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    /// Async socket carrying TLS-encrypted bytes.
    socket: S,
    /// rustls state machine in post-handshake mode.
    conn: ServerConnection,
    /// Plaintext bytes decrypted but not yet drained by
    /// `recv`.
    rx_plaintext: Vec<u8>,
    /// `true` once `close` has been called.
    closed: bool,
}

impl<S> RealityServerConnection<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    /// Wrap a post-handshake `ServerConnection` + socket
    /// into a Lakko `Connection`. The caller must confirm
    /// the handshake completed (typically: just returned
    /// from `drive_server_handshake`); this constructor
    /// does not re-verify.
    #[must_use]
    pub fn new(socket: S, conn: ServerConnection) -> Self {
        Self {
            socket,
            conn,
            rx_plaintext: Vec::with_capacity(8 * 1024),
            closed: false,
        }
    }

    /// Drain rustls's encrypted-write queue to the socket.
    async fn flush_writes(&mut self) -> Result<()> {
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

    /// Read until at least `needed` bytes of plaintext are
    /// buffered.
    async fn read_until(&mut self, needed: usize) -> Result<()> {
        while self.rx_plaintext.len() < needed {
            let mut chunk = [0u8; SOCKET_READ_CHUNK];
            let n = self
                .socket
                .read(&mut chunk)
                .await
                .map_err(|e| Error::Io(format!("socket read: {e}")))?;
            if n == 0 {
                return Err(Error::PeerClosed);
            }
            let mut cur = Cursor::new(&chunk[..n]);
            self.conn
                .read_tls(&mut cur)
                .map_err(|e| Error::Io(format!("read_tls: {e}")))?;
            self.conn
                .process_new_packets()
                .map_err(|e| Error::Io(format!("process_new_packets: {e}")))?;
            self.pump_decrypt()?;
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
                Ok(0) => break,
                Ok(n) => self.rx_plaintext.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(Error::Io(format!("rustls reader: {e}"))),
            }
        }
        Ok(())
    }
}

#[async_trait]
impl<S> Connection for RealityServerConnection<S>
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
        let len_be = u32::try_from(frame.len())
            .expect("bounded by MAX_FRAME_BYTES check above")
            .to_be_bytes();
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
        self.read_until(FRAME_LEN_PREFIX + len).await?;
        self.rx_plaintext.drain(..FRAME_LEN_PREFIX);
        let frame: Vec<u8> = self.rx_plaintext.drain(..len).collect();
        Ok(Bytes::from(frame))
    }

    async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.conn.send_close_notify();
        let _ = self.flush_writes().await;
        let _ = self.socket.shutdown().await;
        self.closed = true;
        Ok(())
    }
}
