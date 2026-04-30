// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Endpoint addressing for Lakko transports.
//!
//! An [`Endpoint`] uniquely identifies a peer. Phase 0 supports direct
//! socket-address endpoints; bootstrap and rendezvous-resolved endpoints
//! land in Phase 5 along with the anti-censorship transport stack
//! (see ADR-0004).

use std::fmt;
use std::net::SocketAddr;

/// A network destination a transport can connect to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// Direct IP + port. Used for tests, LAN deployments, and as the
    /// resolved form of higher-level endpoints.
    Direct(SocketAddr),

    /// Hostname + port; resolution happens at connect time. The default
    /// resolver is the OS DNS — anti-censorship overrides (DoH-based
    /// rendezvous) are wired later.
    Hostname {
        /// Hostname (used for SNI / certificate matching).
        host: String,
        /// Destination port.
        port: u16,
    },
}

impl Endpoint {
    /// Direct IP-address endpoint.
    #[must_use]
    pub const fn direct(addr: SocketAddr) -> Self {
        Self::Direct(addr)
    }

    /// Hostname-based endpoint.
    #[must_use]
    pub fn hostname(host: impl Into<String>, port: u16) -> Self {
        Self::Hostname {
            host: host.into(),
            port,
        }
    }

    /// Hostname for TLS SNI / certificate matching, if applicable.
    #[must_use]
    pub fn server_name(&self) -> Option<&str> {
        match self {
            Self::Hostname { host, .. } => Some(host.as_str()),
            Self::Direct(_) => None,
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct(addr) => write!(f, "{addr}"),
            Self::Hostname { host, port } => write!(f, "{host}:{port}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_endpoint_has_no_server_name() {
        let ep = Endpoint::direct("127.0.0.1:443".parse().unwrap());
        assert!(ep.server_name().is_none());
    }

    #[test]
    fn hostname_endpoint_exposes_sni() {
        let ep = Endpoint::hostname("lakko.app", 443);
        assert_eq!(ep.server_name(), Some("lakko.app"));
    }

    #[test]
    fn endpoint_display_matches_expected() {
        let direct = Endpoint::direct("127.0.0.1:8080".parse().unwrap());
        assert_eq!(direct.to_string(), "127.0.0.1:8080");
        let host = Endpoint::hostname("api.lakko.app", 443);
        assert_eq!(host.to_string(), "api.lakko.app:443");
    }
}
