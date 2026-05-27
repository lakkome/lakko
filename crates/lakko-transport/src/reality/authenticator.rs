// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! REALITY v2 authenticator — pre-shared-key Path C
//! per the architectural-blocker analysis in
//! `docs/security/transport.md` T-13.
//!
//! ## Why pre-shared-key (no ECDH)
//!
//! Upstream xtls/REALITY ties its authenticator to the
//! TLS 1.3 `key_share` ephemeral X25519 via ECDH. That
//! requires the TLS library to either (a) accept a
//! caller-supplied ephemeral, or (b) expose its
//! generated ephemeral after the fact. rustls 0.23 does
//! neither. Lakko's REALITY v2 therefore drops the ECDH
//! and authenticates via a pre-shared key the publisher
//! distributes per-client through the signed bootstrap
//! rendezvous channel.
//!
//! The cover-side TLS 1.3 handshake still gets fresh
//! forward secrecy from rustls's own ephemeral
//! (independent of REALITY auth), so a censor that
//! captures the TLS bytes cannot decrypt the cover
//! traffic later. What is lost is forward secrecy
//! *of the REALITY authenticator itself*: if an attacker
//! exfiltrates a client's `tag_secret` from the device,
//! they can forge authenticators for that client until
//! the publisher rotates that `tag_secret` through a
//! fresh bootstrap config.
//!
//! ## Wire layout
//!
//! ```text
//!     0           8           16                          32
//!     |  short_id  |  ts_be8    |       hmac_tag[..16]      |
//! ```
//!
//! - `short_id` (8 bytes): publisher-assigned per-client
//!   tag. Plaintext so the receiver can look up the
//!   matching `tag_secret` in O(1) before doing any
//!   HMAC work.
//! - `ts_be8` (8 bytes, u64 big-endian unix seconds): the
//!   client's wall-clock at handshake time. The receiver
//!   refuses outside `tolerance_secs` of its own clock.
//! - `hmac_tag[..16]`: first 16 bytes of
//!   `HMAC-SHA-256(mac_key, short_id || ts_be8)` where
//!   `mac_key = HKDF-SHA-256(tag_secret, "", "lakko/reality/v2")`.
//!
//! ## Mutual authentication
//!
//! The server proves *it* knows the same `tag_secret` by
//! writing a 32-byte server-response into the TLS 1.3
//! ServerHello.Random slot. The client extracts it with
//! [`crate::reality::handshake::extract_server_hello_random`]
//! and verifies via [`verify_server_response`].
//!
//! Server response is `HMAC(mac_key,
//! "v2-server-response" || short_id || ts_be8)[..32]`.
//! Both sides have `mac_key` (HKDF'd from `tag_secret`);
//! both can compute. An attacker without `tag_secret`
//! cannot forge the server response, so a man-in-the-
//! middle that simply proxies the ClientHello to a real
//! TLS server (active-probing case) yields a
//! ServerHello.Random that is plain randomness, NOT our
//! HMAC — the client refuses with `BadServerResponse`.

use lakko_crypto::kdf::hkdf_sha256;
use subtle::ConstantTimeEq as _;
use thiserror::Error;

use crate::reality::config::{RealityShortId, RealityTagSecret, SHORT_ID_LEN};

/// Total length of the REALITY authenticator — exactly
/// the size of TLS 1.3 ClientHello.session_id.
pub const AUTHENTICATOR_LEN: usize = 32;

/// Length of the embedded big-endian unix-seconds
/// timestamp.
pub const TIMESTAMP_LEN: usize = 8;

/// Length of the HMAC tag truncation. 16 bytes is the
/// HMAC-SHA-256 truncation per RFC 4868 §2.6, sufficient
/// for the threat model — an attacker without `tag_secret`
/// has at most 2^-128 odds of forging a valid tag.
pub const HMAC_TAG_LEN: usize = 16;

/// Length of the server-response written into
/// TLS 1.3 ServerHello.Random by a REALITY-aware server.
pub const SERVER_RESPONSE_LEN: usize = 32;

/// HKDF label that pins this implementation's authenticator
/// to the Lakko-specific byte layout. Changing this label
/// is a breaking protocol change.
pub const HKDF_LABEL: &[u8] = b"lakko/reality/v2";

/// Domain-separation tag the server prepends when emitting
/// the mutual-auth proof in ServerHello.Random. Pinning
/// this prevents a client-side forgery (an attacker who
/// somehow got the client to sign anything cannot use that
/// signature in the server-response position).
pub const SERVER_RESPONSE_TAG: &[u8] = b"v2-server-response";

/// Length of the HKDF-derived HMAC key in bytes.
const HMAC_KEY_LEN: usize = 32;

const _CONST_CHECK: () = assert!(
    SHORT_ID_LEN + TIMESTAMP_LEN + HMAC_TAG_LEN == AUTHENTICATOR_LEN,
    "REALITY wire layout must total exactly 32 bytes"
);

/// Unix-seconds wall-clock value the authenticator binds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealityTimestamp(pub u64);

/// The 32-byte payload bound for TLS ClientHello.session_id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealityAuthenticator(pub [u8; AUTHENTICATOR_LEN]);

/// Failure modes the verifier surfaces. Branch on the
/// variant; never on the message string.
#[derive(Debug, Error)]
pub enum RealityAuthError {
    /// Embedded short_id did not match any short_id the
    /// receiver authorises. The variant carries the one
    /// observed (telemetry-only).
    #[error("short_id {got_hex} not in allowed set")]
    ShortIdNotAllowed {
        /// Hex of the observed short_id.
        got_hex: String,
    },
    /// Embedded timestamp is outside the receiver's clock
    /// window. Defends against capture-and-replay.
    #[error("timestamp drift {drift_secs}s exceeds tolerance {tolerance_secs}s")]
    TimestampSkew {
        /// Drift between client claim and receiver clock.
        drift_secs: u64,
        /// Receiver's configured tolerance.
        tolerance_secs: u64,
    },
    /// HMAC tag did not verify under the derived key.
    /// Authentic failure mode — either tampered wire,
    /// wrong `tag_secret`, or attacker without the secret.
    #[error("HMAC tag did not verify")]
    TagMismatch,
    /// Server-response in ServerHello.Random did not
    /// match the expected HMAC. Server-side mutual auth
    /// failure — either the peer is a plain TLS server
    /// (active-probe scenario; ServerHello.Random is
    /// random, not our HMAC), or the peer is a
    /// REALITY-aware server with a different
    /// `tag_secret`.
    #[error("server-response in ServerHello.Random did not verify")]
    BadServerResponse,
}

/// Convenience alias.
pub type RealityAuthResult<T> = core::result::Result<T, RealityAuthError>;

/// Derive the HMAC key from a pre-shared `tag_secret`.
/// Centralised so client + server stay in lock-step.
fn mac_key_for(tag_secret: &RealityTagSecret) -> [u8; HMAC_KEY_LEN] {
    let derived = hkdf_sha256::<HMAC_KEY_LEN>(&tag_secret.0, b"", HKDF_LABEL)
        .expect("HKDF-SHA-256 32->32 byte expansion never fails");
    *derived.as_bytes()
}

/// Build a REALITY authenticator for the TLS 1.3
/// ClientHello.session_id slot.
///
/// `tag_secret` is the pre-shared key the publisher
/// handed this client through the signed bootstrap
/// rendezvous. `short_id` is the publisher-assigned tag
/// (8 bytes). `timestamp` is current unix-seconds wall
/// clock.
#[must_use]
pub fn compute(
    tag_secret: &RealityTagSecret,
    short_id: &RealityShortId,
    timestamp: RealityTimestamp,
) -> RealityAuthenticator {
    let mac_key = mac_key_for(tag_secret);

    let mut payload = [0u8; SHORT_ID_LEN + TIMESTAMP_LEN];
    payload[..SHORT_ID_LEN].copy_from_slice(&short_id.0);
    payload[SHORT_ID_LEN..].copy_from_slice(&timestamp.0.to_be_bytes());

    let tag = hmac_sha256_truncated(&mac_key, &payload);

    let mut out = [0u8; AUTHENTICATOR_LEN];
    out[..SHORT_ID_LEN].copy_from_slice(&short_id.0);
    out[SHORT_ID_LEN..SHORT_ID_LEN + TIMESTAMP_LEN].copy_from_slice(&timestamp.0.to_be_bytes());
    out[SHORT_ID_LEN + TIMESTAMP_LEN..].copy_from_slice(&tag);
    RealityAuthenticator(out)
}

/// Verify an inbound REALITY authenticator against the
/// receiver's allow-list of (short_id, tag_secret) pairs.
///
/// `allowed_pairs` is the set the receiver authorises.
/// The receiver typically holds one pair per publisher
/// (the master_key → tag_secret derivation is done off-
/// the-hot-path by the publisher tooling). `now` is the
/// receiver's wall clock; `tolerance_secs` bounds the
/// replay window.
///
/// # Errors
///
/// See [`RealityAuthError`].
pub fn verify(
    allowed_pairs: &[(RealityShortId, RealityTagSecret)],
    auth: &RealityAuthenticator,
    now: RealityTimestamp,
    tolerance_secs: u64,
) -> RealityAuthResult<(RealityShortId, RealityTimestamp)> {
    // 1. Extract short_id; lookup against allow-set.
    let mut short_id_bytes = [0u8; SHORT_ID_LEN];
    short_id_bytes.copy_from_slice(&auth.0[..SHORT_ID_LEN]);
    let short_id = RealityShortId(short_id_bytes);

    let tag_secret = allowed_pairs
        .iter()
        .find(|(s, _)| s == &short_id)
        .map(|(_, ts)| ts)
        .ok_or_else(|| RealityAuthError::ShortIdNotAllowed {
            got_hex: hex::encode(short_id_bytes),
        })?;

    // 2. Extract timestamp; window-check.
    let mut ts_bytes = [0u8; TIMESTAMP_LEN];
    ts_bytes.copy_from_slice(&auth.0[SHORT_ID_LEN..SHORT_ID_LEN + TIMESTAMP_LEN]);
    let claimed = u64::from_be_bytes(ts_bytes);
    let drift = claimed.abs_diff(now.0);
    if drift > tolerance_secs {
        return Err(RealityAuthError::TimestampSkew {
            drift_secs: drift,
            tolerance_secs,
        });
    }

    // 3. Recompute HMAC under tag_secret + constant-time
    //    compare.
    let mac_key = mac_key_for(tag_secret);
    let mut payload = [0u8; SHORT_ID_LEN + TIMESTAMP_LEN];
    payload[..SHORT_ID_LEN].copy_from_slice(&short_id_bytes);
    payload[SHORT_ID_LEN..].copy_from_slice(&ts_bytes);
    let expected_tag = hmac_sha256_truncated(&mac_key, &payload);
    let observed_tag = &auth.0[SHORT_ID_LEN + TIMESTAMP_LEN..];

    if expected_tag.ct_eq(observed_tag).unwrap_u8() == 1 {
        Ok((short_id, RealityTimestamp(claimed)))
    } else {
        Err(RealityAuthError::TagMismatch)
    }
}

/// Compute the 32-byte mutual-auth proof a REALITY-aware
/// server writes into TLS 1.3 ServerHello.Random.
///
/// The proof binds `(short_id, client_timestamp)` under
/// the same `mac_key` the client used. A non-REALITY
/// server (or REALITY server with a different
/// `tag_secret`) cannot produce this value — its
/// ServerHello.Random will be uncorrelated randomness, so
/// the client's [`verify_server_response`] check refuses
/// the connection with [`RealityAuthError::BadServerResponse`].
#[must_use]
pub fn compute_server_response(
    tag_secret: &RealityTagSecret,
    short_id: &RealityShortId,
    client_timestamp: RealityTimestamp,
) -> [u8; SERVER_RESPONSE_LEN] {
    let mac_key = mac_key_for(tag_secret);

    let mut input = Vec::with_capacity(SERVER_RESPONSE_TAG.len() + SHORT_ID_LEN + TIMESTAMP_LEN);
    input.extend_from_slice(SERVER_RESPONSE_TAG);
    input.extend_from_slice(&short_id.0);
    input.extend_from_slice(&client_timestamp.0.to_be_bytes());

    let full = lakko_crypto::hash::hmac_sha256(&mac_key, &input);
    let mut out = [0u8; SERVER_RESPONSE_LEN];
    out.copy_from_slice(&full[..SERVER_RESPONSE_LEN]);
    out
}

/// Verify a server-response read out of TLS 1.3
/// ServerHello.Random matches the expected HMAC under
/// `tag_secret`.
///
/// # Errors
///
/// [`RealityAuthError::BadServerResponse`] on mismatch
/// (constant-time compare).
pub fn verify_server_response(
    tag_secret: &RealityTagSecret,
    short_id: &RealityShortId,
    client_timestamp: RealityTimestamp,
    observed: &[u8; SERVER_RESPONSE_LEN],
) -> RealityAuthResult<()> {
    let expected = compute_server_response(tag_secret, short_id, client_timestamp);
    if expected.ct_eq(observed).unwrap_u8() == 1 {
        Ok(())
    } else {
        Err(RealityAuthError::BadServerResponse)
    }
}

/// HMAC-SHA-256(key, data) truncated to [`HMAC_TAG_LEN`].
fn hmac_sha256_truncated(key: &[u8], data: &[u8]) -> [u8; HMAC_TAG_LEN] {
    let full = lakko_crypto::hash::hmac_sha256(key, data);
    let mut out = [0u8; HMAC_TAG_LEN];
    out.copy_from_slice(&full[..HMAC_TAG_LEN]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> (RealityTagSecret, RealityShortId, RealityTimestamp) {
        let tag_secret = RealityTagSecret([0x42; 32]);
        let short_id = RealityShortId([0xAB; SHORT_ID_LEN]);
        let timestamp = RealityTimestamp(1_700_000_000);
        (tag_secret, short_id, timestamp)
    }

    #[test]
    fn compute_verify_round_trip() {
        let (tag_secret, short_id, ts) = fixtures();
        let auth = compute(&tag_secret, &short_id, ts);
        let (got_short_id, got_ts) =
            verify(&[(short_id, tag_secret)], &auth, ts, 30).expect("round-trip");
        assert_eq!(got_short_id, short_id);
        assert_eq!(got_ts, ts);
    }

    #[test]
    fn verify_rejects_unauthorised_short_id() {
        let (tag_secret, short_id, ts) = fixtures();
        let auth = compute(&tag_secret, &short_id, ts);
        let other = RealityShortId([0xCD; SHORT_ID_LEN]);
        match verify(&[(other, tag_secret)], &auth, ts, 30) {
            Err(RealityAuthError::ShortIdNotAllowed { got_hex }) => {
                assert_eq!(got_hex, "abababababababab");
            },
            other => panic!("expected ShortIdNotAllowed, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_timestamp_outside_window() {
        let (tag_secret, short_id, ts) = fixtures();
        let auth = compute(&tag_secret, &short_id, ts);
        let later = RealityTimestamp(ts.0 + 60);
        match verify(&[(short_id, tag_secret)], &auth, later, 30) {
            Err(RealityAuthError::TimestampSkew {
                drift_secs,
                tolerance_secs,
            }) => {
                assert_eq!(drift_secs, 60);
                assert_eq!(tolerance_secs, 30);
            },
            other => panic!("expected TimestampSkew, got {other:?}"),
        }
    }

    #[test]
    fn verify_accepts_timestamp_inside_window() {
        let (tag_secret, short_id, ts) = fixtures();
        let auth = compute(&tag_secret, &short_id, ts);
        let later = RealityTimestamp(ts.0 + 20);
        verify(&[(short_id, tag_secret)], &auth, later, 30).expect("inside tolerance must verify");
    }

    #[test]
    fn verify_rejects_tampered_tag() {
        let (tag_secret, short_id, ts) = fixtures();
        let mut auth = compute(&tag_secret, &short_id, ts);
        auth.0[SHORT_ID_LEN + TIMESTAMP_LEN] ^= 0x80;
        match verify(&[(short_id, tag_secret)], &auth, ts, 30) {
            Err(RealityAuthError::TagMismatch) => {},
            other => panic!("expected TagMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_wrong_tag_secret() {
        let (tag_secret, short_id, ts) = fixtures();
        let auth = compute(&tag_secret, &short_id, ts);
        let wrong = RealityTagSecret([0x99; 32]);
        match verify(&[(short_id, wrong)], &auth, ts, 30) {
            Err(RealityAuthError::TagMismatch) => {},
            other => panic!("expected TagMismatch, got {other:?}"),
        }
    }

    #[test]
    fn authenticator_is_32_bytes() {
        let (tag_secret, short_id, ts) = fixtures();
        let auth = compute(&tag_secret, &short_id, ts);
        assert_eq!(auth.0.len(), AUTHENTICATOR_LEN);
    }

    #[test]
    fn short_id_extractable_from_authenticator() {
        let (tag_secret, short_id, ts) = fixtures();
        let auth = compute(&tag_secret, &short_id, ts);
        assert_eq!(&auth.0[..SHORT_ID_LEN], &short_id.0);
    }

    #[test]
    fn same_short_id_distinct_timestamps_yield_distinct_tags() {
        let (tag_secret, short_id, _) = fixtures();
        let auth_a = compute(&tag_secret, &short_id, RealityTimestamp(1_700_000_000));
        let auth_b = compute(&tag_secret, &short_id, RealityTimestamp(1_700_000_001));
        assert_eq!(&auth_a.0[..SHORT_ID_LEN], &auth_b.0[..SHORT_ID_LEN]);
        assert_ne!(&auth_a.0[16..], &auth_b.0[16..]);
    }

    #[test]
    fn server_response_round_trip() {
        let (tag_secret, short_id, ts) = fixtures();
        let resp = compute_server_response(&tag_secret, &short_id, ts);
        verify_server_response(&tag_secret, &short_id, ts, &resp)
            .expect("server response verifies");
    }

    #[test]
    fn server_response_rejects_wrong_tag_secret() {
        let (tag_secret, short_id, ts) = fixtures();
        let resp = compute_server_response(&tag_secret, &short_id, ts);
        let wrong = RealityTagSecret([0xEE; 32]);
        match verify_server_response(&wrong, &short_id, ts, &resp) {
            Err(RealityAuthError::BadServerResponse) => {},
            other => panic!("expected BadServerResponse, got {other:?}"),
        }
    }

    #[test]
    fn server_response_rejects_wrong_short_id() {
        let (tag_secret, short_id, ts) = fixtures();
        let resp = compute_server_response(&tag_secret, &short_id, ts);
        let wrong_short_id = RealityShortId([0x77; SHORT_ID_LEN]);
        match verify_server_response(&tag_secret, &wrong_short_id, ts, &resp) {
            Err(RealityAuthError::BadServerResponse) => {},
            other => panic!("expected BadServerResponse, got {other:?}"),
        }
    }

    #[test]
    fn server_response_rejects_wrong_timestamp() {
        let (tag_secret, short_id, ts) = fixtures();
        let resp = compute_server_response(&tag_secret, &short_id, ts);
        match verify_server_response(&tag_secret, &short_id, RealityTimestamp(ts.0 + 1), &resp) {
            Err(RealityAuthError::BadServerResponse) => {},
            other => panic!("expected BadServerResponse, got {other:?}"),
        }
    }

    #[test]
    fn server_response_distinguishes_client_response() {
        // A reflected client tag MUST NOT verify in the
        // server-response position — domain separation
        // via SERVER_RESPONSE_TAG prefix guarantees this.
        let (tag_secret, short_id, ts) = fixtures();
        let client_auth = compute(&tag_secret, &short_id, ts);
        let mut padded_client_tag = [0u8; SERVER_RESPONSE_LEN];
        padded_client_tag[..AUTHENTICATOR_LEN].copy_from_slice(&client_auth.0);
        match verify_server_response(&tag_secret, &short_id, ts, &padded_client_tag) {
            Err(RealityAuthError::BadServerResponse) => {},
            other => panic!("client tag MUST NOT verify as server response, got {other:?}"),
        }
    }
}
