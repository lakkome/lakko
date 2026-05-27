// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Environment-driven configuration for the SFU.
//!
//! All knobs live in `/etc/lakko/lakko.env` in production
//! (loaded by systemd via `EnvironmentFile=`) and in `.env`
//! locally (loaded by `dotenvy` in debug builds).
//!
//! Keep this module trivial — no derive-heavy crates, no
//! schema validation framework. Each knob is a single
//! `std::env::var` call wrapped in a friendly error.

use std::env;
use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;

/// Default WebSocket signaling bind address.
const DEFAULT_BIND: &str = "127.0.0.1:7880";
/// Default Prometheus exporter bind address.
const DEFAULT_METRICS_BIND: &str = "127.0.0.1:9641";
/// Default JWKS refresh interval — five minutes is the
/// shortest cadence that does not hammer the auth service.
const DEFAULT_JWKS_REFRESH_SECS: u64 = 300;
/// Default WS handshake timeout — 10 seconds is plenty for a
/// JWT verify + initial signaling round trip; longer means a
/// stale connection holds a slot in the registry for no
/// useful reason.
const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 10;
/// Default per-peer signaling-message rate limit (frames per
/// second). The SFU emits ~ 30 RTCP packets / sec under
/// normal load; 100 fps leaves plenty of slack for ICE
/// candidate bursts at handshake.
const DEFAULT_PEER_MSG_RATE_PER_SEC: u32 = 100;

/// Failure modes for loading the runtime config.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Required env var was unset or empty.
    #[error("missing required env var: {0}")]
    Missing(&'static str),

    /// Env var was present but could not be parsed.
    #[error("invalid env var {name}: {detail}")]
    Invalid {
        /// Variable name.
        name: &'static str,
        /// Human-readable parse failure.
        detail: String,
    },
}

/// Fully-resolved runtime config.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the WebSocket signaling endpoint listens.
    pub signaling_bind: SocketAddr,
    /// Where the Prometheus `/metrics` endpoint listens. We
    /// expose this on a separate socket so the nginx
    /// front-door can apply mTLS exclusively to that endpoint
    /// without affecting the signaling path.
    pub metrics_bind: SocketAddr,
    /// URL to fetch the call-JWT JWKS from. Required at
    /// startup — without it the SFU has no way to verify
    /// incoming call admissions.
    pub jwks_url: String,
    /// How often to refresh the JWKS cache.
    pub jwks_refresh: Duration,
    /// Hard cap on the initial WS-upgrade + JWT-verify
    /// round-trip latency. Read by the routing sprint once
    /// the WS upgrade handler enforces a per-handshake
    /// budget.
    #[allow(dead_code)]
    pub handshake_timeout: Duration,
    /// Per-peer signaling-message rate limit (frames / sec).
    /// Read by the routing sprint once per-peer rate limiting
    /// is wired into the WS frame loop.
    #[allow(dead_code)]
    pub peer_msg_rate_per_sec: u32,
}

impl Config {
    /// Load from the process environment. In debug builds, a
    /// `.env` file is read first via `dotenvy` (see
    /// [`crate::dotenv_in_debug`]).
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] on missing or unparseable values.
    pub fn from_env() -> Result<Self, ConfigError> {
        let signaling_bind = parse_socket("LAKKO_SFU_BIND", DEFAULT_BIND)?;
        let metrics_bind = parse_socket("LAKKO_SFU_METRICS_BIND", DEFAULT_METRICS_BIND)?;

        let jwks_url = env::var("LAKKO_SFU_JWKS_URL")
            .map_err(|_| ConfigError::Missing("LAKKO_SFU_JWKS_URL"))?;
        if jwks_url.is_empty() {
            return Err(ConfigError::Missing("LAKKO_SFU_JWKS_URL"));
        }

        let jwks_refresh = parse_secs("LAKKO_SFU_JWKS_REFRESH_SECS", DEFAULT_JWKS_REFRESH_SECS)?;
        let handshake_timeout = parse_secs(
            "LAKKO_SFU_HANDSHAKE_TIMEOUT_SECS",
            DEFAULT_HANDSHAKE_TIMEOUT_SECS,
        )?;
        let peer_msg_rate_per_sec = parse_u32(
            "LAKKO_SFU_PEER_MSG_RATE_PER_SEC",
            DEFAULT_PEER_MSG_RATE_PER_SEC,
        )?;

        Ok(Self {
            signaling_bind,
            metrics_bind,
            jwks_url,
            jwks_refresh,
            handshake_timeout,
            peer_msg_rate_per_sec,
        })
    }
}

fn parse_socket(name: &'static str, fallback: &str) -> Result<SocketAddr, ConfigError> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_owned());
    raw.parse::<SocketAddr>().map_err(|e| ConfigError::Invalid {
        name,
        detail: e.to_string(),
    })
}

fn parse_secs(name: &'static str, fallback: u64) -> Result<Duration, ConfigError> {
    let raw = match env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(Duration::from_secs(fallback)),
    };
    let n: u64 = raw.parse().map_err(|_| ConfigError::Invalid {
        name,
        detail: format!("expected unsigned integer, got {raw:?}"),
    })?;
    Ok(Duration::from_secs(n))
}

fn parse_u32(name: &'static str, fallback: u32) -> Result<u32, ConfigError> {
    let raw = match env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(fallback),
    };
    raw.parse::<u32>().map_err(|_| ConfigError::Invalid {
        name,
        detail: format!("expected unsigned integer, got {raw:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    /// `Config::from_env` reads process-global env state; the
    /// three test cases below would race if cargo ran them in
    /// parallel (cargo's default within one test binary is
    /// multi-threaded). Serialise via a module-local lock so
    /// the env-var scratch never leaks across test boundaries.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Helper — clear every env var we care about, install
    /// `pairs`, run the closure, restore. Holds the
    /// `ENV_LOCK` for the duration.
    fn with_env<F: FnOnce() -> R, R>(pairs: &[(&str, &str)], f: F) -> R {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "LAKKO_SFU_JWKS_URL",
            "LAKKO_SFU_BIND",
            "LAKKO_SFU_METRICS_BIND",
            "LAKKO_SFU_JWKS_REFRESH_SECS",
            "LAKKO_SFU_HANDSHAKE_TIMEOUT_SECS",
            "LAKKO_SFU_PEER_MSG_RATE_PER_SEC",
        ] {
            env::remove_var(key);
        }
        for (k, v) in pairs {
            env::set_var(k, v);
        }
        let out = f();
        for (k, _) in pairs {
            env::remove_var(k);
        }
        out
    }

    #[test]
    fn missing_jwks_url_is_fatal() {
        with_env(&[], || {
            let err = Config::from_env().unwrap_err();
            assert!(matches!(err, ConfigError::Missing("LAKKO_SFU_JWKS_URL")));
        });
    }

    #[test]
    fn minimal_valid_config() {
        with_env(
            &[(
                "LAKKO_SFU_JWKS_URL",
                "https://auth.lakko.me/.well-known/jwks.json",
            )],
            || {
                let cfg = Config::from_env().expect("valid minimal env");
                assert_eq!(cfg.jwks_url, "https://auth.lakko.me/.well-known/jwks.json");
                assert_eq!(cfg.signaling_bind.to_string(), "127.0.0.1:7880");
                assert_eq!(cfg.metrics_bind.to_string(), "127.0.0.1:9641");
                assert_eq!(cfg.jwks_refresh, Duration::from_secs(300));
            },
        );
    }

    #[test]
    fn invalid_socket_rejected() {
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_BIND", "not-a-socket"),
            ],
            || {
                let err = Config::from_env().unwrap_err();
                assert!(matches!(
                    err,
                    ConfigError::Invalid {
                        name: "LAKKO_SFU_BIND",
                        ..
                    }
                ));
            },
        );
    }
}
