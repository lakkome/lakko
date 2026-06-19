// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Environment configuration for the obfs proxy (ADR-0026, Phase 1).
//!
//! Production reads these from the systemd `EnvironmentFile`; debug builds also
//! pick up a workspace `.env` via `dotenvy`. The REALITY listener is optional —
//! when its bind/pairs are unset the proxy runs control-plane only (health), so
//! a deploy can stage the secrets before opening the public port.

use std::net::SocketAddr;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_transport::reality::config::{
    RealityShortId, RealityTagSecret, SHORT_ID_LEN, TAG_SECRET_LEN,
};

/// Default control-plane (health) bind. Distinct from the other services'
/// ports (auth 8081 … witness 8087); the obfs ports are separate + raw.
const DEFAULT_CONTROL_BIND: &str = "127.0.0.1:8088";

/// Default REALITY allow-list freshness tolerance (seconds).
const DEFAULT_REALITY_TOLERANCE_SECS: u64 = 30;

/// Parsed proxy configuration.
pub(crate) struct ProxyConfig {
    /// The internal HTTP address every tunnel is relayed to (e.g. nginx on
    /// localhost). Fixed — a tunnel can never reach another host (no open
    /// relay).
    pub internal_target: SocketAddr,
    /// The control-plane (health / ready / version) bind.
    pub control_bind: SocketAddr,
    /// REALITY listener config, or `None` to run control-plane only.
    pub reality: Option<RealityProxyConfig>,
}

/// REALITY-listener-specific config.
pub(crate) struct RealityProxyConfig {
    /// Public TCP bind for the REALITY listener.
    pub bind: SocketAddr,
    /// The cover SNI the self-signed server cert is issued for (the apparent
    /// site a probe sees). Must be non-empty.
    pub cover_sni: String,
    /// The publisher-distributed `(short_id, tag_secret)` allow-list.
    pub allowed_pairs: Vec<(RealityShortId, RealityTagSecret)>,
    /// Replay-window / timestamp tolerance in seconds.
    pub tolerance_secs: u64,
}

/// Configuration errors — surfaced loudly at boot, never silently defaulted
/// into an insecure state.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    /// A required variable was missing.
    #[error("missing required env var {0}")]
    Missing(&'static str),
    /// A value failed to parse.
    #[error("invalid {var}: {detail}")]
    Invalid {
        /// The offending variable.
        var: &'static str,
        /// Human-readable detail.
        detail: String,
    },
}

impl ProxyConfig {
    /// Load from the process environment.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] if a required var is missing or a value is malformed.
    pub(crate) fn from_env() -> Result<Self, ConfigError> {
        let internal_target =
            req("LAKKO_OBFS_PROXY_TARGET")?
                .parse()
                .map_err(|e| ConfigError::Invalid {
                    var: "LAKKO_OBFS_PROXY_TARGET",
                    detail: format!("not a socket address: {e}"),
                })?;

        let control_bind = std::env::var("LAKKO_OBFS_PROXY_CONTROL_BIND")
            .unwrap_or_else(|_| DEFAULT_CONTROL_BIND.to_string())
            .parse()
            .map_err(|e| ConfigError::Invalid {
                var: "LAKKO_OBFS_PROXY_CONTROL_BIND",
                detail: format!("not a socket address: {e}"),
            })?;

        let reality = match std::env::var("LAKKO_OBFS_PROXY_REALITY_BIND") {
            Ok(bind) if !bind.is_empty() => Some(parse_reality(&bind)?),
            _ => None,
        };

        Ok(Self {
            internal_target,
            control_bind,
            reality,
        })
    }
}

fn req(var: &'static str) -> Result<String, ConfigError> {
    std::env::var(var).map_err(|_| ConfigError::Missing(var))
}

fn parse_reality(bind: &str) -> Result<RealityProxyConfig, ConfigError> {
    let bind: SocketAddr = bind.parse().map_err(|e| ConfigError::Invalid {
        var: "LAKKO_OBFS_PROXY_REALITY_BIND",
        detail: format!("not a socket address: {e}"),
    })?;

    let cover_sni = req("LAKKO_OBFS_PROXY_REALITY_SNI")?;
    if cover_sni.is_empty() {
        return Err(ConfigError::Invalid {
            var: "LAKKO_OBFS_PROXY_REALITY_SNI",
            detail: "cover SNI must be non-empty".into(),
        });
    }

    let pairs_raw = req("LAKKO_OBFS_PROXY_REALITY_PAIRS")?;
    let allowed_pairs = parse_pairs(&pairs_raw)?;
    if allowed_pairs.is_empty() {
        return Err(ConfigError::Invalid {
            var: "LAKKO_OBFS_PROXY_REALITY_PAIRS",
            detail: "no (short_id:tag_secret) pairs".into(),
        });
    }

    let tolerance_secs = match std::env::var("LAKKO_OBFS_PROXY_REALITY_TOLERANCE_SECS") {
        Ok(v) if !v.is_empty() => v.parse().map_err(|e| ConfigError::Invalid {
            var: "LAKKO_OBFS_PROXY_REALITY_TOLERANCE_SECS",
            detail: format!("not an integer: {e}"),
        })?,
        _ => DEFAULT_REALITY_TOLERANCE_SECS,
    };

    Ok(RealityProxyConfig {
        bind,
        cover_sni,
        allowed_pairs,
        tolerance_secs,
    })
}

/// Parse a CSV of `short_id_hex:tag_secret_b64` pairs into the typed allow-list.
fn parse_pairs(raw: &str) -> Result<Vec<(RealityShortId, RealityTagSecret)>, ConfigError> {
    const VAR: &str = "LAKKO_OBFS_PROXY_REALITY_PAIRS";
    let mut out = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (sid_hex, ts_b64) = entry.split_once(':').ok_or(ConfigError::Invalid {
            var: VAR,
            detail: format!("entry {entry:?} is not short_id_hex:tag_secret_b64"),
        })?;

        let sid_bytes = hex::decode(sid_hex).map_err(|e| ConfigError::Invalid {
            var: VAR,
            detail: format!("short_id not hex: {e}"),
        })?;
        let sid: [u8; SHORT_ID_LEN] =
            sid_bytes
                .try_into()
                .map_err(|v: Vec<u8>| ConfigError::Invalid {
                    var: VAR,
                    detail: format!("short_id is {} bytes, expected {SHORT_ID_LEN}", v.len()),
                })?;

        let ts_bytes = B64.decode(ts_b64).map_err(|e| ConfigError::Invalid {
            var: VAR,
            detail: format!("tag_secret not base64: {e}"),
        })?;
        let ts: [u8; TAG_SECRET_LEN] =
            ts_bytes
                .try_into()
                .map_err(|v: Vec<u8>| ConfigError::Invalid {
                    var: VAR,
                    detail: format!("tag_secret is {} bytes, expected {TAG_SECRET_LEN}", v.len()),
                })?;

        out.push((RealityShortId(sid), RealityTagSecret(ts)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pairs_round_trips_a_valid_entry() {
        let sid_hex = hex::encode([0xAB_u8; SHORT_ID_LEN]);
        let ts_b64 = B64.encode([0xCD_u8; TAG_SECRET_LEN]);
        let raw = format!("{sid_hex}:{ts_b64}");
        let pairs = parse_pairs(&raw).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0 .0, [0xAB_u8; SHORT_ID_LEN]);
        assert_eq!(pairs[0].1 .0, [0xCD_u8; TAG_SECRET_LEN]);
    }

    #[test]
    fn parse_pairs_rejects_wrong_short_id_length() {
        let ts_b64 = B64.encode([0xCD_u8; TAG_SECRET_LEN]);
        let raw = format!("ab:{ts_b64}"); // 1 byte, not 8
        assert!(parse_pairs(&raw).is_err());
    }

    #[test]
    fn parse_pairs_rejects_missing_colon() {
        assert!(parse_pairs("deadbeef").is_err());
    }

    #[test]
    fn parse_pairs_accepts_multiple_and_skips_blanks() {
        let sid = hex::encode([0x01_u8; SHORT_ID_LEN]);
        let ts = B64.encode([0x02_u8; TAG_SECRET_LEN]);
        let raw = format!("{sid}:{ts}, ,{sid}:{ts}");
        assert_eq!(parse_pairs(&raw).unwrap().len(), 2);
    }
}
