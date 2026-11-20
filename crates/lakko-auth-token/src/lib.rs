// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HMAC-SHA256 session-token issuance + verification: the contract between
//! `lakko-svc-auth` (issues) and every service that verifies a bearer
//! (`auth` itself, `discovery`, `storage`, `gif-proxy`).
//!
//! # Wire format
//!
//! Four credentials, dotted strings ([ADR-0073] section 3.1 and its
//! 2026-10-04 amendment; [ADR-0074] section 0.4 for the device and the issue
//! time; [ADR-0098] section 4 for the restricted session):
//!
//! ```text
//! access      = v3.<account_id>.<key_generation>.<device_id>.<issued_at_ms>.<expires_at_ms>.<tag>
//! refresh     = r2.<account_id>.<key_generation>.<device_id>.<issued_at_ms>.<expires_at_ms>.<tag>
//! restricted  = s1.<account_id>.<key_generation>.<device_id>.<issued_at_ms>.<expires_at_ms>.<tag>
//! fresh grant = k1.<account_id>.<key_generation>.<expires_at_ms>.<tag>
//! ```
//!
//! Where:
//!
//! - `v3` / `r2` / `s1` / `k1` is the version marker, which also tells the kinds
//!   apart. `v1`, `v2` and `r1` are refused: there are no users and no tokens
//!   to keep valid, so each was retired on a flag day.
//! - `account_id` is base64url-no-pad of the raw 16-byte UUIDv7 account id
//!   (RFC 4648 section 5).
//! - `key_generation` is the account's key generation the credential was
//!   issued under (`accounts.identity_version` in auth; `1` is genesis),
//!   as an ASCII decimal integer.
//! - `device_id` is base64url-no-pad of 32 bytes: the device's conversation
//!   leaf signature key, which is the public device id of discovery's
//!   directory. It is asserted by the client when the session lands and is
//!   not proven. All zero ([`UNBOUND_DEVICE_ID`]) means "no device identity
//!   yet".
//! - `issued_at_ms` is the unix-millisecond instant auth issued the
//!   credential, as an ASCII decimal integer. Auth compares it with the
//!   device's session floor (a logout or a removal) to revoke one device.
//! - `expires_at_ms` is the unix-millisecond expiry, as an ASCII decimal
//!   integer.
//! - `tag` is base64url-no-pad of the 32-byte HMAC-SHA256 over the earlier
//!   segments joined by dots (without the trailing dot or the tag).
//!
//! The access tag is computed under the shared secret itself; the refresh tag
//! under a key derived from it, `BLAKE3-derive-key("lakko/session-refresh/v1",
//! secret)`. So an access verifier's [`TokenSecret::verify`] refuses a refresh
//! credential by its version marker, and even a verifier that dropped the
//! version check could never mistake one tag for the other. The fresh-grant
//! tag is computed under a third key, `BLAKE3-derive-key(
//! "lakko/fresh-auth-grant/v1", secret)`, for the same reason: a grant is
//! neither a bearer nor a refresh credential, and neither of those is a grant.
//! A grant names no device: it is proof about the person, not the device.
//! The restricted-session tag is computed under a fourth key,
//! `BLAKE3-derive-key("lakko/suspended-session/v1", secret)`: a restricted
//! session is what a suspended account holds instead of an access token, and
//! auth accepts it on four routes only (read the suspension, send an appeal,
//! delete the account, sign out). No other verifier accepts it.
//!
//! # Why a custom format and not JWT
//!
//! JWT carries header + payload + signature as three base64url segments. For
//! this use (single algorithm, single audience, no key rotation, a handful of
//! fields in the payload) the header is noise and the JSON parse is one more
//! place for a parser-differential bug to land. This format reads in a few
//! dozen lines and has no JSON.
//!
//! # Security posture
//!
//! - Tag verification uses [`subtle::ConstantTimeEq`] for the final compare.
//! - The HMAC secret loads from `LAKKO_SESSION_TOKEN_SECRET` (base64, 32+
//!   bytes); every issuer and verifier MUST hold the same value. Mismatched
//!   secrets surface as a "verify failed" 401, never as a silent accept.
//! - An access token is **stateless** at every verifier but auth: it carries
//!   the generation, the device and the issue time, and auth refuses one
//!   whose generation is below the account's current one, or whose issue time
//!   is at or below its device's session floor, on every bearer request. The
//!   other verifiers keep the stateless check, so their revocation latency is
//!   the access TTL (`LAKKO_SESSION_TOKEN_TTL_SECS`, 15 minutes by default).
//! - A refresh credential is accepted by auth's `POST /auth/session/refresh`
//!   only, which reads the account's current generation and the device's
//!   floor. It carries no random id and has no database row: it is revoked by
//!   the generation (every device) or by the device floor (one device).
//! - A **fresh-auth grant** is proof that the person proved who they are
//!   moments ago, with the password or a passkey. It is short-lived
//!   (`LAKKO_FRESH_GRANT_TTL_SECS`, 10 minutes by default) and stateless. It
//!   names the key generation it was issued under and a gated route accepts
//!   it only while the account is at exactly that generation. It grants
//!   nothing on its own (every gated route still needs the bearer).
//! - No credential carries plaintext PII: the account id (already known
//!   to every verifier), a public device id and integers. A leaked access
//!   token grants its holder the bearer routes for the embedded account until
//!   it expires; the holder cannot decrypt messages, impersonate signatures,
//!   or read private inbox bytes.
//!
//! [ADR-0073]: ../../../docs/adr/0073-account-key-reset.md
//! [ADR-0074]: ../../../docs/adr/0074-account-takeover-resistance.md
//! [ADR-0098]: ../../../docs/adr/0098-reporting-and-moderation.md

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Length of a raw account_id in bytes (UUIDv7 — 128 bits).
pub const ACCOUNT_ID_LEN: usize = 16;

/// Length of a device id in bytes: the device's conversation leaf signature
/// key (Ed25519), the id discovery's device directory is keyed on.
pub const DEVICE_ID_LEN: usize = 32;

/// The device id of a session that names no device yet (a sign-in before the
/// device has keys). Such a session cannot publish a KeyPackage and cannot be
/// revoked by device; auth's `POST /auth/session/bind-device` replaces it.
pub const UNBOUND_DEVICE_ID: [u8; DEVICE_ID_LEN] = [0u8; DEVICE_ID_LEN];

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
/// (so a `v2` token, or a refresh credential, is refused there).
pub const VERSION: &str = "v3";

/// Version marker of a refresh credential. [`TokenSecret::verify_refresh`]
/// refuses any other marker, so an access token presented as a refresh
/// credential fails with [`TokenError::UnsupportedVersion`].
pub const REFRESH_VERSION: &str = "r2";

/// BLAKE3 derive-key context of the refresh key: `refresh_key =
/// BLAKE3-derive-key(REFRESH_KEY_DOMAIN, secret)`.
pub const REFRESH_KEY_DOMAIN: &str = "lakko/session-refresh/v1";

/// Version marker of a fresh-auth grant. [`TokenSecret::verify_fresh_grant`]
/// refuses any other marker, so an access token or a refresh credential
/// presented as a grant fails with [`TokenError::UnsupportedVersion`].
pub const FRESH_GRANT_VERSION: &str = "k1";

/// BLAKE3 derive-key context of the fresh-grant key: `fresh_grant_key =
/// BLAKE3-derive-key(FRESH_GRANT_KEY_DOMAIN, secret)`.
pub const FRESH_GRANT_KEY_DOMAIN: &str = "lakko/fresh-auth-grant/v1";

/// Version marker of a restricted session (a suspended account's credential,
/// ADR-0098 section 4). [`TokenSecret::verify_restricted`] refuses any other
/// marker, and [`TokenSecret::verify`] / [`TokenSecret::verify_refresh`]
/// refuse this one.
pub const RESTRICTED_VERSION: &str = "s1";

/// BLAKE3 derive-key context of the restricted-session key:
/// `restricted_key = BLAKE3-derive-key(RESTRICTED_KEY_DOMAIN, secret)`.
pub const RESTRICTED_KEY_DOMAIN: &str = "lakko/suspended-session/v1";

/// Number of dot-separated segments in an access token, a refresh credential
/// and a restricted session.
const SESSION_SEGMENTS: usize = 7;

/// Number of dot-separated segments in a fresh-auth grant.
const GRANT_SEGMENTS: usize = 5;

/// What a verified access token or refresh credential says: which account,
/// under which key generation, for which device, and when it was issued. The
/// verifier of a bearer request compares the generation with the account's
/// current one and the issue time with the device's floor where it can
/// (auth); the other services accept any within the expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedToken {
    /// The raw 16-byte account id the credential was issued for.
    pub account_id: [u8; ACCOUNT_ID_LEN],
    /// The key generation the credential was issued under.
    pub key_generation: u64,
    /// The device the credential names ([`UNBOUND_DEVICE_ID`] for none).
    pub device_id: [u8; DEVICE_ID_LEN],
    /// The unix-millisecond instant the credential was issued.
    pub issued_at_ms: i64,
}

impl VerifiedToken {
    /// `true` when the credential names a device (its id is not
    /// [`UNBOUND_DEVICE_ID`]).
    #[must_use]
    pub fn is_device_bound(&self) -> bool {
        self.device_id != UNBOUND_DEVICE_ID
    }
}

/// What a verified fresh-auth grant says: which account, and under which key
/// generation it was issued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedGrant {
    /// The raw 16-byte account id the grant was issued for.
    pub account_id: [u8; ACCOUNT_ID_LEN],
    /// The key generation the grant was issued under.
    pub key_generation: u64,
}

/// Shared HMAC secret. Loaded once at service startup from the
/// `LAKKO_SESSION_TOKEN_SECRET` environment variable (base64-encoded
/// bytes), then cloned cheaply into per-request state.
///
/// Wraps the raw secret bytes plus the derived refresh and fresh-grant keys;
/// do NOT debug-print this type — the `Debug` impl is deliberately opaque so
/// accidental log lines don't leak any of them.
#[derive(Clone)]
pub struct TokenSecret {
    bytes: Vec<u8>,
    refresh_key: [u8; 32],
    fresh_grant_key: [u8; 32],
    restricted_key: [u8; 32],
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
        let fresh_grant_key = blake3::derive_key(FRESH_GRANT_KEY_DOMAIN, &bytes);
        let restricted_key = blake3::derive_key(RESTRICTED_KEY_DOMAIN, &bytes);
        Ok(Self {
            bytes,
            refresh_key,
            fresh_grant_key,
            restricted_key,
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

    /// Issue an access token for the given account at `key_generation`,
    /// naming `device_id` and stamped `issued_at_ms`.
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
        device_id: &[u8; DEVICE_ID_LEN],
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<String, TokenError> {
        Self::issue_session(
            &self.bytes,
            VERSION,
            account_id,
            key_generation,
            device_id,
            issued_at_ms,
            expires_at_ms,
        )
    }

    /// Issue a refresh credential for the given account at `key_generation`,
    /// naming `device_id` and stamped `issued_at_ms`, tagged under the
    /// derived refresh key. Only auth's refresh route accepts it
    /// ([`Self::verify_refresh`]).
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::WrongAccountIdLength`] if `account_id`
    /// is not exactly [`ACCOUNT_ID_LEN`] bytes.
    pub fn issue_refresh(
        &self,
        account_id: &[u8],
        key_generation: u64,
        device_id: &[u8; DEVICE_ID_LEN],
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<String, TokenError> {
        Self::issue_session(
            &self.refresh_key,
            REFRESH_VERSION,
            account_id,
            key_generation,
            device_id,
            issued_at_ms,
            expires_at_ms,
        )
    }

    /// Issue a restricted session for a suspended account at
    /// `key_generation`, naming `device_id` and stamped `issued_at_ms`, tagged
    /// under the derived restricted key. Only auth's restricted-capable routes
    /// accept it ([`Self::verify_restricted`]); its lifetime is the access
    /// token's.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::WrongAccountIdLength`] if `account_id`
    /// is not exactly [`ACCOUNT_ID_LEN`] bytes.
    pub fn issue_restricted(
        &self,
        account_id: &[u8],
        key_generation: u64,
        device_id: &[u8; DEVICE_ID_LEN],
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<String, TokenError> {
        Self::issue_session(
            &self.restricted_key,
            RESTRICTED_VERSION,
            account_id,
            key_generation,
            device_id,
            issued_at_ms,
            expires_at_ms,
        )
    }

    /// Issue a fresh-auth grant for the given account at `key_generation`,
    /// tagged under the derived fresh-grant key. Auth mints one only in the
    /// answer of a successful password or passkey login; only its
    /// fresh-auth-gated routes accept it ([`Self::verify_fresh_grant`]).
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::WrongAccountIdLength`] if `account_id`
    /// is not exactly [`ACCOUNT_ID_LEN`] bytes.
    pub fn issue_fresh_grant(
        &self,
        account_id: &[u8],
        key_generation: u64,
        expires_at_ms: i64,
    ) -> Result<String, TokenError> {
        check_account_id(account_id)?;
        let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);
        let message =
            format!("{FRESH_GRANT_VERSION}.{account_id_b64}.{key_generation}.{expires_at_ms}");
        let tag = compute_tag(&self.fresh_grant_key, message.as_bytes());
        Ok(format!("{message}.{}", URL_SAFE_NO_PAD.encode(tag)))
    }

    /// Verify an access token at the given wall-clock instant.
    ///
    /// Returns the embedded account id, key generation, device id and issue
    /// time on success. Constant-time tag compare; the only timing leak is
    /// the parse step (which is on every code path regardless of validity).
    ///
    /// # Errors
    ///
    /// Maps every failure mode to a [`TokenError`] variant the
    /// caller can branch on:
    ///
    /// - [`TokenError::Malformed`] — wrong number of dot-segments,
    ///   wrong base64, non-integer generation, issue time or expiry, an
    ///   account id or device id of the wrong length after decode.
    /// - [`TokenError::UnsupportedVersion`] — segment 0 is not
    ///   [`VERSION`] (a `v2` token or a refresh credential).
    /// - [`TokenError::Expired`] — `expires_at_ms <= now_ms`.
    /// - [`TokenError::BadSignature`] — HMAC mismatch.
    pub fn verify(&self, token: &str, now_ms: i64) -> Result<VerifiedToken, TokenError> {
        Self::verify_session(&self.bytes, VERSION, token, now_ms)
    }

    /// Verify a refresh credential at the given wall-clock instant, under the
    /// derived refresh key. Same error vocabulary as [`Self::verify`]; an
    /// access token presented here is [`TokenError::UnsupportedVersion`].
    ///
    /// # Errors
    ///
    /// See [`Self::verify`].
    pub fn verify_refresh(&self, token: &str, now_ms: i64) -> Result<VerifiedToken, TokenError> {
        Self::verify_session(&self.refresh_key, REFRESH_VERSION, token, now_ms)
    }

    /// Verify a restricted session (a suspended account's credential) at the
    /// given wall-clock instant, under the derived restricted key. Same error
    /// vocabulary as [`Self::verify`]; an access token or a refresh credential
    /// presented here is [`TokenError::UnsupportedVersion`], and a fresh-auth
    /// grant is [`TokenError::Malformed`] (its segment count differs).
    ///
    /// # Errors
    ///
    /// See [`Self::verify`].
    pub fn verify_restricted(&self, token: &str, now_ms: i64) -> Result<VerifiedToken, TokenError> {
        Self::verify_session(&self.restricted_key, RESTRICTED_VERSION, token, now_ms)
    }

    /// Verify a fresh-auth grant at the given wall-clock instant, under the
    /// derived fresh-grant key. Same error vocabulary as [`Self::verify`]; an
    /// access token or a refresh credential presented here is
    /// [`TokenError::Malformed`] (its segment count differs) and a five-part
    /// string with another marker is [`TokenError::UnsupportedVersion`].
    ///
    /// This checks the grant itself (shape, tag, expiry). The CALLER must
    /// still compare the returned account id with the bearer's and the
    /// returned key generation with the account's current one (equal, not
    /// merely not-lower): that comparison is what makes a key reset spend
    /// the grant, and what refuses a grant from before a reset.
    ///
    /// # Errors
    ///
    /// See [`Self::verify`].
    pub fn verify_fresh_grant(
        &self,
        token: &str,
        now_ms: i64,
    ) -> Result<VerifiedGrant, TokenError> {
        let segments: Vec<&str> = token.split('.').collect();
        if segments.len() != GRANT_SEGMENTS {
            return Err(TokenError::Malformed(format!(
                "expected {GRANT_SEGMENTS} dot-segments"
            )));
        }
        if segments[0] != FRESH_GRANT_VERSION {
            return Err(TokenError::UnsupportedVersion(segments[0].to_string()));
        }
        let account_id = decode_account_id(segments[1])?;
        let key_generation = parse_u64(segments[2], "key_generation")?;
        let expires_at_ms = parse_i64(segments[3], "expires_at")?;
        let tag = decode_tag(segments[4])?;

        // The message is rebuilt from the PARSED integers, so a non-canonical
        // spelling of a value ("007") is the same credential as its canonical
        // one, never a different credential. The tag is checked before the
        // expiry so the two failures are one observable result.
        let message = format!(
            "{FRESH_GRANT_VERSION}.{}.{key_generation}.{expires_at_ms}",
            segments[1]
        );
        let expected = compute_tag(&self.fresh_grant_key, message.as_bytes());
        if expected.ct_eq(&tag).unwrap_u8() != 1 {
            return Err(TokenError::BadSignature);
        }
        if expires_at_ms <= now_ms {
            return Err(TokenError::Expired {
                expires_at_ms,
                now_ms,
            });
        }
        Ok(VerifiedGrant {
            account_id,
            key_generation,
        })
    }

    fn issue_session(
        key: &[u8],
        version: &str,
        account_id: &[u8],
        key_generation: u64,
        device_id: &[u8; DEVICE_ID_LEN],
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<String, TokenError> {
        check_account_id(account_id)?;
        let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);
        let device_id_b64 = URL_SAFE_NO_PAD.encode(device_id);
        // Canonical message: every segment but the tag, joined by dots.
        // Concatenating as a single string lets the verifier recompute the
        // same bytes without re-allocating around separator handling.
        let message = format!(
            "{version}.{account_id_b64}.{key_generation}.{device_id_b64}.{issued_at_ms}.{expires_at_ms}"
        );
        let tag = compute_tag(key, message.as_bytes());
        Ok(format!("{message}.{}", URL_SAFE_NO_PAD.encode(tag)))
    }

    fn verify_session(
        key: &[u8],
        expected_version: &str,
        token: &str,
        now_ms: i64,
    ) -> Result<VerifiedToken, TokenError> {
        // Split into exactly seven segments. `splitn` would collapse
        // trailing dots into the last segment; for a strict format we
        // count segments and reject extras.
        let segments: Vec<&str> = token.split('.').collect();
        if segments.len() != SESSION_SEGMENTS {
            return Err(TokenError::Malformed(format!(
                "expected {SESSION_SEGMENTS} dot-segments"
            )));
        }
        let version = segments[0];
        if version != expected_version {
            return Err(TokenError::UnsupportedVersion(version.to_string()));
        }

        let account_id = decode_account_id(segments[1])?;
        let key_generation = parse_u64(segments[2], "key_generation")?;
        let device_id_bytes = URL_SAFE_NO_PAD
            .decode(segments[3])
            .map_err(|e| TokenError::Malformed(format!("device_id base64: {e}")))?;
        let device_id: [u8; DEVICE_ID_LEN] =
            device_id_bytes.as_slice().try_into().map_err(|_| {
                TokenError::Malformed(format!(
                    "device_id length {} != {}",
                    device_id_bytes.len(),
                    DEVICE_ID_LEN
                ))
            })?;
        let issued_at_ms = parse_i64(segments[4], "issued_at")?;
        let expires_at_ms = parse_i64(segments[5], "expires_at")?;
        let tag = decode_tag(segments[6])?;

        // Verify the HMAC BEFORE checking expiry — otherwise an
        // attacker can distinguish "valid signature + expired" vs
        // "invalid signature" via timing. The two failure paths
        // collapse into one observable result on the wire. The message is
        // rebuilt from the PARSED integers, so a non-canonical spelling of a
        // value ("007") is the same credential as its canonical one, never a
        // different credential.
        let message = format!(
            "{version}.{}.{key_generation}.{}.{issued_at_ms}.{expires_at_ms}",
            segments[1], segments[3]
        );
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
            device_id,
            issued_at_ms,
        })
    }
}

fn check_account_id(account_id: &[u8]) -> Result<(), TokenError> {
    if account_id.len() == ACCOUNT_ID_LEN {
        Ok(())
    } else {
        Err(TokenError::WrongAccountIdLength {
            got: account_id.len(),
            expected: ACCOUNT_ID_LEN,
        })
    }
}

fn decode_account_id(segment: &str) -> Result<[u8; ACCOUNT_ID_LEN], TokenError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|e| TokenError::Malformed(format!("account_id base64: {e}")))?;
    bytes.as_slice().try_into().map_err(|_| {
        TokenError::Malformed(format!(
            "account_id length {} != {}",
            bytes.len(),
            ACCOUNT_ID_LEN
        ))
    })
}

fn decode_tag(segment: &str) -> Result<Vec<u8>, TokenError> {
    let tag = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|e| TokenError::Malformed(format!("tag base64: {e}")))?;
    if tag.len() != TAG_LEN {
        return Err(TokenError::Malformed(format!(
            "tag length {} != {}",
            tag.len(),
            TAG_LEN
        )));
    }
    Ok(tag)
}

fn parse_u64(segment: &str, what: &str) -> Result<u64, TokenError> {
    segment
        .parse()
        .map_err(|e| TokenError::Malformed(format!("{what} parse: {e}")))
}

fn parse_i64(segment: &str, what: &str) -> Result<i64, TokenError> {
    segment
        .parse()
        .map_err(|e| TokenError::Malformed(format!("{what} parse: {e}")))
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
            .field("fresh_grant_key_len", &self.fresh_grant_key.len())
            .field("restricted_key_len", &self.restricted_key.len())
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
    /// credential, [`RESTRICTED_VERSION`] for a restricted session,
    /// [`FRESH_GRANT_VERSION`] for a fresh-auth grant).
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

    /// Drive [`TokenSecret::verify`], [`TokenSecret::verify_refresh`],
    /// [`TokenSecret::verify_fresh_grant`] AND
    /// [`TokenSecret::verify_restricted`] on an attacker-controlled
    /// credential string.
    ///
    /// A client or peer presents an access token to a service on every
    /// bearer request, a refresh credential to auth's refresh route, and a
    /// fresh-auth grant to auth's fresh-auth-gated routes (a JSON body field
    /// or the `x-lakko-fresh-grant` header), so the bytes are fully untrusted. The harness verifies against a FIXED
    /// 32-byte secret (the fuzzer cannot forge a valid HMAC, so it exercises
    /// the parse + verify path: dot-split, version gate, base64url account_id
    /// decode with a 16-byte length check, `u64` generation parse, base64url
    /// device_id decode with a 32-byte length check, `i64` issue-time and
    /// expiry parses, base64url tag decode with a 32-byte length check, HMAC
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
        let grant = secret.verify_fresh_grant(token, 0).is_ok();
        let restricted = secret.verify_restricted(token, 0).is_ok();
        access || refresh || grant || restricted
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

    const DEVICE: [u8; DEVICE_ID_LEN] = [0xD1; DEVICE_ID_LEN];
    const ISSUED: i64 = 999_999_000_000;
    const FAR_FUTURE: i64 = 2_000_000_000_000;
    const NOW: i64 = 1_000_000_000_000;

    fn access(secret: &TokenSecret, generation: u64) -> String {
        secret
            .issue(
                &sample_account_id(),
                generation,
                &DEVICE,
                ISSUED,
                FAR_FUTURE,
            )
            .unwrap()
    }

    fn refresh(secret: &TokenSecret, generation: u64) -> String {
        secret
            .issue_refresh(
                &sample_account_id(),
                generation,
                &DEVICE,
                ISSUED,
                FAR_FUTURE,
            )
            .unwrap()
    }

    #[test]
    fn issue_then_verify_round_trip_carries_every_field() {
        let secret = test_secret();
        let token = access(&secret, 7);
        assert!(token.starts_with("v3."));
        assert_eq!(token.matches('.').count(), 6);
        let got = secret.verify(&token, NOW).unwrap();
        assert_eq!(&got.account_id[..], &sample_account_id()[..]);
        assert_eq!(got.key_generation, 7);
        assert_eq!(got.device_id, DEVICE);
        assert_eq!(got.issued_at_ms, ISSUED);
        assert!(got.is_device_bound());
    }

    #[test]
    fn refresh_round_trip_carries_every_field() {
        let secret = test_secret();
        let token = refresh(&secret, 3);
        assert!(token.starts_with("r2."));
        assert_eq!(token.matches('.').count(), 6);
        let got = secret.verify_refresh(&token, NOW).unwrap();
        assert_eq!(&got.account_id[..], &sample_account_id()[..]);
        assert_eq!(got.key_generation, 3);
        assert_eq!(got.device_id, DEVICE);
        assert_eq!(got.issued_at_ms, ISSUED);
    }

    #[test]
    fn an_unbound_session_names_the_all_zero_device() {
        let secret = test_secret();
        let token = secret
            .issue(
                &sample_account_id(),
                1,
                &UNBOUND_DEVICE_ID,
                ISSUED,
                FAR_FUTURE,
            )
            .unwrap();
        let got = secret.verify(&token, NOW).unwrap();
        assert_eq!(got.device_id, UNBOUND_DEVICE_ID);
        assert!(!got.is_device_bound());
    }

    #[test]
    fn access_and_refresh_are_not_interchangeable() {
        let secret = test_secret();
        let access = access(&secret, 1);
        let refresh = refresh(&secret, 1);
        // The version gate refuses each at the other verifier.
        assert!(matches!(
            secret.verify(&refresh, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "r2"
        ));
        assert!(matches!(
            secret.verify_refresh(&access, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "v3"
        ));
        // And even with the marker rewritten, the tag was computed under a
        // different key, so it never verifies.
        let relabelled = format!("v3{}", refresh.strip_prefix("r2").unwrap());
        assert!(matches!(
            secret.verify(&relabelled, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
        let relabelled = format!("r2{}", access.strip_prefix("v3").unwrap());
        assert!(matches!(
            secret.verify_refresh(&relabelled, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
    }

    #[test]
    fn retired_versions_are_refused() {
        let secret = test_secret();
        let id_b64 = URL_SAFE_NO_PAD.encode(sample_account_id());

        // The retired five-part `v2` / `r1` shape, with tags that were valid
        // under it: the segment count alone refuses them.
        let message = format!("v2.{id_b64}.1.{FAR_FUTURE}");
        let tag = URL_SAFE_NO_PAD.encode(compute_tag(&[0u8; 32], message.as_bytes()));
        let v2 = format!("{message}.{tag}");
        assert!(matches!(
            secret.verify(&v2, NOW).unwrap_err(),
            TokenError::Malformed(_)
        ));
        let message = format!("r1.{id_b64}.1.{FAR_FUTURE}");
        let tag = URL_SAFE_NO_PAD.encode(compute_tag(&secret.refresh_key, message.as_bytes()));
        let r1 = format!("{message}.{tag}");
        assert!(matches!(
            secret.verify_refresh(&r1, NOW).unwrap_err(),
            TokenError::Malformed(_)
        ));

        // A seven-part string under a retired marker is refused by the
        // version gate, for every retired marker at both verifiers.
        let current = access(&secret, 1);
        let body = current.strip_prefix("v3").unwrap();
        for retired in ["v1", "v2", "r1", "k1"] {
            let relabelled = format!("{retired}{body}");
            assert!(
                matches!(
                    secret.verify(&relabelled, NOW).unwrap_err(),
                    TokenError::UnsupportedVersion(v) if v == retired
                ),
                "{retired} at the access verifier"
            );
            assert!(
                matches!(
                    secret.verify_refresh(&relabelled, NOW).unwrap_err(),
                    TokenError::UnsupportedVersion(v) if v == retired
                ),
                "{retired} at the refresh verifier"
            );
        }
        // And a future marker likewise.
        let v4 = format!("v4{body}");
        assert!(matches!(
            secret.verify(&v4, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "v4"
        ));
    }

    #[test]
    fn verify_rejects_expired_token() {
        let secret = test_secret();
        let token = secret
            .issue(&sample_account_id(), 1, &DEVICE, 500, 1_000)
            .unwrap();
        assert!(matches!(
            secret.verify(&token, 2_000).unwrap_err(),
            TokenError::Expired { .. }
        ));
        assert!(secret.verify(&token, 999).is_ok());
        let refresh = secret
            .issue_refresh(&sample_account_id(), 1, &DEVICE, 500, 1_000)
            .unwrap();
        // The boundary instant is already expired.
        assert!(matches!(
            secret.verify_refresh(&refresh, 1_000).unwrap_err(),
            TokenError::Expired { .. }
        ));
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let issuer = test_secret();
        let verifier = TokenSecret::new(vec![1u8; MIN_SECRET_LEN]).unwrap();
        assert!(matches!(
            verifier.verify(&access(&issuer, 1), NOW).unwrap_err(),
            TokenError::BadSignature
        ));
        assert!(matches!(
            verifier
                .verify_refresh(&refresh(&issuer, 1), NOW)
                .unwrap_err(),
            TokenError::BadSignature
        ));
    }

    /// Every signed segment is covered by the tag: rewriting the account,
    /// the generation, the device, the issue time (the thing a device floor
    /// revokes by) or the expiry never survives, on either kind.
    #[test]
    fn verify_rejects_every_rewritten_segment() {
        let secret = test_secret();
        let evil_id = URL_SAFE_NO_PAD.encode([0u8; ACCOUNT_ID_LEN]);
        let evil_device = URL_SAFE_NO_PAD.encode([0xEEu8; DEVICE_ID_LEN]);
        let unbound = URL_SAFE_NO_PAD.encode(UNBOUND_DEVICE_ID);
        let later = (NOW - 1).to_string();
        let far = (FAR_FUTURE + 1).to_string();
        let zero_tag = URL_SAFE_NO_PAD.encode([0u8; TAG_LEN]);
        let rewrites: [(usize, &str); 7] = [
            (1, &evil_id),
            (2, "2"),
            (3, &evil_device),
            (3, &unbound),
            (4, &later),
            (5, &far),
            (6, &zero_tag),
        ];
        let access = access(&secret, 1);
        let refresh = refresh(&secret, 1);
        for (index, value) in rewrites {
            let mut parts: Vec<&str> = access.split('.').collect();
            parts[index] = value;
            assert!(
                matches!(
                    secret.verify(&parts.join("."), NOW).unwrap_err(),
                    TokenError::BadSignature
                ),
                "access segment {index} rewritten to {value}"
            );
            let mut parts: Vec<&str> = refresh.split('.').collect();
            parts[index] = value;
            assert!(
                matches!(
                    secret.verify_refresh(&parts.join("."), NOW).unwrap_err(),
                    TokenError::BadSignature
                ),
                "refresh segment {index} rewritten to {value}"
            );
        }
        // A non-canonical spelling of the SAME integer is re-serialised
        // canonically before the tag is recomputed: it is the same credential,
        // not a way to claim a different value.
        let mut parts: Vec<&str> = access.split('.').collect();
        parts[2] = "01";
        assert_eq!(
            secret.verify(&parts.join("."), NOW).unwrap().key_generation,
            1
        );
    }

    #[test]
    fn verify_rejects_malformed_shapes() {
        let secret = test_secret();
        let id = "AAAAAAAAAAAAAAAAAAAAAA";
        let dev = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let tag = dev;
        let bad_shapes = [
            String::new(),
            "v3".to_string(),
            "v3.aaa".to_string(),
            "v3.aaa.bbb.ccc.ddd".to_string(),
            format!("v3.{id}.1.{dev}.1.1"),                  // too few
            format!("v3.{id}.1.{dev}.1.1.{tag}.extra"),      // too many
            format!("v3.!@#.1.{dev}.1.1.{tag}"),             // bad account base64
            format!("v3.AAAA.1.{dev}.1.1.{tag}"),            // short account id
            format!("v3.{id}.x.{dev}.1.1.{tag}"),            // bad generation
            format!("v3.{id}.-1.{dev}.1.1.{tag}"),           // negative generation
            format!("v3.{id}.1.!@#.1.1.{tag}"),              // bad device base64
            format!("v3.{id}.1.{id}.1.1.{tag}"),             // 16-byte device id
            format!("v3.{id}.1..1.1.{tag}"),                 // empty device id
            format!("v3.{id}.1.{dev}.x.1.{tag}"),            // bad issue time
            format!("v3.{id}.1.{dev}.1.not_a_number.{tag}"), // bad expiry
            format!("v3.{id}.1.{dev}.1.1.AAAA"),             // short tag
            format!("v3.{id}.1.{dev}.1.1.!@#"),              // bad tag base64
        ];
        for bad in &bad_shapes {
            assert!(
                matches!(
                    secret.verify(bad, NOW).unwrap_err(),
                    TokenError::Malformed(_)
                ),
                "expected Malformed for {bad:?}"
            );
            let as_refresh = bad.replacen("v3", "r2", 1);
            assert!(
                matches!(
                    secret.verify_refresh(&as_refresh, NOW).unwrap_err(),
                    TokenError::Malformed(_)
                ),
                "expected Malformed for {as_refresh:?} as a refresh credential"
            );
        }
    }

    #[test]
    fn issue_rejects_wrong_account_id_length() {
        let secret = test_secret();
        match secret.issue(&[0u8; 8], 1, &DEVICE, 0, 1_000).unwrap_err() {
            TokenError::WrongAccountIdLength { got, expected } => {
                assert_eq!(got, 8);
                assert_eq!(expected, ACCOUNT_ID_LEN);
            },
            other => panic!("expected WrongAccountIdLength, got {other:?}"),
        }
        assert!(matches!(
            secret
                .issue_refresh(&[0u8; 8], 1, &DEVICE, 0, 1_000)
                .unwrap_err(),
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
        let recovered = secret.verify(&access(&secret, 1), NOW).unwrap();
        assert_eq!(&recovered.account_id[..], &sample_account_id()[..]);
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
    fn fresh_grant_round_trip_carries_the_account_and_generation() {
        let secret = test_secret();
        let account_id = sample_account_id();
        let grant = secret
            .issue_fresh_grant(&account_id, 4, FAR_FUTURE)
            .unwrap();
        assert!(grant.starts_with("k1."));
        assert_eq!(grant.matches('.').count(), 4);
        let got = secret.verify_fresh_grant(&grant, NOW).unwrap();
        assert_eq!(&got.account_id[..], &account_id[..]);
        assert_eq!(got.key_generation, 4);
    }

    #[test]
    fn fresh_grant_is_not_a_bearer_nor_a_refresh_credential_and_vice_versa() {
        let secret = test_secret();
        let id = sample_account_id();
        let access = access(&secret, 1);
        let refresh = refresh(&secret, 1);
        let grant = secret.issue_fresh_grant(&id, 1, FAR_FUTURE).unwrap();

        // The shapes differ (five segments against seven), so each kind is
        // refused at the other's verifier before any tag is looked at.
        assert!(matches!(
            secret.verify(&grant, NOW).unwrap_err(),
            TokenError::Malformed(_)
        ));
        assert!(matches!(
            secret.verify_refresh(&grant, NOW).unwrap_err(),
            TokenError::Malformed(_)
        ));
        assert!(matches!(
            secret.verify_fresh_grant(&access, NOW).unwrap_err(),
            TokenError::Malformed(_)
        ));
        assert!(matches!(
            secret.verify_fresh_grant(&refresh, NOW).unwrap_err(),
            TokenError::Malformed(_)
        ));

        // A grant-shaped string tagged under the access or the refresh key
        // is refused by the tag, and a session-shaped string tagged under
        // the grant key likewise: the three keys are distinct.
        let id_b64 = URL_SAFE_NO_PAD.encode(&id);
        let grant_message = format!("k1.{id_b64}.1.{FAR_FUTURE}");
        for key in [&secret.bytes[..], &secret.refresh_key[..]] {
            let tag = URL_SAFE_NO_PAD.encode(compute_tag(key, grant_message.as_bytes()));
            assert!(matches!(
                secret
                    .verify_fresh_grant(&format!("{grant_message}.{tag}"), NOW)
                    .unwrap_err(),
                TokenError::BadSignature
            ));
        }
        let device_b64 = URL_SAFE_NO_PAD.encode(DEVICE);
        for (version, by_refresh) in [("v3", false), ("r2", true)] {
            let message = format!("{version}.{id_b64}.1.{device_b64}.{ISSUED}.{FAR_FUTURE}");
            let tag =
                URL_SAFE_NO_PAD.encode(compute_tag(&secret.fresh_grant_key, message.as_bytes()));
            let forged = format!("{message}.{tag}");
            let err = if by_refresh {
                secret.verify_refresh(&forged, NOW).unwrap_err()
            } else {
                secret.verify(&forged, NOW).unwrap_err()
            };
            assert!(matches!(err, TokenError::BadSignature), "{version}");
        }
    }

    #[test]
    fn fresh_grant_refusals() {
        let secret = test_secret();
        let id = sample_account_id();

        // Expired (the boundary instant is already expired).
        let grant = secret.issue_fresh_grant(&id, 1, 1_000).unwrap();
        assert!(matches!(
            secret.verify_fresh_grant(&grant, 1_000).unwrap_err(),
            TokenError::Expired { .. }
        ));
        assert!(secret.verify_fresh_grant(&grant, 999).is_ok());

        // Bad tag: another secret, a flipped tag, a rewritten account, a
        // rewritten generation, a rewritten expiry.
        let grant = secret.issue_fresh_grant(&id, 1, FAR_FUTURE).unwrap();
        let other = TokenSecret::new(vec![1u8; MIN_SECRET_LEN]).unwrap();
        assert!(matches!(
            other.verify_fresh_grant(&grant, NOW).unwrap_err(),
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
                        .verify_fresh_grant(&tampered.join("."), NOW)
                        .unwrap_err(),
                    TokenError::BadSignature
                ),
                "segment {index} rewritten"
            );
        }

        // Wrong version prefix and malformed shapes.
        let k2 = format!("k2{}", grant.strip_prefix("k1").unwrap());
        assert!(matches!(
            secret.verify_fresh_grant(&k2, NOW).unwrap_err(),
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
                    secret.verify_fresh_grant(bad, NOW).unwrap_err(),
                    TokenError::Malformed(_)
                ),
                "expected Malformed for {bad:?}"
            );
        }

        // The verifier reports the account and the generation; refusing a
        // grant for another account or a stale generation is the caller's
        // comparison, and these are the values it compares.
        let got = secret.verify_fresh_grant(&grant, NOW).unwrap();
        assert_ne!(got.account_id, [0xEEu8; ACCOUNT_ID_LEN]);
        assert_eq!(got.key_generation, 1);

        assert!(matches!(
            secret.issue_fresh_grant(&[0u8; 8], 1, 1_000).unwrap_err(),
            TokenError::WrongAccountIdLength { .. }
        ));
    }

    #[test]
    fn fresh_grant_key_is_the_documented_derivation() {
        let raw = vec![7u8; MIN_SECRET_LEN];
        let secret = TokenSecret::new(raw.clone()).unwrap();
        assert_eq!(
            secret.fresh_grant_key,
            blake3::derive_key("lakko/fresh-auth-grant/v1", &raw)
        );
        assert_ne!(secret.fresh_grant_key, secret.refresh_key);
        assert_ne!(&secret.fresh_grant_key[..], &raw[..]);
    }

    /// The fuzz entry point accepts nothing it was not issued, and accepts
    /// each of the four kinds it was.
    #[test]
    fn fuzz_entry_point_accepts_only_issued_credentials() {
        let secret = TokenSecret::new(vec![0x11; MIN_SECRET_LEN]).unwrap();
        let id = sample_account_id();
        let access = secret.issue(&id, 1, &DEVICE, 0, 1).unwrap();
        let refresh = secret.issue_refresh(&id, 1, &DEVICE, 0, 1).unwrap();
        let grant = secret.issue_fresh_grant(&id, 1, 1).unwrap();
        let restricted = secret.issue_restricted(&id, 1, &DEVICE, 0, 1).unwrap();
        for good in [&access, &refresh, &grant, &restricted] {
            assert!(fuzz_api::verify_token(good.as_bytes()));
        }
        assert!(!fuzz_api::verify_token(b"v3.a.b.c.d.e.f"));
        assert!(!fuzz_api::verify_token(&[0xFF, 0xFE]));
    }

    #[test]
    fn restricted_round_trip_carries_every_field() {
        let secret = test_secret();
        let token = secret
            .issue_restricted(&sample_account_id(), 4, &DEVICE, ISSUED, FAR_FUTURE)
            .unwrap();
        assert!(token.starts_with("s1."));
        let got = secret.verify_restricted(&token, NOW).unwrap();
        assert_eq!(got.account_id.to_vec(), sample_account_id());
        assert_eq!(got.key_generation, 4);
        assert_eq!(got.device_id, DEVICE);
        assert_eq!(got.issued_at_ms, ISSUED);
    }

    /// A restricted session is accepted only by `verify_restricted`, and
    /// `verify_restricted` accepts none of the other three kinds.
    #[test]
    fn restricted_is_not_interchangeable_with_any_other_kind() {
        let secret = test_secret();
        let id = sample_account_id();
        let restricted = secret
            .issue_restricted(&id, 1, &DEVICE, ISSUED, FAR_FUTURE)
            .unwrap();
        assert!(matches!(
            secret.verify(&restricted, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "s1"
        ));
        assert!(matches!(
            secret.verify_refresh(&restricted, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "s1"
        ));
        assert!(matches!(
            secret.verify_fresh_grant(&restricted, NOW).unwrap_err(),
            TokenError::Malformed(_)
        ));

        let access = access(&secret, 1);
        let refresh = refresh(&secret, 1);
        let grant = secret.issue_fresh_grant(&id, 1, FAR_FUTURE).unwrap();
        assert!(matches!(
            secret.verify_restricted(&access, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "v3"
        ));
        assert!(matches!(
            secret.verify_restricted(&refresh, NOW).unwrap_err(),
            TokenError::UnsupportedVersion(v) if v == "r2"
        ));
        assert!(matches!(
            secret.verify_restricted(&grant, NOW).unwrap_err(),
            TokenError::Malformed(_)
        ));

        // An access token relabelled `s1` keeps its access tag, which is not a
        // restricted tag: the relabelling is a forgery, not a conversion.
        let relabelled = format!("s1{}", access.strip_prefix("v3").unwrap());
        assert!(matches!(
            secret.verify_restricted(&relabelled, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
        let relabelled = format!("v3{}", restricted.strip_prefix("s1").unwrap());
        assert!(matches!(
            secret.verify(&relabelled, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
    }

    #[test]
    fn restricted_refusals() {
        let secret = test_secret();
        let id = sample_account_id();
        let expired = secret
            .issue_restricted(&id, 1, &DEVICE, ISSUED, NOW)
            .unwrap();
        assert!(matches!(
            secret.verify_restricted(&expired, NOW).unwrap_err(),
            TokenError::Expired { .. }
        ));
        let other = TokenSecret::new(vec![9u8; MIN_SECRET_LEN]).unwrap();
        let foreign = other
            .issue_restricted(&id, 1, &DEVICE, ISSUED, FAR_FUTURE)
            .unwrap();
        assert!(matches!(
            secret.verify_restricted(&foreign, NOW).unwrap_err(),
            TokenError::BadSignature
        ));
        assert!(matches!(
            secret
                .issue_restricted(&[0u8; 8], 1, &DEVICE, ISSUED, FAR_FUTURE)
                .unwrap_err(),
            TokenError::WrongAccountIdLength { .. }
        ));
    }

    #[test]
    fn restricted_key_is_the_documented_derivation() {
        let raw = vec![7u8; MIN_SECRET_LEN];
        let secret = TokenSecret::new(raw.clone()).unwrap();
        assert_eq!(
            secret.restricted_key,
            blake3::derive_key("lakko/suspended-session/v1", &raw)
        );
        assert_ne!(secret.restricted_key, secret.refresh_key);
        assert_ne!(secret.restricted_key, secret.fresh_grant_key);
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
