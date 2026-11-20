// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The obfs proxy's own counters ([ADR-0096] section 3).
//!
//! Labels name the transport and the outcome only, from the enums in this
//! file. Nothing here records a client address, a REALITY short id, an SNI
//! or a byte count of one tunnel.
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md

/// Inbound tunnels, by `transport` (`reality`, `shadowsocks`,
/// `webtransport`) and `outcome` (`accepted`; `refused`: the accept or the
/// obfs handshake failed, which is what an active probe produces).
pub(crate) const TUNNELS_TOTAL: &str = "lakko_obfs_proxy_tunnels_total";
/// Tunnels being relayed now, by `transport`.
pub(crate) const TUNNELS_ACTIVE: &str = "lakko_obfs_proxy_tunnels_active";
/// Relays that ended, by `outcome` (`closed`: either side closed;
/// `connect_failed`: the internal target refused the connection;
/// `internal_write_failed`: writing to the internal socket failed;
/// `tunnel_send_failed`: sending back into the tunnel failed).
pub(crate) const RELAYS_TOTAL: &str = "lakko_obfs_proxy_relays_total";

/// An obfs transport the proxy listens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// REALITY over TCP.
    Reality,
    /// Shadowsocks-2022 over TCP.
    Shadowsocks,
    /// WebTransport over QUIC.
    WebTransport,
}

impl Transport {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Reality => "reality",
            Self::Shadowsocks => "shadowsocks",
            Self::WebTransport => "webtransport",
        }
    }
}

/// How a relay ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayEnd {
    /// Either side closed.
    Closed,
    /// The internal target refused the connection.
    ConnectFailed,
    /// Writing to the internal socket failed.
    InternalWriteFailed,
    /// Sending back into the tunnel failed.
    TunnelSendFailed,
}

impl RelayEnd {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::ConnectFailed => "connect_failed",
            Self::InternalWriteFailed => "internal_write_failed",
            Self::TunnelSendFailed => "tunnel_send_failed",
        }
    }
}

/// Count an accepted tunnel.
pub fn tunnel_accepted(transport: Transport) {
    tunnel(transport, "accepted");
}

/// Count a refused tunnel (the accept or the obfs handshake failed).
pub fn tunnel_refused(transport: Transport) {
    tunnel(transport, "refused");
}

fn tunnel(transport: Transport, outcome: &'static str) {
    lakko_metrics::metrics::counter!(
        TUNNELS_TOTAL,
        "transport" => transport.as_str(),
        "outcome" => outcome,
    )
    .increment(1);
}

/// Counts one tunnel in [`TUNNELS_ACTIVE`] for as long as it lives. Held by
/// the task relaying the tunnel, so a relay that ends in any way, a panic or
/// a cancellation included, takes its count back.
#[must_use = "the tunnel counts as active only while the guard lives"]
pub struct ActiveTunnel(Transport);

impl ActiveTunnel {
    /// Count a tunnel of `transport` as active until the guard is dropped.
    pub fn start(transport: Transport) -> Self {
        lakko_metrics::metrics::gauge!(TUNNELS_ACTIVE, "transport" => transport.as_str())
            .increment(1.0);
        Self(transport)
    }
}

impl Drop for ActiveTunnel {
    fn drop(&mut self) {
        lakko_metrics::metrics::gauge!(TUNNELS_ACTIVE, "transport" => self.0.as_str())
            .decrement(1.0);
    }
}

/// Count a relay that ended.
pub(crate) fn relay_ended(end: RelayEnd) {
    lakko_metrics::metrics::counter!(RELAYS_TOTAL, "outcome" => end.as_str()).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSPORTS: [Transport; 3] = [
        Transport::Reality,
        Transport::Shadowsocks,
        Transport::WebTransport,
    ];
    const ENDS: [RelayEnd; 4] = [
        RelayEnd::Closed,
        RelayEnd::ConnectFailed,
        RelayEnd::InternalWriteFailed,
        RelayEnd::TunnelSendFailed,
    ];

    #[tokio::test]
    async fn every_series_renders_clean() {
        let metrics = lakko_metrics::install("obfs-proxy", "0.0.0").unwrap();
        for transport in TRANSPORTS {
            tunnel_accepted(transport);
            tunnel_refused(transport);
            let guard = ActiveTunnel::start(transport);
            drop(guard);
        }
        for end in ENDS {
            relay_ended(end);
        }
        let text = metrics.render();
        lakko_metrics::exposition::assert_clean(&text, &["transport"], &[]);
        for transport in TRANSPORTS {
            let t = transport.as_str();
            for outcome in ["accepted", "refused"] {
                let line = format!("{TUNNELS_TOTAL}{{transport=\"{t}\",outcome=\"{outcome}\"}}");
                assert!(text.contains(&line), "{line}\n{text}");
            }
            // Started and dropped: back at zero.
            let line = format!("{TUNNELS_ACTIVE}{{transport=\"{t}\"}} 0");
            assert!(text.contains(&line), "{line}\n{text}");
        }
        for end in ENDS {
            let line = format!("{RELAYS_TOTAL}{{outcome=\"{}\"}}", end.as_str());
            assert!(text.contains(&line), "{line}\n{text}");
        }
    }
}
