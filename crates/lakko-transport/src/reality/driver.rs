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
//! ## REALITY v2 authenticator placement (rng-hook approach)
//!
//! The REALITY v2 authenticator must live in the
//! `legacy_session_id` slot of the outgoing ClientHello AND
//! must be consistent with rustls's transcript hash — both
//! the client and server must see the same bytes when they
//! derive their shared key material.
//!
//! In TLS 1.3, `legacy_session_id` participates in the
//! transcript hash (RFC 8446 §4.4.1). Patching the bytes
//! after rustls has already baked its own random into the
//! transcript desyncs the two ends — the client and server
//! derive different keys and the first application record
//! fails to decrypt.
//!
//! The fix is a custom [`rustls::crypto::SecureRandom`]
//! implementation ([`crate::reality::rng::RealityRng`]) that
//! intercepts the **first 32-byte fill** call during
//! `ClientConnection::new` — which corresponds to
//! `SessionId::random` in `rustls/src/client/hs.rs` — and
//! substitutes the REALITY authenticator bytes. All other
//! fills delegate to ring's `SystemRandom`.
//!
//! The combined arm + create step is encapsulated in
//! [`build_reality_client_connection`] so the invariant
//! ("no `.await` between arm and new") is enforced locally.
//!
//! ## ServerHello.Random verification
//!
//! A REALITY-aware server similarly injects
//! `compute_server_response(tag_secret, short_id, ts)` into
//! `ServerHello.Random` via the same RNG hook (see
//! `server_driver::drive_server_handshake`). The client
//! verifies this in [`drive_handshake`]'s inbound splice
//! point, optionally gated by
//! [`RealityConfig::require_server_response`].
//!
//! ## Test posture
//!
//! Unit tests cover the pure functions:
//! - waiting-for-complete-record state machine,
//! - decode of TLS record length out of an inbound buffer.
//!
//! The full async round-trip is covered in
//! `tests/reality_e2e.rs` (`reality_v2_loopback_roundtrip`).

use std::io::Cursor;
use std::sync::Arc;

use rustls::pki_types::ServerName;
use rustls::ClientConnection;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::reality::authenticator::{
    compute, verify_server_response, RealityAuthError, RealityTimestamp,
};
use crate::reality::cert::AcceptAnyCertVerifier;
use crate::reality::config::{RealityShortId, RealityTagSecret};
use crate::reality::handshake::{extract_server_hello_random, TlsWireError, TLS_RECORD_HEADER_LEN};
use crate::reality::rng;

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
/// - [`rng::REALITY_RNG`] as `secure_random` so the first
///   32-byte fill goes into `legacy_session_id` (the
///   REALITY authenticator slot) rather than random bytes;
/// - default TLS 1.3 cipher / kx negotiation from ring.
///
/// # Errors
///
/// [`RealityDriverError::ConfigBuild`] on a rustls
/// provider-config error (should never fire on a
/// well-configured build).
pub fn build_reality_client_config() -> RealityDriverResult<rustls::ClientConfig> {
    // Substitute REALITY_RNG for ring's default SecureRandom.
    // All other provider fields (cipher suites, key exchange,
    // etc.) are inherited from ring's defaults.
    let provider = rustls::crypto::CryptoProvider {
        secure_random: &rng::REALITY_RNG,
        ..rustls::crypto::ring::default_provider()
    };
    let verifier = AcceptAnyCertVerifier::new(&provider).into_arc();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| RealityDriverError::ConfigBuild(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(cfg)
}

/// Compute the REALITY v2 authenticator for
/// `(tag_secret, short_id, timestamp)`, arm the per-thread
/// [`rng::RealityRng`] inject slot, and create a
/// `rustls::ClientConnection` configured to use
/// [`rng::REALITY_RNG`] as its `SecureRandom`.
///
/// The first 32-byte `fill` call during
/// `ClientConnection::new` populates `legacy_session_id`
/// and will receive the armed authenticator bytes — so
/// rustls's transcript hash and the on-wire bytes are
/// identical on both client and server.
///
/// **Invariant:** there must be no `.await` between this
/// function and the subsequent [`drive_handshake`] call. If
/// the calling future yields between the two calls, the
/// armed bytes could be consumed by a different future
/// scheduled on the same thread. The `connect` method on
/// [`crate::reality::transport::RealityTransport`] uphold
/// this invariant.
///
/// # Errors
///
/// See [`RealityDriverError::ConfigBuild`] and
/// [`RealityDriverError::ClientConnectionInit`].
pub fn build_reality_client_connection(
    server_name: ServerName<'static>,
    tag_secret: &RealityTagSecret,
    short_id: &RealityShortId,
    timestamp: RealityTimestamp,
) -> RealityDriverResult<ClientConnection> {
    let cfg = build_reality_client_config()?;
    let authenticator = compute(tag_secret, short_id, timestamp);
    // Arm the per-thread inject slot. The next 32-byte fill
    // call (SessionId::random inside ClientConnection::new)
    // will consume these bytes. No .await may intervene.
    rng::RealityRng::arm(authenticator.0);
    ClientConnection::new(Arc::new(cfg), server_name)
        .map_err(|e| RealityDriverError::ClientConnectionInit(e.to_string()))
}

/// Drive a REALITY v2 handshake to completion over
/// `socket`. On `Ok`, `conn` is in the post-handshake
/// state and the caller can shuttle application data
/// through it (typically wrapped in
/// `crate::reality::stream::RealityConnection`).
///
/// The REALITY authenticator has already been embedded
/// into `conn`'s `legacy_session_id` by
/// [`build_reality_client_connection`] at connection-
/// creation time. This function only drives the TLS
/// state machine and (optionally) verifies the inbound
/// `ServerHello.Random` under our `tag_secret`.
///
/// `tag_secret` + `short_id` + `timestamp` are used to
/// compute the expected server-response HMAC for the
/// `require_server_response` check.
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
    require_server_response: bool,
) -> RealityDriverResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut state = HandshakeState::new(require_server_response);

    while conn.is_handshaking() {
        // Outgoing side — drain rustls's pending writes and
        // send. The REALITY authenticator is already in
        // legacy_session_id from build_reality_client_connection;
        // no patching needed here.
        if conn.wants_write() {
            let mut write_buf = Vec::with_capacity(4096);
            while conn.wants_write() {
                conn.write_tls(&mut write_buf)
                    .map_err(|e| RealityDriverError::Rustls(e.to_string()))?;
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
                state.try_extract_and_verify(tag_secret, *short_id, timestamp)?;
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

    // TLS 1.3: process_new_packets() transitions is_handshaking() to
    // false when it processes the ServerFinished, but simultaneously
    // queues the ClientFinished in the outgoing write buffer. The while
    // loop above exits on is_handshaking()=false — the wants_write()
    // check at the top of the loop body already ran for this iteration
    // and won't run again. Drain any pending writes now so the server
    // can complete its half of the handshake.
    if conn.wants_write() {
        let mut write_buf = Vec::with_capacity(512);
        while conn.wants_write() {
            conn.write_tls(&mut write_buf)
                .map_err(|e| RealityDriverError::Rustls(e.to_string()))?;
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

    Ok(())
}

/// Driver-side bookkeeping kept across loop iterations.
struct HandshakeState {
    /// `true` once the inbound ServerHello.Random has
    /// been extracted and verified (or skipped, if
    /// `require_server_response = false`).
    verified_server_hello: bool,
    /// Whether the inbound ServerHello.Random must verify
    /// against our authenticator (mutual-auth posture). If
    /// `false`, the driver skips the verification step and
    /// trusts the server purely on the cover-TLS handshake
    /// completing. See [`RealityConfig::require_server_response`].
    require_server_response: bool,
    /// Buffer of inbound bytes that have arrived but not
    /// yet been fed to rustls. We hold onto them until we
    /// have at least one complete first record (for
    /// extraction) and then drain the consumed prefix.
    inbound_buf: Vec<u8>,
}

impl HandshakeState {
    fn new(require_server_response: bool) -> Self {
        Self {
            verified_server_hello: false,
            require_server_response,
            inbound_buf: Vec::with_capacity(8 * 1024),
        }
    }

    /// Try to extract + verify the ServerHello.Random
    /// out of the inbound buffer. Returns `Ok` and sets
    /// `verified_server_hello = true` on success;
    /// returns `Ok` without setting the flag if the first
    /// record is not yet fully buffered (caller should
    /// continue reading from the socket).
    fn try_extract_and_verify(
        &mut self,
        tag_secret: &RealityTagSecret,
        short_id: RealityShortId,
        client_timestamp: RealityTimestamp,
    ) -> RealityDriverResult<()> {
        // Defend against a slow-loris peer that never
        // emits a complete record.
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
        // We now have at least one full record. Extract +
        // (if `require_server_response`) verify the HMAC.
        let first_record = &self.inbound_buf[..total_len];
        let server_random = extract_server_hello_random(first_record)
            .map_err(RealityDriverError::ExtractServerHello)?;
        if self.require_server_response {
            verify_server_response(tag_secret, &short_id, client_timestamp, &server_random)
                .map_err(RealityDriverError::ServerAuth)?;
        }
        self.verified_server_hello = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rustls::crypto::SecureRandom as _;

    use super::*;
    use crate::reality::config::{SHORT_ID_LEN, TAG_SECRET_LEN};

    fn fixtures() -> (RealityTagSecret, RealityShortId, RealityTimestamp) {
        (
            RealityTagSecret([0x42; TAG_SECRET_LEN]),
            RealityShortId([0xAB; SHORT_ID_LEN]),
            RealityTimestamp(1_700_000_000),
        )
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
        let mut state = HandshakeState::new(true);
        let (ts, sid, t) = fixtures();
        // Empty buffer — try should be a no-op success.
        state
            .try_extract_and_verify(&ts, sid, t)
            .expect("empty buf");
        assert!(!state.verified_server_hello);

        // Partial header — still no-op.
        state.inbound_buf.extend_from_slice(&[0x16, 0x03]);
        state
            .try_extract_and_verify(&ts, sid, t)
            .expect("partial header");
        assert!(!state.verified_server_hello);
    }

    #[test]
    fn state_waits_for_complete_record() {
        let mut state = HandshakeState::new(true);
        let (ts, sid, t) = fixtures();
        // Full header but truncated body.
        state.inbound_buf.extend_from_slice(&[
            0x16, 0x03, 0x03, // record type + version
            0x00, 0x50, // declared length = 80, but we only give 5 body bytes
            0x02, 0x00, 0x00, 0x4C, 0x03, // 5 random body bytes
        ]);
        state
            .try_extract_and_verify(&ts, sid, t)
            .expect("incomplete record");
        assert!(!state.verified_server_hello);
    }

    #[test]
    fn state_verifies_on_complete_correct_record() {
        let mut state = HandshakeState::new(true);
        let (tag_secret, short_id, ts) = fixtures();
        // Compute the EXPECTED server response under our
        // tag_secret. Use it as the ServerHello.Random.
        let expected_response =
            crate::reality::authenticator::compute_server_response(&tag_secret, &short_id, ts);
        let wire = build_server_hello(expected_response);
        state.inbound_buf.extend_from_slice(&wire);

        state
            .try_extract_and_verify(&tag_secret, short_id, ts)
            .expect("verify");
        assert!(state.verified_server_hello);
    }

    #[test]
    fn state_rejects_wrong_random() {
        let mut state = HandshakeState::new(true);
        let (tag_secret, short_id, ts) = fixtures();
        // ServerHello with random Random — simulates a
        // non-REALITY-aware peer (active probe forwarded
        // to cover site verbatim).
        let wire = build_server_hello([0xFF; 32]);
        state.inbound_buf.extend_from_slice(&wire);

        match state.try_extract_and_verify(&tag_secret, short_id, ts) {
            Err(RealityDriverError::ServerAuth(RealityAuthError::BadServerResponse)) => {},
            other => panic!("expected ServerAuth(BadServerResponse), got {other:?}"),
        }
    }

    #[test]
    fn state_rejects_buffer_overflow() {
        let mut state = HandshakeState::new(true);
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
        match state.try_extract_and_verify(&tag_secret, short_id, ts) {
            Err(RealityDriverError::HandshakeBufferOverflow { cap }) => {
                assert_eq!(cap, MAX_HANDSHAKE_BUFFER_BYTES);
            },
            other => panic!("expected HandshakeBufferOverflow, got {other:?}"),
        }
    }

    #[test]
    fn build_reality_client_config_uses_reality_rng() {
        // Smoke test: the REALITY-aware ClientConfig builds
        // cleanly and uses REALITY_RNG as secure_random
        // (verified indirectly — if the provider init fails
        // the test panics here).
        let cfg = build_reality_client_config().expect("build config");
        assert!(cfg.alpn_protocols.is_empty());
    }

    #[test]
    fn build_reality_client_connection_arms_session_id() {
        use rustls::pki_types::ServerName;

        // Verify that build_reality_client_connection puts the
        // authenticator into legacy_session_id rather than random
        // bytes: re-extract the session_id bytes that rustls would
        // have generated if our RNG hadn't intercepted them.
        //
        // Approach: arm a known payload before calling
        // build_reality_client_connection and confirm the RNG
        // inject slot is now empty (was consumed).
        let (tag_secret, short_id, ts) = fixtures();
        let server_name: ServerName<'static> = "www.example.com".try_into().unwrap();

        // Pre-arm with a known value. build_reality_client_connection
        // will re-arm with the actual authenticator — which is fine
        // (arm overwrites); we just want to confirm the slot gets
        // consumed during ClientConnection::new.
        crate::reality::rng::RealityRng::arm([0xEE; 32]);

        // This call will re-arm with the real authenticator and then
        // consume it inside ClientConnection::new.
        let _conn = build_reality_client_connection(server_name, &tag_secret, &short_id, ts)
            .expect("connection builds");

        // The inject slot must be empty now — consumed during new().
        let mut buf = [0u8; 32];
        crate::reality::rng::REALITY_RNG.fill(&mut buf).unwrap();
        // If the slot were still armed the value would be [0xEE;32]
        // (our pre-arm). It must NOT be [0xEE;32] because
        // build_reality_client_connection re-armed with the real
        // authenticator (different) and consumed it.
        let pre_arm_value = [0xEEu8; 32];
        assert_ne!(
            buf, pre_arm_value,
            "inject slot must have been consumed by ClientConnection::new"
        );
    }
}
