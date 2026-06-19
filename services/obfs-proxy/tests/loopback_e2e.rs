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

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use lakko_svc_obfs_proxy::relay::relay;
use lakko_transport::{
    build_reality_server_config, make_self_signed_cert, tunneled_request, Endpoint, RealityConfig,
    RealityListener, RealityShortId, RealityTagSecret, RealityTransport, Transport,
    TunneledRequest,
};
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
