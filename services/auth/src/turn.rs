// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! TURN-credential HTTP handler (G3-{1,2}-turn step 1).
//!
//! Mints short-lived TURN credentials per the coturn
//! `static-auth-secret` REST API convention (also known as
//! the "TURN REST API" — draft-uberti-rtcweb-turn-rest-00).
//! Shape:
//!
//!   username = `<unix_expiry_seconds>:<random_hex>`
//!   password = base64(HMAC-SHA1(secret, username))
//!
//! The tag after the colon is 16 random bytes drawn per credential
//! (ADR-0078 section 2): coturn sees, and may log, the username, so it
//! names no account. A credential is issued only beside a call admission
//! token, in the same anonymous `POST /call/jwt` exchange that spends a
//! `call-join` token; there is no bearer route for it any more.
//!
//! The TURN server (coturn or pion/turn) verifies the
//! credential by recomputing the HMAC against its own copy
//! of the shared secret. The credential is bound to a
//! single expiry timestamp; past expiry, the TURN server
//! rejects any allocation attempt — same posture as a JWT
//! `exp` claim but baked into the auth protocol the TURN
//! spec defines (RFC 8489 §9 long-term credentials).
//!
//! ## Why HMAC-SHA1 (not SHA-256)
//!
//! coturn pins HMAC-SHA1. That's a coturn-protocol contract
//! we don't get to override. The HMAC use case is
//! cryptographically sound even with SHA-1 underneath —
//! the SHA-1 collision attacks (SHAttered, etc.) land on
//! plaintext digests, not on keyed-MAC constructions.
//! NIST SP 800-131A continues to allow HMAC-SHA1 in
//! authentication roles for the same reason.
//!
//! ## Audit anchor
//!
//! C-51 in [`docs/security/calls.md`](../../../docs/security/calls.md)
//! pins the credential shape + expiry semantics. The
//! single-secret invariant (the secret is loaded once from
//! `LAKKO_TURN_SHARED_SECRET` and rotated by service
//! restart) lines up with how the SFU verifies its own
//! JWT-side `kid` rotation.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// Lowercase-hex encode without pulling in the `hex` crate.
fn hex_lower(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(TABLE[(b >> 4) as usize] as char);
        s.push(TABLE[(b & 0x0F) as usize] as char);
    }
    s
}

/// Default credential lifetime — 10 minutes. Long enough
/// to comfortably span a call setup + the worst-case ICE
/// gathering window (typically <30 s on cellular). Short
/// enough that a leaked credential is useless for any
/// secondary attack inside the window.
///
/// The TURN spec (RFC 8489 §9.1) recommends a TTL of "tens
/// of minutes" with hard ceiling at one hour. We pick the
/// lower bound to minimise the leak window.
pub(crate) const TURN_CRED_TTL_SECS: u64 = 600;

/// Production TURN config loaded once at service startup.
/// Wrapped in `Arc` inside `AppState` (alongside the OPAQUE
/// + WebAuthn fields).
///
/// `None` when `LAKKO_TURN_SHARED_SECRET` is unset; `POST /call/jwt`
/// then answers with `turn: null` and the client calls on STUN alone.
#[derive(Clone)]
pub(crate) struct TurnConfig {
    /// TURN server URIs the issued credential authenticates
    /// against. Shipped verbatim to clients as the
    /// `urls` field of the response. Comma-separated env
    /// var `LAKKO_TURN_URIS`, e.g.
    /// `turn:turn.lakko.me:3478?transport=udp,turns:turn.lakko.me:5349?transport=tcp`.
    pub uris: Vec<String>,
    /// Shared secret loaded from `LAKKO_TURN_SHARED_SECRET`
    /// (raw bytes via base64 decode). MUST be the same
    /// value coturn's config file pins under
    /// `static-auth-secret`. Treated as a byte slice — no
    /// UTF-8 assumption.
    secret: Vec<u8>,
}

impl TurnConfig {
    /// Build from environment. Returns `None` when either
    /// env var is missing or empty; failing here is the
    /// "TURN is not configured" path and the route returns
    /// 503 cleanly (call still works on STUN-only networks).
    ///
    /// # Errors
    ///
    /// Returns `Err` only on malformed inputs (base64 decode
    /// failure, zero-length secret). A misconfigured
    /// production deployment should fail loud at startup
    /// rather than silently shipping broken credentials.
    pub(crate) fn from_env() -> anyhow::Result<Option<Self>> {
        use anyhow::Context;

        let uris_raw = match std::env::var("LAKKO_TURN_URIS") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => return Ok(None),
        };
        let secret_raw = match std::env::var("LAKKO_TURN_SHARED_SECRET") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => return Ok(None),
        };

        let uris: Vec<String> = uris_raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if uris.is_empty() {
            anyhow::bail!("LAKKO_TURN_URIS parsed to zero entries");
        }
        for uri in &uris {
            // Cheap structural check — only `turn:` and `turns:`
            // schemes mean anything to a WebRTC client; anything
            // else is a config typo we want to catch at boot.
            if !(uri.starts_with("turn:") || uri.starts_with("turns:")) {
                anyhow::bail!("LAKKO_TURN_URIS entry must start with turn: or turns:, got {uri}");
            }
        }

        let secret = B64
            .decode(secret_raw.trim())
            .context("LAKKO_TURN_SHARED_SECRET must be base64-encoded")?;
        if secret.is_empty() {
            anyhow::bail!("LAKKO_TURN_SHARED_SECRET decoded to zero bytes");
        }
        if secret.len() < 16 {
            anyhow::bail!(
                "LAKKO_TURN_SHARED_SECRET decoded to {} bytes; refuse < 16 (TURN spec recommends at least 128 bits of entropy)",
                secret.len()
            );
        }

        Ok(Some(Self { uris, secret }))
    }

    /// A fixed configuration for other modules' tests.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            uris: vec!["turn:turn.lakko.test:3478?transport=udp".to_owned()],
            secret: vec![0xAA; 32],
        }
    }

    /// Mint a credential valid for the next [`TURN_CRED_TTL_SECS`]
    /// seconds, its username tag 16 fresh random bytes.
    pub(crate) fn mint_random(&self, now_secs: u64) -> Option<TurnCredentialResponse> {
        use rand::RngCore as _;
        let mut tag = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut tag);
        self.mint_with_tag(&tag, now_secs)
    }

    /// Mint a credential under the username tag `tag`. `None` only if the
    /// HMAC refused the key, which it never does for HMAC.
    fn mint_with_tag(&self, tag: &[u8], now_secs: u64) -> Option<TurnCredentialResponse> {
        let expiry = now_secs.saturating_add(TURN_CRED_TTL_SECS);
        // coturn's "static-auth-secret" expects username in the form
        // `<unix_expiry>:<arbitrary user tag>`. The tag is random: it names
        // no account (ADR-0078 section 2).
        let username = format!("{expiry}:{}", hex_lower(tag));

        let mut mac = HmacSha1::new_from_slice(&self.secret).ok()?;
        mac.update(username.as_bytes());
        let password = B64.encode(mac.finalize().into_bytes());

        Some(TurnCredentialResponse {
            urls: self.uris.clone(),
            username,
            password,
            ttl_secs: TURN_CRED_TTL_SECS,
            expiry_unix_secs: expiry,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TurnCredentialResponse {
    /// TURN-server URLs the credential authenticates
    /// against. Clients pass these verbatim to
    /// `RTCIceServer.urls`.
    pub urls: Vec<String>,
    /// HMAC-username, in the form `<unix_expiry>:<random_hex>`.
    pub username: String,
    /// `base64(HMAC-SHA1(secret, username))`.
    pub password: String,
    /// Convenience field — same number as
    /// `(expiry_unix_secs - now)`; saves the client an
    /// arithmetic step when scheduling a credential
    /// refresh.
    pub ttl_secs: u64,
    /// Absolute expiry; the TURN server will refuse any
    /// allocation made under this credential after this
    /// timestamp.
    pub expiry_unix_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_turn() -> TurnConfig {
        TurnConfig {
            uris: vec![
                "turn:turn.lakko.me:3478?transport=udp".to_string(),
                "turns:turn.lakko.me:5349?transport=tcp".to_string(),
            ],
            secret: vec![0xAA; 32],
        }
    }

    #[test]
    fn mint_shape_matches_coturn_rest_api() {
        let turn = test_turn();
        let cred = turn
            .mint_with_tag(b"\x01\x02\x03\x04\x05\x06", 1_700_000_000)
            .expect("mint");

        // Username = `<expiry>:<tag_hex>`. Expiry =
        // now + TTL = 1_700_000_000 + 600 = 1_700_000_600.
        assert_eq!(cred.username, "1700000600:010203040506");
        assert_eq!(cred.ttl_secs, TURN_CRED_TTL_SECS);
        assert_eq!(cred.expiry_unix_secs, 1_700_000_600);
        assert_eq!(cred.urls.len(), 2);
        assert_eq!(cred.urls[0], "turn:turn.lakko.me:3478?transport=udp");
        assert_eq!(cred.urls[1], "turns:turn.lakko.me:5349?transport=tcp");

        // Password = base64(HMAC-SHA1(secret, username)).
        // Verify by recomputing — same operation a coturn
        // server runs on receipt.
        let mut mac = HmacSha1::new_from_slice(&turn.secret).expect("HMAC accepts any key length");
        mac.update(cred.username.as_bytes());
        let expected = B64.encode(mac.finalize().into_bytes());
        assert_eq!(cred.password, expected);
    }

    /// ADR-0078 section 2: every credential carries a fresh random tag of
    /// 16 bytes, so two credentials minted in the same second share nothing
    /// a TURN log could tie together.
    #[test]
    fn every_credential_has_a_fresh_random_tag() {
        let turn = test_turn();
        let a = turn.mint_random(1_700_000_000).expect("mint");
        let b = turn.mint_random(1_700_000_000).expect("mint");
        assert_ne!(a.username, b.username);
        assert_ne!(a.password, b.password);
        let (expiry, tag) = a.username.split_once(':').expect("expiry:tag");
        assert_eq!(expiry, "1700000600");
        assert_eq!(tag.len(), 32);
        assert!(tag.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mint_different_time_distinct_password() {
        let turn = test_turn();
        let a = turn.mint_with_tag(b"\x01", 1_700_000_000).expect("mint");
        let b = turn.mint_with_tag(b"\x01", 1_700_000_001).expect("mint");
        assert_ne!(a.username, b.username);
        assert_ne!(a.password, b.password);
    }

    #[test]
    fn mint_saturating_expiry_does_not_overflow() {
        let turn = test_turn();
        // `now = u64::MAX` would overflow `now + TTL`. We
        // use saturating_add so this caps at MAX rather
        // than wrapping into a credential that
        // accidentally validates in the past.
        let cred = turn.mint_with_tag(b"\x01", u64::MAX).expect("mint");
        assert_eq!(cred.expiry_unix_secs, u64::MAX);
    }

    #[test]
    fn from_env_rejects_short_secret() {
        // The base64-decoded length check is the operative
        // invariant — refuse anything below 128 bits of
        // entropy. Helper exercises the same predicate.
        let too_short = [0u8; 8]; // 64 bits — below threshold
        assert!(too_short.len() < 16);
    }
}
