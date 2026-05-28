// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `RealityTransport` — `Transport` trait impl for the
//! REALITY pluggable transport.
//!
//! ## What `connect` does
//!
//! 1. Resolves [`Endpoint`] to a TCP target and opens a
//!    [`tokio::net::TcpStream`] to it.
//! 2. Gets the current wall-clock timestamp (needed for the
//!    REALITY v2 authenticator embedded in the `ClientHello`).
//! 3. Calls [`crate::reality::driver::build_reality_client_connection`]
//!    which (a) computes the authenticator, (b) arms the
//!    per-thread [`crate::reality::rng::RealityRng`] inject slot,
//!    and (c) creates a `rustls::ClientConnection` so the
//!    authenticator lands in `legacy_session_id` INSIDE
//!    rustls's transcript hash — not patched after the fact.
//! 4. Runs [`crate::reality::driver::drive_handshake`] which
//!    drives the TLS state machine and optionally verifies the
//!    inbound `ServerHello.Random` (the server's mutual-auth
//!    proof) under the shared `tag_secret`.
//! 5. On success, hands the post-handshake socket + rustls
//!    state to [`crate::reality::stream::RealityConnection`]
//!    which implements the [`Connection`] trait via
//!    length-prefixed frames inside the TLS tunnel.
//!
//! Any failure on the path returns a typed [`Error`] — the
//! connection is **never** opened on a partial handshake (no
//! silent downgrade to plain TLS or plaintext, per CLAUDE.md
//! security invariant #4).

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::reality::authenticator::RealityTimestamp;
use crate::reality::config::{RealityConfig, RealityConfigError};
use crate::reality::driver::{
    build_reality_client_connection, drive_handshake, RealityDriverError,
};
use crate::reality::stream::RealityConnection;
use crate::transport::{Connection, Transport};

/// REALITY transport — typed wrapper around a parsed
/// [`RealityConfig`].
#[derive(Debug)]
pub struct RealityTransport {
    cfg: RealityConfig,
}

impl RealityTransport {
    /// Build the transport from a parsed config. Validates
    /// the config's lengths + formats upfront so a bad
    /// bootstrap config surfaces at construction, not at
    /// connect time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] when the
    /// underlying [`RealityConfig::parse`] fails. The
    /// `NotImplemented` shape (rather than `Config`) is
    /// preserved for backwards-compat with the bootstrap
    /// loader, which already branches on it.
    pub fn new(cfg: RealityConfig) -> Result<Self> {
        // Validate the config eagerly so any wire-shape
        // surprise surfaces here, not at connect time.
        cfg.parse().map_err(|e| Error::NotImplemented {
            feature: "reality (bad config)",
            sprint: match e {
                RealityConfigError::BadTagSecretBase64 => "S-5-reality (tag_secret_b64 invalid)",
                RealityConfigError::TagSecretLength { .. } => {
                    "S-5-reality (tag_secret wrong length)"
                },
                RealityConfigError::BadShortIdHex => "S-5-reality (short_id_hex invalid)",
                RealityConfigError::ShortIdLength { .. } => "S-5-reality (short_id wrong length)",
                RealityConfigError::EmptySni => "S-5-reality (sni empty)",
                RealityConfigError::EmptyRemote => "S-5-reality (remote empty)",
            },
        })?;
        Ok(Self { cfg })
    }

    /// Borrow the underlying config — handy for tests and
    /// for the future TLS-handshake-injection layer that
    /// needs to read the SNI / remote endpoint.
    #[must_use]
    pub fn config(&self) -> &RealityConfig {
        &self.cfg
    }
}

/// Resolve an [`Endpoint`] to a `tokio::net::TcpStream`.
///
/// `Direct` → connect by SocketAddr; `Hostname` → let
/// tokio resolve via system getaddrinfo. The bootstrap-
/// rendezvous DoH path resolves the cover host upstream
/// and hands us a `Direct` endpoint; the `Hostname` branch
/// is a fallback for tests / local dev.
async fn open_tcp(endpoint: &Endpoint) -> Result<TcpStream> {
    match endpoint {
        Endpoint::Direct(addr) => TcpStream::connect(addr)
            .await
            .map_err(|e| Error::Io(format!("tcp connect {addr}: {e}"))),
        Endpoint::Hostname { host, port } => TcpStream::connect((host.as_str(), *port))
            .await
            .map_err(|e| Error::Io(format!("tcp connect {host}:{port}: {e}"))),
    }
}

/// Map driver-side error variants to transport-layer
/// errors. Keeps the public error surface coarse — no
/// timing-side-channel leak through error discriminants.
fn map_driver_error(e: RealityDriverError) -> Error {
    match e {
        RealityDriverError::ConfigBuild(msg)
        | RealityDriverError::InvalidSni(msg)
        | RealityDriverError::ClientConnectionInit(msg)
        | RealityDriverError::Rustls(msg) => Error::Tls(msg),
        RealityDriverError::SocketIo(msg) => Error::Io(msg),
        RealityDriverError::PeerClosed => Error::PeerClosed,
        RealityDriverError::ExtractServerHello(e) => {
            Error::Tls(format!("extract server hello: {e}"))
        },
        RealityDriverError::ServerAuth(e) => Error::Tls(format!("reality server auth: {e}")),
        RealityDriverError::HandshakeBufferOverflow { cap } => {
            Error::Io(format!("handshake buffer exceeded {cap} bytes"))
        },
    }
}

#[async_trait]
impl Transport for RealityTransport {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        // 1. Re-parse the config to get typed
        //    (tag_secret, short_id). `new()` already
        //    validated the wire shape, so this is cheap
        //    + must not fail in practice.
        let (tag_secret, short_id) = self
            .cfg
            .parse()
            .map_err(|e| Error::Config(format!("reality config: {e}")))?;

        // 2. TCP connect to the resolved endpoint.
        let mut socket = open_tcp(endpoint).await?;

        // 3. Resolve the cover SNI to a rustls ServerName.
        let server_name = ServerName::try_from(self.cfg.sni.clone())
            .map_err(|e| Error::Tls(format!("invalid reality SNI {:?}: {e}", self.cfg.sni)))?;

        // 4. Current wall-clock for the authenticator's
        //    timestamp slot. Must be captured BEFORE
        //    build_reality_client_connection so both the
        //    authenticator baked into session_id AND the
        //    server-response verification use the same ts.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Config(format!("system clock before UNIX_EPOCH: {e}")))?
            .as_secs();
        let timestamp = RealityTimestamp(now);

        // 5. Arm the per-thread RNG inject slot + create
        //    the rustls ClientConnection. The authenticator
        //    lands in legacy_session_id INSIDE rustls's
        //    transcript hash. No .await between arm and
        //    new() — this call is fully synchronous.
        let mut conn =
            build_reality_client_connection(server_name, &tag_secret, &short_id, timestamp)
                .map_err(map_driver_error)?;

        // 6. Drive the handshake to completion. On Ok the
        //    socket carries TLS-encrypted bytes and `conn`
        //    is in transport-mode. Mutual-auth (verifying
        //    the server's HMAC in ServerHello.Random) is
        //    gated on `cfg.require_server_response`.
        drive_handshake(
            &mut socket,
            &mut conn,
            &tag_secret,
            &short_id,
            timestamp,
            self.cfg.require_server_response,
        )
        .await
        .map_err(map_driver_error)?;

        // 7. Wrap into the Connection-trait implementer.
        Ok(Box::new(RealityConnection::new(socket, conn)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

    fn happy_config() -> RealityConfig {
        RealityConfig {
            sni: "www.microsoft.com".into(),
            tag_secret_b64: B64.encode([0xCA; 32]),
            short_id_hex: hex::encode([0xAB; 8]),
            allowed_inner_servers: vec!["lakko.me".into()],
            remote: "203.0.113.7:443".into(),
            require_server_response: true,
        }
    }

    #[test]
    fn new_accepts_valid_config() {
        let cfg = happy_config();
        let t = RealityTransport::new(cfg.clone()).expect("valid config");
        assert_eq!(t.config(), &cfg);
    }

    #[test]
    fn new_rejects_bad_tag_secret() {
        let mut cfg = happy_config();
        cfg.tag_secret_b64 = "!!!".into();
        let err = RealityTransport::new(cfg).expect_err("bad config must reject");
        match err {
            Error::NotImplemented { feature, sprint } => {
                assert_eq!(feature, "reality (bad config)");
                assert!(sprint.contains("tag_secret_b64 invalid"));
            },
            other => panic!("expected NotImplemented (bad config), got {other:?}"),
        }
    }

    #[test]
    fn new_rejects_empty_sni() {
        let mut cfg = happy_config();
        cfg.sni = String::new();
        let err = RealityTransport::new(cfg).expect_err("empty sni must reject");
        match err {
            Error::NotImplemented { sprint, .. } => {
                assert!(sprint.contains("sni empty"));
            },
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    /// Connect against an unreachable endpoint surfaces a
    /// typed I/O error — proves the connect path is now
    /// wired beyond the old `NotImplemented` stub.
    ///
    /// We use `127.0.0.1:1` (privileged port, almost
    /// certainly no listener) so the test is deterministic
    /// and stays inside the loopback interface — no
    /// outbound traffic, no DNS, no firewall surprises.
    #[tokio::test]
    async fn connect_surfaces_io_error_on_unreachable_endpoint() {
        let t = RealityTransport::new(happy_config()).unwrap();
        let endpoint = Endpoint::direct("127.0.0.1:1".parse().unwrap());
        let err = t
            .connect(&endpoint)
            .await
            .err()
            .expect("connect to :1 must fail");
        match err {
            Error::Io(msg) => {
                assert!(
                    msg.contains("tcp connect"),
                    "expected tcp-connect prefix, got {msg:?}"
                );
            },
            other => panic!("expected Error::Io, got {other:?}"),
        }
    }

    /// Sanity that the SNI string is fed into rustls.
    /// `ServerName::try_from` rejects something that
    /// isn't a syntactically-valid DNS name or IP. We
    /// build the transport with `cfg.sni = "not a host"`,
    /// connect to a deliberately-reachable loopback port,
    /// and expect the TLS branch to fire BEFORE the
    /// handshake-overrun branch.
    ///
    /// The TCP connect runs first in our pipeline, so a
    /// loopback target that *accepts* but does nothing
    /// would let the TLS error fire next. To avoid pulling
    /// in a fake server, we instead bind a one-shot
    /// listener that immediately accepts + drops, giving
    /// the connect leg an Ok and forcing the bad SNI to
    /// fail at the `ServerName::try_from` step.
    #[tokio::test]
    async fn connect_rejects_bad_sni_after_tcp_connect() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();
        // accept+drop so our connect leg succeeds
        let _accept_task = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let mut cfg = happy_config();
        cfg.sni = "definitely not a hostname!".into();
        // Skip the eager validator (it doesn't check SNI
        // syntax beyond non-empty) by constructing the
        // transport directly with the new() path — the
        // `EmptySni` rejection above proves the eager
        // check fires when SNI is empty, but a non-empty
        // garbage SNI passes new() and only fails inside
        // rustls's ServerName::try_from.
        let t = RealityTransport::new(cfg).expect("non-empty garbage SNI passes new()");
        let endpoint = Endpoint::direct(bound);

        let err = t.connect(&endpoint).await.err().expect("must fail");
        match err {
            Error::Tls(msg) => {
                assert!(
                    msg.contains("invalid reality SNI"),
                    "expected SNI-rejection message, got {msg:?}"
                );
            },
            other => panic!("expected Error::Tls (bad SNI), got {other:?}"),
        }
    }
}
