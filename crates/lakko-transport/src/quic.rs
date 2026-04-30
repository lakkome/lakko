// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! QUIC transport for Lakko (powered by [`quinn`]).
//!
//! Each Lakko [`Connection`] is backed by a single QUIC bi-directional
//! stream (the client opens, the server accepts). Frames are length-prefixed
//! with a 4-byte big-endian length header.
//!
//! Phase 0 ships a usable client + listener with self-signed certificate
//! helpers for tests. Production-grade certificate management (cert pinning,
//! CT log verification, ACME) and pluggable-transport adapters land in
//! Phase 1+ and Phase 5 respectively.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, ServerConfig};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::transport::{Connection, Listener, Transport, MAX_FRAME_BYTES};

/// QUIC ALPN identifier negotiated by Lakko endpoints.
///
/// Distinct ALPN avoids accidental peering with non-Lakko QUIC traffic on
/// shared ports.
pub const ALPN: &[u8] = b"lakko/0";

/// Self-signed certificate material for tests / dev environments.
pub struct SelfSignedCert {
    /// Certificate chain (single self-signed cert).
    pub chain: Vec<CertificateDer<'static>>,
    /// PKCS#8-encoded private key.
    pub key: PrivateKeyDer<'static>,
}

impl SelfSignedCert {
    /// Generate a fresh self-signed certificate covering `subject`.
    ///
    /// `subject` becomes a `DNSName` SAN, so the client must connect with
    /// the same SNI for verification to succeed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Tls`] on certificate generation failure.
    pub fn generate(subject: &str) -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec![subject.to_string()])
            .map_err(|e| Error::Tls(format!("rcgen: {e}")))?;
        let chain = vec![CertificateDer::from(cert.cert.der().to_vec())];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
        Ok(Self { chain, key })
    }
}

/// Outbound QUIC transport.
pub struct QuicTransport {
    endpoint: quinn::Endpoint,
}

impl QuicTransport {
    /// Build a client transport that trusts `roots`.
    ///
    /// The transport binds to `local`; pass `0.0.0.0:0` to let the OS pick
    /// a port.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Tls`] for invalid trust anchors and [`Error::Setup`]
    /// for socket binding failures.
    pub fn client(local: SocketAddr, roots: Vec<CertificateDer<'static>>) -> Result<Self> {
        let mut store = rustls::RootCertStore::empty();
        for cert in roots {
            store
                .add(cert)
                .map_err(|e| Error::Tls(format!("invalid root cert: {e}")))?;
        }

        let mut tls_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(store)
            .with_no_client_auth();
        tls_cfg.alpn_protocols = vec![ALPN.to_vec()];

        let quic_cfg = QuicClientConfig::try_from(tls_cfg)
            .map_err(|e| Error::Tls(format!("client config: {e}")))?;

        let mut endpoint =
            quinn::Endpoint::client(local).map_err(|e| Error::Setup(format!("bind: {e}")))?;
        endpoint.set_default_client_config(ClientConfig::new(Arc::new(quic_cfg)));

        Ok(Self { endpoint })
    }
}

#[async_trait]
impl Transport for QuicTransport {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        let (addr, server_name) = match endpoint {
            Endpoint::Direct(addr) => (*addr, "localhost".to_string()),
            Endpoint::Hostname { host, port } => {
                let resolved = tokio::net::lookup_host((host.as_str(), *port))
                    .await
                    .map_err(|e| Error::Setup(format!("dns: {e}")))?
                    .next()
                    .ok_or_else(|| Error::Setup(format!("dns: no addresses for {host}")))?;
                (resolved, host.clone())
            },
        };

        let connecting = self
            .endpoint
            .connect(addr, &server_name)
            .map_err(|e| Error::Setup(format!("connect: {e}")))?;
        let conn = connecting
            .await
            .map_err(|e| Error::Setup(format!("handshake: {e}")))?;

        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| Error::Io(format!("open_bi: {e}")))?;

        Ok(Box::new(QuicConnection {
            conn: Some(conn),
            send: Some(send),
            recv,
        }))
    }
}

/// Inbound QUIC listener.
pub struct QuicListener {
    endpoint: quinn::Endpoint,
}

impl QuicListener {
    /// Bind a listener with the given certificate chain.
    ///
    /// `local` may be `0.0.0.0:0` to let the OS pick a port; check
    /// [`Listener::local_endpoint`] afterwards for the actual address.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Tls`] for an invalid cert chain and [`Error::Setup`]
    /// for socket binding failures.
    pub fn bind(local: SocketAddr, cert: SelfSignedCert) -> Result<Self> {
        let mut tls_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert.chain, cert.key)
            .map_err(|e| Error::Tls(format!("server cert: {e}")))?;
        tls_cfg.alpn_protocols = vec![ALPN.to_vec()];

        let quic_cfg = QuicServerConfig::try_from(tls_cfg)
            .map_err(|e| Error::Tls(format!("server config: {e}")))?;
        let server_cfg = ServerConfig::with_crypto(Arc::new(quic_cfg));

        let endpoint = quinn::Endpoint::server(server_cfg, local)
            .map_err(|e| Error::Setup(format!("bind: {e}")))?;

        Ok(Self { endpoint })
    }
}

#[async_trait]
impl Listener for QuicListener {
    async fn accept(&mut self) -> Result<Box<dyn Connection>> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| Error::Setup("listener closed".into()))?;
        let conn = incoming
            .await
            .map_err(|e| Error::Setup(format!("handshake: {e}")))?;

        let (send, recv) = conn
            .accept_bi()
            .await
            .map_err(|e| Error::Io(format!("accept_bi: {e}")))?;

        Ok(Box::new(QuicConnection {
            conn: Some(conn),
            send: Some(send),
            recv,
        }))
    }

    fn local_endpoint(&self) -> Endpoint {
        match self.endpoint.local_addr() {
            Ok(addr) => Endpoint::Direct(addr),
            Err(_) => Endpoint::Direct("0.0.0.0:0".parse().expect("static addr parses")),
        }
    }
}

/// QUIC bi-stream wrapped as a Lakko [`Connection`].
struct QuicConnection {
    conn: Option<quinn::Connection>,
    send: Option<quinn::SendStream>,
    recv: quinn::RecvStream,
}

#[async_trait]
impl Connection for QuicConnection {
    async fn send(&mut self, frame: Bytes) -> Result<()> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err(Error::FrameTooLarge {
                actual: frame.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        let send = self.send.as_mut().ok_or(Error::LocalClosed)?;

        let len = u32::try_from(frame.len()).map_err(|_| Error::FrameTooLarge {
            actual: frame.len(),
            limit: MAX_FRAME_BYTES,
        })?;
        send.write_all(&len.to_be_bytes())
            .await
            .map_err(|e| Error::Io(format!("write len: {e}")))?;
        send.write_all(&frame)
            .await
            .map_err(|e| Error::Io(format!("write body: {e}")))?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Bytes> {
        let mut len_buf = [0u8; 4];
        self.recv.read_exact(&mut len_buf).await.map_err(|e| {
            use quinn::ReadExactError;
            match e {
                ReadExactError::FinishedEarly(0) => Error::PeerClosed,
                _ => Error::Io(format!("read len: {e}")),
            }
        })?;

        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(Error::FrameTooLarge {
                actual: len,
                limit: MAX_FRAME_BYTES,
            });
        }

        let mut buf = vec![0u8; len];
        self.recv
            .read_exact(&mut buf)
            .await
            .map_err(|e| Error::Io(format!("read body: {e}")))?;
        Ok(Bytes::from(buf))
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(mut send) = self.send.take() {
            // Best-effort finish; if the peer is gone, that's fine.
            let _ = send.finish();
        }
        if let Some(conn) = self.conn.take() {
            conn.close(0u32.into(), b"close");
        }
        Ok(())
    }
}
