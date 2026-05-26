// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Environment-driven configuration for the channels
//! service. Same shape pattern as `services/sfu/src/config.rs`
//! — every knob is a single `std::env::var` call wrapped in
//! a friendly error.

use std::env;
use std::net::SocketAddr;

use thiserror::Error;

/// Default HTTP bind address.
const DEFAULT_BIND: &str = "127.0.0.1:8090";
/// Default per-post body cap. ChannelPost ciphertexts can
/// be sizeable (up to ~16 MiB plaintext + overhead), but
/// each *post* is a single message — a 16 MiB cap leaves
/// 64× headroom over the typical few-KiB text message.
/// The lakko-channels crate enforces 16 MiB plaintext;
/// adding the SFrame-style overhead the wire ceiling is
/// closer to ~17 MiB.
const DEFAULT_MAX_POST_BYTES: usize = 17 * 1024 * 1024;
/// Default page size for the cursor-paginated fetch
/// endpoint. Clients can override per-request with
/// `?limit=`.
const DEFAULT_FETCH_LIMIT: usize = 50;
/// Hard cap on the fetch endpoint's `limit` query param.
const DEFAULT_FETCH_LIMIT_MAX: usize = 500;

/// Failure modes when loading the runtime config.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Required env var was unset or empty. (We don't have
    /// any required-by-default vars yet; this is left in
    /// for symmetry with the other services and used when
    /// MariaDB persistence lands.)
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
    /// Where the HTTP server listens.
    pub bind: SocketAddr,
    /// Per-post body cap in bytes.
    pub max_post_bytes: usize,
    /// Default page size for the cursor fetch endpoint
    /// when the client does not pass `?limit=`.
    pub fetch_limit_default: usize,
    /// Maximum value the client may pass to `?limit=`.
    pub fetch_limit_max: usize,
}

impl Config {
    /// Load from the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] on missing or unparseable
    /// values.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind = parse_socket("LAKKO_CHANNELS_BIND", DEFAULT_BIND)?;
        let max_post_bytes =
            parse_usize("LAKKO_CHANNELS_MAX_POST_BYTES", DEFAULT_MAX_POST_BYTES)?;
        let fetch_limit_default =
            parse_usize("LAKKO_CHANNELS_FETCH_LIMIT_DEFAULT", DEFAULT_FETCH_LIMIT)?;
        let fetch_limit_max =
            parse_usize("LAKKO_CHANNELS_FETCH_LIMIT_MAX", DEFAULT_FETCH_LIMIT_MAX)?;
        if fetch_limit_default > fetch_limit_max {
            return Err(ConfigError::Invalid {
                name: "LAKKO_CHANNELS_FETCH_LIMIT_DEFAULT",
                detail: format!(
                    "default ({fetch_limit_default}) exceeds max ({fetch_limit_max})"
                ),
            });
        }
        Ok(Self {
            bind,
            max_post_bytes,
            fetch_limit_default,
            fetch_limit_max,
        })
    }
}

fn parse_socket(name: &'static str, fallback: &str) -> Result<SocketAddr, ConfigError> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_owned());
    raw.parse::<SocketAddr>()
        .map_err(|e| ConfigError::Invalid {
            name,
            detail: e.to_string(),
        })
}

fn parse_usize(name: &'static str, fallback: usize) -> Result<usize, ConfigError> {
    let raw = match env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(fallback),
    };
    raw.parse::<usize>().map_err(|_| ConfigError::Invalid {
        name,
        detail: format!("expected unsigned integer, got {raw:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce() -> R, R>(pairs: &[(&str, &str)], f: F) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "LAKKO_CHANNELS_BIND",
            "LAKKO_CHANNELS_MAX_POST_BYTES",
            "LAKKO_CHANNELS_FETCH_LIMIT_DEFAULT",
            "LAKKO_CHANNELS_FETCH_LIMIT_MAX",
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
    fn defaults_work() {
        with_env(&[], || {
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.bind.to_string(), "127.0.0.1:8090");
            assert_eq!(cfg.max_post_bytes, DEFAULT_MAX_POST_BYTES);
            assert_eq!(cfg.fetch_limit_default, DEFAULT_FETCH_LIMIT);
            assert_eq!(cfg.fetch_limit_max, DEFAULT_FETCH_LIMIT_MAX);
        });
    }

    #[test]
    fn rejects_default_above_max() {
        with_env(
            &[
                ("LAKKO_CHANNELS_FETCH_LIMIT_DEFAULT", "1000"),
                ("LAKKO_CHANNELS_FETCH_LIMIT_MAX", "100"),
            ],
            || {
                let err = Config::from_env().unwrap_err();
                assert!(matches!(
                    err,
                    ConfigError::Invalid {
                        name: "LAKKO_CHANNELS_FETCH_LIMIT_DEFAULT",
                        ..
                    }
                ));
            },
        );
    }

    #[test]
    fn parses_overrides() {
        with_env(
            &[
                ("LAKKO_CHANNELS_BIND", "0.0.0.0:9999"),
                ("LAKKO_CHANNELS_FETCH_LIMIT_DEFAULT", "10"),
                ("LAKKO_CHANNELS_FETCH_LIMIT_MAX", "20"),
            ],
            || {
                let cfg = Config::from_env().unwrap();
                assert_eq!(cfg.bind.to_string(), "0.0.0.0:9999");
                assert_eq!(cfg.fetch_limit_default, 10);
                assert_eq!(cfg.fetch_limit_max, 20);
            },
        );
    }
}
