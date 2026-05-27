// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! CONNECT-UDP request builder per RFC 9298 §3.
//!
//! Emits the `(method, scheme, authority, path, headers)`
//! tuple a future h3 client wires through
//! `:method = CONNECT`, `:protocol = connect-udp`,
//! `:scheme = https`, `:authority = <proxy host:port>`,
//! `:path = /.well-known/masque/udp/{host}/{port}/`.
//! Headers include `capsule-protocol: ?1` (RFC 9297 §3.4)
//! and optional `authorization: Bearer <token>`.
//!
//! Per RFC 3986, the `host` and `port` URI components MUST
//! be percent-encoded. The builder handles the percent-
//! encoding for the host (DNS names + IPv6 literals);
//! ports are bounded `0..=65535` so they emit as-is.

use crate::masque::config::{MasqueBearer, MasqueInnerTarget, MasqueProxyUrl};

/// Value of the `:protocol` pseudo-header for CONNECT-UDP,
/// per RFC 9298 §3.
pub const PROTOCOL_CONNECT_UDP: &str = "connect-udp";

/// URI template prefix for the CONNECT-UDP path. Followed
/// by `{target_host}/{target_port}/`.
pub const MASQUE_PATH_PREFIX: &str = "/.well-known/masque/udp/";

/// CONNECT-UDP request shape. The fields map 1-to-1 onto
/// the HTTP/3 pseudo-headers + regular headers an h3 client
/// would emit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectUdpRequest {
    /// `:method` — always `"CONNECT"` per RFC 9298 §3.
    pub method: &'static str,
    /// `:protocol` — always [`PROTOCOL_CONNECT_UDP`].
    pub protocol: &'static str,
    /// `:scheme` — always `"https"`.
    pub scheme: &'static str,
    /// `:authority` — `host` or `host:port` of the proxy.
    pub authority: String,
    /// `:path` — `/.well-known/masque/udp/{target_host}/{target_port}/`
    /// with percent-encoded host.
    pub path: String,
    /// Regular headers: `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
}

/// Build the CONNECT-UDP request from a validated proxy +
/// inner target + optional bearer.
///
/// The result is wire-format-ready: the caller hands it to
/// an h3 client which serialises the pseudo-headers as
/// HTTP/3 frames over a fresh request stream.
#[must_use]
pub fn build_connect_udp_request(
    proxy: &MasqueProxyUrl,
    target: &MasqueInnerTarget,
    bearer: Option<&MasqueBearer>,
) -> ConnectUdpRequest {
    let authority = if proxy.port == 443 {
        proxy.host.clone()
    } else {
        format!("{}:{}", proxy.host, proxy.port)
    };

    // Percent-encode the host. IPv6 literals need their `:`
    // escaped — RFC 9298 §3 demands the URI template be
    // exactly `{target_host}/{target_port}/`, and `{target_host}`
    // is a level-1 URI Template variable per RFC 6570 §3.2.2
    // (reserved chars get percent-encoded).
    let encoded_host = percent_encode_path_segment(&target.host);
    let path = format!("{MASQUE_PATH_PREFIX}{encoded_host}/{}/", target.port);

    let mut headers = vec![
        // RFC 9297 §3.4 — capsule-protocol structured header.
        // Value `?1` is the structured-fields literal for
        // boolean true, indicating the client understands
        // HTTP Datagrams.
        ("capsule-protocol".to_string(), "?1".to_string()),
    ];
    if let Some(b) = bearer {
        headers.push(("authorization".to_string(), format!("Bearer {}", b.0)));
    }

    ConnectUdpRequest {
        method: "CONNECT",
        protocol: PROTOCOL_CONNECT_UDP,
        scheme: "https",
        authority,
        path,
        headers,
    }
}

/// Percent-encode a single URI path segment. Pragmatic — we
/// escape every byte that is NOT in the RFC 3986 §2.3
/// "unreserved" set, except for `.` and `-` which the
/// `target_host` URI Template explicitly allows.
fn percent_encode_path_segment(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_uri_unreserved(b) {
            out.push(b as char);
        } else {
            // Write! into the existing buffer instead of
            // allocating a fresh String per byte.
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// RFC 3986 §2.3 `unreserved` plus `.` and `-` (already in
/// `unreserved`). Inlined rather than pulling `percent-encoding`
/// as a dep for one call site.
const fn is_uri_unreserved(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::masque::config::MasqueConfig;

    fn parts() -> (MasqueProxyUrl, MasqueInnerTarget, Option<MasqueBearer>) {
        MasqueConfig {
            proxy_url: "https://masque.example.com/".into(),
            inner_target: "lakko.me:443".into(),
            bearer: Some("opaque-token".into()),
        }
        .parse()
        .unwrap()
    }

    #[test]
    fn happy_path_request() {
        let (proxy, target, bearer) = parts();
        let req = build_connect_udp_request(&proxy, &target, bearer.as_ref());
        assert_eq!(req.method, "CONNECT");
        assert_eq!(req.protocol, "connect-udp");
        assert_eq!(req.scheme, "https");
        // Port 443 elides from `:authority`.
        assert_eq!(req.authority, "masque.example.com");
        assert_eq!(req.path, "/.well-known/masque/udp/lakko.me/443/");
        assert_eq!(req.headers.len(), 2);
        assert_eq!(req.headers[0].0, "capsule-protocol");
        assert_eq!(req.headers[0].1, "?1");
        assert_eq!(req.headers[1].0, "authorization");
        assert_eq!(req.headers[1].1, "Bearer opaque-token");
    }

    #[test]
    fn no_bearer_yields_no_authorization_header() {
        let (proxy, target, _) = parts();
        let req = build_connect_udp_request(&proxy, &target, None);
        assert_eq!(req.headers.len(), 1);
        assert!(req.headers.iter().all(|(n, _)| n != "authorization"));
    }

    #[test]
    fn custom_proxy_port_in_authority() {
        let cfg = MasqueConfig {
            proxy_url: "https://masque.example.com:8443/".into(),
            inner_target: "lakko.me:443".into(),
            bearer: None,
        };
        let (proxy, target, _) = cfg.parse().unwrap();
        let req = build_connect_udp_request(&proxy, &target, None);
        assert_eq!(req.authority, "masque.example.com:8443");
    }

    #[test]
    fn ipv6_inner_target_percent_encoded() {
        let cfg = MasqueConfig {
            proxy_url: "https://masque.example.com/".into(),
            inner_target: "[2001:db8::1]:8443".into(),
            bearer: None,
        };
        let (proxy, target, _) = cfg.parse().unwrap();
        let req = build_connect_udp_request(&proxy, &target, None);
        // IPv6 colons + the colon between groups percent-encode to %3A.
        assert!(
            req.path.contains("2001%3Adb8%3A%3A1"),
            "ipv6 host not encoded: {}",
            req.path
        );
        assert!(req.path.ends_with("/8443/"));
    }

    #[test]
    fn dns_host_passes_through_unchanged() {
        let cfg = MasqueConfig {
            proxy_url: "https://masque.example.com/".into(),
            inner_target: "lakko.me:443".into(),
            bearer: None,
        };
        let (proxy, target, _) = cfg.parse().unwrap();
        let req = build_connect_udp_request(&proxy, &target, None);
        assert_eq!(req.path, "/.well-known/masque/udp/lakko.me/443/");
    }

    #[test]
    fn percent_encode_only_reserved_bytes() {
        assert_eq!(
            percent_encode_path_segment("abc-XYZ_123.~"),
            "abc-XYZ_123.~"
        );
        assert_eq!(percent_encode_path_segment("a b"), "a%20b");
        assert_eq!(percent_encode_path_segment("a/b"), "a%2Fb");
    }
}
