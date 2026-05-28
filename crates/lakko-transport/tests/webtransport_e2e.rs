// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! E2E WebTransport test — closes T-14 with positive
//! integration coverage.
//!
//! Spins a real `web_transport_quinn::Server` on
//! `127.0.0.1:0` with an rcgen-generated self-signed
//! cert, then a `WebTransportPt` client points at the
//! bound port with the cert's SHA-256 hash pinned in
//! `cert_hashes_b64`. The client opens the WebTransport
//! session, sends a u32-BE length-prefixed frame, and
//! reads the echoed reply back. Validates the full
//! `WebTransportPt::connect` → `Connection::send` →
//! `Connection::recv` pipeline end-to-end on
//! W3C-`serverCertificateHashes`-pinned trust.

use std::sync::Once;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD_NO_PAD as B64, Engine as _};
use bytes::Bytes;
use lakko_transport::{Endpoint, Transport, WebTransportConfig, WebTransportPt};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

static RUSTLS_INIT: Once = Once::new();

fn install_crypto_provider() {
    RUSTLS_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Self-signed cert helper — same shape REALITY's
/// `make_self_signed_cert` uses, replicated here so the
/// test stands alone without depending on REALITY's
/// public surface.
fn make_self_signed_cert(sni: &str) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec![sni.to_owned()]).expect("rcgen");
    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
    (vec![cert_der], key_der)
}

/// Full WebTransport loopback round-trip on cert-hash
/// pinned trust. Spins a quinn-backed `web_transport_quinn`
/// server with a self-signed cert, then a `WebTransportPt`
/// client connects with the cert's SHA-256 pinned in
/// `cert_hashes_b64`, sends an application frame, and
/// reads the echoed reply.
///
/// Pinned in audit-pack invariant T-14.
#[tokio::test]
async fn webtransport_loopback_roundtrip() {
    install_crypto_provider();

    // SAN = `localhost` so the URL we hand the client
    // (`https://localhost:<port>/`) matches the cert. The
    // cert-hash pin is the cryptographic check; SAN
    // matching is a separate W3C requirement the
    // WebTransport spec inherits from the URL.
    let (chain, key) = make_self_signed_cert("localhost");
    let cert_hash = Sha256::digest(chain[0].as_ref());
    let cert_hash_b64 = B64.encode(cert_hash);

    let bind: std::net::SocketAddr = "127.0.0.1:0".parse().expect("hardcoded bind addr is valid");
    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr(bind)
        .with_certificate(chain, key)
        .expect("with_certificate");

    let server_addr = server.local_addr().expect("server local_addr");

    // Server task — accept one session, accept the URL,
    // open a bi-directional stream from the client's
    // perspective, read one length-prefixed frame, echo
    // it back. Mirrors the client framing
    // `WebTransportConnection::send` uses.
    let server_task = tokio::spawn(async move {
        let req = server.accept().await.expect("accept request");
        let session = req.ok().await.expect("session ok");
        // Client side calls `open_bi` after `connect`
        // returns, so the server picks it up here.
        let (mut send, mut recv) = session.accept_bi().await.expect("accept_bi");

        // Length prefix (u32-BE) + payload.
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf).await.expect("read len");
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut payload = vec![0u8; len];
        recv.read_exact(&mut payload).await.expect("read payload");

        // Echo back with the same framing the client
        // recv path expects.
        let echo_len = u32::try_from(payload.len()).expect("fits u32");
        send.write_all(&echo_len.to_be_bytes())
            .await
            .expect("write len");
        send.write_all(&payload).await.expect("write payload");
        send.finish().expect("send finish");
    });

    // Client side — point WebTransportPt at the server
    // and pin the cert hash. We use `127.0.0.1` (not
    // `localhost`) in the URL so the client+server land
    // on the same IP family. `localhost` resolves to
    // `::1` first on macOS while quinn::Endpoint::client
    // binds to `[::]:0` dual-stack; the server is bound
    // to `127.0.0.1:0` and would never receive the
    // packets. Cert-hash pinning ignores SAN mismatch
    // (the SHA-256 of the DER is the only check), so
    // IP-literal SNI is fine here.
    let cfg = WebTransportConfig {
        url: format!("https://127.0.0.1:{}/", server_addr.port()),
        cert_hashes_b64: vec![cert_hash_b64],
        ech_config_list_b64: None,
    };
    let pt = WebTransportPt::new(cfg).expect("WebTransportPt::new");
    // The `Endpoint` arg to `connect` is ignored by
    // `WebTransportPt::connect` — `cfg.url` is the
    // canonical destination.
    let endpoint = Endpoint::hostname("127.0.0.1", server_addr.port());
    let mut conn = tokio::time::timeout(Duration::from_secs(10), pt.connect(&endpoint))
        .await
        .expect("connect did not time out")
        .expect("connect succeeds");

    let frame = Bytes::from_static(b"lakko-webtransport-roundtrip");
    conn.send(frame.clone()).await.expect("send");

    let echo = tokio::time::timeout(Duration::from_secs(10), conn.recv())
        .await
        .expect("recv did not time out")
        .expect("recv succeeds");
    assert_eq!(&echo[..], &frame[..]);

    conn.close().await.expect("close");
    let _ = server_task.await;
}
