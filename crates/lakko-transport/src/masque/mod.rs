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
//!   the config eagerly; `connect` still returns
//!   `NotImplemented` until the h3 + quinn wire-up sub-
//!   sprint (`S-5-masque-handshake`) lands.
//!
//! ## What is NOT here yet
//!
//! - **h3 + quinn integration.** Real connect requires
//!   opening a quinn HTTP/3 connection, building an
//!   extended-CONNECT request via the `h3` crate, and
//!   feeding `h3-datagram` frames through the resulting
//!   stream. The `h3-datagram` crate is at 0.0.2 (very
//!   early) on crates.io; a follow-up sprint either takes
//!   that as a dep or implements the QUIC DATAGRAM frame
//!   handling directly on top of `quinn`'s
//!   `Connection::send_datagram` API. Tracked as
//!   `S-5-masque-handshake`.
//! - **Authentication response handling.** Once the server
//!   returns its `2xx` response (per RFC 9298 §3), the
//!   client knows the tunnel is up. The response-handling
//!   path lands in the same handshake sub-sprint.
//! - **Re-key + idle / migration semantics.** Out of scope
//!   for the foundation sprint.
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
