// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! E2E MASQUE test — closes the live in-process proxy
//! roundtrip gap left open in T-15 (`docs/security/transport.md`).
//!
//! Spins a real in-process h3 CONNECT-UDP proxy on
//! `127.0.0.1:0`:
//!
//!   * a quinn server endpoint with ALPN `h3` and an
//!     rcgen self-signed cert (IP SAN `127.0.0.1`),
//!   * an `h3::server` with `enable_extended_connect` +
//!     `enable_datagram`, accepting the client's
//!     `CONNECT :protocol=connect-udp` request and
//!     answering `200`,
//!   * a verbatim QUIC-datagram echo on a clone of the
//!     accepted `quinn::Connection`.
//!
//! Then a `MasqueTransport` built via
//! [`MasqueTransport::with_extra_roots`] (the private-CA
//! trust hook) points at the bound port with the server's
//! self-signed cert folded into its WebPKI root store,
//! completes the full client handshake
//! (`connect` → quinn QUIC + TLS 1.3 → h3 extended-CONNECT
//! → `200`), then sends an application frame through
//! [`Connection::send`] and reads the echoed reply back
//! through [`Connection::recv`].
//!
//! The echo is byte-verbatim at the raw QUIC-datagram
//! level: the client `send` wraps the UDP payload in the
//! RFC 9297/9298 Context-ID prefix and the client `recv`
//! strips it, so the proxy only has to bounce the exact
//! datagram bytes — it never needs to understand the
//! HTTP-Datagram codec. This validates the full
//! `MasqueTransport::connect` → `send` → `recv` pipeline
//! end-to-end against a live extended-CONNECT proxy on
//! private-CA-pinned trust.

use std::sync::Once;
use std::time::Duration;

use bytes::Bytes;
use h3_quinn::quinn;
use lakko_transport::{Endpoint, MasqueConfig, MasqueTransport, Transport};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

static RUSTLS_INIT: Once = Once::new();

fn install_crypto_provider() {
    RUSTLS_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Self-signed cert helper. `"127.0.0.1"` is detected by
/// rcgen as an IP literal, so the SAN is an `IpAddress`
/// entry — which is what WebPKI matches against when the
/// MASQUE client uses the IP-literal proxy host as the TLS
/// `server_name`. (MASQUE does full WebPKI validation, not
/// WebTransport's cert-hash bypass — so the SAN must match.)
fn make_self_signed_cert(sni: &str) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec![sni.to_owned()]).expect("rcgen");
    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
    (vec![cert_der], key_der)
}

/// Build a quinn server endpoint with ALPN `h3` and the
/// given self-signed cert chain.
fn build_server_endpoint(
    bind: std::net::SocketAddr,
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> quinn::Endpoint {
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("server with_single_cert");
    // QUIC requires the h3 ALPN token (RFC 9114 §3.1).
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let qsc =
        quinn::crypto::rustls::QuicServerConfig::try_from(tls).expect("QuicServerConfig::try_from");
    let server_config = quinn::ServerConfig::with_crypto(std::sync::Arc::new(qsc));
    quinn::Endpoint::server(server_config, bind).expect("quinn server endpoint")
}

/// Drive one accepted QUIC connection as a minimal
/// CONNECT-UDP proxy: clone the connection for a verbatim
/// datagram echo loop, then run the h3 control channel —
/// accept the extended-CONNECT request and answer `200`.
async fn serve_one_connection(conn: quinn::Connection) {
    // Datagram echo on a cheap clone of the connection
    // (quinn::Connection is an Arc handle). Bounce the raw
    // wire bytes verbatim — the client's codec adds/strips
    // the RFC 9297 Context-ID, so the proxy is codec-blind.
    let dg_conn = conn.clone();
    let echo = tokio::spawn(async move {
        // Ends when the peer closes the connection
        // (`read_datagram` errors) or the echo write fails.
        while let Ok(dg) = dg_conn.read_datagram().await {
            if dg_conn.send_datagram(dg).is_err() {
                break;
            }
        }
    });

    let mut h3_conn = h3::server::builder()
        .enable_extended_connect(true)
        .enable_datagram(true)
        .build::<_, bytes::Bytes>(h3_quinn::Connection::new(conn))
        .await
        .expect("h3 server build");

    // Hold accepted request streams open for the lifetime of
    // the connection — closing the CONNECT-UDP request stream
    // tears the tunnel down (RFC 9298 §4.2), and the client
    // parks its half reading capsules. Each accepted request
    // is also asserted to be an extended CONNECT, which proves
    // the client negotiated extended-connect and sent the
    // `:protocol = connect-udp` request the production path
    // builds.
    // The loop ends on `Ok(None)` (graceful h3 close) or
    // `Err(_)` (the client closing the control connection with
    // `H3_NO_ERROR` when the `MasqueConnection` is dropped at
    // test teardown) — both fall out of the `while let`.
    let mut open_streams = Vec::new();
    while let Ok(Some(resolver)) = h3_conn.accept().await {
        let (req, mut stream) = resolver.resolve_request().await.expect("resolve_request");
        assert_eq!(
            req.method(),
            http::Method::CONNECT,
            "MASQUE tunnel must arrive as a CONNECT request"
        );
        let resp = http::Response::builder()
            .status(http::StatusCode::OK)
            .body(())
            .expect("build 200 response");
        stream.send_response(resp).await.expect("send_response");
        open_streams.push(stream);
    }

    echo.abort();
}

/// Full MASQUE loopback roundtrip on private-CA-pinned
/// trust. Closes the live-proxy gap noted in audit-pack
/// invariant T-15.
#[tokio::test]
async fn masque_loopback_roundtrip() {
    install_crypto_provider();

    // SAN = `127.0.0.1` so the client's IP-literal proxy
    // host (used verbatim as the TLS server_name) matches
    // the cert SAN under full WebPKI validation.
    let (chain, key) = make_self_signed_cert("127.0.0.1");
    let server_cert = chain[0].clone();

    let bind: std::net::SocketAddr = "127.0.0.1:0".parse().expect("hardcoded bind addr is valid");
    let endpoint = build_server_endpoint(bind, chain, key);
    let server_addr = endpoint.local_addr().expect("server local_addr");

    // Server task — accept exactly one connection and serve
    // it as a CONNECT-UDP echo proxy.
    let server_task = tokio::spawn(async move {
        if let Some(incoming) = endpoint.accept().await {
            let conn = incoming.await.expect("server accept handshake");
            serve_one_connection(conn).await;
        }
        // Keep the endpoint alive until the task is aborted at
        // test teardown so in-flight datagrams are not dropped.
        endpoint
    });

    // Client side — point MasqueTransport at the bound port,
    // pinning the server's self-signed cert via the
    // private-CA trust hook. Full WebPKI validation then
    // accepts the loopback server.
    let cfg = MasqueConfig {
        proxy_url: format!("https://127.0.0.1:{}/", server_addr.port()),
        // Arbitrary inner target — the echo proxy ignores it
        // (it never actually opens an upstream UDP socket; it
        // just bounces datagrams). Must be valid host:port.
        inner_target: "127.0.0.1:9".to_string(),
        bearer: None,
    };
    let transport =
        MasqueTransport::with_extra_roots(cfg, vec![server_cert]).expect("MasqueTransport");

    // Pay the OS trust store's per-process first-use cost before
    // the clock starts. On macOS the first
    // `SecTrustSettingsCopyCertificates` of a process looks up the
    // main bundle, which for a bare test executable means reading
    // its whole directory; in a large `target/debug/deps` that
    // alone took longer than the 10 s handshake budget below. The
    // budget is for the QUIC + h3 handshake, so keep that cost out
    // of it. Later loads in the same process are fast.
    let _ = tokio::task::spawn_blocking(rustls_native_certs::load_native_certs)
        .await
        .expect("native cert warm-up task");

    // The `Endpoint` arg is ignored by `MasqueTransport::connect`
    // — `cfg.proxy_url` is the canonical destination.
    let endpoint_arg = Endpoint::hostname("127.0.0.1", server_addr.port());
    let mut conn = tokio::time::timeout(Duration::from_secs(10), transport.connect(&endpoint_arg))
        .await
        .expect("connect did not time out")
        .expect("connect succeeds");

    let frame = Bytes::from_static(b"lakko-masque-connect-udp-roundtrip");
    conn.send(frame.clone()).await.expect("send");

    let echo = tokio::time::timeout(Duration::from_secs(10), conn.recv())
        .await
        .expect("recv did not time out")
        .expect("recv succeeds");
    assert_eq!(&echo[..], &frame[..]);

    conn.close().await.expect("close");
    server_task.abort();
}
