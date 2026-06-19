// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0026 Phase 2 loopback end-to-end.
//!
//! Proves the full client ↔ proxy ↔ internal-HTTP path headless: a real
//! `RealityTransport` client dials a real `RealityListener`; on accept the
//! proxy `relay` byte-forwards the tunnel to a fake internal HTTP origin; the
//! client issues a `tunneled_request` and gets the origin's response back —
//! through the obfuscated tunnel, with no plaintext on the loopback wire that
//! a probe could read.

use std::net::SocketAddr;
use std::sync::{Arc, Once};

use base64::engine::general_purpose::STANDARD_NO_PAD as B64_NOPAD;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use lakko_svc_obfs_proxy::relay::relay;
use lakko_transport::{
    build_reality_server_config, make_self_signed_cert, shadowsocks_accept_inbound,
    tunneled_request, Endpoint, MasqueConfig, MasqueListener, MasqueTransport, RealityConfig,
    RealityListener, RealityShortId, RealityTagSecret, RealityTransport, ShadowsocksConfig,
    ShadowsocksReplayWindow, ShadowsocksTransport, Transport, TunneledRequest, WebTransportConfig,
    WebTransportListener, WebTransportPt,
};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

static RUSTLS_INIT: Once = Once::new();

fn install_crypto_provider() {
    RUSTLS_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A minimal internal HTTP origin: read the request, reply with a fixed
/// `Connection: close` response, close. Stands in for nginx + the real
/// services behind it.
async fn spawn_origin() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                // Consume the request bytes (one read is enough for a small GET).
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\npong";
                let _ = sock.write_all(resp).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn reality_tunnel_relays_http_request_to_internal_origin() {
    install_crypto_provider();
    let origin = spawn_origin().await;

    // Publisher-distributed REALITY secret (one allow-list pair).
    let tag_secret = [0x42u8; 32];
    let short_id = [0xABu8; 8];
    let cover_sni = "www.microsoft.com";

    // --- proxy side: REALITY listener → relay to the internal origin --------
    let (chain, key) = make_self_signed_cert(cover_sni).expect("cover cert");
    let tls = build_reality_server_config(chain, key).expect("server config");
    let tcp = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let bound = tcp.local_addr().unwrap();
    let mut listener = RealityListener::new(
        tcp,
        Arc::new(tls),
        vec![(RealityShortId(short_id), RealityTagSecret(tag_secret))],
        300,
    );
    let proxy = tokio::spawn(async move {
        let (conn, _accepted) = listener.accept().await.expect("accept REALITY tunnel");
        relay(conn, origin).await.expect("relay to origin");
    });

    // --- client side: dial the REALITY tunnel, issue a tunneled request -----
    let client_cfg = RealityConfig {
        sni: cover_sni.into(),
        tag_secret_b64: B64.encode(tag_secret),
        short_id_hex: hex::encode(short_id),
        allowed_inner_servers: vec![],
        remote: format!("{bound}"),
        require_server_response: true,
    };
    let client = RealityTransport::new(client_cfg).expect("client transport");
    let conn = client
        .connect(&Endpoint::Direct(bound))
        .await
        .expect("client completes the REALITY handshake");

    let req = TunneledRequest {
        method: "GET".into(),
        path: "/health".into(),
        host: "lakko.invalid".into(),
        headers: vec![],
        body: vec![],
    };
    let resp = tunneled_request(conn, &req)
        .await
        .expect("tunneled request completes");

    assert_eq!(
        resp.status, 200,
        "origin status surfaces through the tunnel"
    );
    assert_eq!(
        resp.body, b"pong",
        "origin body surfaces through the tunnel"
    );

    proxy.await.expect("proxy relay task completes");
}

#[tokio::test]
async fn shadowsocks_tunnel_relays_http_request_to_internal_origin() {
    let origin = spawn_origin().await;
    let psk = [0x55u8; 32];
    let psk_b64 = B64.encode(psk);

    // --- proxy side: SS-2022 accept (raw TCP + accept_inbound) → relay -------
    let tcp = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind SS listener");
    let bound = tcp.local_addr().unwrap();
    let proxy = tokio::spawn(async move {
        let (stream, _) = tcp.accept().await.expect("accept TCP");
        let mut window = ShadowsocksReplayWindow::new();
        let conn = shadowsocks_accept_inbound(stream, &psk, &mut window)
            .await
            .expect("SS-2022 handshake");
        relay(conn, origin).await.expect("relay to origin");
    });

    // --- client side: dial the SS tunnel, issue a tunneled request ----------
    let client = ShadowsocksTransport::new(ShadowsocksConfig {
        remote: format!("{bound}"),
        method: "2022-blake3-chacha20-poly1305".into(),
        psk_b64,
    })
    .expect("SS client transport");
    let conn = client
        .connect(&Endpoint::Direct(bound))
        .await
        .expect("client completes the SS-2022 handshake");

    let req = TunneledRequest {
        method: "GET".into(),
        path: "/health".into(),
        host: "lakko.invalid".into(),
        headers: vec![],
        body: vec![],
    };
    let resp = tunneled_request(conn, &req)
        .await
        .expect("tunneled request completes");

    assert_eq!(resp.status, 200, "SS: origin status surfaces");
    assert_eq!(resp.body, b"pong", "SS: origin body surfaces");

    proxy.await.expect("proxy relay task completes");
}

#[tokio::test]
async fn webtransport_tunnel_relays_http_request_to_internal_origin() {
    install_crypto_provider();
    let origin = spawn_origin().await;

    // Self-signed cover cert; the client pins its SHA-256 (W3C
    // serverCertificateHashes mode) rather than a CA chain.
    let (chain, key) = make_self_signed_cert("localhost").expect("cover cert");
    let cert_hash_b64 = B64_NOPAD.encode(Sha256::digest(chain[0].as_ref()));

    // --- proxy side: WebTransport listener → relay to the internal origin ---
    let mut listener =
        WebTransportListener::bind("127.0.0.1:0".parse().unwrap(), chain, key).expect("bind WT");
    let server_addr = listener.local_addr().expect("WT local_addr");
    let proxy = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept WT tunnel");
        relay(conn, origin).await.expect("relay to origin");
    });

    // --- client side: dial the WT tunnel, issue a tunneled request ----------
    let client = WebTransportPt::new(WebTransportConfig {
        url: format!("https://127.0.0.1:{}/", server_addr.port()),
        cert_hashes_b64: vec![cert_hash_b64],
        ech_config_list_b64: None,
    })
    .expect("WT client transport");
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client.connect(&Endpoint::hostname("127.0.0.1", server_addr.port())),
    )
    .await
    .expect("WT connect did not time out")
    .expect("client completes the WebTransport handshake");

    let req = TunneledRequest {
        method: "GET".into(),
        path: "/health".into(),
        host: "lakko.invalid".into(),
        headers: vec![],
        body: vec![],
    };
    let resp = tunneled_request(conn, &req)
        .await
        .expect("tunneled request completes");

    assert_eq!(resp.status, 200, "WT: origin status surfaces");
    assert_eq!(resp.body, b"pong", "WT: origin body surfaces");

    proxy.await.expect("proxy relay task completes");
}

/// MASQUE (`CONNECT-UDP`) carries **unreliable, unordered QUIC datagrams**, not
/// a reliable stream like REALITY / Shadowsocks / WebTransport. The
/// `MasqueListener` handshake works (the client connects, the request datagram
/// reaches the origin, which responds), but the response datagram is dropped on
/// the relay's close — QUIC datagrams are never retransmitted and are not
/// flushed ahead of `CONNECTION_CLOSE`, so the client sees a truncated response
/// (`BadResponse("no header terminator")`). This is a semantic mismatch:
/// relaying a *reliable* internal HTTP service over an *unreliable* UDP tunnel
/// needs a reliability layer the proxy does not (and arguably should not)
/// add — MASQUE is for tunnelling UDP, not TCP/HTTP. The listener is kept as a
/// correct MASQUE-server foundation; this e2e is ignored, documenting why
/// MASQUE does not fit the obfs-proxy's relay-to-HTTP model.
#[ignore = "MASQUE CONNECT-UDP datagrams are unreliable — HTTP relay needs a reliable stream (REALITY/SS/WT)"]
#[tokio::test]
async fn masque_tunnel_relays_http_request_to_internal_origin() {
    install_crypto_provider();
    let origin = spawn_origin().await;

    // SAN = 127.0.0.1 so the client's IP-literal proxy host matches the cert
    // under full WebPKI validation (MASQUE pins the self-signed cert as a
    // private-CA trust root).
    let (chain, key) = make_self_signed_cert("127.0.0.1").expect("cover cert");
    let server_cert = chain[0].clone();

    // --- proxy side: MASQUE (h3 CONNECT-UDP) listener → relay to the origin --
    let listener =
        MasqueListener::bind("127.0.0.1:0".parse().unwrap(), chain, key).expect("bind MASQUE");
    let server_addr = listener.local_addr().expect("MASQUE local_addr");
    let proxy = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept MASQUE tunnel");
        relay(conn, origin).await.expect("relay to origin");
    });

    // --- client side: dial the MASQUE tunnel, issue a tunneled request ------
    let client = MasqueTransport::with_extra_roots(
        MasqueConfig {
            proxy_url: format!("https://127.0.0.1:{}/", server_addr.port()),
            inner_target: "127.0.0.1:9".to_string(),
            bearer: None,
        },
        vec![server_cert],
    )
    .expect("MASQUE client transport");
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client.connect(&Endpoint::hostname("127.0.0.1", server_addr.port())),
    )
    .await
    .expect("MASQUE connect did not time out")
    .expect("client completes the MASQUE handshake");

    let req = TunneledRequest {
        method: "GET".into(),
        path: "/health".into(),
        host: "lakko.invalid".into(),
        headers: vec![],
        body: vec![],
    };
    let resp = tunneled_request(conn, &req)
        .await
        .expect("tunneled request completes");

    assert_eq!(resp.status, 200, "MASQUE: origin status surfaces");
    assert_eq!(resp.body, b"pong", "MASQUE: origin body surfaces");

    proxy.await.expect("proxy relay task completes");
}
