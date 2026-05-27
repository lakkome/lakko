// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! TLS 1.3 ClientHello + ServerHello wire-format parsers
//! for REALITY.
//!
//! REALITY injects its 32-byte authenticator into the
//! TLS 1.3 ClientHello.session_id slot, and reads the
//! 32-byte server auth-response out of the TLS 1.3
//! ServerHello.Random slot. Neither rustls 0.23 nor any
//! other Rust TLS library on crates.io exposes hooks for
//! either operation today — the only path is to intercept
//! the raw TLS handshake bytes between the rustls state
//! machine and the underlying socket.
//!
//! This module ships the two byte-slice parsers the
//! interceptor needs. Both are pure functions over `&[u8]`
//! / `&mut [u8]`; the actual handshake driver (TCP +
//! rustls byte intercept) is `S-5-reality-driver`, a
//! follow-up sub-sprint.
//!
//! ## Wire formats
//!
//! TLS record (RFC 8446 §5.1):
//!
//! ```text
//!     1 byte    content_type (22 = handshake)
//!     2 bytes   legacy_record_version
//!     2 bytes   length (big-endian)
//!     N bytes   fragment
//! ```
//!
//! Handshake message (RFC 8446 §4):
//!
//! ```text
//!     1 byte    msg_type (1 = ClientHello, 2 = ServerHello)
//!     3 bytes   length (big-endian uint24)
//!     N bytes   body
//! ```
//!
//! ClientHello body (RFC 8446 §4.1.2):
//!
//! ```text
//!     2 bytes   legacy_version (0x0303)
//!     32 bytes  Random
//!     1 byte    legacy_session_id length (0..=32)
//!     N bytes   legacy_session_id   <-- REALITY authenticator lives here (length = 32)
//!     2 bytes   cipher_suites length
//!     N bytes   cipher_suites
//!     ...
//! ```
//!
//! ServerHello body (RFC 8446 §4.1.3) — identical
//! preamble up to `Random`:
//!
//! ```text
//!     2 bytes   legacy_version (0x0303)
//!     32 bytes  Random              <-- REALITY auth-response lives here
//!     ...
//! ```
//!
//! The two parsers below decode just enough of those
//! structures to locate the REALITY-relevant 32-byte
//! windows. Trailing extensions / cipher-suite lists are
//! NOT parsed — that is rustls's job.

use thiserror::Error;

use crate::reality::authenticator::AUTHENTICATOR_LEN;

/// TLS record content type for handshake messages
/// (RFC 8446 §5.1).
pub const TLS_CONTENT_TYPE_HANDSHAKE: u8 = 22;

/// Handshake message type for ClientHello (RFC 8446 §4).
pub const HANDSHAKE_TYPE_CLIENT_HELLO: u8 = 0x01;

/// Handshake message type for ServerHello (RFC 8446 §4).
pub const HANDSHAKE_TYPE_SERVER_HELLO: u8 = 0x02;

/// Length of the TLS record header.
pub const TLS_RECORD_HEADER_LEN: usize = 5;

/// Length of the handshake message header (1-byte type +
/// 3-byte length).
pub const HANDSHAKE_HEADER_LEN: usize = 4;

/// Length of the legacy_version field in
/// Client/ServerHello body.
pub const LEGACY_VERSION_LEN: usize = 2;

/// Length of the Random field in Client/ServerHello body.
pub const RANDOM_LEN: usize = 32;

/// Byte offset of the session_id length prefix inside a
/// well-formed ClientHello record:
///
/// `5 (record header) + 4 (handshake header) + 2 (legacy_version) + 32 (random) = 43`
///
/// The session_id itself starts at offset 44.
const CLIENT_HELLO_SESSION_ID_LEN_OFFSET: usize =
    TLS_RECORD_HEADER_LEN + HANDSHAKE_HEADER_LEN + LEGACY_VERSION_LEN + RANDOM_LEN;

/// Byte offset of the Random field inside a well-formed
/// ServerHello record:
///
/// `5 (record header) + 4 (handshake header) + 2 (legacy_version) = 11`
const SERVER_HELLO_RANDOM_OFFSET: usize =
    TLS_RECORD_HEADER_LEN + HANDSHAKE_HEADER_LEN + LEGACY_VERSION_LEN;

/// Failure modes the wire parsers surface. Branch on the
/// variant; never on the message string.
#[derive(Debug, Error)]
pub enum TlsWireError {
    /// Buffer shorter than the next header / field
    /// expects.
    #[error("truncated TLS wire: need {needed}, have {have}")]
    Truncated {
        /// Bytes the next read expected.
        needed: usize,
        /// Bytes actually remaining.
        have: usize,
    },
    /// TLS record `content_type` was not `handshake` (22).
    #[error("expected handshake record (type 22), got {got}")]
    NotHandshake {
        /// Observed content-type byte.
        got: u8,
    },
    /// Handshake message type did not match the expected
    /// ClientHello / ServerHello discriminator.
    #[error("expected handshake type {expected:#x}, got {got:#x}")]
    WrongHandshakeType {
        /// Observed `msg_type` byte.
        got: u8,
        /// Expected `msg_type` byte.
        expected: u8,
    },
    /// session_id length in the ClientHello was not the
    /// 32 bytes REALITY requires. Production REALITY
    /// clients always emit a 32-byte session_id slot to
    /// hold the authenticator; a different length means
    /// either rustls is configured wrong on the local
    /// side, or the upstream library changed its
    /// behaviour.
    #[error("ClientHello.session_id length is {got}, REALITY requires {AUTHENTICATOR_LEN}")]
    UnexpectedSessionIdLen {
        /// Observed length byte.
        got: u8,
    },
    /// The TLS record's declared `length` field disagrees
    /// with the buffer size — either short read or
    /// trailing junk.
    #[error("record length mismatch: header says {declared}, buffer has {actual}")]
    RecordLengthMismatch {
        /// Length the record header claimed.
        declared: usize,
        /// Actual bytes in the buffer past the header.
        actual: usize,
    },
}

/// Convenience alias.
pub type TlsWireResult<T> = core::result::Result<T, TlsWireError>;

/// Overwrite the 32-byte `legacy_session_id` field of a
/// TLS 1.3 ClientHello record with the REALITY
/// authenticator bytes.
///
/// The caller hands a buffer that contains exactly one
/// complete TLS record carrying one ClientHello message.
/// On success the buffer is mutated in place; the rest of
/// the record (cipher suites, extensions, ...) is
/// untouched.
///
/// REALITY pins the session_id length to 32 — the function
/// refuses a ClientHello whose declared session_id length
/// is anything else, since rustls's default behaviour
/// emits 32 bytes (a TLS 1.2 "compatibility" random) on
/// every TLS 1.3 ClientHello.
///
/// # Errors
///
/// See [`TlsWireError`].
pub fn patch_client_hello_session_id(
    buf: &mut [u8],
    session_id: &[u8; AUTHENTICATOR_LEN],
) -> TlsWireResult<()> {
    parse_handshake_header(buf, HANDSHAKE_TYPE_CLIENT_HELLO)?;

    // Bounds: we need at least up through
    // CLIENT_HELLO_SESSION_ID_LEN_OFFSET + 1 (the length
    // byte) + AUTHENTICATOR_LEN (the slot we will
    // overwrite).
    let needed = CLIENT_HELLO_SESSION_ID_LEN_OFFSET + 1 + AUTHENTICATOR_LEN;
    if buf.len() < needed {
        return Err(TlsWireError::Truncated {
            needed,
            have: buf.len(),
        });
    }

    let session_id_len = buf[CLIENT_HELLO_SESSION_ID_LEN_OFFSET];
    if session_id_len as usize != AUTHENTICATOR_LEN {
        return Err(TlsWireError::UnexpectedSessionIdLen {
            got: session_id_len,
        });
    }

    let dst_start = CLIENT_HELLO_SESSION_ID_LEN_OFFSET + 1;
    let dst_end = dst_start + AUTHENTICATOR_LEN;
    buf[dst_start..dst_end].copy_from_slice(session_id);
    Ok(())
}

/// Extract the 32-byte `Random` field from a TLS 1.3
/// ServerHello record. REALITY's server side writes its
/// auth-response there.
///
/// The caller hands a buffer that contains exactly one
/// complete TLS record carrying one ServerHello message.
/// On success returns a copy of the 32-byte Random.
///
/// # Errors
///
/// See [`TlsWireError`].
pub fn extract_server_hello_random(buf: &[u8]) -> TlsWireResult<[u8; RANDOM_LEN]> {
    parse_handshake_header(buf, HANDSHAKE_TYPE_SERVER_HELLO)?;

    let end = SERVER_HELLO_RANDOM_OFFSET + RANDOM_LEN;
    if buf.len() < end {
        return Err(TlsWireError::Truncated {
            needed: end,
            have: buf.len(),
        });
    }

    let mut out = [0u8; RANDOM_LEN];
    out.copy_from_slice(&buf[SERVER_HELLO_RANDOM_OFFSET..end]);
    Ok(out)
}

/// Extract the 32-byte `legacy_session_id` field from a TLS
/// 1.3 ClientHello record. REALITY's server side reads its
/// authenticator (short_id || timestamp || hmac_tag[..16])
/// out of this slot to decide whether to accept the peer
/// as a REALITY client or fall through to the cover-site
/// proxy path.
///
/// The caller hands a buffer that contains exactly one
/// complete TLS record carrying one ClientHello message.
/// On success returns a copy of the 32-byte session_id.
///
/// REALITY pins the session_id length to 32 — the function
/// refuses a ClientHello whose declared session_id length
/// is anything else. A 0-length or shorter session_id from
/// a real (non-REALITY) browser is therefore reported as
/// `UnexpectedSessionIdLen`, which the server uses as the
/// "this is not a REALITY peer" signal.
///
/// # Errors
///
/// See [`TlsWireError`].
pub fn extract_client_hello_session_id(buf: &[u8]) -> TlsWireResult<[u8; AUTHENTICATOR_LEN]> {
    parse_handshake_header(buf, HANDSHAKE_TYPE_CLIENT_HELLO)?;

    // Bounds: we need at least the length byte +
    // AUTHENTICATOR_LEN bytes after it.
    let needed = CLIENT_HELLO_SESSION_ID_LEN_OFFSET + 1 + AUTHENTICATOR_LEN;
    if buf.len() < needed {
        return Err(TlsWireError::Truncated {
            needed,
            have: buf.len(),
        });
    }

    let session_id_len = buf[CLIENT_HELLO_SESSION_ID_LEN_OFFSET];
    if session_id_len as usize != AUTHENTICATOR_LEN {
        return Err(TlsWireError::UnexpectedSessionIdLen {
            got: session_id_len,
        });
    }

    let src_start = CLIENT_HELLO_SESSION_ID_LEN_OFFSET + 1;
    let src_end = src_start + AUTHENTICATOR_LEN;
    let mut out = [0u8; AUTHENTICATOR_LEN];
    out.copy_from_slice(&buf[src_start..src_end]);
    Ok(out)
}

/// Overwrite the 32-byte `Random` field of a TLS 1.3
/// ServerHello record with the REALITY v2 server-response
/// bytes.
///
/// The caller hands a buffer that contains exactly one
/// complete TLS record carrying one ServerHello message.
/// On success the buffer is mutated in place; the rest of
/// the record (cipher_suite, extensions, ...) is untouched.
///
/// This is the server-side mirror of
/// [`extract_server_hello_random`]: the server computes
/// `compute_server_response(tag_secret, short_id, ts)` and
/// patches the result into the outgoing ServerHello so the
/// REALITY-aware client can verify mutual-auth via
/// `verify_server_response` before any application data
/// flows.
///
/// # Errors
///
/// See [`TlsWireError`].
pub fn patch_server_hello_random(buf: &mut [u8], random: &[u8; RANDOM_LEN]) -> TlsWireResult<()> {
    parse_handshake_header(buf, HANDSHAKE_TYPE_SERVER_HELLO)?;

    let end = SERVER_HELLO_RANDOM_OFFSET + RANDOM_LEN;
    if buf.len() < end {
        return Err(TlsWireError::Truncated {
            needed: end,
            have: buf.len(),
        });
    }

    buf[SERVER_HELLO_RANDOM_OFFSET..end].copy_from_slice(random);
    Ok(())
}

/// Shared prefix check — verifies the TLS record header
/// plus the handshake header look like the expected
/// message type. Returns `Ok(())` if everything up through
/// the handshake header is well-formed.
fn parse_handshake_header(buf: &[u8], expected_msg_type: u8) -> TlsWireResult<()> {
    if buf.len() < TLS_RECORD_HEADER_LEN + HANDSHAKE_HEADER_LEN {
        return Err(TlsWireError::Truncated {
            needed: TLS_RECORD_HEADER_LEN + HANDSHAKE_HEADER_LEN,
            have: buf.len(),
        });
    }

    // Record header.
    if buf[0] != TLS_CONTENT_TYPE_HANDSHAKE {
        return Err(TlsWireError::NotHandshake { got: buf[0] });
    }
    // Skip legacy_record_version (buf[1..3]) — TLS 1.3
    // clients legitimately emit either 0x0301 or 0x0303;
    // we accept either.
    let record_length = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    let body_actual = buf.len() - TLS_RECORD_HEADER_LEN;
    if record_length != body_actual {
        return Err(TlsWireError::RecordLengthMismatch {
            declared: record_length,
            actual: body_actual,
        });
    }

    // Handshake header.
    let msg_type = buf[TLS_RECORD_HEADER_LEN];
    if msg_type != expected_msg_type {
        return Err(TlsWireError::WrongHandshakeType {
            got: msg_type,
            expected: expected_msg_type,
        });
    }
    // We deliberately do not validate the inner uint24
    // handshake length — its consistency is rustls's
    // responsibility, and we only touch fixed-offset
    // fields anyway.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal-shape TLS 1.3 ClientHello record
    /// with all REALITY-relevant fields in their canonical
    /// positions. The body past `session_id` is filled with
    /// placeholder bytes — the parsers under test do not
    /// look at them.
    fn build_client_hello(session_id: [u8; 32]) -> Vec<u8> {
        let mut out = Vec::new();
        // Body — everything inside the record body.
        let mut body = Vec::new();

        // Handshake header.
        body.push(HANDSHAKE_TYPE_CLIENT_HELLO);
        // 3-byte length placeholder; we patch with the
        // correct value below.
        body.extend_from_slice(&[0, 0, 0]);

        // ClientHello body.
        let hs_body_start = body.len();
        body.extend_from_slice(&[0x03, 0x03]); // legacy_version
        body.extend_from_slice(&[0xAA; RANDOM_LEN]); // Random
        body.push(u8::try_from(AUTHENTICATOR_LEN).expect("AUTHENTICATOR_LEN ≤ 255")); // session_id length
        body.extend_from_slice(&session_id); // session_id
                                             // cipher_suites: 2-byte length + 2 bytes one suite.
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]);
        // compression_methods: 1-byte length + 1 byte.
        body.extend_from_slice(&[0x01, 0x00]);
        // extensions length placeholder + content.
        body.extend_from_slice(&[0x00, 0x00]);

        // Patch handshake length (3-byte BE).
        let hs_body_len = body.len() - hs_body_start;
        let hs_len_bytes = u32::try_from(hs_body_len)
            .expect("test fixture < 4 GiB")
            .to_be_bytes(); // 4 bytes
        body[1] = hs_len_bytes[1];
        body[2] = hs_len_bytes[2];
        body[3] = hs_len_bytes[3];

        // Wrap in record header.
        out.push(TLS_CONTENT_TYPE_HANDSHAKE);
        out.extend_from_slice(&[0x03, 0x01]); // legacy_record_version
        out.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Build a minimal-shape TLS 1.3 ServerHello record
    /// with the Random field set to the supplied value.
    fn build_server_hello(random: [u8; 32]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut body = Vec::new();
        body.push(HANDSHAKE_TYPE_SERVER_HELLO);
        body.extend_from_slice(&[0, 0, 0]); // hs length placeholder
        let hs_body_start = body.len();
        body.extend_from_slice(&[0x03, 0x03]); // legacy_version
        body.extend_from_slice(&random); // Random
        body.push(0x00); // session_id_echo length = 0
        body.extend_from_slice(&[0x13, 0x01]); // cipher_suite
        body.push(0x00); // legacy_compression_method
        body.extend_from_slice(&[0x00, 0x00]); // extensions length

        let hs_body_len = body.len() - hs_body_start;
        let hs_len_bytes = u32::try_from(hs_body_len)
            .expect("test fixture < 4 GiB")
            .to_be_bytes();
        body[1] = hs_len_bytes[1];
        body[2] = hs_len_bytes[2];
        body[3] = hs_len_bytes[3];

        out.push(TLS_CONTENT_TYPE_HANDSHAKE);
        out.extend_from_slice(&[0x03, 0x03]);
        out.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn patch_session_id_round_trip() {
        let original = [0xAA; AUTHENTICATOR_LEN];
        let replacement = [0xBB; AUTHENTICATOR_LEN];
        let mut wire = build_client_hello(original);
        patch_client_hello_session_id(&mut wire, &replacement).expect("patch");

        let dst_start = CLIENT_HELLO_SESSION_ID_LEN_OFFSET + 1;
        let dst_end = dst_start + AUTHENTICATOR_LEN;
        assert_eq!(&wire[dst_start..dst_end], &replacement);
    }

    #[test]
    fn patch_preserves_everything_else() {
        // The rest of the record (cipher_suites,
        // compression, extensions) must come out
        // bit-identical after the patch.
        let mut wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        let before = wire.clone();
        patch_client_hello_session_id(&mut wire, &[0xBB; AUTHENTICATOR_LEN]).unwrap();
        // Bytes before the session_id slot (length byte
        // included via the inclusive range).
        assert_eq!(
            &wire[..=CLIENT_HELLO_SESSION_ID_LEN_OFFSET],
            &before[..=CLIENT_HELLO_SESSION_ID_LEN_OFFSET]
        );
        // Bytes after the session_id slot.
        let after_start = CLIENT_HELLO_SESSION_ID_LEN_OFFSET + 1 + AUTHENTICATOR_LEN;
        assert_eq!(&wire[after_start..], &before[after_start..]);
    }

    #[test]
    fn patch_rejects_non_handshake_record() {
        let mut wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        wire[0] = 23; // application_data
        match patch_client_hello_session_id(&mut wire, &[0xBB; AUTHENTICATOR_LEN]) {
            Err(TlsWireError::NotHandshake { got }) => assert_eq!(got, 23),
            other => panic!("expected NotHandshake, got {other:?}"),
        }
    }

    #[test]
    fn patch_rejects_wrong_handshake_type() {
        // Substitute the handshake type with ServerHello (2).
        let mut wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        wire[TLS_RECORD_HEADER_LEN] = HANDSHAKE_TYPE_SERVER_HELLO;
        match patch_client_hello_session_id(&mut wire, &[0xBB; AUTHENTICATOR_LEN]) {
            Err(TlsWireError::WrongHandshakeType { got, expected }) => {
                assert_eq!(got, HANDSHAKE_TYPE_SERVER_HELLO);
                assert_eq!(expected, HANDSHAKE_TYPE_CLIENT_HELLO);
            },
            other => panic!("expected WrongHandshakeType, got {other:?}"),
        }
    }

    #[test]
    fn patch_rejects_short_session_id_length() {
        let mut wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        wire[CLIENT_HELLO_SESSION_ID_LEN_OFFSET] = 16; // not 32
        match patch_client_hello_session_id(&mut wire, &[0xBB; AUTHENTICATOR_LEN]) {
            Err(TlsWireError::UnexpectedSessionIdLen { got }) => assert_eq!(got, 16),
            other => panic!("expected UnexpectedSessionIdLen, got {other:?}"),
        }
    }

    #[test]
    fn patch_rejects_truncated_buffer() {
        let wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        let mut truncated = wire[..30].to_vec();
        match patch_client_hello_session_id(&mut truncated, &[0xBB; AUTHENTICATOR_LEN]) {
            Err(TlsWireError::Truncated { .. } | TlsWireError::RecordLengthMismatch { .. }) => {},
            other => panic!("expected truncation rejection, got {other:?}"),
        }
    }

    #[test]
    fn patch_rejects_record_length_mismatch() {
        let mut wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        // Bump the record length by 100 — buffer no longer
        // matches.
        let mismatched_len = u16::from_be_bytes([wire[3], wire[4]]) + 100;
        wire[3..5].copy_from_slice(&mismatched_len.to_be_bytes());
        match patch_client_hello_session_id(&mut wire, &[0xBB; AUTHENTICATOR_LEN]) {
            Err(TlsWireError::RecordLengthMismatch { .. }) => {},
            other => panic!("expected RecordLengthMismatch, got {other:?}"),
        }
    }

    #[test]
    fn extract_random_round_trip() {
        let random = {
            let mut r = [0u8; RANDOM_LEN];
            for (i, b) in r.iter_mut().enumerate() {
                *b = u8::try_from(i).unwrap();
            }
            r
        };
        let wire = build_server_hello(random);
        let extracted = extract_server_hello_random(&wire).expect("extract");
        assert_eq!(extracted, random);
    }

    #[test]
    fn extract_rejects_non_handshake_record() {
        let mut wire = build_server_hello([0u8; RANDOM_LEN]);
        wire[0] = 20; // change_cipher_spec
        match extract_server_hello_random(&wire) {
            Err(TlsWireError::NotHandshake { got }) => assert_eq!(got, 20),
            other => panic!("expected NotHandshake, got {other:?}"),
        }
    }

    #[test]
    fn extract_rejects_wrong_handshake_type() {
        let mut wire = build_server_hello([0u8; RANDOM_LEN]);
        wire[TLS_RECORD_HEADER_LEN] = HANDSHAKE_TYPE_CLIENT_HELLO;
        match extract_server_hello_random(&wire) {
            Err(TlsWireError::WrongHandshakeType { got, expected }) => {
                assert_eq!(got, HANDSHAKE_TYPE_CLIENT_HELLO);
                assert_eq!(expected, HANDSHAKE_TYPE_SERVER_HELLO);
            },
            other => panic!("expected WrongHandshakeType, got {other:?}"),
        }
    }

    #[test]
    fn extract_rejects_truncated_buffer() {
        let wire = build_server_hello([0u8; RANDOM_LEN]);
        let truncated = &wire[..20];
        match extract_server_hello_random(truncated) {
            Err(TlsWireError::Truncated { .. } | TlsWireError::RecordLengthMismatch { .. }) => {},
            other => panic!("expected truncation rejection, got {other:?}"),
        }
    }

    #[test]
    fn extract_rejects_record_length_mismatch() {
        let mut wire = build_server_hello([0u8; RANDOM_LEN]);
        let bad_len = u16::from_be_bytes([wire[3], wire[4]]) + 50;
        wire[3..5].copy_from_slice(&bad_len.to_be_bytes());
        match extract_server_hello_random(&wire) {
            Err(TlsWireError::RecordLengthMismatch { .. }) => {},
            other => panic!("expected RecordLengthMismatch, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_through_compute_then_extract() {
        // Sanity: an integration shape — compute REALITY
        // authenticator bytes (v2 pre-shared-key), patch
        // them into a ClientHello, parse them back out.
        // This pins the wire-layout contract between
        // authenticator.rs and this module.
        use crate::reality::authenticator::{compute, RealityTimestamp};
        use crate::reality::config::{RealityShortId, RealityTagSecret};

        let tag_secret = RealityTagSecret([0x42; 32]);
        let short_id = RealityShortId([0xAB; 8]);
        let ts = RealityTimestamp(1_700_000_000);

        let auth = compute(&tag_secret, &short_id, ts);

        let mut wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        patch_client_hello_session_id(&mut wire, &auth.0).unwrap();

        let dst_start = CLIENT_HELLO_SESSION_ID_LEN_OFFSET + 1;
        let dst_end = dst_start + AUTHENTICATOR_LEN;
        assert_eq!(&wire[dst_start..dst_end], &auth.0);
    }

    // -------- extract_client_hello_session_id --------

    #[test]
    fn extract_session_id_round_trip() {
        let mut original = [0u8; AUTHENTICATOR_LEN];
        for (i, b) in original.iter_mut().enumerate() {
            *b = u8::try_from(i).unwrap();
        }
        let wire = build_client_hello(original);
        let extracted = extract_client_hello_session_id(&wire).expect("extract");
        assert_eq!(extracted, original);
    }

    #[test]
    fn extract_session_id_rejects_non_handshake_record() {
        let mut wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        wire[0] = 23; // application_data
        match extract_client_hello_session_id(&wire) {
            Err(TlsWireError::NotHandshake { got }) => assert_eq!(got, 23),
            other => panic!("expected NotHandshake, got {other:?}"),
        }
    }

    #[test]
    fn extract_session_id_rejects_wrong_handshake_type() {
        let mut wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        wire[TLS_RECORD_HEADER_LEN] = HANDSHAKE_TYPE_SERVER_HELLO;
        match extract_client_hello_session_id(&wire) {
            Err(TlsWireError::WrongHandshakeType { got, expected }) => {
                assert_eq!(got, HANDSHAKE_TYPE_SERVER_HELLO);
                assert_eq!(expected, HANDSHAKE_TYPE_CLIENT_HELLO);
            },
            other => panic!("expected WrongHandshakeType, got {other:?}"),
        }
    }

    #[test]
    fn extract_session_id_rejects_short_session_id_length() {
        // A genuine non-REALITY browser ClientHello with
        // session_id_len = 0 or 16 should be reported via
        // UnexpectedSessionIdLen — the server uses this
        // discriminator to fall through to the cover-site
        // proxy path.
        let mut wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        wire[CLIENT_HELLO_SESSION_ID_LEN_OFFSET] = 16;
        match extract_client_hello_session_id(&wire) {
            Err(TlsWireError::UnexpectedSessionIdLen { got }) => assert_eq!(got, 16),
            other => panic!("expected UnexpectedSessionIdLen, got {other:?}"),
        }
    }

    #[test]
    fn extract_session_id_rejects_truncated_buffer() {
        let wire = build_client_hello([0xAA; AUTHENTICATOR_LEN]);
        let truncated = &wire[..30];
        match extract_client_hello_session_id(truncated) {
            Err(TlsWireError::Truncated { .. } | TlsWireError::RecordLengthMismatch { .. }) => {},
            other => panic!("expected truncation rejection, got {other:?}"),
        }
    }

    // -------- patch_server_hello_random --------

    #[test]
    fn patch_random_round_trip() {
        let replacement = {
            let mut r = [0u8; RANDOM_LEN];
            for (i, b) in r.iter_mut().enumerate() {
                *b = u8::try_from(i ^ 0x5A).unwrap();
            }
            r
        };
        let mut wire = build_server_hello([0u8; RANDOM_LEN]);
        patch_server_hello_random(&mut wire, &replacement).expect("patch");

        let end = SERVER_HELLO_RANDOM_OFFSET + RANDOM_LEN;
        assert_eq!(&wire[SERVER_HELLO_RANDOM_OFFSET..end], &replacement);
    }

    #[test]
    fn patch_random_preserves_everything_else() {
        // Same posture as patch_preserves_everything_else
        // for the ClientHello — everything outside the
        // Random window must be bit-identical after the
        // patch.
        let mut wire = build_server_hello([0u8; RANDOM_LEN]);
        let before = wire.clone();
        patch_server_hello_random(&mut wire, &[0x42; RANDOM_LEN]).unwrap();
        // Bytes up through the start of Random.
        assert_eq!(
            &wire[..SERVER_HELLO_RANDOM_OFFSET],
            &before[..SERVER_HELLO_RANDOM_OFFSET]
        );
        // Bytes after the Random slot.
        let after_start = SERVER_HELLO_RANDOM_OFFSET + RANDOM_LEN;
        assert_eq!(&wire[after_start..], &before[after_start..]);
    }

    #[test]
    fn patch_random_rejects_non_handshake_record() {
        let mut wire = build_server_hello([0u8; RANDOM_LEN]);
        wire[0] = 20; // change_cipher_spec
        match patch_server_hello_random(&mut wire, &[0u8; RANDOM_LEN]) {
            Err(TlsWireError::NotHandshake { got }) => assert_eq!(got, 20),
            other => panic!("expected NotHandshake, got {other:?}"),
        }
    }

    #[test]
    fn patch_random_rejects_wrong_handshake_type() {
        let mut wire = build_server_hello([0u8; RANDOM_LEN]);
        wire[TLS_RECORD_HEADER_LEN] = HANDSHAKE_TYPE_CLIENT_HELLO;
        match patch_server_hello_random(&mut wire, &[0u8; RANDOM_LEN]) {
            Err(TlsWireError::WrongHandshakeType { got, expected }) => {
                assert_eq!(got, HANDSHAKE_TYPE_CLIENT_HELLO);
                assert_eq!(expected, HANDSHAKE_TYPE_SERVER_HELLO);
            },
            other => panic!("expected WrongHandshakeType, got {other:?}"),
        }
    }

    #[test]
    fn patch_random_rejects_truncated_buffer() {
        let wire = build_server_hello([0u8; RANDOM_LEN]);
        let mut truncated = wire[..20].to_vec();
        match patch_server_hello_random(&mut truncated, &[0u8; RANDOM_LEN]) {
            Err(TlsWireError::Truncated { .. } | TlsWireError::RecordLengthMismatch { .. }) => {},
            other => panic!("expected truncation rejection, got {other:?}"),
        }
    }

    /// Full server-side wire-format integration round-trip:
    /// `compute_server_response` → `patch_server_hello_random`
    /// → `extract_server_hello_random` → `verify_server_response`.
    /// Pins the server-side wire contract end-to-end.
    #[test]
    fn server_side_round_trip_through_compute_patch_extract_verify() {
        use crate::reality::authenticator::{
            compute_server_response, verify_server_response, RealityTimestamp,
        };
        use crate::reality::config::{RealityShortId, RealityTagSecret};

        let tag_secret = RealityTagSecret([0x42; 32]);
        let short_id = RealityShortId([0xAB; 8]);
        let ts = RealityTimestamp(1_700_000_000);

        let server_response = compute_server_response(&tag_secret, &short_id, ts);

        let mut wire = build_server_hello([0u8; RANDOM_LEN]);
        patch_server_hello_random(&mut wire, &server_response).unwrap();

        let extracted = extract_server_hello_random(&wire).unwrap();
        verify_server_response(&tag_secret, &short_id, ts, &extracted)
            .expect("server-response round-trip must verify");
    }

    /// Symmetric round-trip for the client-side authenticator
    /// path on the server: extract a REALITY authenticator
    /// from a ClientHello and verify it against the allowed
    /// (short_id, tag_secret) pair list.
    #[test]
    fn server_side_extract_then_verify_authenticator() {
        use crate::reality::authenticator::{
            compute, verify, RealityAuthenticator, RealityTimestamp,
        };
        use crate::reality::config::{RealityShortId, RealityTagSecret};

        let tag_secret = RealityTagSecret([0x99; 32]);
        let short_id = RealityShortId([0x77; 8]);
        let ts = RealityTimestamp(1_700_000_000);

        let auth = compute(&tag_secret, &short_id, ts);

        let mut wire = build_client_hello([0u8; AUTHENTICATOR_LEN]);
        patch_client_hello_session_id(&mut wire, &auth.0).unwrap();

        let extracted = extract_client_hello_session_id(&wire).unwrap();
        // Wrap back into the typed authenticator + run the
        // server-side verifier with a single-entry allow-set.
        let allowed = [(short_id, tag_secret)];
        verify(
            &allowed,
            &RealityAuthenticator(extracted),
            ts,
            // generous skew tolerance for the deterministic
            // test clock
            300,
        )
        .expect("server-side authenticator verify must succeed");
    }
}
