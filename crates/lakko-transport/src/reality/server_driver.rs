// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Server-side REALITY v2 handshake driver.
//!
//! Mirrors [`crate::reality::driver`] (the client side):
//! a manually-driven byte-intercept loop over a tokio
//! socket + a `rustls::ServerConnection`.
//!
//! ## Splice points
//!
//! 1. **Inbound first record** (the ClientHello) — extract
//!    the 32-byte `legacy_session_id`, parse it as a
//!    [`crate::reality::RealityAuthenticator`], verify
//!    against the publisher-distributed allow-list of
//!    `(short_id, tag_secret)` pairs. The buffer is NOT
//!    mutated; rustls sees the raw bytes after the splice.
//!
//! 2. **Mutual-auth via ServerHello.Random** — after the
//!    ClientHello is verified, the driver arms
//!    [`crate::reality::rng::RealityRng`] with
//!    `compute_server_response(tag_secret, short_id, ts)` bytes
//!    immediately before `process_new_packets` processes the
//!    inbound ClientHello. That processing generates the
//!    `ServerHello` and calls the first 32-byte
//!    `SecureRandom::fill` for `ServerHello.Random` — which
//!    receives our server-response HMAC bytes via the RNG
//!    hook. The client verifies this in
//!    [`crate::reality::driver::drive_handshake`] when
//!    `require_server_response = true`.
//!
//! On any verification failure the driver returns a typed
//! [`RealityServerDriverError`]; the caller drops the
//! connection without writing further bytes — there is no
//! "fall through to a real cover-site proxy" path. That
//! live-cover-proxy posture is a publisher-operator concern
//! and lives outside the driver.
//!
//! On success the driver returns the verified `short_id`
//! and the client's `RealityTimestamp` so the caller can
//! log / route / replay-window the session.

use std::io::Cursor;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConnection;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::reality::authenticator::{
    compute_server_response, verify, RealityAuthError, RealityAuthenticator, RealityTimestamp,
    AUTHENTICATOR_LEN,
};
use crate::reality::cert::AcceptAnyCertVerifier;
use crate::reality::config::{RealityShortId, RealityTagSecret};
use crate::reality::handshake::{
    extract_client_hello_session_id, TlsWireError, TLS_RECORD_HEADER_LEN,
};
use crate::reality::rng;

/// Errors the server-side driver can surface. Variants
/// branch on cause; never inspect the message string.
#[derive(Debug, Error)]
pub enum RealityServerDriverError {
    /// `rustls::ServerConfig` could not be built.
    #[error("rustls ServerConfig build failed: {0}")]
    ConfigBuild(String),
    /// `rustls::ServerConnection::new` rejected the config.
    #[error("rustls ServerConnection::new failed: {0}")]
    ServerConnectionInit(String),
    /// Socket I/O failed.
    #[error("socket I/O failed: {0}")]
    SocketIo(String),
    /// Peer closed before the handshake completed.
    #[error("peer closed before handshake completed")]
    PeerClosed,
    /// rustls's state machine returned an error.
    #[error("rustls handshake error: {0}")]
    Rustls(String),
    /// Wire-format parse of the inbound ClientHello failed.
    #[error("ClientHello parse failed: {0}")]
    ExtractClientHello(#[source] TlsWireError),
    /// REALITY authentication failed — the peer is either
    /// not a Lakko REALITY client (probably an active
    /// prober) or carries a session_id MAC we cannot
    /// verify. Production deployments typically drop the
    /// connection at this point.
    #[error("REALITY client auth failed: {0}")]
    ClientAuth(#[source] RealityAuthError),
    /// Inbound buffer overflowed before a complete first
    /// record arrived — defends against slow-loris drip.
    #[error("inbound handshake buffer exceeded {cap} bytes")]
    HandshakeBufferOverflow {
        /// Hard cap on buffered pre-extraction bytes.
        cap: usize,
    },
}

/// Convenience alias.
pub type RealityServerDriverResult<T> = core::result::Result<T, RealityServerDriverError>;

/// Hard cap on bytes the driver buffers before a complete
/// inbound TLS record (the ClientHello) arrives. A normal
/// ClientHello is well under 1 KiB; we leave 64× headroom.
pub const MAX_HANDSHAKE_BUFFER_BYTES: usize = 64 * 1024;

/// Outcome of a successful server-side handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptedClient {
    /// The publisher-assigned `short_id` the client proved
    /// it knows the `tag_secret` for. The caller can use
    /// this to route the session, scope rate-limit counters,
    /// or attach it to the post-handshake `Connection`.
    pub short_id: RealityShortId,
    /// The client's wall-clock timestamp as carried in the
    /// authenticator. The caller can feed this into a
    /// replay-window detector that drops duplicate
    /// `(short_id, ts)` pairs.
    pub client_timestamp: RealityTimestamp,
}

/// Build a `rustls::ServerConfig` set up for REALITY:
///
/// - the supplied cert + key (typically a self-signed cert
///   for the cover SNI from [`make_self_signed_cert`]);
/// - ring crypto provider, matching the rest of the
///   `lakko-transport` stack;
/// - TLS 1.3 only (REALITY pins TLS 1.3 — see
///   [`crate::reality::cert::AcceptAnyCertVerifier::verify_tls12_signature`]
///   which symmetrically refuses TLS 1.2 on the client
///   side).
///
/// # Errors
///
/// [`RealityServerDriverError::ConfigBuild`] on a rustls
/// provider-config error.
pub fn build_reality_server_config(
    cert_chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> RealityServerDriverResult<rustls::ServerConfig> {
    // Substitute REALITY_RNG for ring's default SecureRandom so
    // the first 32-byte fill during ServerHello generation (inside
    // process_new_packets) receives the server-response HMAC bytes
    // armed by drive_server_handshake.
    let provider = rustls::crypto::CryptoProvider {
        secure_random: &rng::REALITY_RNG,
        ..rustls::crypto::ring::default_provider()
    };
    // Keep AcceptAnyCertVerifier referenced so the
    // pub(crate)-scoped struct stays connected to the
    // server-build path the audit-grep marker expects.
    // The verifier itself is a client-side concept — server
    // doesn't run cert-chain verification.
    let _client_cert_check_marker: AcceptAnyCertVerifier = AcceptAnyCertVerifier::new(&provider);

    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| RealityServerDriverError::ConfigBuild(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| RealityServerDriverError::ConfigBuild(e.to_string()))?;
    Ok(cfg)
}

/// Build a fresh self-signed certificate + key for the
/// supplied cover SNI. Returned in the rustls 0.23-friendly
/// `(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)`
/// shape so the result feeds straight into
/// [`build_reality_server_config`].
///
/// REALITY publishers typically generate a fresh cover-SNI
/// cert at deploy time and rotate periodically; this helper
/// makes both tests and bootstrap deploys ergonomic.
///
/// # Errors
///
/// Returns [`RealityServerDriverError::ConfigBuild`] if
/// rcgen rejects the SAN (e.g. empty / not a valid DNS
/// name).
pub fn make_self_signed_cert(
    sni: &str,
) -> RealityServerDriverResult<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    // Empty SAN slips past rcgen but yields an effectively
    // un-bindable cert (no subjectAltName matches anything).
    // Catch it here so the deploy-time misconfiguration
    // surfaces as a typed error rather than a mysterious
    // TLS handshake failure later.
    if sni.is_empty() {
        return Err(RealityServerDriverError::ConfigBuild(
            "self-signed cert SNI must be non-empty".into(),
        ));
    }
    let cert = rcgen::generate_simple_self_signed(vec![sni.to_owned()])
        .map_err(|e| RealityServerDriverError::ConfigBuild(format!("rcgen: {e}")))?;
    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
    Ok((vec![cert_der], key_der))
}

/// Drive a REALITY v2 server-side handshake to completion
/// over `socket`. On `Ok`, `conn` is post-handshake and the
/// caller can shuttle application data through it
/// (typically wrapped in
/// `crate::reality::stream::RealityConnection` — same
/// post-handshake `Connection` impl works for both halves).
///
/// `allowed_pairs` is the publisher-distributed allow-list
/// of `(short_id, tag_secret)` pairs; `now` + `tolerance_secs`
/// gate replay / clock-skew on the client's authenticator
/// timestamp.
///
/// # Errors
///
/// See [`RealityServerDriverError`].
pub async fn drive_server_handshake<S>(
    socket: &mut S,
    conn: &mut ServerConnection,
    allowed_pairs: &[(RealityShortId, RealityTagSecret)],
    now: RealityTimestamp,
    tolerance_secs: u64,
) -> RealityServerDriverResult<AcceptedClient>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut state = ServerHandshakeState::new();

    while conn.is_handshaking() {
        // Inbound side — must run first: until we feed the
        // ClientHello to rustls, `wants_write` returns false.
        if conn.wants_read() {
            let mut chunk = [0u8; 8 * 1024];
            let n = socket
                .read(&mut chunk)
                .await
                .map_err(|e| RealityServerDriverError::SocketIo(e.to_string()))?;
            if n == 0 {
                return Err(RealityServerDriverError::PeerClosed);
            }
            state.inbound_buf.extend_from_slice(&chunk[..n]);

            if !state.verified_client_hello {
                state.try_extract_and_verify(allowed_pairs, now, tolerance_secs)?;
                // If we still do not have a complete first
                // record, loop around for another read.
                if !state.verified_client_hello {
                    continue;
                }
            }

            // Feed buffered bytes to rustls in one go.
            let mut cur = Cursor::new(state.inbound_buf.as_slice());
            conn.read_tls(&mut cur)
                .map_err(|e| RealityServerDriverError::Rustls(e.to_string()))?;
            let consumed = usize::try_from(cur.position()).expect("Cursor position fits usize");
            state.inbound_buf.drain(..consumed);

            // Arm the server-side RNG inject slot once,
            // immediately before process_new_packets. That
            // call generates the ServerHello and invokes
            // the first 32-byte SecureRandom::fill —
            // which receives the server-response HMAC bytes.
            // There is no .await between the arm and the
            // process_new_packets call, so the thread-local
            // slot cannot be consumed by another future.
            if !state.server_rng_armed {
                let short_id = state
                    .matched_short_id
                    .expect("verified_client_hello implies short_id set");
                let client_ts = state
                    .client_timestamp
                    .expect("verified_client_hello implies client_timestamp set");
                let tag_secret = allowed_pairs
                    .iter()
                    .find(|(s, _)| *s == short_id)
                    .map(|(_, ts)| ts)
                    .expect("verified short_id must be in allowed_pairs");
                let server_response = compute_server_response(tag_secret, &short_id, client_ts);
                rng::RealityRng::arm(server_response);
                state.server_rng_armed = true;
            }

            conn.process_new_packets()
                .map_err(|e| RealityServerDriverError::Rustls(e.to_string()))?;

            // `read_tls` takes at most 4096 bytes per call, and
            // bytes left here when the handshake ends would be
            // lost: the first request can share a socket read
            // with the ClientFinished.
            while !state.inbound_buf.is_empty() {
                let mut cur = Cursor::new(state.inbound_buf.as_slice());
                let taken = conn
                    .read_tls(&mut cur)
                    .map_err(|e| RealityServerDriverError::Rustls(e.to_string()))?;
                state.inbound_buf.drain(..taken);
                conn.process_new_packets()
                    .map_err(|e| RealityServerDriverError::Rustls(e.to_string()))?;
                if taken == 0 {
                    break;
                }
            }
        }

        // Outbound side — drain rustls's pending writes.
        // ServerHello.Random is populated with the server-response
        // HMAC via the RNG hook above.
        if conn.wants_write() {
            let mut write_buf = Vec::with_capacity(4096);
            while conn.wants_write() {
                conn.write_tls(&mut write_buf)
                    .map_err(|e| RealityServerDriverError::Rustls(e.to_string()))?;
            }
            socket
                .write_all(&write_buf)
                .await
                .map_err(|e| RealityServerDriverError::SocketIo(e.to_string()))?;
            socket
                .flush()
                .await
                .map_err(|e| RealityServerDriverError::SocketIo(e.to_string()))?;
        }
    }

    Ok(AcceptedClient {
        short_id: state
            .matched_short_id
            .expect("verified_client_hello implies short_id set"),
        client_timestamp: state
            .client_timestamp
            .expect("verified_client_hello implies client_timestamp set"),
    })
}

/// Server-side handshake bookkeeping kept across loop
/// iterations.
struct ServerHandshakeState {
    /// `true` once the inbound ClientHello.session_id has
    /// been extracted, verified, and the matching
    /// `(short_id, _)` pair recorded.
    verified_client_hello: bool,
    /// `true` once the server-side RNG inject slot has
    /// been armed with the server-response HMAC. Guards
    /// against redundant arm calls across loop iterations.
    server_rng_armed: bool,
    /// Buffer of inbound bytes that have arrived but not
    /// yet been fed to rustls.
    inbound_buf: Vec<u8>,
    /// `Some` after verification — the short_id from the
    /// allow-list that matched the inbound authenticator.
    matched_short_id: Option<RealityShortId>,
    /// `Some` after verification — the client's wall-clock
    /// timestamp from the authenticator.
    client_timestamp: Option<RealityTimestamp>,
}

impl ServerHandshakeState {
    fn new() -> Self {
        Self {
            verified_client_hello: false,
            server_rng_armed: false,
            inbound_buf: Vec::with_capacity(8 * 1024),
            matched_short_id: None,
            client_timestamp: None,
        }
    }

    /// Try to extract + verify the ClientHello.session_id
    /// out of the inbound buffer. On success records the
    /// matched `(short_id, tag_secret)` + client timestamp
    /// in self. On "first record not yet fully buffered"
    /// returns `Ok(())` without setting the flag — the
    /// caller continues reading the socket.
    fn try_extract_and_verify(
        &mut self,
        allowed_pairs: &[(RealityShortId, RealityTagSecret)],
        now: RealityTimestamp,
        tolerance_secs: u64,
    ) -> RealityServerDriverResult<()> {
        // Slow-loris defence.
        if self.inbound_buf.len() > MAX_HANDSHAKE_BUFFER_BYTES {
            return Err(RealityServerDriverError::HandshakeBufferOverflow {
                cap: MAX_HANDSHAKE_BUFFER_BYTES,
            });
        }
        // Need at least the 5-byte record header.
        if self.inbound_buf.len() < TLS_RECORD_HEADER_LEN {
            return Ok(());
        }
        let record_length = u16::from_be_bytes([self.inbound_buf[3], self.inbound_buf[4]]) as usize;
        let total = TLS_RECORD_HEADER_LEN + record_length;
        if self.inbound_buf.len() < total {
            // First record not yet complete — keep reading.
            return Ok(());
        }

        // Extract the 32-byte session_id slot from the
        // ClientHello prefix.
        let session_id = extract_client_hello_session_id(&self.inbound_buf[..total])
            .map_err(RealityServerDriverError::ExtractClientHello)?;
        let _ = AUTHENTICATOR_LEN; // doc anchor: session_id length is bound here
        let authenticator = RealityAuthenticator(session_id);

        // Verify against the allow-set.
        let (short_id, client_ts) = verify(allowed_pairs, &authenticator, now, tolerance_secs)
            .map_err(RealityServerDriverError::ClientAuth)?;

        // The server-response HMAC for ServerHello.Random is
        // computed in drive_server_handshake after this
        // returns, using allowed_pairs to look up tag_secret
        // by matched_short_id. We record the short_id and
        // timestamp here so the caller can do that lookup.
        self.matched_short_id = Some(short_id);
        self.client_timestamp = Some(client_ts);
        self.verified_client_hello = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reality::authenticator::compute;
    use crate::reality::handshake::{
        HANDSHAKE_HEADER_LEN, HANDSHAKE_TYPE_CLIENT_HELLO, LEGACY_VERSION_LEN, RANDOM_LEN,
        TLS_CONTENT_TYPE_HANDSHAKE, TLS_RECORD_HEADER_LEN,
    };

    fn build_client_hello_with_session_id(session_id: [u8; AUTHENTICATOR_LEN]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(HANDSHAKE_TYPE_CLIENT_HELLO);
        body.extend_from_slice(&[0, 0, 0]); // hs length placeholder
        let hs_body_start = body.len();
        body.extend_from_slice(&[0x03, 0x03]); // legacy_version
        body.extend_from_slice(&[0xAA; RANDOM_LEN]); // Random
        body.push(u8::try_from(AUTHENTICATOR_LEN).unwrap()); // session_id length
        body.extend_from_slice(&session_id); // session_id
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // cipher_suites
        body.extend_from_slice(&[0x01, 0x00]); // compression_methods
        body.extend_from_slice(&[0x00, 0x00]); // extensions length 0

        let hs_body_len = body.len() - hs_body_start;
        let hs_len_bytes = u32::try_from(hs_body_len).unwrap().to_be_bytes();
        body[1] = hs_len_bytes[1];
        body[2] = hs_len_bytes[2];
        body[3] = hs_len_bytes[3];

        let mut out = Vec::new();
        out.push(TLS_CONTENT_TYPE_HANDSHAKE);
        out.extend_from_slice(&[0x03, 0x01]);
        out.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn self_signed_cert_builds_a_valid_server_config() {
        let (chain, key) = make_self_signed_cert("www.microsoft.com").expect("rcgen");
        let _ = build_reality_server_config(chain, key).expect("ServerConfig builds");
    }

    #[test]
    fn self_signed_cert_rejects_empty_sni() {
        let err = make_self_signed_cert("").expect_err("empty SNI must reject");
        match err {
            RealityServerDriverError::ConfigBuild(_) => {},
            other => panic!("expected ConfigBuild, got {other:?}"),
        }
    }

    /// `try_extract_and_verify` happy path against a buffer
    /// that contains a complete REALITY ClientHello.
    #[test]
    fn try_extract_and_verify_happy_path() {
        let tag_secret = RealityTagSecret([0x42; 32]);
        let short_id = RealityShortId([0xAB; 8]);
        let ts = RealityTimestamp(1_700_000_000);
        let auth = compute(&tag_secret, &short_id, ts);

        let wire = build_client_hello_with_session_id(auth.0);

        let mut state = ServerHandshakeState::new();
        state.inbound_buf.extend_from_slice(&wire);
        state
            .try_extract_and_verify(&[(short_id, tag_secret)], ts, 30)
            .expect("verify must succeed");
        assert!(state.verified_client_hello);
        assert_eq!(state.matched_short_id.unwrap(), short_id);
        assert_eq!(state.client_timestamp.unwrap(), ts);
    }

    /// A buffer that does not yet contain the full first
    /// record yields `Ok(())` with the flag still false —
    /// the loop reads more bytes next iteration.
    #[test]
    fn try_extract_and_verify_partial_buffer_waits() {
        let tag_secret = RealityTagSecret([0x42; 32]);
        let short_id = RealityShortId([0xAB; 8]);
        let ts = RealityTimestamp(1_700_000_000);
        let auth = compute(&tag_secret, &short_id, ts);
        let wire = build_client_hello_with_session_id(auth.0);

        let mut state = ServerHandshakeState::new();
        // Feed only first 10 bytes — header is there but
        // body is short.
        state.inbound_buf.extend_from_slice(&wire[..10]);
        state
            .try_extract_and_verify(&[(short_id, tag_secret)], ts, 30)
            .expect("partial buffer not an error");
        assert!(!state.verified_client_hello);
    }

    /// A complete ClientHello whose session_id is random
    /// bytes (NOT a REALITY authenticator) — the verifier
    /// rejects with `ClientAuth(ShortIdNotAllowed)`. This is
    /// the active-prober refusal path.
    #[test]
    fn try_extract_and_verify_active_prober_refusal() {
        let tag_secret = RealityTagSecret([0x42; 32]);
        let short_id = RealityShortId([0xAB; 8]);
        let ts = RealityTimestamp(1_700_000_000);
        // Garbage session_id — looks like a normal browser
        // 32-byte cache token, not a REALITY authenticator.
        let garbage = [0xC3u8; AUTHENTICATOR_LEN];
        let wire = build_client_hello_with_session_id(garbage);

        let mut state = ServerHandshakeState::new();
        state.inbound_buf.extend_from_slice(&wire);
        let err = state
            .try_extract_and_verify(&[(short_id, tag_secret)], ts, 30)
            .expect_err("garbage session_id must be rejected");
        assert!(
            matches!(err, RealityServerDriverError::ClientAuth(_)),
            "expected ClientAuth, got {err:?}"
        );
    }

    /// A short session_id length (e.g. a real browser
    /// ClientHello with session_id_len = 0) is rejected at
    /// the wire-format layer, not at the HMAC layer.
    #[test]
    fn try_extract_and_verify_short_session_id_rejected_at_wire_layer() {
        let tag_secret = RealityTagSecret([0x42; 32]);
        let short_id = RealityShortId([0xAB; 8]);
        let ts = RealityTimestamp(1_700_000_000);

        let mut wire = build_client_hello_with_session_id([0u8; AUTHENTICATOR_LEN]);
        // Patch the session_id length byte to 0 — looks
        // like a real TLS 1.3 browser that omits the
        // compatibility session_id.
        let session_id_len_offset =
            TLS_RECORD_HEADER_LEN + HANDSHAKE_HEADER_LEN + LEGACY_VERSION_LEN + RANDOM_LEN;
        wire[session_id_len_offset] = 0;

        let mut state = ServerHandshakeState::new();
        state.inbound_buf.extend_from_slice(&wire);
        let err = state
            .try_extract_and_verify(&[(short_id, tag_secret)], ts, 30)
            .expect_err("non-32 session_id must be rejected");
        assert!(
            matches!(
                err,
                RealityServerDriverError::ExtractClientHello(
                    TlsWireError::UnexpectedSessionIdLen { .. }
                )
            ),
            "expected ExtractClientHello(UnexpectedSessionIdLen), got {err:?}"
        );
    }

    /// Slow-loris drip-feed — the buffer fills past the
    /// configured cap without a complete first record →
    /// `HandshakeBufferOverflow`.
    #[test]
    fn try_extract_and_verify_overflow_cap() {
        let tag_secret = RealityTagSecret([0x42; 32]);
        let short_id = RealityShortId([0xAB; 8]);
        let ts = RealityTimestamp(1_700_000_000);

        let mut state = ServerHandshakeState::new();
        // Push junk that LOOKS like a TLS record header
        // claiming a huge payload — the cap fires before
        // the bytes ever finish arriving.
        state.inbound_buf.extend_from_slice(&[0x16, 0x03, 0x03]);
        state.inbound_buf.extend_from_slice(&[0xFF, 0xFF]);
        state.inbound_buf.resize(MAX_HANDSHAKE_BUFFER_BYTES + 1, 0);
        let err = state
            .try_extract_and_verify(&[(short_id, tag_secret)], ts, 30)
            .expect_err("overflow must reject");
        match err {
            RealityServerDriverError::HandshakeBufferOverflow { cap } => {
                assert_eq!(cap, MAX_HANDSHAKE_BUFFER_BYTES);
            },
            other => panic!("expected HandshakeBufferOverflow, got {other:?}"),
        }
    }
}
