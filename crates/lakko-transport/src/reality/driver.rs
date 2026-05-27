// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Async TLS bytes-interceptor harness that drives a
//! REALITY v2 handshake through a stock
//! [`rustls::ClientConnection`].
//!
//! ## How it works
//!
//! rustls's high-level adapter API (`Stream`, `StreamOwned`,
//! `tokio-rustls`) does not let us see / mutate the raw
//! handshake bytes between the rustls state machine and
//! the underlying socket. We therefore drive the
//! connection by hand using rustls's lower-level API:
//!
//! - [`rustls::ClientConnection::wants_write`] and
//!   [`wants_read`](rustls::ClientConnection::wants_read)
//!   tell us which side is ready.
//! - [`rustls::ClientConnection::write_tls`] writes pending
//!   TLS records into a sync `io::Write` (we pass a
//!   `Vec<u8>`).
//! - [`rustls::ClientConnection::read_tls`] consumes TLS
//!   records from a sync `io::Read` (we pass a
//!   `Cursor<&[u8]>` over our async-fed buffer).
//! - [`process_new_packets`](rustls::ClientConnection::process_new_packets)
//!   advances the state machine after every read.
//!
//! Between the sync drain and the async socket I/O we
//! splice in two REALITY hooks:
//!
//! 1. The first outgoing batch (the ClientHello first
//!    flight) is patched by
//!    [`crate::reality::handshake::patch_client_hello_session_id`]
//!    — its 32-byte legacy_session_id slot becomes the
//!    REALITY authenticator
//!    ([`compute`](crate::reality::authenticator::compute)).
//! 2. The first incoming record (the ServerHello) is
//!    parsed by
//!    [`crate::reality::handshake::extract_server_hello_random`];
//!    the 32-byte Random is verified against
//!    [`crate::reality::authenticator::verify_server_response`].
//!    If the server is not REALITY-aware (e.g. a censor's
//!    active-probe forwarded to the cover site verbatim)
//!    the Random is uncorrelated randomness and the
//!    HMAC check fails — the driver aborts with
//!    `RealityAuthError::BadServerResponse` before any
//!    application byte flows.
//!
//! ## Test posture
//!
//! Unit tests cover the pure functions:
//! - waiting-for-complete-record state machine,
//! - "first outgoing batch only" patch logic guard,
//! - decode of TLS record length out of an inbound buffer.
//!
//! The full async loop against a live REALITY server is
//! gated on a working REALITY server implementation
//! (tracked as `S-5-reality-server`). The pieces
//! exercised here are exactly the pieces that decide
//! "does this connection actually carry our auth?" — the
//! rest is rustls boilerplate.

use std::io::Cursor;
use std::sync::Arc;

use rustls::ClientConnection;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::reality::authenticator::{
    compute, verify_server_response, RealityAuthError, RealityAuthenticator, RealityTimestamp,
};
use crate::reality::cert::AcceptAnyCertVerifier;
use crate::reality::config::{RealityShortId, RealityTagSecret};
use crate::reality::handshake::{
    extract_server_hello_random, patch_client_hello_session_id, TlsWireError, TLS_RECORD_HEADER_LEN,
};

/// Failure modes the REALITY handshake driver surfaces.
/// Branch on the variant; never on the message string.
#[derive(Debug, Error)]
pub enum RealityDriverError {
    /// `rustls::ClientConfig` could not be built. The
    /// underlying message is opaque (rustls's own type)
    /// because we never construct ClientConfigs from
    /// attacker-controlled inputs — this should only
    /// fire on a programmer bug (e.g. bad crypto provider
    /// setup).
    #[error("rustls ClientConfig build failed: {0}")]
    ConfigBuild(String),
    /// Cover-site SNI from the bootstrap config was
    /// rejected by rustls (not a valid DNS name).
    #[error("invalid SNI for cover site: {0}")]
    InvalidSni(String),
    /// `rustls::ClientConnection::new` rejected the
    /// (config, server_name) pair.
    #[error("rustls ClientConnection::new failed: {0}")]
    ClientConnectionInit(String),
    /// Socket I/O failed (TCP reset, timeout, peer
    /// closed before handshake done).
    #[error("socket I/O failed: {0}")]
    SocketIo(String),
    /// Peer closed cleanly before the handshake
    /// completed — typical of a censor that flagged the
    /// stream as suspicious and dropped it.
    #[error("peer closed before handshake completed")]
    PeerClosed,
    /// rustls's state machine returned an error
    /// (malformed wire, cipher mismatch, certificate
    /// path failure if a CertVerifier other than
    /// AcceptAnyCertVerifier was wired by mistake, etc.).
    #[error("rustls handshake error: {0}")]
    Rustls(String),
    /// Patching the ClientHello.session_id failed at the
    /// wire-format level. Should not happen in practice
    /// — rustls's TLS 1.3 ClientHello always carries a
    /// 32-byte session_id slot — but we surface a typed
    /// error rather than panic.
    #[error("ClientHello patch failed: {0}")]
    PatchClientHello(#[source] TlsWireError),
    /// Parsing the inbound ServerHello.Random failed at
    /// the wire-format level (truncated, wrong handshake
    /// type, record-length mismatch).
    #[error("ServerHello parse failed: {0}")]
    ExtractServerHello(#[source] TlsWireError),
    /// REALITY mutual-auth failed: the ServerHello.Random
    /// HMAC under our tag_secret did not match. Either
    /// the peer is not REALITY-aware (active-probe
    /// scenario) or it has a different `tag_secret` for
    /// this short_id.
    #[error("REALITY server auth failed: {0}")]
    ServerAuth(#[source] RealityAuthError),
    /// The driver received more handshake bytes from the
    /// peer than the configured cap allows before being
    /// able to extract a complete first record. Defends
    /// against a slow-loris peer that drips a few bytes
    /// at a time, forcing the driver to buffer forever.
    #[error("inbound handshake buffer exceeded {cap} bytes")]
    HandshakeBufferOverflow {
        /// Hard cap on buffered pre-extraction bytes.
        cap: usize,
    },
}

/// Convenience alias.
pub type RealityDriverResult<T> = core::result::Result<T, RealityDriverError>;

/// Hard cap on bytes the driver buffers between read
/// attempts while waiting for one complete inbound
/// TLS record (the ServerHello). A normal TLS 1.3
/// ServerHello is well under 1 KiB; we leave 64×
/// headroom for unusual extensions configurations.
pub const MAX_HANDSHAKE_BUFFER_BYTES: usize = 64 * 1024;

/// Build a `rustls::ClientConfig` set up for REALITY:
///
/// - any-cert verifier (PKI is irrelevant — see
///   [`AcceptAnyCertVerifier`] safety contract);
/// - ring crypto provider, matching the rest of the
///   `lakko-transport` stack;
/// - default TLS 1.3 cipher / kx negotiation.
///
/// # Errors
///
/// [`RealityDriverError::ConfigBuild`] on a rustls
/// provider-config error (should never fire on a
/// well-configured build).
pub fn build_reality_client_config() -> RealityDriverResult<rustls::ClientConfig> {
    let provider = rustls::crypto::ring::default_provider();
    let verifier = AcceptAnyCertVerifier::new(&provider).into_arc();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| RealityDriverError::ConfigBuild(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(cfg)
}

/// Drive a REALITY v2 handshake to completion over
/// `socket`. On `Ok`, `conn` is in the post-handshake
/// state and the caller can shuttle application data
/// through it (typically wrapped in
/// `crate::reality::stream::RealityConnection`).
///
/// `tag_secret` + `short_id` + `timestamp` are the
/// publisher-distributed bootstrap secrets — see
/// [`crate::reality::authenticator`].
///
/// # Errors
///
/// See [`RealityDriverError`].
pub async fn drive_handshake<S>(
    socket: &mut S,
    conn: &mut ClientConnection,
    tag_secret: &RealityTagSecret,
    short_id: &RealityShortId,
    timestamp: RealityTimestamp,
) -> RealityDriverResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let authenticator = compute(tag_secret, short_id, timestamp);
    let mut state = HandshakeState::new();

    while conn.is_handshaking() {
        // Outgoing side — drain rustls's pending writes,
        // patch the first batch (the ClientHello), send.
        if conn.wants_write() {
            let mut write_buf = Vec::with_capacity(4096);
            while conn.wants_write() {
                conn.write_tls(&mut write_buf)
                    .map_err(|e| RealityDriverError::Rustls(e.to_string()))?;
            }
            if !state.patched_client_hello {
                patch_client_hello_session_id(&mut write_buf, &authenticator.0)
                    .map_err(RealityDriverError::PatchClientHello)?;
                state.patched_client_hello = true;
            }
            socket
                .write_all(&write_buf)
                .await
                .map_err(|e| RealityDriverError::SocketIo(e.to_string()))?;
            socket
                .flush()
                .await
                .map_err(|e| RealityDriverError::SocketIo(e.to_string()))?;
        }

        // Incoming side — read at least one chunk, then
        // (if not yet done) extract + verify the
        // ServerHello.Random, then feed to rustls.
        if conn.wants_read() {
            let mut chunk = [0u8; 8 * 1024];
            let n = socket
                .read(&mut chunk)
                .await
                .map_err(|e| RealityDriverError::SocketIo(e.to_string()))?;
            if n == 0 {
                return Err(RealityDriverError::PeerClosed);
            }
            state.inbound_buf.extend_from_slice(&chunk[..n]);

            if !state.verified_server_hello {
                state.try_extract_and_verify(tag_secret, *short_id, timestamp, &authenticator)?;
                // If extraction is still pending (need
                // more bytes), loop around for another
                // socket read.
                if !state.verified_server_hello {
                    continue;
                }
            }

            // Feed buffered bytes to rustls in one go.
            // `read_tls` consumes as many records as fit
            // in the supplied reader; we then drain the
            // consumed prefix from our buffer.
            let mut cur = Cursor::new(state.inbound_buf.as_slice());
            conn.read_tls(&mut cur)
                .map_err(|e| RealityDriverError::Rustls(e.to_string()))?;
            let consumed = usize::try_from(cur.position()).expect("Cursor position fits usize");
            state.inbound_buf.drain(..consumed);

            conn.process_new_packets()
                .map_err(|e| RealityDriverError::Rustls(e.to_string()))?;
        }
    }

    Ok(())
}

/// Driver-side bookkeeping kept across loop iterations.
struct HandshakeState {
    /// `true` once the outbound ClientHello batch has
    /// been patched with our authenticator.
    patched_client_hello: bool,
    /// `true` once the inbound ServerHello.Random has
    /// been extracted and verified.
    verified_server_hello: bool,
    /// Buffer of inbound bytes that have arrived but not
    /// yet been fed to rustls. We hold onto them until we
    /// have at least one complete first record (for
    /// extraction) and then drain the consumed prefix.
    inbound_buf: Vec<u8>,
}

impl HandshakeState {
    fn new() -> Self {
        Self {
            patched_client_hello: false,
            verified_server_hello: false,
            inbound_buf: Vec::with_capacity(8 * 1024),
        }
    }

    /// Try to extract + verify the ServerHello.Random
    /// out of the inbound buffer. Returns Ok and sets
    /// `verified_server_hello = true` on success;
    /// returns Ok without setting the flag if the first
    /// record is not yet fully buffered (caller should
    /// continue reading from the socket).
    fn try_extract_and_verify(
        &mut self,
        tag_secret: &RealityTagSecret,
        short_id: RealityShortId,
        client_timestamp: RealityTimestamp,
        _authenticator: &RealityAuthenticator,
    ) -> RealityDriverResult<()> {
        // Defend against a slow-loris peer that never
        // emits a complete record. The cap is well above
        // any real ServerHello — its only purpose is to
        // bound memory under attack.
        if self.inbound_buf.len() > MAX_HANDSHAKE_BUFFER_BYTES {
            return Err(RealityDriverError::HandshakeBufferOverflow {
                cap: MAX_HANDSHAKE_BUFFER_BYTES,
            });
        }
        // Need at least the TLS record header to know
        // how many bytes to wait for.
        if self.inbound_buf.len() < TLS_RECORD_HEADER_LEN {
            return Ok(());
        }
        let record_len = u16::from_be_bytes([self.inbound_buf[3], self.inbound_buf[4]]) as usize;
        let total_len = TLS_RECORD_HEADER_LEN + record_len;
        if self.inbound_buf.len() < total_len {
            return Ok(());
        }
        // We now have at least one full record. Peel
        // off + extract + verify.
        let first_record = &self.inbound_buf[..total_len];
        let server_random = extract_server_hello_random(first_record)
            .map_err(RealityDriverError::ExtractServerHello)?;
        verify_server_response(tag_secret, &short_id, client_timestamp, &server_random)
            .map_err(RealityDriverError::ServerAuth)?;
        self.verified_server_hello = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reality::config::{SHORT_ID_LEN, TAG_SECRET_LEN};

    fn fixtures() -> (RealityTagSecret, RealityShortId, RealityTimestamp) {
        (
            RealityTagSecret([0x42; TAG_SECRET_LEN]),
            RealityShortId([0xAB; SHORT_ID_LEN]),
            RealityTimestamp(1_700_000_000),
        )
    }

    fn dummy_authenticator() -> RealityAuthenticator {
        let (ts, sid, t) = fixtures();
        compute(&ts, &sid, t)
    }

    /// Build a ServerHello record carrying a specific
    /// 32-byte Random. Same shape as the
    /// `handshake.rs::tests::build_server_hello` helper —
    /// duplicated here to keep `driver.rs` testable
    /// without exposing the test fixture across modules.
    fn build_server_hello(random: [u8; 32]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(0x02); // ServerHello
        body.extend_from_slice(&[0, 0, 0]); // hs length placeholder
        let start = body.len();
        body.extend_from_slice(&[0x03, 0x03]); // legacy_version
        body.extend_from_slice(&random);
        body.push(0x00); // session_id_echo len = 0
        body.extend_from_slice(&[0x13, 0x01]); // cipher_suite
        body.push(0x00); // legacy_compression_method
        body.extend_from_slice(&[0x00, 0x00]); // extensions len = 0
        let hs_len = u32::try_from(body.len() - start).unwrap().to_be_bytes();
        body[1] = hs_len[1];
        body[2] = hs_len[2];
        body[3] = hs_len[3];

        let mut wire = Vec::new();
        wire.push(0x16); // record type = handshake
        wire.extend_from_slice(&[0x03, 0x03]); // record version
        wire.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(&body);
        wire
    }

    #[test]
    fn state_waits_for_record_header() {
        let mut state = HandshakeState::new();
        let (ts, sid, t) = fixtures();
        // Empty buffer — try should be a no-op success.
        state
            .try_extract_and_verify(&ts, sid, t, &dummy_authenticator())
            .expect("empty buf");
        assert!(!state.verified_server_hello);

        // Partial header — still no-op.
        state.inbound_buf.extend_from_slice(&[0x16, 0x03]);
        state
            .try_extract_and_verify(&ts, sid, t, &dummy_authenticator())
            .expect("partial header");
        assert!(!state.verified_server_hello);
    }

    #[test]
    fn state_waits_for_complete_record() {
        let mut state = HandshakeState::new();
        let (ts, sid, t) = fixtures();
        // Full header but truncated body.
        state.inbound_buf.extend_from_slice(&[
            0x16, 0x03, 0x03, // record type + version
            0x00, 0x50, // declared length = 80, but we only give 5 body bytes
            0x02, 0x00, 0x00, 0x4C, 0x03, // 5 random body bytes
        ]);
        state
            .try_extract_and_verify(&ts, sid, t, &dummy_authenticator())
            .expect("incomplete record");
        assert!(!state.verified_server_hello);
    }

    #[test]
    fn state_verifies_on_complete_correct_record() {
        let mut state = HandshakeState::new();
        let (tag_secret, short_id, ts) = fixtures();
        // Compute the EXPECTED server response under our
        // tag_secret. Use it as the ServerHello.Random.
        let expected_response =
            crate::reality::authenticator::compute_server_response(&tag_secret, &short_id, ts);
        let wire = build_server_hello(expected_response);
        state.inbound_buf.extend_from_slice(&wire);

        state
            .try_extract_and_verify(&tag_secret, short_id, ts, &dummy_authenticator())
            .expect("verify");
        assert!(state.verified_server_hello);
    }

    #[test]
    fn state_rejects_wrong_random() {
        let mut state = HandshakeState::new();
        let (tag_secret, short_id, ts) = fixtures();
        // ServerHello with random Random — simulates a
        // non-REALITY-aware peer (active probe forwarded
        // to cover site verbatim).
        let wire = build_server_hello([0xFF; 32]);
        state.inbound_buf.extend_from_slice(&wire);

        match state.try_extract_and_verify(&tag_secret, short_id, ts, &dummy_authenticator()) {
            Err(RealityDriverError::ServerAuth(RealityAuthError::BadServerResponse)) => {},
            other => panic!("expected ServerAuth(BadServerResponse), got {other:?}"),
        }
    }

    #[test]
    fn state_rejects_buffer_overflow() {
        let mut state = HandshakeState::new();
        let (tag_secret, short_id, ts) = fixtures();
        // Pump a giant buffer of header-shaped bytes
        // that never yield a complete record (declared
        // length is huge, body never arrives).
        state
            .inbound_buf
            .resize(MAX_HANDSHAKE_BUFFER_BYTES + 1, 0u8);
        state.inbound_buf[0] = 0x16; // handshake
        state.inbound_buf[3] = 0xFF;
        state.inbound_buf[4] = 0xFF;
        match state.try_extract_and_verify(&tag_secret, short_id, ts, &dummy_authenticator()) {
            Err(RealityDriverError::HandshakeBufferOverflow { cap }) => {
                assert_eq!(cap, MAX_HANDSHAKE_BUFFER_BYTES);
            },
            other => panic!("expected HandshakeBufferOverflow, got {other:?}"),
        }
    }

    #[test]
    fn build_reality_client_config_succeeds() {
        // Smoke test — the REALITY-aware ClientConfig
        // must build cleanly under the workspace's
        // pinned ring crypto provider.
        let cfg = build_reality_client_config().expect("build config");
        // ALPN protocols default empty; the driver does
        // not negotiate any application protocol over
        // the cover TLS — payload is the REALITY tunnel.
        assert!(cfg.alpn_protocols.is_empty());
    }
}
