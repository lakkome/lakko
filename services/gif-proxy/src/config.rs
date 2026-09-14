// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Environment configuration.
//!
//! Every value is read through a lookup function so the parser is unit-tested
//! without touching the process environment. The upstream target (GIPHY's API
//! base URL, the media host allowlist, the request timeout) is deliberately
//! NOT an environment variable: production always points at GIPHY, and only
//! tests construct a different [`UpstreamSettings`].

use std::net::SocketAddr;
use std::time::Duration;

use lakko_auth_token::TokenSecret;

use crate::giphy::MediaHostPolicy;

/// Default listen address. 8091 is unused by every other Lakko service
/// (auth 8081, ds 8082, storage 8083, discovery 8084, push-gw 8085,
/// channels 8086/8090, transparency 8087, witness + obfs-proxy control 8088,
/// CI witnesses 8089/8090).
pub(crate) const DEFAULT_BIND: &str = "127.0.0.1:8091";

/// GIPHY's GIF API root. The routes append `/trending`, `/search` and `/{id}`.
pub(crate) const GIPHY_API_BASE: &str = "https://api.giphy.com/v1/gifs";

/// Total budget for one upstream request, body included.
pub(crate) const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);

/// Search + trending: sustained requests per minute per account. Chosen, not
/// measured: a debounced search box sends one query per pause in typing.
pub(crate) const DEFAULT_SEARCH_RATE_PER_MINUTE: u32 = 30;
/// Search + trending: burst size per account.
pub(crate) const DEFAULT_SEARCH_BURST: u32 = 10;
/// Media: sustained requests per minute per account. Chosen, not measured:
/// one results page is 24 previews, so this is roughly a page every 2.4 s.
pub(crate) const DEFAULT_MEDIA_RATE_PER_MINUTE: u32 = 600;
/// Media: burst size per account — five pages of previews scrolled at once.
pub(crate) const DEFAULT_MEDIA_BURST: u32 = 120;

/// Upper bound on any configured rate or burst value.
const MAX_RATE_VALUE: u32 = 100_000;

/// A token-bucket shape: `per_minute` sustained, `burst` at most at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateLimitConfig {
    pub(crate) per_minute: u32,
    pub(crate) burst: u32,
}

/// Where the proxy sends its outbound requests. Not read from the environment.
#[derive(Debug, Clone)]
pub(crate) struct UpstreamSettings {
    /// GIPHY API root, without a trailing slash.
    pub(crate) api_base: String,
    /// Which hosts a media fetch may reach.
    pub(crate) media_hosts: MediaHostPolicy,
    /// Total per-request timeout.
    pub(crate) timeout: Duration,
}

impl UpstreamSettings {
    /// The only upstream a running service ever uses.
    pub(crate) fn production() -> Self {
        Self {
            api_base: GIPHY_API_BASE.to_owned(),
            media_hosts: MediaHostPolicy::Giphy,
            timeout: UPSTREAM_TIMEOUT,
        }
    }
}

/// Resolved service configuration. Deliberately not `Debug`: it holds the
/// GIPHY API key and the session-token secret.
pub(crate) struct Config {
    pub(crate) bind: SocketAddr,
    pub(crate) session_token_secret: TokenSecret,
    /// `None` when `LAKKO_GIF_GIPHY_API_KEY` is unset or blank; every `/gifs`
    /// route then answers 503.
    pub(crate) giphy_api_key: Option<String>,
    pub(crate) search_rate: RateLimitConfig,
    pub(crate) media_rate: RateLimitConfig,
    pub(crate) upstream: UpstreamSettings,
}

/// Why the environment could not be turned into a [`Config`]. The messages
/// name the variable and never echo a secret value.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    #[error("LAKKO_GIF_PROXY_BIND is not a valid host:port")]
    InvalidBind,
    #[error(
        "LAKKO_SESSION_TOKEN_SECRET must be set (base64-encoded HMAC-SHA256 key, >= 32 bytes after decode)"
    )]
    MissingSessionSecret,
    #[error("LAKKO_SESSION_TOKEN_SECRET is not valid base64 of at least 32 bytes")]
    InvalidSessionSecret,
    #[error("{0} must be an integer between 1 and 100000")]
    InvalidRate(&'static str),
}

impl Config {
    /// Read the configuration from the process environment.
    pub(crate) fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Read the configuration through `lookup` (the testable core).
    pub(crate) fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let non_blank = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };

        let bind = non_blank("LAKKO_GIF_PROXY_BIND")
            .unwrap_or_else(|| DEFAULT_BIND.to_owned())
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::InvalidBind)?;

        let secret_raw =
            non_blank("LAKKO_SESSION_TOKEN_SECRET").ok_or(ConfigError::MissingSessionSecret)?;
        let session_token_secret =
            TokenSecret::from_base64(&secret_raw).map_err(|_| ConfigError::InvalidSessionSecret)?;

        let giphy_api_key = non_blank("LAKKO_GIF_GIPHY_API_KEY");

        let rate = |key: &'static str, default: u32| -> Result<u32, ConfigError> {
            non_blank(key).map_or(Ok(default), |v| {
                v.parse::<u32>()
                    .ok()
                    .filter(|n| (1..=MAX_RATE_VALUE).contains(n))
                    .ok_or(ConfigError::InvalidRate(key))
            })
        };

        let search_rate = RateLimitConfig {
            per_minute: rate(
                "LAKKO_GIF_SEARCH_RATE_PER_MINUTE",
                DEFAULT_SEARCH_RATE_PER_MINUTE,
            )?,
            burst: rate("LAKKO_GIF_SEARCH_BURST", DEFAULT_SEARCH_BURST)?,
        };
        let media_rate = RateLimitConfig {
            per_minute: rate(
                "LAKKO_GIF_MEDIA_RATE_PER_MINUTE",
                DEFAULT_MEDIA_RATE_PER_MINUTE,
            )?,
            burst: rate("LAKKO_GIF_MEDIA_BURST", DEFAULT_MEDIA_BURST)?,
        };

        Ok(Self {
            bind,
            session_token_secret,
            giphy_api_key,
            search_rate,
            media_rate,
            upstream: UpstreamSettings::production(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 32 bytes of 0x42, base64.
    const SECRET_B64: &str = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=";

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn defaults_apply_when_only_the_secret_is_set() {
        let cfg = Config::from_lookup(lookup(&[("LAKKO_SESSION_TOKEN_SECRET", SECRET_B64)]))
            .expect("config");
        assert_eq!(cfg.bind.to_string(), DEFAULT_BIND);
        assert!(cfg.giphy_api_key.is_none());
        assert_eq!(
            cfg.search_rate,
            RateLimitConfig {
                per_minute: DEFAULT_SEARCH_RATE_PER_MINUTE,
                burst: DEFAULT_SEARCH_BURST
            }
        );
        assert_eq!(
            cfg.media_rate,
            RateLimitConfig {
                per_minute: DEFAULT_MEDIA_RATE_PER_MINUTE,
                burst: DEFAULT_MEDIA_BURST
            }
        );
        assert_eq!(cfg.upstream.api_base, GIPHY_API_BASE);
        assert_eq!(cfg.upstream.media_hosts, MediaHostPolicy::Giphy);
        assert_eq!(cfg.upstream.timeout, UPSTREAM_TIMEOUT);
    }

    #[test]
    fn explicit_values_are_parsed() {
        let cfg = Config::from_lookup(lookup(&[
            ("LAKKO_SESSION_TOKEN_SECRET", SECRET_B64),
            ("LAKKO_GIF_PROXY_BIND", "0.0.0.0:9999"),
            ("LAKKO_GIF_GIPHY_API_KEY", "  abcDEF123  "),
            ("LAKKO_GIF_SEARCH_RATE_PER_MINUTE", "12"),
            ("LAKKO_GIF_SEARCH_BURST", "3"),
            ("LAKKO_GIF_MEDIA_RATE_PER_MINUTE", "100"),
            ("LAKKO_GIF_MEDIA_BURST", "40"),
        ]))
        .expect("config");
        assert_eq!(cfg.bind.to_string(), "0.0.0.0:9999");
        assert_eq!(cfg.giphy_api_key.as_deref(), Some("abcDEF123"));
        assert_eq!(
            cfg.search_rate,
            RateLimitConfig {
                per_minute: 12,
                burst: 3
            }
        );
        assert_eq!(
            cfg.media_rate,
            RateLimitConfig {
                per_minute: 100,
                burst: 40
            }
        );
    }

    #[test]
    fn a_blank_api_key_means_not_configured() {
        let cfg = Config::from_lookup(lookup(&[
            ("LAKKO_SESSION_TOKEN_SECRET", SECRET_B64),
            ("LAKKO_GIF_GIPHY_API_KEY", "   "),
        ]))
        .expect("config");
        assert!(cfg.giphy_api_key.is_none());
    }

    #[test]
    fn a_missing_or_invalid_secret_is_refused() {
        assert!(matches!(
            Config::from_lookup(lookup(&[])),
            Err(ConfigError::MissingSessionSecret)
        ));
        assert!(matches!(
            Config::from_lookup(lookup(&[("LAKKO_SESSION_TOKEN_SECRET", "c2hvcnQ=")])),
            Err(ConfigError::InvalidSessionSecret)
        ));
    }

    #[test]
    fn an_invalid_bind_is_refused() {
        assert!(matches!(
            Config::from_lookup(lookup(&[
                ("LAKKO_SESSION_TOKEN_SECRET", SECRET_B64),
                ("LAKKO_GIF_PROXY_BIND", "not-an-address"),
            ])),
            Err(ConfigError::InvalidBind)
        ));
    }

    #[test]
    fn out_of_range_rates_are_refused_by_name() {
        for (key, value) in [
            ("LAKKO_GIF_SEARCH_RATE_PER_MINUTE", "0"),
            ("LAKKO_GIF_SEARCH_BURST", "-1"),
            ("LAKKO_GIF_MEDIA_RATE_PER_MINUTE", "100001"),
            ("LAKKO_GIF_MEDIA_BURST", "lots"),
        ] {
            let result = Config::from_lookup(lookup(&[
                ("LAKKO_SESSION_TOKEN_SECRET", SECRET_B64),
                (key, value),
            ]));
            match result {
                Err(ConfigError::InvalidRate(named)) => assert_eq!(named, key),
                _ => panic!("{key}={value} should be refused"),
            }
        }
    }

    #[test]
    fn config_errors_never_echo_the_secret() {
        let err = Config::from_lookup(lookup(&[("LAKKO_SESSION_TOKEN_SECRET", "c2hvcnQ=")]))
            .err()
            .expect("error");
        assert!(!err.to_string().contains("c2hvcnQ="));
    }
}
