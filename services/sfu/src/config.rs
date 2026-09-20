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
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use thiserror::Error;

use crate::media::{resolve_advertise_ip, AdvertiseError};

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
/// How often the signaling socket pings a silent peer.
/// Chosen, not measured: three unanswered pings inside the
/// idle window below declare the peer gone. Tightened from
/// 20 s / 60 s once the call's end became the server's
/// call (`call_lifetime.rs`): a participant whose app died
/// is the last one in the room more often than not, and
/// the room cannot be seen to be empty until the socket is.
const DEFAULT_SIGNALING_PING_SECS: u64 = 5;
/// How long a peer may go without any inbound frame (a
/// Pong answers a ping, so a live peer is never silent this
/// long) before it is dropped and announced as left.
const DEFAULT_SIGNALING_IDLE_SECS: u64 = 15;

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

    /// Env var is optional in general but required by the
    /// value of another one.
    #[error("missing env var {name}: {reason}")]
    RequiredBy {
        /// Variable name.
        name: &'static str,
        /// Which other value makes it required, and why.
        reason: String,
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
    /// Address the media-routing task binds its UDP
    /// socket to. `None` (env var unset or empty) turns
    /// the media half off — the SFU runs as a signaling-
    /// only relay, and `ClientFrame::SdpOffer` /
    /// `IceCandidate` frames surface as
    /// `RouterError::MediaNotWired`. Set this in
    /// production via `LAKKO_SFU_MEDIA_BIND`, e.g. a fixed
    /// `0.0.0.0:43210` if the firewall pins the port (or
    /// `:0` to let the OS pick one). Binding an unspecified
    /// address (`0.0.0.0` / `::`) REQUIRES
    /// [`Self::media_advertise_ip`]: startup fails without
    /// it.
    pub media_bind: Option<SocketAddr>,
    /// IP the SFU advertises as its ICE host candidate in
    /// every SDP answer (the port is always the one the
    /// media socket bound). Read from
    /// `LAKKO_SFU_MEDIA_ADVERTISE_IP` and resolved at load:
    ///
    /// - set → that IP, which must be a usable host-candidate
    ///   address (not unspecified / multicast / broadcast /
    ///   link-local) of the same family as the bind IP;
    /// - unset with a concrete bind IP → the bind IP;
    /// - unset with an unspecified bind IP → load fails
    ///   ([`ConfigError::RequiredBy`]), because str0m refuses
    ///   an unspecified host candidate and every answer would
    ///   ship without one, so no client could connect media.
    ///
    /// Always `Some` when [`Self::media_bind`] is `Some`.
    /// With media off it holds the raw value (or `None`) so
    /// startup can warn that it is being ignored.
    pub media_advertise_ip: Option<IpAddr>,
    /// Media diagnostics cadence (`LAKKO_SFU_MEDIA_DIAG_SECS`):
    /// when set, str0m reports per-stream statistics and the media
    /// loop logs them plus a per-subscriber fanout summary this
    /// often. Unset or `0` means off, the production default; the
    /// output is one line per stream per peer per interval.
    pub media_diagnostics: Option<Duration>,
    /// How often the signaling socket sends a WebSocket
    /// protocol ping to each peer (`LAKKO_SFU_SIGNALING_PING_SECS`).
    /// Every client stack answers a protocol ping on its own
    /// (URLSession, OkHttp, browsers, tungstenite), so this
    /// needs nothing from the app layer.
    pub signaling_ping_interval: Duration,
    /// How long a peer may send nothing at all before it is
    /// dropped and announced as left
    /// (`LAKKO_SFU_SIGNALING_IDLE_SECS`). Must exceed the ping
    /// interval, or every peer would be dropped between pings.
    /// Without this a phone that lost its network stayed on
    /// every other participant's roster until nginx gave up on
    /// the idle socket an hour later.
    pub signaling_idle_timeout: Duration,
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
        let signaling_ping_interval =
            parse_secs("LAKKO_SFU_SIGNALING_PING_SECS", DEFAULT_SIGNALING_PING_SECS)?;
        let signaling_idle_timeout =
            parse_secs("LAKKO_SFU_SIGNALING_IDLE_SECS", DEFAULT_SIGNALING_IDLE_SECS)?;
        check_signaling_liveness(signaling_ping_interval, signaling_idle_timeout)?;
        let media_bind = parse_optional_socket("LAKKO_SFU_MEDIA_BIND")?;
        let raw_advertise_ip = parse_optional_ip(MEDIA_ADVERTISE_IP)?;
        let media_advertise_ip = match media_bind {
            Some(bind) => Some(resolve_media_advertise_ip(bind, raw_advertise_ip)?),
            None => raw_advertise_ip,
        };
        let media_diagnostics = parse_optional_secs("LAKKO_SFU_MEDIA_DIAG_SECS")?;

        Ok(Self {
            signaling_bind,
            metrics_bind,
            jwks_url,
            jwks_refresh,
            handshake_timeout,
            peer_msg_rate_per_sec,
            media_bind,
            media_advertise_ip,
            media_diagnostics,
            signaling_ping_interval,
            signaling_idle_timeout,
        })
    }
}

/// A zero ping interval would spin; a zero idle timeout, or one
/// no longer than the ping interval, would drop every peer
/// between two pings. Refused at load rather than discovered
/// as an empty roster in production.
fn check_signaling_liveness(ping: Duration, idle: Duration) -> Result<(), ConfigError> {
    if ping.is_zero() {
        return Err(ConfigError::Invalid {
            name: "LAKKO_SFU_SIGNALING_PING_SECS",
            detail: "must be at least 1".to_owned(),
        });
    }
    if idle <= ping {
        return Err(ConfigError::Invalid {
            name: "LAKKO_SFU_SIGNALING_IDLE_SECS",
            detail: format!(
                "must exceed LAKKO_SFU_SIGNALING_PING_SECS ({} s), got {} s",
                ping.as_secs(),
                idle.as_secs()
            ),
        });
    }
    Ok(())
}

fn parse_socket(name: &'static str, fallback: &str) -> Result<SocketAddr, ConfigError> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_owned());
    raw.parse::<SocketAddr>().map_err(|e| ConfigError::Invalid {
        name,
        detail: e.to_string(),
    })
}

/// Parse an optional `SocketAddr` from `name`. Returns
/// `Ok(None)` when the env var is unset or empty (the
/// signaling-only mode signal). Returns
/// [`ConfigError::Invalid`] when the value is present but
/// malformed — silent fallback to `None` on a typo would
/// mask a production misconfiguration.
fn parse_optional_socket(name: &'static str) -> Result<Option<SocketAddr>, ConfigError> {
    let raw = match env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(None),
    };
    raw.parse::<SocketAddr>()
        .map(Some)
        .map_err(|e| ConfigError::Invalid {
            name,
            detail: e.to_string(),
        })
}

/// Env var carrying the advertised media IP.
const MEDIA_ADVERTISE_IP: &str = "LAKKO_SFU_MEDIA_ADVERTISE_IP";

/// Parse an optional bare `IpAddr` (no port) from `name`.
/// Unset or empty → `Ok(None)`; malformed → loud
/// [`ConfigError::Invalid`].
fn parse_optional_ip(name: &'static str) -> Result<Option<IpAddr>, ConfigError> {
    let raw = match env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(None),
    };
    raw.parse::<IpAddr>()
        .map(Some)
        .map_err(|e| ConfigError::Invalid {
            name,
            detail: format!("expected a bare IP address (no port), got {raw:?}: {e}"),
        })
}

/// Resolve the advertised media IP with the media layer's
/// own rules ([`resolve_advertise_ip`]) so config and the
/// media task can never disagree, mapping each refusal onto
/// the env var an operator has to fix.
fn resolve_media_advertise_ip(
    bind: SocketAddr,
    advertise: Option<IpAddr>,
) -> Result<IpAddr, ConfigError> {
    resolve_advertise_ip(bind.ip(), advertise).map_err(|e| match e {
        AdvertiseError::UnspecifiedBind { .. } => ConfigError::RequiredBy {
            name: MEDIA_ADVERTISE_IP,
            reason: format!(
                "LAKKO_SFU_MEDIA_BIND={bind} binds an unspecified address, which cannot be \
                 advertised as an ICE host candidate; set {MEDIA_ADVERTISE_IP} to the IP \
                 clients reach this socket on"
            ),
        },
        AdvertiseError::BindNotAdvertisable { .. } => ConfigError::Invalid {
            name: "LAKKO_SFU_MEDIA_BIND",
            detail: format!("{e}; set {MEDIA_ADVERTISE_IP} to a usable IP"),
        },
        AdvertiseError::AdvertiseNotAdvertisable { .. } | AdvertiseError::FamilyMismatch { .. } => {
            ConfigError::Invalid {
                name: MEDIA_ADVERTISE_IP,
                detail: e.to_string(),
            }
        },
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

/// A duration in whole seconds that is absent when the variable is
/// unset, empty or `0`.
fn parse_optional_secs(name: &'static str) -> Result<Option<Duration>, ConfigError> {
    let raw = match env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(None),
    };
    let n: u64 = raw.parse().map_err(|_| ConfigError::Invalid {
        name,
        detail: format!("expected unsigned integer, got {raw:?}"),
    })?;
    Ok((n > 0).then(|| Duration::from_secs(n)))
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
            "LAKKO_SFU_MEDIA_BIND",
            "LAKKO_SFU_MEDIA_ADVERTISE_IP",
            "LAKKO_SFU_SIGNALING_PING_SECS",
            "LAKKO_SFU_SIGNALING_IDLE_SECS",
            "LAKKO_SFU_MEDIA_DIAG_SECS",
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
    fn media_diagnostics_is_off_unless_a_positive_cadence_is_set() {
        let jwks = ("LAKKO_SFU_JWKS_URL", "https://auth.example/jwks.json");
        with_env(&[jwks], || {
            assert_eq!(Config::from_env().unwrap().media_diagnostics, None);
        });
        with_env(&[jwks, ("LAKKO_SFU_MEDIA_DIAG_SECS", "0")], || {
            assert_eq!(
                Config::from_env().unwrap().media_diagnostics,
                None,
                "0 means off"
            );
        });
        with_env(&[jwks, ("LAKKO_SFU_MEDIA_DIAG_SECS", "5")], || {
            assert_eq!(
                Config::from_env().unwrap().media_diagnostics,
                Some(Duration::from_secs(5))
            );
        });
        with_env(&[jwks, ("LAKKO_SFU_MEDIA_DIAG_SECS", "soon")], || {
            let err = Config::from_env().unwrap_err();
            assert!(matches!(
                err,
                ConfigError::Invalid {
                    name: "LAKKO_SFU_MEDIA_DIAG_SECS",
                    ..
                }
            ));
        });
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

    /// The liveness defaults: a ping every 5 s, gone after 15 s
    /// of silence. Pinned so a change is a decision, not a drift.
    #[test]
    fn signaling_liveness_defaults() {
        with_env(&[("LAKKO_SFU_JWKS_URL", "https://a/x")], || {
            let cfg = Config::from_env().expect("valid minimal env");
            assert_eq!(cfg.signaling_ping_interval, Duration::from_secs(5));
            assert_eq!(cfg.signaling_idle_timeout, Duration::from_secs(15));
        });
    }

    /// An idle window that does not exceed the ping interval would
    /// drop every peer between two pings; refused at load.
    #[test]
    fn signaling_idle_must_exceed_ping() {
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_SIGNALING_PING_SECS", "30"),
                ("LAKKO_SFU_SIGNALING_IDLE_SECS", "30"),
            ],
            || {
                let err = Config::from_env().unwrap_err();
                assert!(matches!(
                    err,
                    ConfigError::Invalid {
                        name: "LAKKO_SFU_SIGNALING_IDLE_SECS",
                        ..
                    }
                ));
            },
        );
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_SIGNALING_PING_SECS", "0"),
            ],
            || {
                let err = Config::from_env().unwrap_err();
                assert!(matches!(
                    err,
                    ConfigError::Invalid {
                        name: "LAKKO_SFU_SIGNALING_PING_SECS",
                        ..
                    }
                ));
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

    /// `LAKKO_SFU_MEDIA_BIND` unset → `media_bind == None`
    /// (signaling-only mode, the default). Verified against
    /// the minimal-valid setup so a future regression that
    /// flips the default to "always bind" is caught here.
    #[test]
    fn media_bind_default_is_none() {
        with_env(&[("LAKKO_SFU_JWKS_URL", "https://a/x")], || {
            let cfg = Config::from_env().expect("valid minimal env");
            assert!(cfg.media_bind.is_none(), "media off when env unset");
        });
    }

    /// `LAKKO_SFU_MEDIA_BIND=""` (empty string) is treated
    /// identically to "unset" — the systemd
    /// `EnvironmentFile=` syntax tends to surface an
    /// uncommented-but-empty value, and we want production
    /// to behave the same as a clean unset.
    #[test]
    fn media_bind_empty_is_none() {
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_MEDIA_BIND", ""),
            ],
            || {
                let cfg = Config::from_env().expect("valid minimal env");
                assert!(cfg.media_bind.is_none(), "empty env value == unset");
            },
        );
    }

    /// A valid `host:port` parses through to a `Some(addr)`
    /// — the production happy path: every interface bound,
    /// the public IP advertised.
    #[test]
    fn media_bind_valid_socket_addr() {
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_MEDIA_BIND", "0.0.0.0:43210"),
                ("LAKKO_SFU_MEDIA_ADVERTISE_IP", "203.0.113.7"),
            ],
            || {
                let cfg = Config::from_env().expect("valid env with media");
                assert_eq!(
                    cfg.media_bind.map(|s| s.to_string()),
                    Some("0.0.0.0:43210".to_owned())
                );
                assert_eq!(
                    cfg.media_advertise_ip,
                    Some("203.0.113.7".parse::<IpAddr>().unwrap())
                );
            },
        );
    }

    /// An unspecified media bind without an advertise IP is
    /// FATAL. This is the configuration production shipped
    /// with: str0m refuses a `0.0.0.0` host candidate, the
    /// refusal was swallowed, and every SDP answer went out
    /// with no SFU candidate — media could never connect.
    #[test]
    fn unspecified_media_bind_without_advertise_ip_is_fatal() {
        for bind in ["0.0.0.0:43210", "[::]:43210"] {
            with_env(
                &[
                    ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                    ("LAKKO_SFU_MEDIA_BIND", bind),
                ],
                || {
                    let err = Config::from_env().unwrap_err();
                    assert!(
                        matches!(
                            err,
                            ConfigError::RequiredBy {
                                name: "LAKKO_SFU_MEDIA_ADVERTISE_IP",
                                ..
                            }
                        ),
                        "bind {bind}: expected RequiredBy, got {err:?}"
                    );
                },
            );
        }
    }

    /// An empty advertise value is "unset", so it does not
    /// rescue an unspecified bind either.
    #[test]
    fn empty_advertise_ip_does_not_rescue_an_unspecified_bind() {
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_MEDIA_BIND", "0.0.0.0:43210"),
                ("LAKKO_SFU_MEDIA_ADVERTISE_IP", ""),
            ],
            || {
                let err = Config::from_env().unwrap_err();
                assert!(matches!(err, ConfigError::RequiredBy { .. }));
            },
        );
    }

    /// A concrete media bind without an advertise IP
    /// advertises the bind IP — unchanged behaviour for a
    /// bind such as the local stack's LAN address.
    #[test]
    fn concrete_media_bind_without_advertise_ip_advertises_the_bind_ip() {
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_MEDIA_BIND", "192.168.1.20:43210"),
            ],
            || {
                let cfg = Config::from_env().expect("concrete bind is valid alone");
                assert_eq!(
                    cfg.media_advertise_ip,
                    Some("192.168.1.20".parse::<IpAddr>().unwrap())
                );
            },
        );
    }

    /// An advertise IP that can never be an ICE host
    /// candidate is refused at load, naming the variable.
    #[test]
    fn unusable_advertise_ip_is_rejected() {
        for bad in [
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "169.254.10.20",
            "::",
            "ff02::1",
            "fe80::1",
        ] {
            let bind = if bad.contains(':') {
                "[::]:43210"
            } else {
                "0.0.0.0:43210"
            };
            with_env(
                &[
                    ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                    ("LAKKO_SFU_MEDIA_BIND", bind),
                    ("LAKKO_SFU_MEDIA_ADVERTISE_IP", bad),
                ],
                || {
                    let err = Config::from_env().unwrap_err();
                    assert!(
                        matches!(
                            err,
                            ConfigError::Invalid {
                                name: "LAKKO_SFU_MEDIA_ADVERTISE_IP",
                                ..
                            }
                        ),
                        "advertise {bad}: expected Invalid, got {err:?}"
                    );
                },
            );
        }
    }

    /// A malformed advertise value (not an IP, or an IP with
    /// a port) is refused rather than silently ignored.
    #[test]
    fn malformed_advertise_ip_is_rejected() {
        for bad in ["not-an-ip", "203.0.113.7:43210"] {
            with_env(
                &[
                    ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                    ("LAKKO_SFU_MEDIA_BIND", "0.0.0.0:43210"),
                    ("LAKKO_SFU_MEDIA_ADVERTISE_IP", bad),
                ],
                || {
                    let err = Config::from_env().unwrap_err();
                    assert!(matches!(
                        err,
                        ConfigError::Invalid {
                            name: "LAKKO_SFU_MEDIA_ADVERTISE_IP",
                            ..
                        }
                    ));
                },
            );
        }
    }

    /// An advertise IP of the other address family from the
    /// bind is refused at load.
    #[test]
    fn advertise_ip_family_mismatch_is_rejected() {
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_MEDIA_BIND", "0.0.0.0:43210"),
                ("LAKKO_SFU_MEDIA_ADVERTISE_IP", "2001:db8::7"),
            ],
            || {
                let err = Config::from_env().unwrap_err();
                assert!(matches!(
                    err,
                    ConfigError::Invalid {
                        name: "LAKKO_SFU_MEDIA_ADVERTISE_IP",
                        ..
                    }
                ));
            },
        );
    }

    /// A concrete but unusable media bind (link-local)
    /// without an advertise IP is refused, naming the bind.
    #[test]
    fn unusable_concrete_media_bind_is_rejected() {
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_MEDIA_BIND", "169.254.1.1:43210"),
            ],
            || {
                let err = Config::from_env().unwrap_err();
                assert!(matches!(
                    err,
                    ConfigError::Invalid {
                        name: "LAKKO_SFU_MEDIA_BIND",
                        ..
                    }
                ));
            },
        );
    }

    /// A malformed `LAKKO_SFU_MEDIA_BIND` MUST fail loud —
    /// silently degrading to `None` would mask a production
    /// typo as a non-WebRTC SFU and surface as confused
    /// client-side ICE failures.
    #[test]
    fn media_bind_invalid_socket_rejected() {
        with_env(
            &[
                ("LAKKO_SFU_JWKS_URL", "https://a/x"),
                ("LAKKO_SFU_MEDIA_BIND", "not-a-socket"),
            ],
            || {
                let err = Config::from_env().unwrap_err();
                assert!(matches!(
                    err,
                    ConfigError::Invalid {
                        name: "LAKKO_SFU_MEDIA_BIND",
                        ..
                    }
                ));
            },
        );
    }
}
