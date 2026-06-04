// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `MasqueTransport` — `Transport` trait impl for the
//! MASQUE / CONNECT-UDP pluggable transport
//! (RFC 9298 + RFC 9297).
//!
//! ## Handshake outline
//!
//! 1. Resolve the proxy host:port to a `SocketAddr`.
//! 2. Build a rustls `ClientConfig` with
//!    `alpn_protocols = ["h3"]`, system CA roots, and
//!    TLS 1.3 (the only version QUIC accepts).
//! 3. Build a quinn `ClientConfig` from it + open a
//!    `quinn::Endpoint` on `[::]:0` (dual-stack), then
//!    `connect(proxy_addr, proxy_host)` to establish
//!    the QUIC + TLS handshake.
//! 4. Wrap the quinn connection in
//!    `h3_quinn::Connection` and pass it to
//!    `h3::client::builder()
//!       .enable_extended_connect(true)
//!       .enable_datagram(true)
//!       .build(..)` — the SETTINGS handshake the
//!    builder runs negotiates both extended-CONNECT
//!    (RFC 8441) and HTTP Datagrams (RFC 9297 §2.1.1).
//! 5. Build an `http::Request<()>` from our
//!    [`super::request::ConnectUdpRequest`] (pseudo
//!    headers + `capsule-protocol: ?1` + optional
//!    `authorization: Bearer ..`) and `send_request` on
//!    the h3 client; await `recv_response` and verify
//!    the proxy returned a 2xx (RFC 9298 §4.2).
//! 6. Hand the original `quinn::Connection` (a clone of
//!    the one passed to h3) to [`MasqueConnection`].
//!    UDP frames flow directly through
//!    `quinn::Connection::{send_datagram, read_datagram}`
//!    using our [`super::datagram::encode_udp_payload`]
//!    / [`super::datagram::decode_udp_payload`] codec —
//!    h3-datagram's higher-level surface is not needed
//!    because we run exactly one tunnel (Context ID 0)
//!    per quinn::Connection.
//!
//! The h3 connection driver + the request stream that
//! carries the extended-CONNECT signaling are spawned
//! as background tokio tasks; their `JoinHandle`s are
//! held on [`MasqueConnection`] so [`Drop`] aborts them
//! and tears the tunnel down cleanly.

use std::net::ToSocketAddrs;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use h3_quinn::quinn;
use http::HeaderName;

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::masque::config::MasqueConfig;
use crate::masque::datagram::{decode_udp_payload, encode_udp_payload};
use crate::masque::request::{build_connect_udp_request, ConnectUdpRequest};
use crate::transport::{Connection, Transport};

/// MASQUE transport — typed wrapper around a parsed
/// [`MasqueConfig`].
#[derive(Debug)]
pub struct MasqueTransport {
    cfg: MasqueConfig,
    /// Extra trust anchors folded into the rustls root store
    /// alongside the OS native certs at connect time. Empty in
    /// the default [`Self::new`] path (production proxies use a
    /// publicly-trusted cert). Populated via
    /// [`Self::with_extra_roots`] to pin a **private-CA** MASQUE
    /// proxy (an operator running their own gateway) — the same
    /// "trust only this CA" posture the DoH fetcher takes with
    /// IP-pinning and REALITY takes with its custom verifier. It
    /// is also what lets the loopback e2e test trust a
    /// self-signed server cert.
    extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
}

impl MasqueTransport {
    /// Build the transport from a parsed config. Validates
    /// every URL / target / bearer constraint upfront so a
    /// bad bootstrap config surfaces at construction, not
    /// at connect time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when
    /// [`MasqueConfig::parse`] fails.
    pub fn new(cfg: MasqueConfig) -> Result<Self> {
        cfg.parse()
            .map_err(|e| Error::Config(format!("masque config: {e}")))?;
        Ok(Self {
            cfg,
            extra_roots: Vec::new(),
        })
    }

    /// Build the transport with additional DER-encoded trust
    /// anchors folded into the connect-time root store on top of
    /// the OS native certs. Use this to pin a private-CA proxy
    /// (or, in tests, a self-signed loopback server). The certs
    /// are added to the same WebPKI root set the standard path
    /// uses — they do NOT replace the native roots, so a proxy
    /// presenting a publicly-trusted cert still validates.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when
    /// [`MasqueConfig::parse`] fails.
    pub fn with_extra_roots(
        cfg: MasqueConfig,
        extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
    ) -> Result<Self> {
        cfg.parse()
            .map_err(|e| Error::Config(format!("masque config: {e}")))?;
        Ok(Self { cfg, extra_roots })
    }

    /// Borrow the underlying config — handy for tests + for
    /// callers that need to read the proxy URL / inner
    /// target / bearer.
    #[must_use]
    pub fn config(&self) -> &MasqueConfig {
        &self.cfg
    }
}

#[async_trait]
impl Transport for MasqueTransport {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        let (proxy, target, bearer) = self
            .cfg
            .parse()
            .map_err(|e| Error::Config(format!("masque config: {e}")))?;

        // Resolve the proxy host:port. We use blocking
        // resolution (the std DNS path) because `tokio::net::lookup_host`
        // would tie us to the multi-threaded runtime — masque is
        // expected to be called from generic async contexts.
        // Picks the first address; production deployments may
        // want happy-eyeballs-style racing.
        let proxy_authority = (proxy.host.as_str(), proxy.port);
        let proxy_addr = proxy_authority
            .to_socket_addrs()
            .map_err(|e| Error::Io(format!("masque proxy resolve: {e}")))?
            .next()
            .ok_or_else(|| {
                Error::Io(format!(
                    "masque proxy {:?} resolved to no addresses",
                    proxy.host
                ))
            })?;

        // rustls + quinn config — TLS 1.3, h3 ALPN, system
        // root CAs.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for cert in native.certs {
            // Drop individual cert load failures silently —
            // matches what web-transport-quinn::ClientBuilder
            // does on its own with_system_roots branch.
            let _ = roots.add(cert);
        }
        // Fold in any caller-supplied trust anchors (private-CA
        // proxy pin / loopback test cert). These ADD to the
        // native roots, never replace them.
        for cert in &self.extra_roots {
            let _ = roots.add(cert.clone());
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| Error::Config(format!("masque rustls versions: {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        // ALPN MUST be exactly "h3" per RFC 9114 §3.1.
        tls.alpn_protocols = vec![b"h3".to_vec()];

        let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|e| Error::Config(format!("masque QuicClientConfig: {e}")))?;
        let client_config = quinn::ClientConfig::new(Arc::new(quic));

        let bind: std::net::SocketAddr = "[::]:0".parse().expect("hardcoded bind addr is valid");
        let mut endpoint = quinn::Endpoint::client(bind)
            .map_err(|e| Error::Io(format!("masque endpoint: {e}")))?;
        endpoint.set_default_client_config(client_config);

        let quinn_conn = endpoint
            .connect(proxy_addr, proxy.host.as_str())
            .map_err(|e| Error::Io(format!("masque quinn connect: {e}")))?
            .await
            .map_err(|e| Error::Io(format!("masque quinn handshake: {e}")))?;

        // Hold a clone of the quinn::Connection for the
        // datagram-level send/recv path. The other clone is
        // moved into the h3 wrapper for the control channel.
        let datagram_conn = quinn_conn.clone();

        let h3_quinn_conn = h3_quinn::Connection::new(quinn_conn);
        let (mut h3_driver, mut send_request) = h3::client::builder()
            .enable_extended_connect(true)
            .enable_datagram(true)
            .build::<_, _, bytes::Bytes>(h3_quinn_conn)
            .await
            .map_err(|e| Error::Io(format!("masque h3 build: {e}")))?;

        // Spawn the h3 control-channel driver — it pumps
        // SETTINGS, GOAWAY, qpack updates, etc. for the
        // lifetime of the connection. Aborted by
        // `MasqueConnection::drop`.
        let driver_handle = tokio::spawn(async move {
            let _ = std::future::poll_fn(|cx| h3_driver.poll_close(cx)).await;
        });

        // Build the extended-CONNECT request and send it.
        let conn_req = build_connect_udp_request(&proxy, &target, bearer.as_ref());
        let http_req = build_http_request_from(&conn_req)?;

        let mut stream = send_request
            .send_request(http_req)
            .await
            .map_err(|e| Error::Io(format!("masque send_request: {e}")))?;
        // No request body for CONNECT — close the send
        // half but keep the receive half open for the
        // proxy's capsule traffic.
        stream
            .finish()
            .await
            .map_err(|e| Error::Io(format!("masque stream.finish: {e}")))?;
        let resp = stream
            .recv_response()
            .await
            .map_err(|e| Error::Io(format!("masque recv_response: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Io(format!(
                "masque proxy refused tunnel: HTTP {}",
                resp.status()
            )));
        }

        // The CONNECT-UDP stream MUST remain open for the
        // lifetime of the tunnel (RFC 9298 §4.2 — closing
        // the request stream terminates the tunnel). We
        // park it in a background task; reading from it
        // drains any capsules the proxy sends, which we
        // do not consume at this layer.
        let stream_handle = tokio::spawn(async move {
            // The recv_data loop ends when the proxy
            // half-closes its side or errors. Either way
            // the tunnel is dead from then on.
            while let Ok(Some(_chunk)) = stream.recv_data().await {
                // We intentionally drop capsules — the
                // datagram path runs on quinn directly.
            }
        });

        Ok(Box::new(MasqueConnection {
            quinn_conn: datagram_conn,
            // Keep the sole `SendRequest` alive for the
            // tunnel's lifetime — dropping it here would
            // close the HTTP/3 connection (`H3_NO_ERROR`)
            // and tear the datagram tunnel down immediately.
            _send_request: send_request,
            driver_handle: Some(driver_handle),
            stream_handle: Some(stream_handle),
            closed: false,
        }))
    }
}

/// Convert our typed [`ConnectUdpRequest`] into the
/// `http::Request<()>` shape `h3::client::SendRequest::send_request`
/// consumes.
fn build_http_request_from(req: &ConnectUdpRequest) -> Result<http::Request<()>> {
    let uri_str = format!("{}://{}{}", req.scheme, req.authority, req.path);
    let uri: http::Uri = uri_str
        .parse()
        .map_err(|e| Error::Config(format!("masque URI build: {e}")))?;

    let mut builder = http::Request::builder()
        .method(http::Method::CONNECT)
        .uri(uri)
        // Carry `:protocol = connect-udp` via the
        // canonical HeaderName the h3 stack interprets
        // — it lifts the value into the `:protocol`
        // pseudo-header during header serialisation.
        .extension(h3::ext::Protocol::CONNECT_UDP);

    for (name, value) in &req.headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| Error::Config(format!("masque header name {name:?}: {e}")))?;
        builder = builder.header(header_name, value);
    }

    builder
        .body(())
        .map_err(|e| Error::Config(format!("masque http::Request build: {e}")))
}

/// Post-handshake MASQUE `Connection`. UDP frames
/// flow as RFC 9297 HTTP Datagrams over the QUIC
/// connection's DATAGRAM frame channel; we encode +
/// decode the Context-ID prefix via our masque
/// datagram codec rather than going through
/// h3-datagram (one Context ID per quinn::Connection
/// — no multiplex value-add).
struct MasqueConnection {
    quinn_conn: quinn::Connection,
    /// The h3 `SendRequest` handle for the CONNECT-UDP
    /// control channel. Held — never used again after the
    /// initial request — purely to keep the HTTP/3
    /// connection open: h3's `SendRequest::drop` closes the
    /// whole connection with `H3_NO_ERROR` the moment the
    /// **last** sender handle is dropped (sender refcount
    /// 1 -> 0). Dropping it at the end of `connect` would
    /// therefore tear the QUIC connection — and thus the
    /// datagram tunnel — down the instant it was
    /// established. Keeping it on the connection ties its
    /// lifetime to the tunnel's.
    _send_request: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    driver_handle: Option<tokio::task::JoinHandle<()>>,
    stream_handle: Option<tokio::task::JoinHandle<()>>,
    closed: bool,
}

#[async_trait]
impl Connection for MasqueConnection {
    async fn send(&mut self, frame: Bytes) -> Result<()> {
        if self.closed {
            return Err(Error::LocalClosed);
        }
        // Wrap the UDP payload in (Context-ID = 0 || payload)
        // per RFC 9297 §3 + RFC 9298 §3. Caller's `frame` IS
        // the UDP payload — not yet wrapped.
        let wire = encode_udp_payload(&frame)
            .map_err(|e| Error::Io(format!("masque encode_udp_payload: {e}")))?;
        self.quinn_conn
            .send_datagram(wire.into())
            .map_err(|e| Error::Io(format!("masque send_datagram: {e}")))
    }

    async fn recv(&mut self) -> Result<Bytes> {
        if self.closed {
            return Err(Error::LocalClosed);
        }
        let wire = self
            .quinn_conn
            .read_datagram()
            .await
            .map_err(|e| Error::Io(format!("masque read_datagram: {e}")))?;
        let udp = decode_udp_payload(&wire)
            .map_err(|e| Error::Io(format!("masque decode_udp_payload: {e}")))?;
        Ok(Bytes::copy_from_slice(udp))
    }

    async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        // 0 = application close code; b"" close reason.
        // Quinn ferries this to the peer via CONNECTION_CLOSE.
        self.quinn_conn.close(0u32.into(), b"");
        if let Some(h) = self.stream_handle.take() {
            h.abort();
        }
        if let Some(h) = self.driver_handle.take() {
            h.abort();
        }
        self.closed = true;
        Ok(())
    }
}

impl Drop for MasqueConnection {
    fn drop(&mut self) {
        // Best-effort task abort + connection teardown
        // when the caller drops without explicit close.
        if let Some(h) = self.stream_handle.take() {
            h.abort();
        }
        if let Some(h) = self.driver_handle.take() {
            h.abort();
        }
        if !self.closed {
            self.quinn_conn.close(0u32.into(), b"");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn happy_config() -> MasqueConfig {
        MasqueConfig {
            proxy_url: "https://masque.example.com/".into(),
            inner_target: "lakko.me:443".into(),
            bearer: Some("opaque-token".into()),
        }
    }

    #[test]
    fn new_accepts_valid_config() {
        let cfg = happy_config();
        let t = MasqueTransport::new(cfg.clone()).expect("valid config");
        assert_eq!(t.config(), &cfg);
    }

    #[test]
    fn new_rejects_non_https_proxy() {
        let mut cfg = happy_config();
        cfg.proxy_url = "http://example.com/".into();
        let err = MasqueTransport::new(cfg).expect_err("non-https must reject");
        match err {
            Error::Config(msg) => assert!(msg.contains("https"), "msg: {msg}"),
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn new_rejects_bad_inner_target() {
        let mut cfg = happy_config();
        cfg.inner_target = "lakko.me".into();
        let err = MasqueTransport::new(cfg).expect_err("missing-port must reject");
        match err {
            Error::Config(_) => {},
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn new_has_no_extra_roots_by_default() {
        let t = MasqueTransport::new(happy_config()).expect("valid config");
        assert!(
            t.extra_roots.is_empty(),
            "default path must not pin any extra trust anchors"
        );
    }

    #[test]
    fn with_extra_roots_stores_the_anchors_and_validates_config() {
        let cfg = happy_config();
        // The constructor does NOT parse the cert bytes — it only
        // validates the config + stashes the DER for connect-time
        // root-store loading — so a placeholder DER is fine here.
        let anchor = rustls::pki_types::CertificateDer::from(vec![0xABu8; 16]);
        let t = MasqueTransport::with_extra_roots(cfg.clone(), vec![anchor.clone()])
            .expect("valid config");
        assert_eq!(t.config(), &cfg);
        assert_eq!(t.extra_roots.len(), 1);
        assert_eq!(t.extra_roots[0], anchor);
    }

    #[test]
    fn with_extra_roots_still_rejects_bad_config() {
        let mut cfg = happy_config();
        cfg.proxy_url = "http://example.com/".into();
        let err = MasqueTransport::with_extra_roots(cfg, vec![])
            .expect_err("non-https must reject even with extra roots");
        match err {
            Error::Config(msg) => assert!(msg.contains("https"), "msg: {msg}"),
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    /// CONNECT-UDP request → http::Request<()> translation
    /// preserves the pseudo-headers + capsule-protocol +
    /// bearer the wire layer expects. Validates the
    /// shape before the h3 stack QPACK-encodes it.
    #[test]
    fn http_request_from_connect_udp_request_carries_pseudo_headers() {
        // Build via the public parse path so we don't depend
        // on the internal field shape of `MasqueProxyUrl`
        // (carries a parsed `url::Url` we don't construct
        // by hand).
        let cfg = MasqueConfig {
            proxy_url: "https://proxy.example.com/".into(),
            inner_target: "lakko.me:443".into(),
            bearer: Some("tok".into()),
        };
        let (proxy, target, bearer) = cfg.parse().expect("config parses");
        let cu = build_connect_udp_request(&proxy, &target, bearer.as_ref());
        let req = build_http_request_from(&cu).expect("http::Request builds");

        assert_eq!(req.method(), http::Method::CONNECT);
        let uri = req.uri();
        assert_eq!(uri.scheme_str(), Some("https"));
        assert_eq!(uri.host(), Some("proxy.example.com"));
        assert!(uri.path().starts_with("/.well-known/masque/udp/"));
        assert!(
            req.extensions().get::<h3::ext::Protocol>().copied()
                == Some(h3::ext::Protocol::CONNECT_UDP),
        );
        let capsule = req
            .headers()
            .get("capsule-protocol")
            .expect("capsule-protocol header present");
        assert_eq!(capsule.to_str().unwrap(), "?1");
        let auth = req
            .headers()
            .get("authorization")
            .expect("authorization header present");
        assert_eq!(auth.to_str().unwrap(), "Bearer tok");
    }

    /// Connecting to a port that is not listening must
    /// surface as a typed `Error::Io` and not panic. We
    /// can't easily run a live MASQUE proxy in-process
    /// without a substantial server-side scaffold, so
    /// this test checks the connect path wires up far
    /// enough to surface real network errors.
    #[tokio::test]
    async fn connect_to_unreachable_proxy_surfaces_io_error() {
        // Port 1 is reserved + not listening; the connect
        // attempt should fail at the QUIC handshake stage.
        // We use a short timeout to keep CI fast.
        let cfg = MasqueConfig {
            proxy_url: "https://127.0.0.1:1/".into(),
            inner_target: "127.0.0.1:443".into(),
            bearer: None,
        };
        let t = MasqueTransport::new(cfg).expect("config parses");
        let endpoint = Endpoint::hostname("127.0.0.1", 1);
        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(3), t.connect(&endpoint)).await;
        // Timeout is also acceptable — no panic + no leaks.
        let Ok(res) = outcome else { return };
        match res {
            Err(Error::Io(_) | Error::Config(_)) => {},
            Err(e) => panic!("expected Io/Config error, got {e:?}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }
}
