// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MASQUE bootstrap-config wire shape + eager validation.
//!
//! The signed rendezvous bootstrap config carries this
//! serialised. JSON field names are part of the protocol
//! surface; a rename breaks every previously-pinned
//! config.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// Failure modes the MASQUE config parser surfaces. Branch
/// on the variant; the message strings are
/// telemetry-friendly but not an attack surface.
#[derive(Debug, Error)]
pub enum MasqueConfigError {
    /// `proxy_url` was not parseable.
    #[error("proxy URL parse failed: {0}")]
    BadProxyUrl(String),
    /// `proxy_url` scheme was not `https`. MASQUE proxies
    /// must speak HTTPS — CONNECT-UDP rides on HTTP/3.
    #[error("proxy URL scheme must be https, got {got}")]
    NonHttpsProxyScheme {
        /// The offending scheme.
        got: String,
    },
    /// `proxy_url` had no host component.
    #[error("proxy URL has no host")]
    MissingProxyHost,
    /// `inner_target` is not in `host:port` form.
    #[error("inner_target must be host:port, got {got:?}")]
    BadInnerTarget {
        /// The offending value.
        got: String,
    },
    /// `inner_target` port did not parse as `u16`.
    #[error("inner_target port out of range: {port:?}")]
    BadInnerPort {
        /// The offending port substring.
        port: String,
    },
    /// `inner_target` host is empty.
    #[error("inner_target host is empty")]
    EmptyInnerHost,
    /// `bearer` was set but empty — refuse rather than
    /// silently send an `Authorization:` header with no
    /// value.
    #[error("bearer token is empty")]
    EmptyBearer,
}

/// Convenience alias.
pub type MasqueConfigResult<T> = core::result::Result<T, MasqueConfigError>;

/// Validated MASQUE proxy URL (scheme guaranteed `https`,
/// host non-empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MasqueProxyUrl {
    /// Parsed URL — every field is spec-conforming.
    pub url: Url,
    /// Hostname extracted from the URL for SNI / `:authority`.
    pub host: String,
    /// Port — defaults to 443 if the URL omitted it.
    pub port: u16,
}

/// Validated inner-target (the UDP destination the proxy
/// connects to on the client's behalf).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MasqueInnerTarget {
    /// Target host (DNS name or IP literal).
    pub host: String,
    /// Target port.
    pub port: u16,
}

/// Validated bearer token. Wrapped so a typo at a
/// call site cannot accidentally send an empty header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MasqueBearer(pub String);

/// Bootstrap-config descriptor for a MASQUE endpoint. JSON
/// shape pinned because the rendezvous bootstrap config the
/// client downloads carries this serialised.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MasqueConfig {
    /// H3 proxy URL — the CONNECT-UDP target.
    /// E.g. `https://masque.example.com/`.
    pub proxy_url: String,
    /// Inner UDP `host:port` the proxy should connect to
    /// on the client's behalf.
    pub inner_target: String,
    /// Optional bearer token for the proxy's `Authorization`
    /// header. `None` for open / IP-allowlisted proxies.
    /// If `Some`, the string must be non-empty (refused at
    /// parse time).
    pub bearer: Option<String>,
}

impl MasqueConfig {
    /// Lift the wire-form config into typed parts. Validates
    /// every constraint upfront so a bad bootstrap config
    /// cannot silently flow into the connect path.
    ///
    /// # Errors
    ///
    /// See [`MasqueConfigError`].
    pub fn parse(
        &self,
    ) -> MasqueConfigResult<(MasqueProxyUrl, MasqueInnerTarget, Option<MasqueBearer>)> {
        // ----- proxy URL -----
        let url = Url::parse(&self.proxy_url)
            .map_err(|e| MasqueConfigError::BadProxyUrl(e.to_string()))?;
        if url.scheme() != "https" {
            return Err(MasqueConfigError::NonHttpsProxyScheme {
                got: url.scheme().to_string(),
            });
        }
        let host = url
            .host_str()
            .ok_or(MasqueConfigError::MissingProxyHost)?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(443);

        // ----- inner target -----
        let (inner_host, inner_port) = split_host_port(&self.inner_target)?;
        if inner_host.is_empty() {
            return Err(MasqueConfigError::EmptyInnerHost);
        }

        // ----- bearer -----
        let bearer = if let Some(b) = self.bearer.as_deref() {
            if b.is_empty() {
                return Err(MasqueConfigError::EmptyBearer);
            }
            Some(MasqueBearer(b.to_string()))
        } else {
            None
        };

        Ok((
            MasqueProxyUrl { url, host, port },
            MasqueInnerTarget {
                host: inner_host,
                port: inner_port,
            },
            bearer,
        ))
    }
}

/// Split `host:port`. Supports IPv4 literal, DNS host, and
/// bracketed IPv6 (`[::1]:443`). Returns the host string +
/// parsed u16 port.
fn split_host_port(s: &str) -> MasqueConfigResult<(String, u16)> {
    // Bracketed IPv6 — `[host]:port`. The closing `]` may
    // appear anywhere; the port follows the `:` after it.
    if let Some(stripped) = s.strip_prefix('[') {
        let (host, rest) = stripped
            .split_once(']')
            .ok_or_else(|| MasqueConfigError::BadInnerTarget { got: s.to_string() })?;
        let port_str = rest
            .strip_prefix(':')
            .ok_or_else(|| MasqueConfigError::BadInnerTarget { got: s.to_string() })?;
        let port = port_str
            .parse::<u16>()
            .map_err(|_| MasqueConfigError::BadInnerPort {
                port: port_str.to_string(),
            })?;
        return Ok((host.to_string(), port));
    }
    // DNS host or IPv4 literal — split on the LAST `:` so
    // an accidental colon in a literal host name (which is
    // illegal anyway) still surfaces as a port-parse error.
    let (host, port_str) = s
        .rsplit_once(':')
        .ok_or_else(|| MasqueConfigError::BadInnerTarget { got: s.to_string() })?;
    let port = port_str
        .parse::<u16>()
        .map_err(|_| MasqueConfigError::BadInnerPort {
            port: port_str.to_string(),
        })?;
    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn happy_config() -> MasqueConfig {
        MasqueConfig {
            proxy_url: "https://masque.example.com/".into(),
            inner_target: "lakko.me:443".into(),
            bearer: Some("opaque-token".into()),
        }
    }

    #[test]
    fn parse_happy_path() {
        let cfg = happy_config();
        let (proxy, target, bearer) = cfg.parse().expect("happy parse");
        assert_eq!(proxy.host, "masque.example.com");
        assert_eq!(proxy.port, 443);
        assert_eq!(target.host, "lakko.me");
        assert_eq!(target.port, 443);
        assert_eq!(bearer, Some(MasqueBearer("opaque-token".into())));
    }

    #[test]
    fn parse_allows_no_bearer() {
        let mut cfg = happy_config();
        cfg.bearer = None;
        let (_, _, bearer) = cfg.parse().expect("no-bearer parse");
        assert!(bearer.is_none());
    }

    #[test]
    fn parse_rejects_non_https_proxy() {
        let mut cfg = happy_config();
        cfg.proxy_url = "http://example.com/".into();
        match cfg.parse() {
            Err(MasqueConfigError::NonHttpsProxyScheme { got }) => assert_eq!(got, "http"),
            other => panic!("expected NonHttpsProxyScheme, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_bad_proxy_url() {
        let mut cfg = happy_config();
        cfg.proxy_url = "not a url".into();
        assert!(matches!(
            cfg.parse(),
            Err(MasqueConfigError::BadProxyUrl(_))
        ));
    }

    #[test]
    fn parse_rejects_inner_target_without_port() {
        let mut cfg = happy_config();
        cfg.inner_target = "lakko.me".into();
        assert!(matches!(
            cfg.parse(),
            Err(MasqueConfigError::BadInnerTarget { .. })
        ));
    }

    #[test]
    fn parse_rejects_inner_target_bad_port() {
        let mut cfg = happy_config();
        cfg.inner_target = "lakko.me:abc".into();
        assert!(matches!(
            cfg.parse(),
            Err(MasqueConfigError::BadInnerPort { .. })
        ));
    }

    #[test]
    fn parse_rejects_empty_bearer() {
        let mut cfg = happy_config();
        cfg.bearer = Some(String::new());
        assert!(matches!(cfg.parse(), Err(MasqueConfigError::EmptyBearer)));
    }

    #[test]
    fn parse_accepts_bracketed_ipv6_inner_target() {
        let mut cfg = happy_config();
        cfg.inner_target = "[2001:db8::1]:8443".into();
        let (_, target, _) = cfg.parse().expect("ipv6 parse");
        assert_eq!(target.host, "2001:db8::1");
        assert_eq!(target.port, 8443);
    }

    #[test]
    fn parse_accepts_custom_proxy_port() {
        let mut cfg = happy_config();
        cfg.proxy_url = "https://masque.example.com:8443/".into();
        let (proxy, _, _) = cfg.parse().expect("custom port");
        assert_eq!(proxy.port, 8443);
    }

    #[test]
    fn config_round_trips_through_serde() {
        let cfg = happy_config();
        let json = serde_json::to_string(&cfg).expect("serialise");
        let back: MasqueConfig = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(cfg, back);
    }
}
