// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HMAC-SHA256 session-token issuance + verification: the contract between
//! `lakko-svc-auth` (issues) and every service that verifies a bearer
//! (`auth` itself, `discovery`, `storage`, `gif-proxy`).
//!
//! # Wire format
//!
//! Three credentials, all five-part dotted strings ([ADR-0065] section 3.1
//! and its 2026-10-04 amendment):
//!
//! ```text
//! access      = v2.<account_id_b64url>.<key_generation>.<expires_at_ms>.<tag_b64url>
//! refresh     = r1.<account_id_b64url>.<key_generation>.<expires_at_ms>.<tag_b64url>
//! reset grant = k1.<account_id_b64url>.<key_generation>.<expires_at_ms>.<tag_b64url>
//! ```
//!
//! Where:
//!
//! - `v2` / `r1` / `k1` is the version marker, which also tells the kinds apart.
//!   `v1` (the four-part token without a generation) is refused: there are no
//!   users and no tokens to keep valid, so it was retired on a flag day.
//! - `account_id_b64url` is base64url-no-pad of the raw 16-byte UUIDv7
//!   account id (RFC 4648 section 5).
//! - `key_generation` is the account's key generation the credential was
//!   issued under (`accounts.identity_version` in auth; `1` is genesis),
//!   as an ASCII decimal integer.
//! - `expires_at_ms` is the unix-millisecond expiry, as an ASCII decimal
//!   integer.
//! - `tag_b64url` is base64url-no-pad of the 32-byte HMAC-SHA256 over the
//!   four earlier segments joined by dots (without the trailing dot or the
//!   tag).
//!
//! The access tag is computed under the shared secret itself; the refresh tag
//! under a key derived from it, `BLAKE3-derive-key("lakko/session-refresh/v1",
//! secret)`. So an access verifier's [`TokenSecret::verify`] refuses a refresh
//! credential by its version marker, and even a verifier that dropped the
//! version check could never mistake one tag for the other. The reset-grant
//! tag is computed under a third key, `BLAKE3-derive-key(
//! "lakko/key-reset-grant/v1", secret)`, for the same reason: a grant is
//! neither a bearer nor a refresh credential, and neither of those is a grant.
//!
//! # Why a custom format and not JWT
//!
//! JWT carries header + payload + signature as three base64url segments. For
//! this use (single algorithm, single audience, no key rotation, three fields
//! in the payload) the header is noise and the JSON parse is one more place
//! for a parser-differential bug to land. This format reads in a few dozen
//! lines, allocates two `Vec<u8>` on verify, and has no JSON.
//!
//! # Security posture
//!
//! - Tag verification uses [`subtle::ConstantTimeEq`] for the final compare.
//! - The HMAC secret loads from `LAKKO_SESSION_TOKEN_SECRET` (base64, 32+
//!   bytes); every issuer and verifier MUST hold the same value. Mismatched
//!   secrets surface as a "verify failed" 401, never as a silent accept.
//! - An access token is **stateless** at every verifier but auth: it carries
//!   the generation, and auth refuses one whose generation is below the
//!   account's current one on every bearer request. The other verifiers keep
//!   the stateless check, so their revocation latency is the access TTL
//!   (`LAKKO_SESSION_TOKEN_TTL_SECS`, 15 minutes by default).
//! - A refresh credential is accepted by auth's `POST /auth/session/refresh`
//!   only, which reads the account's current generation and refuses a lower
//!   one (`key-generation-superseded`). It carries no random id and has no
//!   database row: it is revoked by the generation, never individually.
//! - A **key-reset grant** is proof that the account's password was proven
//!   moments ago: auth mints one in the answer of a successful password
//!   OPAQUE login and nowhere else, and `POST /account/keys/reset/{start,
//!   finish}` require one beside the bearer. It is short-lived
//!   (`LAKKO_KEY_RESET_GRANT_TTL_SECS`, 10 minutes by default) and stateless,
//!   and it is single-use by construction: it names the key generation it was
//!   issued under, the reset route accepts it only while the account is at
//!   exactly that generation, and a reset bumps the generation. It grants
//!   nothing on its own (the reset route still needs the bearer).
//! - No credential carries plaintext PII: the account id (already known
//!   to every verifier) and two integers. A leaked access token grants its
//!   holder the bearer routes for the embedded account until it expires; the
//!   holder cannot decrypt messages, impersonate signatures, or read private
//!   inbox bytes.
//!
//! [ADR-0065]: ../../../docs/adr/0065-account-key-reset.md

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
/// (per RFC 2104 section 3 the key should be at least L bytes, where
/// L is the hash output length). Shorter secrets reduce the
/// effective security margin of the HMAC.
pub const MIN_SECRET_LEN: usize = 32;

/// Version marker of an access token. Bump this when the canonical message
/// layout, HMAC algorithm, or field set changes; a token with any other
/// marker fails [`TokenSecret::verify`] with [`TokenError::UnsupportedVersion`]
/// (so a `v1` token, or a refresh credential, is refused there).
pub const VERSION: &str = "v2";

/// Version marker of a refresh credential. [`TokenSecret::verify_refresh`]
/// refuses any other marker, so an access token presented as a refresh
/// credential fails with [`TokenError::UnsupportedVersion`].
pub const REFRESH_VERSION: &str = "r1";

/// BLAKE3 derive-key context of the refresh key: `refresh_key =
/// BLAKE3-derive-key(REFRESH_KEY_DOMAIN, secret)`.
pub const REFRESH_KEY_DOMAIN: &str = "lakko/session-refresh/v1";

/// Version marker of a key-reset grant. [`TokenSecret::verify_reset_grant`]
/// refuses any other marker, so an access token or a refresh credential
/// presented as a grant fails with [`TokenError::UnsupportedVersion`].
pub const RESET_GRANT_VERSION: &str = "k1";

/// BLAKE3 derive-key context of the reset-grant key: `reset_grant_key =
/// BLAKE3-derive-key(RESET_GRANT_KEY_DOMAIN, secret)`.
pub const RESET_GRANT_KEY_DOMAIN: &str = "lakko/key-reset-grant/v1";

/// Number of dot-separated segments in every credential.
const SEGMENTS: usize = 5;

/// What a verified credential says: which account, and under which key
/// generation it was issued. The verifier of a bearer request compares the
/// generation with the account's current one where it can (auth); the other
/// services accept any generation within the expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedToken {
    /// The raw 16-byte account id the credential was issued for.
    pub account_id: [u8; ACCOUNT_ID_LEN],
    /// The key generation the credential was issued under.
    pub key_generation: u64,
}

/// Shared HMAC secret. Loaded once at service startup from the
/// `LAKKO_SESSION_TOKEN_SECRET` environment variable (base64-encoded
/// bytes), then cloned cheaply into per-request state.
///
/// Wraps the raw secret bytes plus the derived refresh and reset-grant keys;
/// do NOT debug-print this type — the `Debug` impl is deliberately opaque so
/// accidental log lines don't leak any of them.
#[derive(Clone)]
pub struct TokenSecret {
    bytes: Vec<u8>,
    refresh_key: [u8; 32],
    reset_grant_key: [u8; 32],
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
        let refresh_key = blake3::derive_key(REFRESH_KEY_DOMAIN, &bytes);
        let reset_grant_key = blake3::derive_key(RESET_GRANT_KEY_DOMAIN, &bytes);
        Ok(Self {
            bytes,
            refresh_key,
            reset_grant_key,
        })
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

    /// Issue an access token for the given account at `key_generation`.
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
    pub fn issue(
        &self,
        account_id: &[u8],
        key_generation: u64,
        expires_at_ms: i64,
    ) -> Result<String, TokenError> {
        Self::issue_with(
            &self.bytes,
            VERSION,
            account_id,
            key_generation,
            expires_at_ms,
        )
    }

    /// Issue a refresh credential for the given account at `key_generation`,
    /// tagged under the derived refresh key. Only auth's refresh route
    /// accepts it ([`Self::verify_refresh`]).
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::WrongAccountIdLength`] if `account_id`
    /// is not exactly [`ACCOUNT_ID_LEN`] bytes.
    pub fn issue_refresh(
        &self,
        account_id: &[u8],
        key_generation: u64,
        expires_at_ms: i64,
    ) -> Result<String, TokenError> {
        Self::issue_with(
            &self.refresh_key,
            REFRESH_VERSION,
            account_id,
            key_generation,
            expires_at_ms,
        )
    }

    /// Issue a key-reset grant for the given account at `key_generation`,
    /// tagged under the derived reset-grant key. Auth mints one only in the
    /// answer of a successful password login; only the key-reset routes
    /// accept it ([`Self::verify_reset_grant`]).
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::WrongAccountIdLength`] if `account_id`
    /// is not exactly [`ACCOUNT_ID_LEN`] bytes.
    pub fn issue_reset_grant(
        &self,
        account_id: &[u8],
        key_generation: u64,
        expires_at_ms: i64,
    ) -> Result<String, TokenError> {
        Self::issue_with(
            &self.reset_grant_key,
            RESET_GRANT_VERSION,
            account_id,
            key_generation,
            expires_at_ms,
        )
    }

    /// Verify an access token at the given wall-clock instant.
    ///
    /// Returns the embedded account id and key generation on success.
    /// Constant-time tag compare; the only timing leak is the parse step
    /// (which is on every code path regardless of validity).
    ///
    /// # Errors
    ///
    /// Maps every failure mode to a [`TokenError`] variant the
    /// caller can branch on:
    ///
    /// - [`TokenError::Malformed`] — wrong number of dot-segments,
    ///   wrong base64, non-integer generation or expiry, account_id
    ///   wrong length after decode.
    /// - [`TokenError::UnsupportedVersion`] — segment 0 is not
    ///   [`VERSION`] (a `v1` token or a refresh credential).
    /// - [`TokenError::Expired`] — `expires_at_ms <= now_ms`.
    /// - [`TokenError::BadSignature`] — HMAC mismatch.
    pub fn verify(&self, token: &str, now_ms: i64) -> Result<VerifiedToken, TokenError> {
        Self::verify_with(&self.bytes, VERSION, token, now_ms)
    }

    /// Verify a refresh credential at the given wall-clock instant, under the
    /// derived refresh key. Same error vocabulary as [`Self::verify`]; an
    /// access token presented here is [`TokenError::UnsupportedVersion`].
    ///
    /// # Errors
    ///
    /// See [`Self::verify`].
    pub fn verify_refresh(&self, token: &str, now_ms: i64) -> Result<VerifiedToken, TokenError> {
        Self::verify_with(&self.refresh_key, REFRESH_VERSION, token, now_ms)
    }

    /// Verify a key-reset grant at the given wall-clock instant, under the
    /// derived reset-grant key. Same error vocabulary as [`Self::verify`]; an
    /// access token or a refresh credential presented here is
    /// [`TokenError::UnsupportedVersion`].
    ///
    /// This checks the grant itself (shape, tag, expiry). The CALLER must
    /// still compare the returned account id with the bearer's and the
    /// returned key generation with the account's current one (equal, not
    /// merely not-lower): that comparison is what makes a grant single-use.
    ///
    /// # Errors
    ///
    /// See [`Self::verify`].
    pub fn verify_reset_grant(
        &self,
        token: &str,
        now_ms: i64,
    ) -> Result<VerifiedToken, TokenError> {
        Self::verify_with(&self.reset_grant_key, RESET_GRANT_VERSION, token, now_ms)
    }

    fn issue_with(
        key: &[u8],
        version: &str,
        account_id: &[u8],
        key_generation: u64,
        expires_at_ms: i64,
    ) -> Result<String, TokenError> {
        if account_id.len() != ACCOUNT_ID_LEN {
            return Err(TokenError::WrongAccountIdLength {
                got: account_id.len(),
                expected: ACCOUNT_ID_LEN,
            });
        }
        let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);
        // Canonical message: `<version>.<account_id_b64>.<generation>.<expires_at_ms>`.
        // Concatenating as a single string lets the verifier recompute the
        // same bytes without re-allocating around separator handling.
        let message = format!("{version}.{account_id_b64}.{key_generation}.{expires_at_ms}");
        let tag = compute_tag(key, message.as_bytes());
        let tag_b64 = URL_SAFE_NO_PAD.encode(tag);
        Ok(format!("{message}.{tag_b64}"))
    }

    fn verify_with(
        key: &[u8],
        expected_version: &str,
        token: &str,
        now_ms: i64,
    ) -> Result<VerifiedToken, TokenError> {
        // Split into exactly five segments. `splitn` would collapse
        // trailing dots into the last segment; for a strict format we
        // count segments and reject extras.
        let segments: Vec<&str> = token.split('.').collect();
        if segments.len() != SEGMENTS {
            return Err(TokenError::Malformed(format!(
                "expected {SEGMENTS} dot-segments"
            )));
        }
        let version = segments[0];
        let account_id_b64 = segments[1];
        let generation_str = segments[2];
        let expires_at_str = segments[3];
        let tag_b64 = segments[4];

        if version != expected_version {
            return Err(TokenError::UnsupportedVersion(version.to_string()));
        }

        let account_id_bytes = URL_SAFE_NO_PAD
            .decode(account_id_b64)
            .map_err(|e| TokenError::Malformed(format!("account_id base64: {e}")))?;
        let account_id: [u8; ACCOUNT_ID_LEN] =
            account_id_bytes.as_slice().try_into().map_err(|_| {
                TokenError::Malformed(format!(
                    "account_id length {} != {}",
                    account_id_bytes.len(),
                    ACCOUNT_ID_LEN
                ))
            })?;

        let key_generation: u64 = generation_str
            .parse()
            .map_err(|e| TokenError::Malformed(format!("key_generation parse: {e}")))?;

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
        // collapse into one observable result on the wire. The message is
        // rebuilt from the PARSED integers, so a non-canonical spelling of a
        // value ("007") is the same credential as its canonical one, never a
        // different credential.
        let message = format!("{version}.{account_id_b64}.{key_generation}.{expires_at_ms}");
        let expected = compute_tag(key, message.as_bytes());
        if expected.ct_eq(&tag).unwrap_u8() != 1 {
            return Err(TokenError::BadSignature);
        }

        if expires_at_ms <= now_ms {
            return Err(TokenError::Expired {
                expires_at_ms,
                now_ms,
            });
        }
        Ok(VerifiedToken {
            account_id,
            key_generation,
        })
    }
}

/// Compute the 32-byte HMAC-SHA256 tag over `message` under `key`.
fn compute_tag(key: &[u8], message: &[u8]) -> [u8; TAG_LEN] {
    // HMAC accepts any key size, so `new_from_slice` cannot fail; the
    // fallback keeps the function total without an `expect`.
    let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(key) else {
        return [0u8; TAG_LEN];
    };
    mac.update(message);
    let out = mac.finalize().into_bytes();
    let mut tag = [0u8; TAG_LEN];
    tag.copy_from_slice(&out);
    tag
}

impl std::fmt::Debug for TokenSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately opaque — accidental `tracing::debug!(?secret, …)`
        // must not leak the bytes of either key.
        f.debug_struct("TokenSecret")
            .field("len", &self.bytes.len())
            .field("refresh_key_len", &self.refresh_key.len())
            .field("reset_grant_key_len", &self.reset_grant_key.len())
            .finish()
    }
}

/// Error vocabulary for [`TokenSecret::issue`] / [`TokenSecret::verify`]
/// / [`TokenSecret::from_base64`] and their refresh twins.
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
    /// Version segment did not match the kind being verified
    /// ([`VERSION`] for an access token, [`REFRESH_VERSION`] for a refresh
    /// credential, [`RESET_GRANT_VERSION`] for a key-reset grant).
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

/// Fuzz-only entry point. Marked `#[doc(hidden)]` — the sole harness
/// surface for `crates/lakko-auth-token/fuzz/`, **not** part of the
/// supported public API.
#[doc(hidden)]
pub mod fuzz_api {
    use super::{TokenSecret, MIN_SECRET_LEN};

    /// Drive [`TokenSecret::verify`], [`TokenSecret::verify_refresh`] AND
    /// [`TokenSecret::verify_reset_grant`] on an attacker-controlled
    /// credential string.
    ///
    /// A client or peer presents an access token to a service on every
    /// bearer request, a refresh credential to auth's refresh route, and a
    /// key-reset grant in the body of auth's key-reset routes, so the bytes
    /// are fully untrusted. The harness verifies against a FIXED
    /// 32-byte secret (the fuzzer cannot forge a valid HMAC, so it exercises
    /// the parse + verify path: dot-split, version gate, base64url account_id
    /// decode with a 16-byte length check, `u64` generation parse, `i64`
    /// expiry parse, base64url tag decode with a 32-byte length check, HMAC
    /// compute, constant-time compare, and the expiry check) at a FIXED
    /// `now_ms`, once per kind. A UTF-8 gate at the harness edge keeps
    /// coverage on the token parser rather than the UTF-8 boundary (`verify`
    /// takes `&str`; only valid UTF-8 reaches it in production — tokens ride
    /// in HTTP headers and JSON bodies). Any panic / overflow here is a
    /// vulnerability. Returns `true` when any kind accepted the input.
    #[must_use]
    pub fn verify_token(bytes: &[u8]) -> bool {
        let Ok(token) = core::str::from_utf8(bytes) else {
            return false;
        };
        let Ok(secret) = TokenSecret::new(vec![0x11; MIN_SECRET_LEN]) else {
            return false;
        };
        let access = secret.verify(token, 0).is_ok();
        let refresh = secret.verify_refresh(token, 0).is_ok();
        let grant = secret.verify_reset_grant(token, 0).is_ok();
        access || refresh || grant
    }
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

    const FAR_FUTURE: i64 = 2_000_000_000_000;
    const NOW: i64 = 1_000_000_000_000;

    #[test]
    fn issue_then_verify_round_trip_carries_the_generation() {
        let secret = test_secret();
        let account_id = sample_account_id();
        let token = secret.issue(&account_id, 7, FAR_FUTURE).unwrap();
        let got = secret.verify(&token, NOW).unwrap();
        assert_eq!(&got.account_id[..], &account_id[..]);
        assert_eq!(got.key_generation, 7);
    }

    #[test]
    fn refresh_round_trip_carries_the_generation() {
        let secret = test_secret();
        let account_id = sample_account_id();
        let token = secret.issue_refresh(&account_id, 3, FAR_FUTURE).unwrap();
        assert!(token.starts_with("r1."));
        let got = secret.verify_refresh(&token, NOW).unwrap();
        assert_eq!(&got.account_id[..], &account_id[..]);
        assert_eq!(got.key_generation, 3);
    }

    #[test]
    fn token_has_five_dot_segments() {
        let secret = test_secret();
        let token = secret.issue(&sample_account_id(), 1, FAR_FUTURE).unwrap();
        assert_eq!(token.matches('.').count(), 4);
        assert!(token.starts_with("v2."));
    }

    #[test]
    fn access_and_refresh_are_not_interchangeable() {
        let secret = test_secret();
        let id = sample_account_id();
        let access = secret.issue(&id, 1, FAR_FUTURE).unwrap();
        let refresh = secret.issue_refresh(&id, 1, FAR_FUTURE).unwrap();
        // The version gate refuses each at the other verifier.
        assert!(matches!(
            secret.verify(&refresh, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "r1"
        ));
        assert!(matches!(
            secret.verify_refresh(&access, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "v2"
        ));
        // And even with the marker rewritten, the tag was computed under a
        // different key, so it never verifies.
        let relabelled = format!("v2{}", refresh.strip_prefix("r1").unwrap());
        assert!(matches!(
            secret.verify(&relabelled, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
        let relabelled = format!("r1{}", access.strip_prefix("v2").unwrap());
        assert!(matches!(
            secret.verify_refresh(&relabelled, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
    }

    #[test]
    fn v1_tokens_are_refused() {
        let secret = test_secret();
        // The retired four-part shape, with a tag that would have been valid
        // under v1: the segment count alone refuses it.
        let id_b64 = URL_SAFE_NO_PAD.encode(sample_account_id());
        let message = format!("v1.{id_b64}.{FAR_FUTURE}");
        let tag = URL_SAFE_NO_PAD.encode(compute_tag(&[0u8; 32], message.as_bytes()));
        let v1 = format!("{message}.{tag}");
        assert!(matches!(
            secret.verify(&v1, NOW).unwrap_err(),
            TokenError::Malformed(_)
        ));
        // A five-part string labelled v1 is refused by the version gate.
        let five = format!("v1.{id_b64}.1.{FAR_FUTURE}.{tag}");
        assert!(matches!(
            secret.verify(&five, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "v1"
        ));
    }

    #[test]
    fn verify_rejects_expired_token() {
        let secret = test_secret();
        let token = secret.issue(&sample_account_id(), 1, 1_000).unwrap();
        assert!(matches!(
            secret.verify(&token, 2_000).unwrap_err(),
            TokenError::Expired { .. }
        ));
        let refresh = secret
            .issue_refresh(&sample_account_id(), 1, 1_000)
            .unwrap();
        assert!(matches!(
            secret.verify_refresh(&refresh, 1_000).unwrap_err(),
            TokenError::Expired { .. }
        ));
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let issuer = test_secret();
        let verifier = TokenSecret::new(vec![1u8; MIN_SECRET_LEN]).unwrap();
        let token = issuer.issue(&sample_account_id(), 1, FAR_FUTURE).unwrap();
        assert!(matches!(
            verifier.verify(&token, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
        let refresh = issuer
            .issue_refresh(&sample_account_id(), 1, FAR_FUTURE)
            .unwrap();
        assert!(matches!(
            verifier.verify_refresh(&refresh, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
    }

    #[test]
    fn verify_rejects_tampered_account_id_and_generation() {
        let secret = test_secret();
        let token = secret.issue(&sample_account_id(), 1, FAR_FUTURE).unwrap();
        let mut parts: Vec<&str> = token.split('.').collect();
        let evil_id = URL_SAFE_NO_PAD.encode([0u8; ACCOUNT_ID_LEN]);
        parts[1] = &evil_id;
        let tampered = parts.join(".");
        assert!(matches!(
            secret.verify(&tampered, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
        // Bumping the generation (the thing the reset revokes by) must not
        // survive the tag either.
        let mut parts: Vec<&str> = token.split('.').collect();
        parts[2] = "2";
        let bumped = parts.join(".");
        assert!(matches!(
            secret.verify(&bumped, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
        // A non-canonical spelling of the SAME generation is re-serialised
        // canonically before the tag is recomputed: it is the same credential,
        // not a way to claim a different generation.
        let mut parts: Vec<&str> = token.split('.').collect();
        parts[2] = "01";
        let padded = parts.join(".");
        assert_eq!(secret.verify(&padded, NOW).unwrap().key_generation, 1);
    }

    #[test]
    fn verify_rejects_unsupported_version() {
        let secret = test_secret();
        let token = secret.issue(&sample_account_id(), 1, FAR_FUTURE).unwrap();
        let evil = format!("v3{}", token.strip_prefix("v2").unwrap());
        match secret.verify(&evil, NOW).unwrap_err() {
            TokenError::UnsupportedVersion(v) => assert_eq!(v, "v3"),
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_malformed_shapes() {
        let secret = test_secret();
        for bad in &[
            "",
            "v2",
            "v2.aaa",
            "v2.aaa.bbb",
            "v2.aaa.bbb.ccc",
            "v2.aaa.bbb.ccc.ddd.eee",              // too many segments
            "v2.!@#.1.1.AAAA",                     // bad base64 in account_id
            "v2.AAAAAAAAAAAAAAAAAAAAAA.x.1.AAAA",  // bad generation
            "v2.AAAAAAAAAAAAAAAAAAAAAA.-1.1.AAAA", // negative generation
            "v2.AAAAAAAAAAAAAAAAAAAAAA.1.not_a_number.AAAA", // bad expiry
            "v2.AAAAAAAAAAAAAAAAAAAAAA.1.1.AAAA",  // short tag
        ] {
            assert!(
                matches!(
                    secret.verify(bad, NOW).unwrap_err(),
                    TokenError::Malformed(_) | TokenError::UnsupportedVersion(_)
                ),
                "expected Malformed for {bad:?}"
            );
            assert!(
                matches!(
                    secret.verify_refresh(bad, NOW).unwrap_err(),
                    TokenError::Malformed(_) | TokenError::UnsupportedVersion(_)
                ),
                "expected Malformed for {bad:?} as a refresh credential"
            );
        }
    }

    #[test]
    fn issue_rejects_wrong_account_id_length() {
        let secret = test_secret();
        match secret.issue(&[0u8; 8], 1, 1_000).unwrap_err() {
            TokenError::WrongAccountIdLength { got, expected } => {
                assert_eq!(got, 8);
                assert_eq!(expected, ACCOUNT_ID_LEN);
            },
            other => panic!("expected WrongAccountIdLength, got {other:?}"),
        }
        assert!(matches!(
            secret.issue_refresh(&[0u8; 8], 1, 1_000).unwrap_err(),
            TokenError::WrongAccountIdLength { .. }
        ));
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
        let token = secret.issue(&account_id, 1, FAR_FUTURE).unwrap();
        let recovered = secret.verify(&token, NOW).unwrap();
        assert_eq!(&recovered.account_id[..], &account_id[..]);
    }

    #[test]
    fn refresh_key_is_the_documented_derivation() {
        let raw = vec![7u8; MIN_SECRET_LEN];
        let secret = TokenSecret::new(raw.clone()).unwrap();
        assert_eq!(
            secret.refresh_key,
            blake3::derive_key("lakko/session-refresh/v1", &raw)
        );
        assert_ne!(&secret.refresh_key[..], &raw[..]);
    }

    #[test]
    fn reset_grant_round_trip_carries_the_account_and_generation() {
        let secret = test_secret();
        let account_id = sample_account_id();
        let grant = secret
            .issue_reset_grant(&account_id, 4, FAR_FUTURE)
            .unwrap();
        assert!(grant.starts_with("k1."));
        assert_eq!(grant.matches('.').count(), 4);
        let got = secret.verify_reset_grant(&grant, NOW).unwrap();
        assert_eq!(&got.account_id[..], &account_id[..]);
        assert_eq!(got.key_generation, 4);
    }

    #[test]
    fn reset_grant_is_not_a_bearer_nor_a_refresh_credential_and_vice_versa() {
        let secret = test_secret();
        let id = sample_account_id();
        let access = secret.issue(&id, 1, FAR_FUTURE).unwrap();
        let refresh = secret.issue_refresh(&id, 1, FAR_FUTURE).unwrap();
        let grant = secret.issue_reset_grant(&id, 1, FAR_FUTURE).unwrap();

        // The version gate refuses a grant at the other two verifiers, and
        // the other two kinds at the grant verifier.
        assert!(matches!(
            secret.verify(&grant, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "k1"
        ));
        assert!(matches!(
            secret.verify_refresh(&grant, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "k1"
        ));
        assert!(matches!(
            secret.verify_reset_grant(&access, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "v2"
        ));
        assert!(matches!(
            secret.verify_reset_grant(&refresh, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "r1"
        ));

        // With the marker rewritten the tag is under the wrong key in every
        // one of the six directions.
        let body = |t: &str| t.split_once('.').unwrap().1.to_string();
        assert!(matches!(
            secret
                .verify(&format!("v2.{}", body(&grant)), NOW)
                .unwrap_err(),
            TokenError::BadSignature
        ));
        assert!(matches!(
            secret
                .verify_refresh(&format!("r1.{}", body(&grant)), NOW)
                .unwrap_err(),
            TokenError::BadSignature
        ));
        assert!(matches!(
            secret
                .verify_reset_grant(&format!("k1.{}", body(&access)), NOW)
                .unwrap_err(),
            TokenError::BadSignature
        ));
        assert!(matches!(
            secret
                .verify_reset_grant(&format!("k1.{}", body(&refresh)), NOW)
                .unwrap_err(),
            TokenError::BadSignature
        ));
    }

    #[test]
    fn reset_grant_refusals() {
        let secret = test_secret();
        let id = sample_account_id();

        // Expired (the boundary instant is already expired).
        let grant = secret.issue_reset_grant(&id, 1, 1_000).unwrap();
        assert!(matches!(
            secret.verify_reset_grant(&grant, 1_000).unwrap_err(),
            TokenError::Expired { .. }
        ));
        assert!(secret.verify_reset_grant(&grant, 999).is_ok());

        // Bad tag: another secret, a flipped tag, a rewritten account, a
        // rewritten generation, a rewritten expiry.
        let grant = secret.issue_reset_grant(&id, 1, FAR_FUTURE).unwrap();
        let other = TokenSecret::new(vec![1u8; MIN_SECRET_LEN]).unwrap();
        assert!(matches!(
            other.verify_reset_grant(&grant, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
        let parts: Vec<&str> = grant.split('.').collect();
        let evil_id = URL_SAFE_NO_PAD.encode([0xEEu8; ACCOUNT_ID_LEN]);
        let zero_tag = URL_SAFE_NO_PAD.encode([0u8; TAG_LEN]);
        let far = (FAR_FUTURE + 1).to_string();
        for (index, value) in [
            (1, evil_id.as_str()),
            (2, "2"),
            (3, far.as_str()),
            (4, zero_tag.as_str()),
        ] {
            let mut tampered = parts.clone();
            tampered[index] = value;
            assert!(
                matches!(
                    secret
                        .verify_reset_grant(&tampered.join("."), NOW)
                        .unwrap_err(),
                    TokenError::BadSignature
                ),
                "segment {index} rewritten"
            );
        }

        // Wrong version prefix and malformed shapes.
        let k2 = format!("k2{}", grant.strip_prefix("k1").unwrap());
        assert!(matches!(
            secret.verify_reset_grant(&k2, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "k2"
        ));
        for bad in [
            "",
            "k1",
            "k1.aaa.1.1",
            "k1.aaa.1.1.AAAA.extra",
            "k1.!@#.1.1.AAAA",
            "k1.AAAAAAAAAAAAAAAAAAAAAA.x.1.AAAA",
            "k1.AAAAAAAAAAAAAAAAAAAAAA.1.x.AAAA",
            "k1.AAAAAAAAAAAAAAAAAAAAAA.1.1.AAAA",
        ] {
            assert!(
                matches!(
                    secret.verify_reset_grant(bad, NOW).unwrap_err(),
                    TokenError::Malformed(_)
                ),
                "expected Malformed for {bad:?}"
            );
        }

        // The verifier reports the account and the generation; refusing a
        // grant for another account or a stale generation is the caller's
        // comparison, and these are the values it compares.
        let got = secret.verify_reset_grant(&grant, NOW).unwrap();
        assert_ne!(got.account_id, [0xEEu8; ACCOUNT_ID_LEN]);
        assert_eq!(got.key_generation, 1);
        assert_ne!(got.key_generation, 2, "a reset moves the row to 2");

        assert!(matches!(
            secret.issue_reset_grant(&[0u8; 8], 1, 1_000).unwrap_err(),
            TokenError::WrongAccountIdLength { .. }
        ));
    }

    #[test]
    fn reset_grant_key_is_the_documented_derivation() {
        let raw = vec![7u8; MIN_SECRET_LEN];
        let secret = TokenSecret::new(raw.clone()).unwrap();
        assert_eq!(
            secret.reset_grant_key,
            blake3::derive_key("lakko/key-reset-grant/v1", &raw)
        );
        assert_ne!(secret.reset_grant_key, secret.refresh_key);
        assert_ne!(&secret.reset_grant_key[..], &raw[..]);
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
