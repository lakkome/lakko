// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Strongly-typed REALITY v2 configuration (Path C).
//!
//! v2 replaces the v1 ECDH-based config (`server_pubkey_b64`)
//! with a pre-shared-key model (`tag_secret_b64`). See the
//! authenticator module docstring for the rationale.
//!
//! Parsing is fail-loud — a malformed short_id or tag
//! secret never silently becomes a no-op.
//! [`RealityConfig::parse`] is the one-shot constructor.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Length of a REALITY tag secret in bytes. Picked to
/// match HMAC-SHA-256 key size + HKDF-SHA-256 IKM
/// preferred length.
pub const TAG_SECRET_LEN: usize = 32;

/// Length of a REALITY short ID. Lakko pins exactly 8
/// bytes so the authenticator layout is fixed-width and
/// trivially audited. Publishers pad shorter values with
/// zeros.
pub const SHORT_ID_LEN: usize = 8;

/// Pre-shared tag secret. The publisher derives one per
/// `(client, short_id)` pair (typically via
/// `HKDF(master_key, "lakko/reality/tag", short_id)`) and
/// distributes it to the client through the signed
/// bootstrap rendezvous channel (T-7 trust anchor).
///
/// Server side: the publisher's REALITY server holds the
/// same master_key, recomputes the tag_secret on demand
/// from the short_id observed in the inbound
/// ClientHello.session_id, and feeds it to
/// [`crate::reality::authenticator::verify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealityTagSecret(pub [u8; TAG_SECRET_LEN]);

/// REALITY short identifier — publisher-assigned per-client
/// tag baked into the bootstrap config. Lets the server
/// quickly look up which `tag_secret` to verify against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealityShortId(pub [u8; SHORT_ID_LEN]);

/// Failure modes the config parser surfaces. Callers
/// branch on the variant; the message strings are
/// telemetry-friendly but never an attack surface.
#[derive(Debug, Error)]
pub enum RealityConfigError {
    /// `tag_secret_b64` was not valid base64.
    #[error("tag_secret base64 decode failed")]
    BadTagSecretBase64,
    /// `tag_secret_b64` decoded to a non-32-byte value.
    #[error("tag_secret is {got} bytes, expected {TAG_SECRET_LEN}")]
    TagSecretLength {
        /// Number of bytes the decoded value carried.
        got: usize,
    },
    /// `short_id_hex` was not valid hex.
    #[error("short_id hex decode failed")]
    BadShortIdHex,
    /// `short_id_hex` decoded to a value of the wrong
    /// length.
    #[error("short_id is {got} bytes, expected {SHORT_ID_LEN}")]
    ShortIdLength {
        /// Decoded length.
        got: usize,
    },
    /// SNI string is empty. A REALITY config without a
    /// cover-site SNI cannot mimic anything — refuse at
    /// parse time rather than at handshake time.
    #[error("sni is empty")]
    EmptySni,
    /// Remote endpoint string is empty.
    #[error("remote is empty")]
    EmptyRemote,
}

/// Convenience alias for `Result<T, RealityConfigError>`.
pub type RealityConfigResult<T> = core::result::Result<T, RealityConfigError>;

/// Bootstrap-config descriptor for a REALITY endpoint.
/// JSON shape pinned because the rendezvous bootstrap
/// config the client downloads carries this serialised; a
/// rename breaks every previously-pinned config.
///
/// Field shape differs from upstream xtls/REALITY — Lakko
/// REALITY v2 (Path C) is wire-NOT-compatible with
/// upstream because of the ECDH-removal redesign. See the
/// authenticator module docstring.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RealityConfig {
    /// Hostname presented in the outer TLS ClientHello —
    /// typically a high-traffic site (e.g. `microsoft.com`)
    /// the censor will not block by SNI alone.
    pub sni: String,
    /// Pre-shared tag secret, base64 (`STANDARD` padded).
    /// 32 bytes after decode. Replaces the v1
    /// `server_pubkey_b64`.
    pub tag_secret_b64: String,
    /// Short id (8 bytes), hex-encoded.
    pub short_id_hex: String,
    /// Server names the inner TLS may legitimately serve.
    /// Used by the server side to gate which inner targets
    /// it forwards traffic to.
    pub allowed_inner_servers: Vec<String>,
    /// Network endpoint (host:port) of the REALITY server.
    pub remote: String,
}

impl RealityConfig {
    /// Lift the wire-form config into typed
    /// `(RealityTagSecret, RealityShortId)`. Validates
    /// every length / format constraint upfront so a bad
    /// bootstrap config cannot silently flow into the
    /// handshake layer.
    ///
    /// # Errors
    ///
    /// See [`RealityConfigError`].
    pub fn parse(&self) -> RealityConfigResult<(RealityTagSecret, RealityShortId)> {
        if self.sni.is_empty() {
            return Err(RealityConfigError::EmptySni);
        }
        if self.remote.is_empty() {
            return Err(RealityConfigError::EmptyRemote);
        }

        let secret_bytes = B64
            .decode(self.tag_secret_b64.as_bytes())
            .map_err(|_| RealityConfigError::BadTagSecretBase64)?;
        if secret_bytes.len() != TAG_SECRET_LEN {
            return Err(RealityConfigError::TagSecretLength {
                got: secret_bytes.len(),
            });
        }
        let mut ts = [0u8; TAG_SECRET_LEN];
        ts.copy_from_slice(&secret_bytes);

        let sid_bytes =
            hex::decode(&self.short_id_hex).map_err(|_| RealityConfigError::BadShortIdHex)?;
        if sid_bytes.len() != SHORT_ID_LEN {
            return Err(RealityConfigError::ShortIdLength {
                got: sid_bytes.len(),
            });
        }
        let mut sid = [0u8; SHORT_ID_LEN];
        sid.copy_from_slice(&sid_bytes);

        Ok((RealityTagSecret(ts), RealityShortId(sid)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn happy_config() -> RealityConfig {
        RealityConfig {
            sni: "www.microsoft.com".into(),
            tag_secret_b64: B64.encode([0xCA; TAG_SECRET_LEN]),
            short_id_hex: hex::encode([0xAB; SHORT_ID_LEN]),
            allowed_inner_servers: vec!["lakko.me".into()],
            remote: "203.0.113.7:443".into(),
        }
    }

    #[test]
    fn parse_happy_path() {
        let cfg = happy_config();
        let (ts, sid) = cfg.parse().expect("happy parse");
        assert_eq!(ts.0, [0xCA; TAG_SECRET_LEN]);
        assert_eq!(sid.0, [0xAB; SHORT_ID_LEN]);
    }

    #[test]
    fn parse_rejects_bad_tag_secret_base64() {
        let mut cfg = happy_config();
        cfg.tag_secret_b64 = "!!!not-base64!!!".into();
        assert!(matches!(
            cfg.parse(),
            Err(RealityConfigError::BadTagSecretBase64)
        ));
    }

    #[test]
    fn parse_rejects_wrong_tag_secret_length() {
        let mut cfg = happy_config();
        cfg.tag_secret_b64 = B64.encode([0xCA; 16]);
        match cfg.parse() {
            Err(RealityConfigError::TagSecretLength { got }) => assert_eq!(got, 16),
            other => panic!("expected TagSecretLength, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_bad_short_id_hex() {
        let mut cfg = happy_config();
        cfg.short_id_hex = "zz".into();
        assert!(matches!(
            cfg.parse(),
            Err(RealityConfigError::BadShortIdHex)
        ));
    }

    #[test]
    fn parse_rejects_wrong_short_id_length() {
        let mut cfg = happy_config();
        cfg.short_id_hex = hex::encode([0xAB; 4]);
        match cfg.parse() {
            Err(RealityConfigError::ShortIdLength { got }) => assert_eq!(got, 4),
            other => panic!("expected ShortIdLength, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_empty_sni() {
        let mut cfg = happy_config();
        cfg.sni = String::new();
        assert!(matches!(cfg.parse(), Err(RealityConfigError::EmptySni)));
    }

    #[test]
    fn parse_rejects_empty_remote() {
        let mut cfg = happy_config();
        cfg.remote = String::new();
        assert!(matches!(cfg.parse(), Err(RealityConfigError::EmptyRemote)));
    }

    #[test]
    fn config_round_trips_through_serde() {
        let cfg = happy_config();
        let json = serde_json::to_string(&cfg).expect("serialise");
        let back: RealityConfig = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(cfg, back);
    }
}
