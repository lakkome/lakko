// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! E2E REALITY v2 test: client and server completing a full
//! handshake against each other over loopback TCP.
//!
//! Spins up a real `RealityListener` bound to `127.0.0.1:0`
//! with a self-signed cover-SNI cert + the publisher's
//! `(short_id, tag_secret)` allow-list, and a real
//! `RealityTransport` client pointed at the same SNI +
//! tag_secret. Drives the handshake to completion and
//! exercises the post-handshake `Connection`-trait
//! send/recv pipeline.

use std::sync::{Arc, Once};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use bytes::Bytes;
use lakko_transport::{
    build_reality_server_config, make_self_signed_cert, Endpoint, RealityConfig, RealityListener,
    RealityShortId, RealityTagSecret, RealityTransport, Transport,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::TlsConnector;

static RUSTLS_INIT: Once = Once::new();

fn install_crypto_provider() {
    RUSTLS_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Full REALITY v2 loopback round-trip: a real
/// `RealityListener` + `RealityTransport` client on the
/// same loopback TCP port complete the handshake end-to-end
/// (mutual authentication included) and exchange a
/// post-handshake application frame.
///
/// S-5-reality-rng-hook landed: the REALITY authenticator
/// is placed into `legacy_session_id` via the rustls
/// `SecureRandom` hook so rustls's transcript hash and the
/// on-wire bytes are consistent on both sides.
/// `ServerHello.Random` carries the server-response HMAC
/// via the same mechanism; `require_server_response: true`
/// exercises the mutual-auth path.
#[tokio::test]
async fn reality_v2_loopback_roundtrip() {
    install_crypto_provider();

    // Publisher-distributed secret: one (short_id, tag_secret) pair
    // that the listener admits and the client carries.
    let tag_secret_bytes = [0x42u8; 32];
    let short_id_bytes = [0xABu8; 8];
    let cover_sni = "www.microsoft.com";

    // --- server side ----------------------------------------
    let (chain, key) =
        make_self_signed_cert(cover_sni).expect("self-signed cert builds for cover SNI");
    let tls_cfg = build_reality_server_config(chain, key).expect("server config builds");

    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let bound_addr = tcp_listener.local_addr().unwrap();
    let mut reality_listener = RealityListener::new(
        tcp_listener,
        Arc::new(tls_cfg),
        vec![(
            RealityShortId(short_id_bytes),
            RealityTagSecret(tag_secret_bytes),
        )],
        // Generous skew for the deterministic test clock.
        300,
    );

    // Server task: accept one connection, recv one frame,
    // echo it back, close.
    let server_task = tokio::spawn(async move {
        let (mut conn, accepted) = reality_listener
            .accept()
            .await
            .expect("server accepts inbound REALITY peer");
        assert_eq!(
            accepted.short_id.0, short_id_bytes,
            "server records the verified short_id"
        );
        let frame = conn.recv().await.expect("recv first frame");
        // Echo with a tag so the client can confirm both
        // directions traversed the TLS tunnel.
        let mut echoed = frame.to_vec();
        echoed.extend_from_slice(b"::echoed");
        conn.send(Bytes::from(echoed))
            .await
            .expect("send echo frame");
        conn.close().await.expect("server-side close");
    });

    // --- client side ----------------------------------------
    let client_cfg = RealityConfig {
        sni: cover_sni.into(),
        tag_secret_b64: B64.encode(tag_secret_bytes),
        short_id_hex: hex::encode(short_id_bytes),
        allowed_inner_servers: vec![],
        remote: format!("{bound_addr}"),
        // S-5-reality-rng-hook landed: server now injects
        // compute_server_response bytes into ServerHello.Random
        // via the RNG hook. Verify mutual-auth here.
        require_server_response: true,
    };
    let client_transport =
        RealityTransport::new(client_cfg).expect("client transport builds from config");

    let endpoint = Endpoint::Direct(bound_addr);
    let mut conn = client_transport
        .connect(&endpoint)
        .await
        .expect("client completes the REALITY handshake");

    let payload = Bytes::from_static(b"hello-reality-v2");
    conn.send(payload.clone())
        .await
        .expect("client sends plaintext frame inside the TLS tunnel");
    let echoed = conn
        .recv()
        .await
        .expect("client receives the server's echo frame");
    assert!(
        echoed.starts_with(&payload),
        "echoed frame must start with the original payload, got {echoed:?}"
    );
    assert!(
        echoed.ends_with(b"::echoed"),
        "echoed frame must carry the server-side tag, got {echoed:?}"
    );
    conn.close().await.expect("client-side close");

    server_task
        .await
        .expect("server task completes without panic");
}

/// An active prober — a TLS 1.3 client that does NOT know
/// the publisher's `tag_secret`. Its session_id is random
/// bytes, so the listener's verifier rejects the connection
/// before the TLS handshake completes.
#[tokio::test]
async fn reality_v2_listener_refuses_active_prober() {
    install_crypto_provider();

    let tag_secret_bytes = [0x42u8; 32];
    let short_id_bytes = [0xABu8; 8];
    let cover_sni = "www.microsoft.com";

    let (chain, key) = make_self_signed_cert(cover_sni).expect("self-signed cert");
    let tls_cfg = build_reality_server_config(chain, key).expect("server config builds");

    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr = tcp_listener.local_addr().unwrap();
    let mut reality_listener = RealityListener::new(
        tcp_listener,
        Arc::new(tls_cfg),
        vec![(
            RealityShortId(short_id_bytes),
            RealityTagSecret(tag_secret_bytes),
        )],
        300,
    );

    // Active prober: a REALITY client whose tag_secret is
    // wrong (different from the publisher's). Same wire
    // shape, so the listener parses the session_id +
    // proceeds to HMAC verification, which then fails.
    let attacker_cfg = RealityConfig {
        sni: cover_sni.into(),
        tag_secret_b64: B64.encode([0x99u8; 32]), // <-- wrong secret
        short_id_hex: hex::encode(short_id_bytes),
        allowed_inner_servers: vec![],
        remote: format!("{bound_addr}"),
        // The server-side mutual-auth path (writing the
        // REALITY v2 server-response HMAC into
        // ServerHello.Random) is deferred to
        // S-5-reality-server-mutual-auth — see
        // server_driver module doc. While that gap is
        // open the client opts out of mutual-auth.
        require_server_response: false,
    };
    let attacker = RealityTransport::new(attacker_cfg).expect("attacker config valid-shape");

    let server_task = tokio::spawn(async move {
        // We expect the listener to fail the handshake with
        // Error::Tls (carrying ClientAuth under it).
        reality_listener.accept().await
    });

    let endpoint = Endpoint::Direct(bound_addr);
    // The client side will also fail — could be at any
    // stage of the TLS handshake depending on which side
    // notices the inconsistency first; we just assert it
    // does not succeed.
    let client_result = attacker.connect(&endpoint).await;
    assert!(
        client_result.is_err(),
        "attacker handshake must NOT succeed"
    );

    let server_result = server_task.await.expect("server task joins");
    assert!(
        server_result.is_err(),
        "server must reject the attacker (Ok = false-positive admit)"
    );
}

/// G5-7-reality-probe — a VANILLA active prober (a plain rustls TLS client, NOT
/// a REALITY client) probing the REALITY listener is REFUSED. Where
/// `reality_v2_listener_refuses_active_prober` uses a REALITY-shaped client with
/// a WRONG tag_secret, this covers the commonest real-world probe vector: a
/// censor simply opening an ordinary TLS connection. The plain ClientHello's
/// `legacy_session_id` is not a valid REALITY authenticator, so the listener
/// fails the HMAC check and drops the handshake (`ClientAuth`).
///
/// PROBE-REFUSAL vs PROBE-RESISTANCE: this proves the listener does not ADMIT a
/// prober (no false-positive tunnel). It does NOT prove full indistinguishability
/// from the cover site — the current impl REFUSES by DROPPING the connection,
/// which a sophisticated active prober could flag as anomalous (a real
/// www.microsoft.com would complete the handshake). Looking byte-identical to the
/// cover site needs the live-cover-proxy fallback — a deferred publisher-operator
/// concern (see the `reality::server_driver` module doc + ADR-0004).
#[tokio::test]
async fn reality_v2_listener_refuses_a_vanilla_tls_prober() {
    install_crypto_provider();

    let tag_secret_bytes = [0x42u8; 32];
    let short_id_bytes = [0xABu8; 8];
    let cover_sni = "www.microsoft.com";

    let (chain, key) = make_self_signed_cert(cover_sni).expect("self-signed cert");
    let tls_cfg = build_reality_server_config(chain, key).expect("server config builds");

    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound_addr = tcp_listener.local_addr().unwrap();
    let mut reality_listener = RealityListener::new(
        tcp_listener,
        Arc::new(tls_cfg),
        vec![(
            RealityShortId(short_id_bytes),
            RealityTagSecret(tag_secret_bytes),
        )],
        300,
    );

    let server_task = tokio::spawn(async move { reality_listener.accept().await });

    // A VANILLA rustls TLS client — no REALITY authenticator in the
    // ClientHello at all (the real-world censor probe).
    let client_cfg = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(client_cfg));
    let server_name =
        ServerName::try_from(cover_sni.to_owned()).expect("cover SNI is a valid DNS name");
    let stream = tokio::net::TcpStream::connect(bound_addr)
        .await
        .expect("tcp connect to REALITY listener");
    let handshake = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        connector.connect(server_name, stream),
    )
    .await;

    // The vanilla TLS handshake must NOT complete: a timeout (the listener
    // dropped after refusing) or a TLS error — never a completed handshake.
    let admitted = matches!(handshake, Ok(Ok(_)));
    assert!(
        !admitted,
        "a vanilla TLS prober must NOT complete a REALITY handshake (no false-positive admit)"
    );

    let server_result = server_task.await.expect("server task joins");
    assert!(
        server_result.is_err(),
        "the REALITY listener must refuse a vanilla TLS prober (Ok = false-positive admit)"
    );
}

/// Accept-any server-cert verifier for the prober client — it is testing
/// whether the listener ADMITS it, not validating PKI. (The handshake fails at
/// the REALITY auth gate, server-side, well before cert validation matters.)
#[derive(Debug)]
struct AcceptAnyCert;

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}
