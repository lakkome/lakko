// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! REALITY authenticator — the 32-byte
//! TLS-ClientHello-session_id-slot construction that
//! REALITY rides on.
//!
//! ## Wire layout
//!
//! ```text
//!     0           8           16                          32
//!     |  short_id  |  ts_be8    |       hmac_tag[..16]      |
//! ```
//!
//! - `short_id` (8 bytes): the publisher-assigned tag the
//!   server uses to look up its key material. Plaintext —
//!   the audit posture is "an attacker who learns the
//!   short_id learns nothing additional, because every
//!   client legitimately receiving a bootstrap config has
//!   the same short_id".
//! - `ts_be8` (8 bytes, u64 big-endian unix seconds): the
//!   client's wall-clock at handshake time. The receiver
//!   refuses a handshake outside a bounded window of its
//!   own clock so a captured authenticator cannot be
//!   replayed indefinitely.
//! - `hmac_tag[..16]`: first 16 bytes of HMAC-SHA-256 over
//!   `short_id || ts_be8`, keyed by the HKDF expansion of
//!   the X25519 ECDH(eph_priv, server_static_pub) shared
//!   secret. The ephemeral X25519 public key the receiver
//!   uses for verification rides in the standard TLS 1.3
//!   `key_share` extension — REALITY does NOT need a
//!   second ephemeral inside session_id.
//!
//! ## Wire compatibility caveat
//!
//! Lakko's REALITY authenticator pins its own canonical
//! bytes via the [`HKDF_LABEL`] string
//! (`"lakko/reality/v1"`) and is therefore NOT
//! byte-for-byte compatible with upstream xtls/REALITY
//! servers. A follow-up sprint aligns the exact label /
//! field layout against the go-xray REALITY source to
//! enable interop with existing tooling; until then,
//! Lakko REALITY clients only handshake with Lakko REALITY
//! servers. The audit invariant (T-13) only pins the
//! cryptographic strength of the primitive, not interop.
//!
//! ## Why session_id and not a TLS extension
//!
//! The session_id is a 32-byte field every TLS 1.3
//! ClientHello carries (originally intended as a TLS 1.2
//! session-resumption cache token; in TLS 1.3 it carries
//! a random "compatibility" value the spec lets the
//! server echo back). To a passive observer the
//! authenticator looks indistinguishable from a normal
//! resumption-cache token — that is the entire point of
//! REALITY.

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::kdf::hkdf_sha256;
use subtle::ConstantTimeEq as _;
use thiserror::Error;

use crate::reality::config::{
    RealityServerPubkey, RealityShortId, REALITY_PUBKEY_LEN, SHORT_ID_LEN,
};

/// Total length of the REALITY authenticator — exactly
/// the size of TLS 1.3 ClientHello.session_id.
pub const AUTHENTICATOR_LEN: usize = 32;

/// Length of the embedded big-endian unix-seconds
/// timestamp.
pub const TIMESTAMP_LEN: usize = 8;

/// Length of the HMAC tag truncation. 16 bytes is the
/// HMAC-SHA-256 truncation per RFC 4868 §2.6, sufficient
/// for the threat model — an attacker without the ECDH
/// shared secret has at most 2^-128 odds of forging a
/// valid tag.
pub const HMAC_TAG_LEN: usize = 16;

/// HKDF label that pins this implementation's authenticator
/// to the Lakko-specific byte layout. Changing this label
/// is a breaking protocol change.
pub const HKDF_LABEL: &[u8] = b"lakko/reality/v1";

/// Length of the HKDF-derived HMAC key in bytes.
const HMAC_KEY_LEN: usize = 32;

const _CONST_CHECK: () = assert!(
    SHORT_ID_LEN + TIMESTAMP_LEN + HMAC_TAG_LEN == AUTHENTICATOR_LEN,
    "REALITY wire layout must total exactly 32 bytes"
);

/// Unix-seconds wall-clock value the authenticator binds.
/// Newtype so tests can pin a fixed timestamp and the
/// production path picks up `SystemTime::now`.
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
    /// receiver authorises. The receiver typically holds
    /// a set of short_ids (one per active publisher
    /// config); the variant carries the one observed.
    #[error("short_id {got_hex} not in allowed set")]
    ShortIdNotAllowed {
        /// Hex of the observed short_id, telemetry-only.
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
    /// wrong server pubkey, or attacker without ECDH
    /// shared secret.
    #[error("HMAC tag did not verify")]
    TagMismatch,
}

/// Convenience alias.
pub type RealityAuthResult<T> = core::result::Result<T, RealityAuthError>;

/// Build a REALITY authenticator for the TLS 1.3
/// ClientHello.session_id slot.
///
/// The caller supplies:
///
/// - `server_pubkey` — the REALITY server's static X25519
///   public key (from the signed bootstrap config).
/// - `short_id` — the publisher-assigned tag.
/// - `ephemeral_secret` — a fresh X25519 secret. In
///   production this is the SAME key whose public half
///   the caller will place in the TLS 1.3 key_share
///   extension; the receiver will reach the same ECDH
///   value because it sees the same public half.
/// - `timestamp` — current unix-seconds wall clock.
#[must_use]
pub fn compute(
    server_pubkey: &RealityServerPubkey,
    short_id: &RealityShortId,
    ephemeral_secret: &X25519SecretKey,
    timestamp: RealityTimestamp,
) -> RealityAuthenticator {
    let server_pk = X25519PublicKey::from_bytes(&server_pubkey.0)
        .expect("32-byte X25519 pubkey decodes — bounded by REALITY_PUBKEY_LEN");
    let shared = ephemeral_secret.diffie_hellman(&server_pk);
    let mac_key = hkdf_sha256::<HMAC_KEY_LEN>(shared.as_bytes(), b"", HKDF_LABEL)
        .expect("HKDF-SHA-256 32->32 byte expansion never fails");

    let mut payload = [0u8; SHORT_ID_LEN + TIMESTAMP_LEN];
    payload[..SHORT_ID_LEN].copy_from_slice(&short_id.0);
    payload[SHORT_ID_LEN..].copy_from_slice(&timestamp.0.to_be_bytes());

    let tag = hmac_sha256_truncated(mac_key.as_bytes(), &payload);

    let mut out = [0u8; AUTHENTICATOR_LEN];
    out[..SHORT_ID_LEN].copy_from_slice(&short_id.0);
    out[SHORT_ID_LEN..SHORT_ID_LEN + TIMESTAMP_LEN].copy_from_slice(&timestamp.0.to_be_bytes());
    out[SHORT_ID_LEN + TIMESTAMP_LEN..].copy_from_slice(&tag);
    RealityAuthenticator(out)
}

/// Verify an inbound REALITY authenticator against the
/// receiver's keys.
///
/// `server_secret` is the receiver's static X25519 secret
/// (the half whose public part the publisher distributed
/// to clients). `client_ephemeral_pub` is the ephemeral
/// X25519 public key from the inbound ClientHello's
/// key_share extension. `allowed_short_ids` is the set
/// of short_ids the receiver is authorised to accept.
/// `now` is the receiver's wall clock; `tolerance_secs`
/// bounds the replay window.
///
/// # Errors
///
/// See [`RealityAuthError`].
pub fn verify(
    server_secret: &X25519SecretKey,
    client_ephemeral_pub: &[u8; REALITY_PUBKEY_LEN],
    allowed_short_ids: &[RealityShortId],
    auth: &RealityAuthenticator,
    now: RealityTimestamp,
    tolerance_secs: u64,
) -> RealityAuthResult<(RealityShortId, RealityTimestamp)> {
    // 1. Extract short_id; lookup against allow-set.
    let mut short_id_bytes = [0u8; SHORT_ID_LEN];
    short_id_bytes.copy_from_slice(&auth.0[..SHORT_ID_LEN]);
    let short_id = RealityShortId(short_id_bytes);
    if !allowed_short_ids.iter().any(|s| s == &short_id) {
        return Err(RealityAuthError::ShortIdNotAllowed {
            got_hex: hex::encode(short_id_bytes),
        });
    }

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

    // 3. Re-derive HMAC key from ECDH(server_secret,
    //    client_ephemeral_pub) — symmetric with `compute`.
    let client_pk =
        X25519PublicKey::from_bytes(client_ephemeral_pub).expect("32-byte X25519 pubkey decodes");
    let shared = server_secret.diffie_hellman(&client_pk);
    let mac_key = hkdf_sha256::<HMAC_KEY_LEN>(shared.as_bytes(), b"", HKDF_LABEL)
        .expect("HKDF-SHA-256 32->32 byte expansion never fails");

    // 4. Recompute tag, constant-time-compare.
    let mut payload = [0u8; SHORT_ID_LEN + TIMESTAMP_LEN];
    payload[..SHORT_ID_LEN].copy_from_slice(&short_id_bytes);
    payload[SHORT_ID_LEN..].copy_from_slice(&ts_bytes);
    let expected_tag = hmac_sha256_truncated(mac_key.as_bytes(), &payload);
    let observed_tag = &auth.0[SHORT_ID_LEN + TIMESTAMP_LEN..];

    // Constant-time comparison so a timing attacker
    // cannot narrow which bytes of the tag mismatched.
    if expected_tag.ct_eq(observed_tag).unwrap_u8() == 1 {
        Ok((short_id, RealityTimestamp(claimed)))
    } else {
        Err(RealityAuthError::TagMismatch)
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
    use lakko_crypto::agreement::SecretKey as X25519SecretKey;

    fn fixtures() -> (
        X25519SecretKey,          // server static secret
        RealityServerPubkey,      // server static pub
        X25519SecretKey,          // client ephemeral secret
        [u8; REALITY_PUBKEY_LEN], // client ephemeral pub
        RealityShortId,
        RealityTimestamp,
    ) {
        let server_secret = X25519SecretKey::from_bytes([0x11; 32]);
        let server_pub_bytes = server_secret.public_key().to_bytes();
        let server_pub = RealityServerPubkey(server_pub_bytes);

        let client_secret = X25519SecretKey::from_bytes([0x22; 32]);
        let client_pub = client_secret.public_key().to_bytes();

        let short_id = RealityShortId([0xAB; SHORT_ID_LEN]);
        let timestamp = RealityTimestamp(1_700_000_000);

        (
            server_secret,
            server_pub,
            client_secret,
            client_pub,
            short_id,
            timestamp,
        )
    }

    #[test]
    fn compute_verify_round_trip() {
        let (server_sec, server_pub, client_sec, client_pub, short_id, ts) = fixtures();
        let auth = compute(&server_pub, &short_id, &client_sec, ts);
        let (got_short_id, got_ts) =
            verify(&server_sec, &client_pub, &[short_id], &auth, ts, 30).expect("round-trip");
        assert_eq!(got_short_id, short_id);
        assert_eq!(got_ts, ts);
    }

    #[test]
    fn verify_rejects_unauthorised_short_id() {
        let (server_sec, server_pub, client_sec, client_pub, short_id, ts) = fixtures();
        let auth = compute(&server_pub, &short_id, &client_sec, ts);
        let other = RealityShortId([0xCD; SHORT_ID_LEN]);
        match verify(&server_sec, &client_pub, &[other], &auth, ts, 30) {
            Err(RealityAuthError::ShortIdNotAllowed { got_hex }) => {
                assert_eq!(got_hex, "abababababababab");
            },
            other => panic!("expected ShortIdNotAllowed, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_timestamp_outside_window() {
        let (server_sec, server_pub, client_sec, client_pub, short_id, ts) = fixtures();
        let auth = compute(&server_pub, &short_id, &client_sec, ts);
        // Receiver clock 60s ahead, tolerance only 30s.
        let later = RealityTimestamp(ts.0 + 60);
        match verify(&server_sec, &client_pub, &[short_id], &auth, later, 30) {
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
        let (server_sec, server_pub, client_sec, client_pub, short_id, ts) = fixtures();
        let auth = compute(&server_pub, &short_id, &client_sec, ts);
        let later = RealityTimestamp(ts.0 + 20); // 20s drift, tolerance 30s
        verify(&server_sec, &client_pub, &[short_id], &auth, later, 30)
            .expect("inside tolerance must verify");
    }

    #[test]
    fn verify_rejects_tampered_tag() {
        let (server_sec, server_pub, client_sec, client_pub, short_id, ts) = fixtures();
        let mut auth = compute(&server_pub, &short_id, &client_sec, ts);
        // Flip the high bit of the first tag byte.
        auth.0[SHORT_ID_LEN + TIMESTAMP_LEN] ^= 0x80;
        match verify(&server_sec, &client_pub, &[short_id], &auth, ts, 30) {
            Err(RealityAuthError::TagMismatch) => {},
            other => panic!("expected TagMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_wrong_server_secret() {
        let (_server_sec, server_pub, client_sec, client_pub, short_id, ts) = fixtures();
        let auth = compute(&server_pub, &short_id, &client_sec, ts);
        // Attacker tries to verify with a DIFFERENT
        // server static secret — ECDH shared secret will
        // differ, so the HMAC tag will not validate.
        let wrong_secret = X25519SecretKey::from_bytes([0x99; 32]);
        match verify(&wrong_secret, &client_pub, &[short_id], &auth, ts, 30) {
            Err(RealityAuthError::TagMismatch) => {},
            other => panic!("expected TagMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_wrong_client_ephemeral() {
        let (server_sec, server_pub, client_sec, _client_pub, short_id, ts) = fixtures();
        let auth = compute(&server_pub, &short_id, &client_sec, ts);
        // Substitute a different ephemeral pubkey — same
        // failure shape as tampered ClientHello.key_share.
        let wrong_pub = X25519SecretKey::from_bytes([0xEE; 32])
            .public_key()
            .to_bytes();
        match verify(&server_sec, &wrong_pub, &[short_id], &auth, ts, 30) {
            Err(RealityAuthError::TagMismatch) => {},
            other => panic!("expected TagMismatch, got {other:?}"),
        }
    }

    #[test]
    fn authenticator_is_32_bytes() {
        let (_, server_pub, client_sec, _, short_id, ts) = fixtures();
        let auth = compute(&server_pub, &short_id, &client_sec, ts);
        assert_eq!(auth.0.len(), 32);
        assert_eq!(auth.0.len(), AUTHENTICATOR_LEN);
    }

    #[test]
    fn short_id_extractable_from_authenticator() {
        // The receiver looks up which key material to use
        // by reading short_id directly from the
        // authenticator before doing any HMAC work — pin
        // that the layout puts short_id first.
        let (_, server_pub, client_sec, _, short_id, ts) = fixtures();
        let auth = compute(&server_pub, &short_id, &client_sec, ts);
        assert_eq!(&auth.0[..SHORT_ID_LEN], &short_id.0);
    }

    #[test]
    fn fresh_ephemeral_each_invocation_yields_distinct_tags() {
        // Even with the same server pub + short_id + ts,
        // a fresh ephemeral X25519 keypair MUST yield a
        // distinct tag — otherwise a passive observer
        // could replay-detect by tag equality across two
        // independent handshakes.
        let (_, server_pub, _, _, short_id, ts) = fixtures();
        let client_a = X25519SecretKey::from_bytes([0x33; 32]);
        let client_b = X25519SecretKey::from_bytes([0x44; 32]);
        let auth_a = compute(&server_pub, &short_id, &client_a, ts);
        let auth_b = compute(&server_pub, &short_id, &client_b, ts);
        // short_id + timestamp identical (first 16 bytes);
        // tag (last 16) MUST differ.
        assert_eq!(&auth_a.0[..16], &auth_b.0[..16]);
        assert_ne!(&auth_a.0[16..], &auth_b.0[16..]);
    }
}
