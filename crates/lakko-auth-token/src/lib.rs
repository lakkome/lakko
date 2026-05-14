// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HMAC-SHA256 session-token issuance + verification for the
//! lakko-svc-auth ↔ lakko-svc-discovery contract (Phase 1.5b).
//!
//! # Wire format
//!
//! Tokens are a four-part dotted string:
//!
//! ```text
//! v1.<account_id_b64url>.<expires_at_ms>.<tag_b64url>
//! ```
//!
//! Where:
//!
//! - `v1` is a string-literal version marker (future-proofing for a
//!   `v2` rotation that bumps the HMAC algorithm or adds fields).
//! - `account_id_b64url` is base64url-no-pad of the raw 16-byte
//!   UUIDv7 account_id (RFC 4648 §5).
//! - `expires_at_ms` is the unix-millisecond expiry timestamp,
//!   formatted as an ASCII decimal integer.
//! - `tag_b64url` is base64url-no-pad of the 32-byte HMAC-SHA256
//!   over the canonical message bytes `v1.<account_id_b64url>.<expires_at_ms>`
//!   (the entire prefix up to the dot before the tag, inclusive of
//!   that dot? No — exclusive; the message is the three earlier
//!   segments separated by dots, **without** the trailing dot or
//!   the tag itself).
//!
//! # Why a custom format and not JWT
//!
//! JWT carries header + payload + signature as three base64url
//! segments. For our use case (single algorithm, single audience,
//! no key rotation in Phase 1, two fields in the payload) the
//! header is pure noise and the payload's JSON-parse step is one
//! more place for a parser-differential bug to land. The custom
//! format reads in ~30 lines, allocates exactly two `Vec<u8>` on
//! verify (account_id + tag), and has no JSON.
//!
//! # Security posture
//!
//! - Tag verification uses [`subtle::ConstantTimeEq`] for the
//!   final byte compare — no leaky `==`.
//! - The HMAC secret loads from `LAKKO_SESSION_TOKEN_SECRET`
//!   (base64-encoded 32+ bytes); both `services/auth` and
//!   `services/discovery` MUST be configured with the same value.
//!   Mismatched secrets surface as a "verify failed" 401 on every
//!   protected discovery endpoint, never as a silent accept.
//! - Tokens are **stateless** in Phase 1 — no DB row, no
//!   revocation list. The 30-day TTL is the only invalidation
//!   path. Re-login through cached password is fast (OPAQUE is
//!   sub-100ms), so a forced rotation costs the user one extra
//!   Face/Touch ID auth, not a friction-y password re-entry. A
//!   revocation list lands when the threat model expects targeted
//!   token-theft attacks (Phase 2+).
//! - The token does NOT carry plaintext PII — only the account_id
//!   (already public-ish on the discovery side) + an expiry. A
//!   leaked token grants the holder discovery-write permission
//!   for the embedded account_id until it expires; the holder
//!   cannot decrypt messages (MLS keys are in the device's
//!   Keychain), cannot impersonate signatures (Ed25519 seed is in
//!   the OPAQUE export key, never on the wire), and cannot read
//!   private inbox bytes (the DS is keyed by pseudonyms, not
//!   account_id).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Length of a raw account_id in bytes (UUIDv7 — 128 bits).
pub const ACCOUNT_ID_LEN: usize = 16;

/// Length of the HMAC-SHA256 tag.
pub const TAG_LEN: usize = 32;

/// Minimum number of bytes the shared HMAC secret must carry.
///
/// 32 bytes = 256 bits = the natural key size for HMAC-SHA256
/// (per RFC 2104 §3 the key should be at least L bytes, where
/// L is the hash output length). Shorter secrets reduce the
/// effective security margin of the HMAC.
pub const MIN_SECRET_LEN: usize = 32;

/// Wire-format version marker. Bump this when the canonical
/// message layout, HMAC algorithm, or field set changes; old
/// tokens with the prior marker fail [`TokenSecret::verify`] with
/// [`TokenError::UnsupportedVersion`].
pub const VERSION: &str = "v1";

/// Shared HMAC secret. Loaded once at service startup from the
/// `LAKKO_SESSION_TOKEN_SECRET` environment variable (base64-encoded
/// bytes), then cloned cheaply into per-request state.
///
/// Wraps the raw secret bytes; do NOT debug-print this type — the
/// `Debug` impl is deliberately opaque so accidental log lines
/// don't leak the secret.
#[derive(Clone)]
pub struct TokenSecret {
    bytes: Vec<u8>,
}

impl TokenSecret {
    /// Wrap raw secret bytes.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::SecretTooShort`] if `bytes` is shorter
    /// than [`MIN_SECRET_LEN`].
    pub fn new(bytes: Vec<u8>) -> Result<Self, TokenError> {
        if bytes.len() < MIN_SECRET_LEN {
            return Err(TokenError::SecretTooShort {
                got: bytes.len(),
                min: MIN_SECRET_LEN,
            });
        }
        Ok(Self { bytes })
    }

    /// Decode a base64-encoded (standard alphabet, optional padding)
    /// secret string. Convenience for env-var loading where the
    /// secret rides as base64 inside `/etc/lakko/lakko.env`.
    ///
    /// # Errors
    ///
    /// - [`TokenError::SecretBase64`] on a base64 decode failure.
    /// - [`TokenError::SecretTooShort`] when the decoded bytes are
    ///   shorter than [`MIN_SECRET_LEN`].
    pub fn from_base64(s: &str) -> Result<Self, TokenError> {
        // Accept both standard and URL-safe alphabets; both are
        // valid choices for an env-var value.
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .or_else(|_| URL_SAFE_NO_PAD.decode(s.trim()))
            .map_err(|e| TokenError::SecretBase64(e.to_string()))?;
        Self::new(bytes)
    }

    /// Issue a token for the given account.
    ///
    /// `expires_at_ms` is the absolute unix-millisecond timestamp
    /// the token is valid until. Callers typically construct it as
    /// `now_ms + token_ttl_ms` — keeping the absolute form in the
    /// API (instead of a relative TTL) lets the issuer + verifier
    /// share zero clock-assumptions about each other.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::WrongAccountIdLength`] if `account_id`
    /// is not exactly [`ACCOUNT_ID_LEN`] bytes.
    pub fn issue(&self, account_id: &[u8], expires_at_ms: i64) -> Result<String, TokenError> {
        if account_id.len() != ACCOUNT_ID_LEN {
            return Err(TokenError::WrongAccountIdLength {
                got: account_id.len(),
                expected: ACCOUNT_ID_LEN,
            });
        }
        let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);
        // Canonical message: `v1.<account_id_b64>.<expires_at_ms>`.
        // Concatenating as a single string lets the verifier
        // recompute the same bytes without re-allocating around
        // separator handling.
        let message = format!("{VERSION}.{account_id_b64}.{expires_at_ms}");
        let tag = self.compute_tag(message.as_bytes());
        let tag_b64 = URL_SAFE_NO_PAD.encode(tag);
        Ok(format!("{message}.{tag_b64}"))
    }

    /// Verify a token string at the given wall-clock instant.
    ///
    /// Returns the embedded account_id bytes on success. Constant-
    /// time tag compare; the only timing leak is the parse step
    /// (which is on every code path regardless of validity).
    ///
    /// # Errors
    ///
    /// Maps every failure mode to a [`TokenError`] variant the
    /// caller can branch on:
    ///
    /// - [`TokenError::Malformed`] — wrong number of dot-segments,
    ///   wrong base64, non-integer expiry, account_id wrong length
    ///   after decode.
    /// - [`TokenError::UnsupportedVersion`] — segment 0 is not
    ///   [`VERSION`].
    /// - [`TokenError::Expired`] — `expires_at_ms <= now_ms`.
    /// - [`TokenError::BadSignature`] — HMAC mismatch.
    pub fn verify(&self, token: &str, now_ms: i64) -> Result<Vec<u8>, TokenError> {
        // Split into exactly four segments. `splitn(4, '.')` would
        // collapse trailing dots into the last segment; for a
        // strict format we count segments and reject extras.
        let segments: Vec<&str> = token.split('.').collect();
        if segments.len() != 4 {
            return Err(TokenError::Malformed("expected 4 dot-segments".into()));
        }
        let version = segments[0];
        let account_id_b64 = segments[1];
        let expires_at_str = segments[2];
        let tag_b64 = segments[3];

        if version != VERSION {
            return Err(TokenError::UnsupportedVersion(version.to_string()));
        }

        let account_id = URL_SAFE_NO_PAD
            .decode(account_id_b64)
            .map_err(|e| TokenError::Malformed(format!("account_id base64: {e}")))?;
        if account_id.len() != ACCOUNT_ID_LEN {
            return Err(TokenError::Malformed(format!(
                "account_id length {} != {}",
                account_id.len(),
                ACCOUNT_ID_LEN
            )));
        }

        let expires_at_ms: i64 = expires_at_str
            .parse()
            .map_err(|e| TokenError::Malformed(format!("expires_at parse: {e}")))?;

        let tag = URL_SAFE_NO_PAD
            .decode(tag_b64)
            .map_err(|e| TokenError::Malformed(format!("tag base64: {e}")))?;
        if tag.len() != TAG_LEN {
            return Err(TokenError::Malformed(format!(
                "tag length {} != {}",
                tag.len(),
                TAG_LEN
            )));
        }

        // Verify the HMAC BEFORE checking expiry — otherwise an
        // attacker can distinguish "valid signature + expired" vs
        // "invalid signature" via timing. The two failure paths
        // collapse into one observable result on the wire.
        let message = format!("{version}.{account_id_b64}.{expires_at_ms}");
        let expected = self.compute_tag(message.as_bytes());
        if expected.ct_eq(&tag).unwrap_u8() != 1 {
            return Err(TokenError::BadSignature);
        }

        if expires_at_ms <= now_ms {
            return Err(TokenError::Expired {
                expires_at_ms,
                now_ms,
            });
        }
        Ok(account_id)
    }

    /// Compute the 32-byte HMAC-SHA256 tag over `message`.
    fn compute_tag(&self, message: &[u8]) -> [u8; TAG_LEN] {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&self.bytes).expect("HMAC accepts any key size");
        mac.update(message);
        let out = mac.finalize().into_bytes();
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&out);
        tag
    }
}

impl std::fmt::Debug for TokenSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately opaque — accidental `tracing::debug!(?secret, …)`
        // must not leak the bytes.
        f.debug_struct("TokenSecret")
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// Error vocabulary for [`TokenSecret::issue`] / [`TokenSecret::verify`]
/// / [`TokenSecret::from_base64`].
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// Caller passed an account_id with the wrong byte length.
    #[error("account_id length {got} != expected {expected}")]
    WrongAccountIdLength {
        /// Length the caller passed.
        got: usize,
        /// Expected (= [`ACCOUNT_ID_LEN`]).
        expected: usize,
    },
    /// Configured secret is shorter than [`MIN_SECRET_LEN`] bytes.
    #[error("session-token secret too short: got {got} bytes, need at least {min}")]
    SecretTooShort {
        /// Length the env var decoded to.
        got: usize,
        /// Floor (= [`MIN_SECRET_LEN`]).
        min: usize,
    },
    /// Base64 decode of the env-var secret failed.
    #[error("session-token secret base64 decode: {0}")]
    SecretBase64(String),
    /// Token string did not parse — wrong shape / base64 / integer.
    /// The inner message is **for logs only** and must not leak to
    /// the client (an attacker could use the parse-fail copy to
    /// distinguish "I'm probing the protocol" from "wrong key").
    #[error("malformed token: {0}")]
    Malformed(String),
    /// Token version segment did not match [`VERSION`].
    #[error("unsupported token version: {0}")]
    UnsupportedVersion(String),
    /// Token expired (or was issued for the past — either way the
    /// caller cannot use it).
    #[error("token expired at {expires_at_ms} (now {now_ms})")]
    Expired {
        /// Embedded expiry.
        expires_at_ms: i64,
        /// Wall-clock instant the verifier saw.
        now_ms: i64,
    },
    /// HMAC tag did not match — either a forgery attempt or the
    /// issuer + verifier are loaded with different secrets.
    #[error("token signature verification failed")]
    BadSignature,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 32-byte all-zero secret — fine for tests, never for production.
    fn test_secret() -> TokenSecret {
        TokenSecret::new(vec![0u8; MIN_SECRET_LEN]).unwrap()
    }

    fn sample_account_id() -> Vec<u8> {
        // Deterministic UUIDv7-shape bytes; the verifier doesn't
        // parse the layout, so any 16 bytes work.
        (0u8..u8::try_from(ACCOUNT_ID_LEN).unwrap()).collect()
    }

    #[test]
    fn issue_then_verify_round_trip() {
        let secret = test_secret();
        let account_id = sample_account_id();
        let exp = 2_000_000_000_000i64;
        let token = secret.issue(&account_id, exp).unwrap();
        let recovered = secret.verify(&token, 1_000_000_000_000).unwrap();
        assert_eq!(recovered, account_id);
    }

    #[test]
    fn token_has_four_dot_segments() {
        let secret = test_secret();
        let token = secret
            .issue(&sample_account_id(), 2_000_000_000_000)
            .unwrap();
        assert_eq!(token.matches('.').count(), 3); // 3 dots → 4 segments
        assert!(token.starts_with("v1."));
    }

    #[test]
    fn verify_rejects_expired_token() {
        let secret = test_secret();
        let token = secret.issue(&sample_account_id(), 1_000).unwrap();
        match secret.verify(&token, 2_000).unwrap_err() {
            TokenError::Expired { .. } => {},
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let issuer = test_secret();
        let verifier = TokenSecret::new(vec![1u8; MIN_SECRET_LEN]).unwrap();
        let token = issuer
            .issue(&sample_account_id(), 2_000_000_000_000)
            .unwrap();
        match verifier.verify(&token, 1_000_000_000_000).unwrap_err() {
            TokenError::BadSignature => {},
            other => panic!("expected BadSignature, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_tampered_account_id() {
        let secret = test_secret();
        let token = secret
            .issue(&sample_account_id(), 2_000_000_000_000)
            .unwrap();
        // Swap the account-id segment for a different valid base64
        // — same length, different bytes; HMAC must reject.
        let mut parts: Vec<&str> = token.split('.').collect();
        let evil_id = URL_SAFE_NO_PAD.encode([0u8; ACCOUNT_ID_LEN]);
        parts[1] = &evil_id;
        let tampered = parts.join(".");
        match secret.verify(&tampered, 1_000_000_000_000).unwrap_err() {
            TokenError::BadSignature => {},
            other => panic!("expected BadSignature, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_unsupported_version() {
        let secret = test_secret();
        let token = secret
            .issue(&sample_account_id(), 2_000_000_000_000)
            .unwrap();
        let evil = format!("v2{}", token.strip_prefix("v1").unwrap());
        match secret.verify(&evil, 1_000_000_000_000).unwrap_err() {
            TokenError::UnsupportedVersion(v) => assert_eq!(v, "v2"),
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_malformed_shapes() {
        let secret = test_secret();
        for bad in &[
            "",
            "v1",
            "v1.aaa",
            "v1.aaa.bbb",
            "v1.aaa.bbb.ccc.ddd",                          // too many segments
            "v1.!@#.1.AAAA",                               // bad base64 in account_id
            "v1.AAAAAAAAAAAAAAAAAAAAAA.not_a_number.AAAA", // bad expiry
        ] {
            assert!(
                matches!(
                    secret.verify(bad, 1_000_000_000_000).unwrap_err(),
                    TokenError::Malformed(_) | TokenError::UnsupportedVersion(_)
                ),
                "expected Malformed for {bad:?}"
            );
        }
    }

    #[test]
    fn issue_rejects_wrong_account_id_length() {
        let secret = test_secret();
        match secret.issue(&[0u8; 8], 1_000).unwrap_err() {
            TokenError::WrongAccountIdLength { got, expected } => {
                assert_eq!(got, 8);
                assert_eq!(expected, ACCOUNT_ID_LEN);
            },
            other => panic!("expected WrongAccountIdLength, got {other:?}"),
        }
    }

    #[test]
    fn secret_rejects_short_input() {
        match TokenSecret::new(vec![0u8; MIN_SECRET_LEN - 1]).unwrap_err() {
            TokenError::SecretTooShort { got, min } => {
                assert_eq!(got, MIN_SECRET_LEN - 1);
                assert_eq!(min, MIN_SECRET_LEN);
            },
            other => panic!("expected SecretTooShort, got {other:?}"),
        }
    }

    #[test]
    fn secret_from_base64_round_trip() {
        let raw = vec![7u8; MIN_SECRET_LEN];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
        let secret = TokenSecret::from_base64(&encoded).unwrap();
        let account_id = sample_account_id();
        let token = secret.issue(&account_id, 2_000_000_000_000).unwrap();
        let recovered = secret.verify(&token, 1_000_000_000_000).unwrap();
        assert_eq!(recovered, account_id);
    }

    #[test]
    fn debug_does_not_leak_bytes() {
        let secret = TokenSecret::new(vec![0xABu8; MIN_SECRET_LEN]).unwrap();
        let debug = format!("{secret:?}");
        assert!(debug.contains("len"));
        assert!(
            !debug.contains("AB"),
            "Debug impl leaked secret bytes: {debug}"
        );
    }
}
