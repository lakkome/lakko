// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! QUIC loopback test: spin up a listener with a self-signed cert and
//! verify a single bi-stream client + server can exchange frames.

use std::sync::Once;

use bytes::Bytes;
use lakko_transport::quic::{QuicListener, QuicTransport, SelfSignedCert};
use lakko_transport::{Listener, Transport};

static RUSTLS_INIT: Once = Once::new();

/// Install the ring-based default crypto provider exactly once.
fn install_crypto_provider() {
    RUSTLS_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[tokio::test]
async fn quic_loopback_bidirectional() {
    install_crypto_provider();

    // ---- listener (server) -------------------------------------------------
    let cert = SelfSignedCert::generate("localhost").expect("self-signed cert");
    let trust = cert.chain.clone();

    let mut listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), cert).expect("bind");
    let server_endpoint = listener.local_endpoint();

    // ---- transport (client) -----------------------------------------------
    let transport = QuicTransport::client("127.0.0.1:0".parse().unwrap(), trust).expect("client");

    // ---- spawn server side ------------------------------------------------
    let server_task = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");

        let req = conn.recv().await.expect("server recv");
        assert_eq!(req, Bytes::from_static(b"ping"));

        conn.send(Bytes::from_static(b"pong"))
            .await
            .expect("server send");

        let req2 = conn.recv().await.expect("server recv 2");
        assert_eq!(req2, Bytes::from_static(b"second"));
        conn.send(Bytes::from_static(b"echo"))
            .await
            .expect("server send 2");

        // Block until the client closes, so the final echo is fully delivered
        // before we tear down our side of the connection.
        let after_echo = conn.recv().await;
        assert!(matches!(
            after_echo,
            Err(lakko_transport::Error::PeerClosed | lakko_transport::Error::Io(_))
        ));
    });

    // ---- client side ------------------------------------------------------
    let mut client = transport
        .connect(&server_endpoint)
        .await
        .expect("client connect");

    client
        .send(Bytes::from_static(b"ping"))
        .await
        .expect("client send 1");
    let reply = client.recv().await.expect("client recv 1");
    assert_eq!(reply, Bytes::from_static(b"pong"));

    client
        .send(Bytes::from_static(b"second"))
        .await
        .expect("client send 2");
    let reply2 = client.recv().await.expect("client recv 2");
    assert_eq!(reply2, Bytes::from_static(b"echo"));

    client.close().await.expect("client close");

    server_task.await.expect("server task");
}
