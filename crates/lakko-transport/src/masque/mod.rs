// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MASQUE / CONNECT-UDP (RFC 9298) — Phase 5 G5-2.
//!
//! Tunnels UDP datagrams inside HTTP/3 so the outer bytes
//! look like ordinary HTTPS to a censor on path. Three
//! standards interact:
//!
//! - [RFC 9298](https://www.rfc-editor.org/rfc/rfc9298.html)
//!   defines the `connect-udp` extended-CONNECT method.
//! - [RFC 9297](https://www.rfc-editor.org/rfc/rfc9297.html)
//!   defines HTTP Datagrams — the framing every CONNECT-UDP
//!   payload uses.
//! - [RFC 9220](https://www.rfc-editor.org/rfc/rfc9220.html)
//!   defines extended CONNECT itself.
//!
//! ## What this module ships now (G5-2 foundation)
//!
//! - [`MasqueConfig`] — bootstrap-config descriptor with
//!   eager parse-time validation (proxy URL must be
//!   `https://`, inner target must be `host:port`,
//!   bearer string if present must be non-empty).
//! - [`request::ConnectUdpRequest`] —  builder that
//!   produces the `(method, scheme, authority, path,
//!   headers)` tuple a future h3 client wires up against
//!   `:method = CONNECT`, `:protocol = connect-udp` per
//!   RFC 9298 §3. URI path follows the spec's URI template
//!   `/.well-known/masque/udp/{target_host}/{target_port}/`.
//! - [`datagram::encode_udp_payload`] /
//!   [`datagram::decode_udp_payload`] — HTTP-Datagram
//!   wire-format codec per RFC 9297 §3 + RFC 9298 §3
//!   (variable-length Context ID — always 0 for
//!   CONNECT-UDP — followed by the UDP payload bytes).
//! - [`MasqueTransport`] — `Transport` impl. `new` validates
//!   the config eagerly; `connect` opens a quinn endpoint
//!   (TLS 1.3, ALPN `h3`), runs the extended-CONNECT-UDP
//!   request, awaits the `2xx`, and returns a connection
//!   that sends and receives UDP payloads over QUIC
//!   datagrams. The h3 `SendRequest` handle is held for the
//!   tunnel's lifetime — dropping it closes the whole
//!   HTTP/3 connection, which is a real bug this module
//!   once had and its loopback e2e caught.
//! - [`MasqueListener`] — the server half: a quinn server
//!   accepting extended CONNECT-UDP and answering `200`.
//!
//! ## What is NOT here, and one thing that will not be
//!
//! - **Relaying a reliable service over this transport.**
//!   `MasqueListener` is deliberately **not** wired into
//!   `services/obfs-proxy`. CONNECT-UDP carries unreliable,
//!   unordered datagrams: a response datagram is dropped on
//!   close, never retransmitted and never flushed ahead of
//!   `CONNECTION_CLOSE`, so relaying a reliable internal
//!   HTTP service over it does not work. The ignored
//!   `masque_tunnel_relays_http_request_to_internal_origin`
//!   in `services/obfs-proxy/tests/loopback_e2e.rs` records
//!   that conclusion; it is a design finding, not a missing
//!   dependency. MASQUE is for tunnelling UDP. The three
//!   reliable-stream transports (REALITY, Shadowsocks-2022,
//!   WebTransport) cover the proxy's use case.
//! - **ECH.** No ECH surface on this transport, unlike
//!   [`crate::http`] and [`crate::webtransport_pt`].
//! - **Re-key + idle / migration semantics.** Still out of
//!   scope.
//!
//! ## Pinned audit invariant
//!
//! - **T-15** — MASQUE config + CONNECT-UDP request +
//!   HTTP Datagram payload codec are spec-aligned and
//!   length-bounded. See `docs/security/transport.md`.

pub mod config;
pub mod datagram;
pub mod request;
mod transport;

pub use config::{
    MasqueBearer, MasqueConfig, MasqueConfigError, MasqueInnerTarget, MasqueProxyUrl,
};
pub use datagram::{
    decode_udp_payload, encode_udp_payload, DatagramError, CONNECT_UDP_CONTEXT_ID,
    MAX_DATAGRAM_PAYLOAD,
};
pub use request::{
    build_connect_udp_request, ConnectUdpRequest, MASQUE_PATH_PREFIX, PROTOCOL_CONNECT_UDP,
};
pub use transport::{MasqueListener, MasqueTransport};
