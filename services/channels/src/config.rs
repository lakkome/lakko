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
/// Default post retention — 30 days, matching the DS
/// service's mailbox retention. Posts older than this
/// are pruned by the background loop. Subscribers
/// offline longer than the retention lose access to
/// pre-retention posts but can still fetch anything
/// since their last cursor.
const DEFAULT_RETENTION_SECS: u64 = 30 * 24 * 3600;
/// Default subscriber retention — 90 days. A subscriber
/// whose client has not polled the envelope endpoint in
/// this long is assumed gone for good; the pruner GCs
/// them so the publisher's rotation fan-out does not
/// scale with the number of historical-ever subscribers.
/// `0` disables the stale-subscriber pruner entirely
/// (loop still runs as no-op).
const DEFAULT_SUBSCRIBER_RETENTION_SECS: u64 = 90 * 24 * 3600;
/// Default cadence for the background prune loop — once
/// an hour. Hot path is a single bulk DELETE on the SQL
/// impl; a tighter cadence buys nothing.
const DEFAULT_PRUNE_INTERVAL_SECS: u64 = 3600;
/// Default number of seed envelopes retained per subscriber
/// (ADR-0031, N-epoch retention). N = 4 covers ~96h at the
/// 24h rotation cadence ADR-0010 targets, so a subscriber
/// offline for up to four days can still catch up on every
/// post it missed. Floored to 1 at parse time — a retention
/// of 0 would prune every envelope on the next write and
/// strand every subscriber. Shared with the store impls as
/// their construction-time default.
pub(crate) const DEFAULT_ENVELOPE_RETENTION_EPOCHS: usize = 4;

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
    /// MariaDB connection URL — when set, the service
    /// uses `SqlChannelStore`; when unset, falls back to
    /// the in-memory store (dev / test). Reads first
    /// `LAKKO_CHANNELS_DATABASE_URL`, then the generic
    /// `DATABASE_URL` so a single-service `cargo run`
    /// dev workflow does not need both vars.
    pub database_url: Option<String>,
    /// Post retention in seconds. Posts whose
    /// `server_ts_ms` is older than
    /// `now_ms - retention_secs * 1000` are pruned by
    /// the background loop. `0` disables retention
    /// entirely (the loop still runs but issues a no-op
    /// log line).
    pub retention_secs: u64,
    /// Subscriber retention in seconds. Subscribers
    /// whose `last_active_at` is older than
    /// `now_ms - subscriber_retention_secs * 1000` are
    /// dropped by the background loop (FK cascade
    /// cleans up their envelope). `0` disables the
    /// stale-subscriber prune step.
    pub subscriber_retention_secs: u64,
    /// Cadence of the background prune loop in
    /// seconds. `0` disables the loop — useful for
    /// integration tests that want to drive prune
    /// manually.
    pub prune_interval_secs: u64,
    /// Number of seed envelopes retained per subscriber
    /// (ADR-0031). Each channel-rotation write files the
    /// envelope at a concrete epoch and prunes that
    /// `(channel, subscriber)` to its highest N epochs, so
    /// a subscriber offline for up to N rotations can still
    /// catch up. Parsed from
    /// `LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS` (default
    /// [`DEFAULT_ENVELOPE_RETENTION_EPOCHS`]); a parsed `0`
    /// is floored to 1 so a rotation cannot strand every
    /// subscriber.
    pub envelope_retention_epochs: usize,
    /// Where the internal moderation listener binds (ADR-0099 step 5),
    /// from `LAKKO_CHANNELS_INTERNAL_BIND`. The listener starts only when
    /// `LAKKO_CHANNELS_INTERNAL_SECRET` and
    /// `LAKKO_CHANNELS_MODERATION_ROOT_KEY` are set; both are read by `main`
    /// and never held here, so this struct's `Debug` cannot print the secret.
    pub internal_bind: SocketAddr,
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
        let max_post_bytes = parse_usize("LAKKO_CHANNELS_MAX_POST_BYTES", DEFAULT_MAX_POST_BYTES)?;
        let fetch_limit_default =
            parse_usize("LAKKO_CHANNELS_FETCH_LIMIT_DEFAULT", DEFAULT_FETCH_LIMIT)?;
        let fetch_limit_max =
            parse_usize("LAKKO_CHANNELS_FETCH_LIMIT_MAX", DEFAULT_FETCH_LIMIT_MAX)?;
        if fetch_limit_default > fetch_limit_max {
            return Err(ConfigError::Invalid {
                name: "LAKKO_CHANNELS_FETCH_LIMIT_DEFAULT",
                detail: format!("default ({fetch_limit_default}) exceeds max ({fetch_limit_max})"),
            });
        }
        let database_url = env::var("LAKKO_CHANNELS_DATABASE_URL")
            .ok()
            .or_else(|| env::var("DATABASE_URL").ok())
            .filter(|s| !s.is_empty());
        let retention_secs = parse_u64("LAKKO_CHANNELS_RETENTION_SECS", DEFAULT_RETENTION_SECS)?;
        let subscriber_retention_secs = parse_u64(
            "LAKKO_CHANNELS_SUBSCRIBER_RETENTION_SECS",
            DEFAULT_SUBSCRIBER_RETENTION_SECS,
        )?;
        let prune_interval_secs = parse_u64(
            "LAKKO_CHANNELS_PRUNE_INTERVAL_SECS",
            DEFAULT_PRUNE_INTERVAL_SECS,
        )?;
        // Floor of 1: a retention of 0 would prune every
        // envelope on the next write and strand every
        // subscriber, so we clamp a parsed 0 up to 1.
        let envelope_retention_epochs = parse_usize(
            "LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS",
            DEFAULT_ENVELOPE_RETENTION_EPOCHS,
        )?
        .max(1);
        let internal_bind = parse_socket(
            "LAKKO_CHANNELS_INTERNAL_BIND",
            crate::internal::DEFAULT_INTERNAL_BIND,
        )?;
        Ok(Self {
            bind,
            max_post_bytes,
            fetch_limit_default,
            fetch_limit_max,
            database_url,
            retention_secs,
            subscriber_retention_secs,
            prune_interval_secs,
            envelope_retention_epochs,
            internal_bind,
        })
    }
}

fn parse_u64(name: &'static str, fallback: u64) -> Result<u64, ConfigError> {
    let raw = match env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(fallback),
    };
    raw.parse::<u64>().map_err(|_| ConfigError::Invalid {
        name,
        detail: format!("expected unsigned integer, got {raw:?}"),
    })
}

fn parse_socket(name: &'static str, fallback: &str) -> Result<SocketAddr, ConfigError> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_owned());
    raw.parse::<SocketAddr>().map_err(|e| ConfigError::Invalid {
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
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "LAKKO_CHANNELS_BIND",
            "LAKKO_CHANNELS_MAX_POST_BYTES",
            "LAKKO_CHANNELS_FETCH_LIMIT_DEFAULT",
            "LAKKO_CHANNELS_FETCH_LIMIT_MAX",
            "LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS",
            "LAKKO_CHANNELS_INTERNAL_BIND",
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
            assert_eq!(cfg.internal_bind.to_string(), "127.0.0.1:8093");
            assert_eq!(cfg.max_post_bytes, DEFAULT_MAX_POST_BYTES);
            assert_eq!(cfg.fetch_limit_default, DEFAULT_FETCH_LIMIT);
            assert_eq!(cfg.fetch_limit_max, DEFAULT_FETCH_LIMIT_MAX);
            assert_eq!(
                cfg.envelope_retention_epochs,
                DEFAULT_ENVELOPE_RETENTION_EPOCHS
            );
        });
    }

    #[test]
    fn envelope_retention_default_is_four() {
        with_env(&[], || {
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.envelope_retention_epochs, 4);
        });
    }

    #[test]
    fn envelope_retention_override_is_parsed() {
        with_env(&[("LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS", "8")], || {
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.envelope_retention_epochs, 8);
        });
    }

    #[test]
    fn envelope_retention_zero_is_floored_to_one() {
        // A retention of 0 would prune every envelope on the
        // next write; the parser floors it to 1.
        with_env(&[("LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS", "0")], || {
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.envelope_retention_epochs, 1);
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
