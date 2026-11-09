// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `RealityListener` — server-side accept loop for the
//! REALITY pluggable transport.
//!
//! ## What `accept` does
//!
//! 1. Waits for an inbound TCP connection on the bound
//!    socket.
//! 2. Builds a [`rustls::ServerConnection`] from the
//!    server config the listener was constructed with.
//! 3. Runs [`crate::reality::server_driver::drive_server_handshake`]
//!    which (a) extracts + verifies the inbound
//!    ClientHello.session_id against the allow-list of
//!    `(short_id, tag_secret)` pairs, and (b) patches the
//!    outbound ServerHello.Random with the REALITY v2
//!    server-response so the client can verify mutual auth.
//! 4. On success, hands the post-handshake socket + rustls
//!    state to [`crate::reality::server_stream::RealityServerConnection`]
//!    and returns both the `Box<dyn Connection>` and the
//!    typed [`AcceptedClient`] (which carries the verified
//!    `short_id` + client timestamp for routing /
//!    replay-window bookkeeping).
//!
//! Any handshake failure surfaces as a typed
//! [`crate::error::Error`]; the underlying TCP socket is
//! dropped (RST). There is **no** fallthrough to a real
//! cover-site proxy in this sprint — that lives outside
//! the driver, in a publisher-side proxy frontend that an
//! operator wires separately.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rustls::ServerConnection;
use tokio::net::{TcpListener, TcpStream};

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::reality::authenticator::RealityTimestamp;
use crate::reality::config::{RealityShortId, RealityTagSecret};
use crate::reality::server_driver::{
    drive_server_handshake, AcceptedClient, RealityServerDriverError,
};
use crate::reality::server_stream::RealityServerConnection;
use crate::transport::Connection;

/// REALITY server-side listener.
///
/// Owns the TCP listener + the rustls server config + the
/// publisher-distributed allow-list. `accept` drives a
/// REALITY v2 handshake on every inbound TCP socket.
pub struct RealityListener {
    tcp: TcpListener,
    tls_config: Arc<rustls::ServerConfig>,
    allowed_pairs: Vec<(RealityShortId, RealityTagSecret)>,
    tolerance_secs: u64,
}

impl RealityListener {
    /// Bind a `RealityListener` to a `tokio::net::TcpListener`
    /// plus the supplied rustls `ServerConfig` (typically
    /// built via
    /// [`crate::reality::server_driver::build_reality_server_config`]).
    ///
    /// `allowed_pairs` is the publisher-distributed allow-list
    /// of `(short_id, tag_secret)` pairs; `tolerance_secs`
    /// gates the receiver's clock-skew window on the
    /// inbound authenticator timestamp.
    #[must_use]
    pub fn new(
        tcp: TcpListener,
        tls_config: Arc<rustls::ServerConfig>,
        allowed_pairs: Vec<(RealityShortId, RealityTagSecret)>,
        tolerance_secs: u64,
    ) -> Self {
        Self {
            tcp,
            tls_config,
            allowed_pairs,
            tolerance_secs,
        }
    }

    /// Local address the TCP listener is bound to. Useful
    /// for tests that bind `127.0.0.1:0` and need the
    /// kernel-assigned port to dial the client back.
    ///
    /// # Errors
    ///
    /// Surfaces [`Error::Io`] if the underlying socket has
    /// no local address (should never fire on a bound
    /// listener).
    pub fn local_endpoint(&self) -> Result<Endpoint> {
        let addr = self
            .tcp
            .local_addr()
            .map_err(|e| Error::Io(format!("local_addr: {e}")))?;
        Ok(Endpoint::Direct(addr))
    }

    /// Accept the next inbound REALITY connection.
    ///
    /// Returns the post-handshake `Connection` boxed for
    /// type-erased downstream use, plus the typed
    /// [`AcceptedClient`] descriptor (verified `short_id` +
    /// client timestamp).
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] on TCP accept failure or driver
    ///   socket-I/O failure.
    /// - [`Error::Tls`] on rustls / handshake-level failure
    ///   (including a refused REALITY authenticator from
    ///   an active prober — that is treated as a TLS-side
    ///   refusal).
    /// - [`Error::PeerClosed`] if the peer dropped before
    ///   the handshake completed.
    pub async fn accept(&mut self) -> Result<(Box<dyn Connection>, AcceptedClient)> {
        let tcp = self.accept_tcp().await?;
        self.handshaker().handshake(tcp).await
    }

    /// Accept the next inbound TCP socket without running the handshake.
    ///
    /// A server runs [`RealityHandshaker::handshake`] for each socket in a
    /// task of its own with a timeout, so a client that connects and sends
    /// nothing never stops the listener accepting everyone else.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on TCP accept failure.
    pub async fn accept_tcp(&self) -> Result<TcpStream> {
        let (tcp, _peer_addr) = self
            .tcp
            .accept()
            .await
            .map_err(|e| Error::Io(format!("tcp accept: {e}")))?;
        Ok(tcp)
    }

    /// What a handshake task needs: the TLS config and the allow-list.
    #[must_use]
    pub fn handshaker(&self) -> RealityHandshaker {
        RealityHandshaker {
            tls_config: self.tls_config.clone(),
            allowed_pairs: Arc::new(self.allowed_pairs.clone()),
            tolerance_secs: self.tolerance_secs,
        }
    }
}

/// Runs the REALITY v2 server handshake on one accepted TCP socket. Cheap to
/// clone; one per handshake task.
#[derive(Clone)]
pub struct RealityHandshaker {
    tls_config: Arc<rustls::ServerConfig>,
    allowed_pairs: Arc<Vec<(RealityShortId, RealityTagSecret)>>,
    tolerance_secs: u64,
}

impl std::fmt::Debug for RealityHandshaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The allow-list holds secrets: only its size is shown.
        f.debug_struct("RealityHandshaker")
            .field("pairs", &self.allowed_pairs.len())
            .field("tolerance_secs", &self.tolerance_secs)
            .finish_non_exhaustive()
    }
}

impl RealityHandshaker {
    /// Drive the handshake on `tcp`. The caller bounds it with a timeout.
    ///
    /// # Errors
    ///
    /// As [`RealityListener::accept`], minus the TCP accept failure.
    pub async fn handshake(
        self,
        mut tcp: TcpStream,
    ) -> Result<(Box<dyn Connection>, AcceptedClient)> {
        let mut conn = ServerConnection::new(self.tls_config.clone())
            .map_err(|e| Error::Tls(format!("rustls ServerConnection::new: {e}")))?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Config(format!("system clock before UNIX_EPOCH: {e}")))?
            .as_secs();
        let now_ts = RealityTimestamp(now);

        let accepted = drive_server_handshake(
            &mut tcp,
            &mut conn,
            &self.allowed_pairs,
            now_ts,
            self.tolerance_secs,
        )
        .await
        .map_err(map_driver_error)?;

        let stream = RealityServerConnection::new(tcp, conn);
        Ok((Box::new(stream), accepted))
    }
}

/// Map driver-side error variants to transport-layer
/// errors. Mirrors the client-side
/// [`crate::reality::transport`] mapper — kept private
/// inside the listener module since it is not part of any
/// public surface.
fn map_driver_error(e: RealityServerDriverError) -> Error {
    match e {
        RealityServerDriverError::ConfigBuild(msg)
        | RealityServerDriverError::ServerConnectionInit(msg)
        | RealityServerDriverError::Rustls(msg) => Error::Tls(msg),
        RealityServerDriverError::SocketIo(msg) => Error::Io(msg),
        RealityServerDriverError::PeerClosed => Error::PeerClosed,
        RealityServerDriverError::ExtractClientHello(e) => {
            Error::Tls(format!("extract client hello: {e}"))
        },
        RealityServerDriverError::ClientAuth(e) => Error::Tls(format!("reality client auth: {e}")),
        RealityServerDriverError::HandshakeBufferOverflow { cap } => {
            Error::Io(format!("handshake buffer exceeded {cap} bytes"))
        },
    }
}
