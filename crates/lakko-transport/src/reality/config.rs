// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Strongly-typed REALITY configuration. The bootstrap
//! rendezvous config carries this serialised; the JSON
//! field names match upstream xtls/REALITY for trivial
//! interop with existing tooling.
//!
//! Parsing is fail-loud — a malformed short_id or pubkey
//! never silently becomes a no-op. The [`RealityConfig::parse`]
//! one-shot constructor lifts the wire form into the typed
//! form in a single step and is the recommended entry point.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Length of an X25519 public key in bytes — pinned by
/// curve. Re-declared here so a caller does not have to
/// reach into `lakko_crypto::agreement::PUBLIC_KEY_LEN`.
pub const REALITY_PUBKEY_LEN: usize = 32;

/// Length of a REALITY short ID. Upstream xtls/REALITY
/// allows variable-length short_id up to 8 bytes; Lakko
/// pins exactly 8 bytes so the authenticator layout is
/// fixed-width and trivially audited. Publishers pad
/// shorter values with zeros, but the audit-grep marker
/// is the exact constant.
pub const SHORT_ID_LEN: usize = 8;

/// REALITY server's static X25519 public key. The
/// publisher publishes this through the signed bootstrap
/// rendezvous (T-7 trust anchor) — under the threat model
/// it is publisher-controlled, never attacker-controlled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealityServerPubkey(pub [u8; REALITY_PUBKEY_LEN]);

/// REALITY short identifier — a publisher-assigned
/// per-client tag baked into the bootstrap config. Lets
/// the server quickly look up which keying material this
/// client is allowed to authenticate against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealityShortId(pub [u8; SHORT_ID_LEN]);

/// Failure modes the config parser surfaces. Callers
/// branch on the variant; the message strings are
/// telemetry-friendly but never an attack surface.
#[derive(Debug, Error)]
pub enum RealityConfigError {
    /// `server_pubkey_b64` was not valid base64.
    #[error("server pubkey base64 decode failed")]
    BadPubkeyBase64,
    /// `server_pubkey_b64` decoded to a non-32-byte
    /// value.
    #[error("server pubkey is {got} bytes, expected {REALITY_PUBKEY_LEN}")]
    PubkeyLength {
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

/// Bootstrap-config descriptor for a REALITY endpoint. JSON
/// shape pinned because the rendezvous bootstrap config the
/// client downloads carries this serialised; a renaming
/// breaks every previously-pinned config.
///
/// Field names mirror upstream xtls/REALITY for trivial
/// interop with existing tooling. Use
/// [`RealityConfig::parse`] to lift this into typed form.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RealityConfig {
    /// Hostname presented in the outer TLS ClientHello —
    /// typically a high-traffic site (e.g. `microsoft.com`)
    /// the censor will not block by SNI alone.
    pub sni: String,
    /// Server-side static X25519 public key, base64
    /// (`STANDARD` padded). 32 bytes after decode.
    pub server_pubkey_b64: String,
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
    /// `(RealityServerPubkey, RealityShortId)`. Validates
    /// every length / format constraint upfront so a bad
    /// bootstrap config cannot silently flow into the
    /// handshake layer.
    ///
    /// # Errors
    ///
    /// See [`RealityConfigError`].
    pub fn parse(&self) -> RealityConfigResult<(RealityServerPubkey, RealityShortId)> {
        if self.sni.is_empty() {
            return Err(RealityConfigError::EmptySni);
        }
        if self.remote.is_empty() {
            return Err(RealityConfigError::EmptyRemote);
        }

        let pk_bytes = B64
            .decode(self.server_pubkey_b64.as_bytes())
            .map_err(|_| RealityConfigError::BadPubkeyBase64)?;
        if pk_bytes.len() != REALITY_PUBKEY_LEN {
            return Err(RealityConfigError::PubkeyLength {
                got: pk_bytes.len(),
            });
        }
        let mut pk = [0u8; REALITY_PUBKEY_LEN];
        pk.copy_from_slice(&pk_bytes);

        let sid_bytes =
            hex::decode(&self.short_id_hex).map_err(|_| RealityConfigError::BadShortIdHex)?;
        if sid_bytes.len() != SHORT_ID_LEN {
            return Err(RealityConfigError::ShortIdLength {
                got: sid_bytes.len(),
            });
        }
        let mut sid = [0u8; SHORT_ID_LEN];
        sid.copy_from_slice(&sid_bytes);

        Ok((RealityServerPubkey(pk), RealityShortId(sid)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn happy_config() -> RealityConfig {
        RealityConfig {
            sni: "www.microsoft.com".into(),
            server_pubkey_b64: B64.encode([0xCA; REALITY_PUBKEY_LEN]),
            short_id_hex: hex::encode([0xAB; SHORT_ID_LEN]),
            allowed_inner_servers: vec!["lakko.me".into()],
            remote: "203.0.113.7:443".into(),
        }
    }

    #[test]
    fn parse_happy_path() {
        let cfg = happy_config();
        let (pk, sid) = cfg.parse().expect("happy parse");
        assert_eq!(pk.0, [0xCA; REALITY_PUBKEY_LEN]);
        assert_eq!(sid.0, [0xAB; SHORT_ID_LEN]);
    }

    #[test]
    fn parse_rejects_bad_pubkey_base64() {
        let mut cfg = happy_config();
        cfg.server_pubkey_b64 = "!!!not-base64!!!".into();
        assert!(matches!(
            cfg.parse(),
            Err(RealityConfigError::BadPubkeyBase64)
        ));
    }

    #[test]
    fn parse_rejects_wrong_pubkey_length() {
        let mut cfg = happy_config();
        // Encode only 16 bytes — decodes fine but length-checks.
        cfg.server_pubkey_b64 = B64.encode([0xCA; 16]);
        match cfg.parse() {
            Err(RealityConfigError::PubkeyLength { got }) => assert_eq!(got, 16),
            other => panic!("expected PubkeyLength, got {other:?}"),
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
        cfg.short_id_hex = hex::encode([0xAB; 4]); // 4 bytes, want 8
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
