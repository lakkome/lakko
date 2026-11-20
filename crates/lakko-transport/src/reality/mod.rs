// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! REALITY (xtls/REALITY) — Phase 5 G5-1.
//!
//! Censorship-resistance transport that hides traffic
//! inside a TLS handshake aimed at a high-traffic cover
//! site (e.g. `www.microsoft.com`). The censor sees a
//! TLS 1.3 ClientHello with the cover-site SNI, a
//! standards-shaped key_share, and a session_id that
//! looks like a normal random cache token. A
//! REALITY-aware server reads the session_id as an
//! authenticator MAC over a publisher-assigned
//! short identifier + a fresh timestamp; a non-aware
//! peer (active-prober) sees only opaque random bytes
//! and reaches a real reverse-proxy to the cover site.
//!
//! ## What this module ships now (G5-1 foundation)
//!
//! - [`RealityTagSecret`] / [`RealityShortId`] /
//!   [`RealityConfig`] — strongly-typed config bound to
//!   the bootstrap-rendezvous wire shape.
//! - [`authenticator::compute`] /
//!   [`authenticator::verify`] — the HKDF + HMAC primitive
//!   that produces / accepts the session_id slot, keyed by
//!   the publisher-distributed pre-shared `tag_secret`.
//!   Reproducible across implementations via the pinned
//!   [`authenticator::HKDF_LABEL`].
//! - `RealityTransport::connect` — a real dial: TCP
//!   connect, a `rustls::ClientConnection` pointed at the
//!   cover SNI, the authenticator injected into the
//!   session_id slot, [`driver::drive_handshake`], then a
//!   [`RealityConnection`] carrying length-prefixed frames
//!   inside the TLS tunnel.
//! - **Server side.** [`RealityListener`] +
//!   [`server_driver`] + [`RealityServerConnection`] — the
//!   accept half, verifying an inbound ClientHello's
//!   session_id against a `(short_id, tag_secret)`
//!   allow-list and answering with a mutual-auth HMAC in
//!   `ServerHello.Random`. Wired into `services/obfs-proxy`.
//!
//! ## What is NOT here
//!
//! - **Wire compatibility with upstream xtls/REALITY.**
//!   The authenticator is REALITY-shaped (same primitives,
//!   same audit posture) but pins its own canonical bytes
//!   via [`authenticator::HKDF_LABEL`]. Lakko is a closed
//!   ecosystem (ADR-0011), so interop with go-xray REALITY
//!   servers is deliberately not a goal.
//! - **Forward secrecy of the authenticator itself.** v2
//!   dropped v1's ECDH because rustls 0.23 exposes no
//!   TLS 1.3 ephemeral-X25519 hook; the cover TLS session
//!   keeps its own forward secrecy, and the residual is
//!   mitigated by rotating `tag_secret` through the
//!   bootstrap config's TTL.
//! - **Probe indistinguishability from the cover site.**
//!   An unauthenticated prober is refused by a connection
//!   *drop*, which is distinguishable from the cover site
//!   answering. Closing that needs a live cover-proxy.
//!
//! ## Pinned audit invariant
//!
//! - **T-13** — the REALITY v2 authenticator is
//!   HMAC-over-(`short_id` || timestamp) keyed by a
//!   publisher-distributed pre-shared `tag_secret`;
//!   timestamp window bounded; tag verified before any
//!   downstream byte is trusted; mutual auth via
//!   `ServerHello.Random`. See
//!   `docs/security/transport.md`.

pub mod authenticator;
pub(crate) mod cert;
pub mod config;
pub mod driver;
pub mod handshake;
pub mod listener;
pub(crate) mod rng;
pub mod server_driver;
pub mod server_stream;
pub mod stream;
mod transport;

pub use authenticator::{
    compute as compute_authenticator, compute_server_response, verify as verify_authenticator,
    verify_server_response, RealityAuthError, RealityAuthenticator, RealityTimestamp,
    AUTHENTICATOR_LEN, HKDF_LABEL, SERVER_RESPONSE_LEN, SERVER_RESPONSE_TAG, TIMESTAMP_LEN,
};
pub use config::{
    RealityConfig, RealityConfigError, RealityShortId, RealityTagSecret, SHORT_ID_LEN,
    TAG_SECRET_LEN,
};
pub use driver::{
    build_reality_client_config, build_reality_client_connection, drive_handshake,
    RealityDriverError, RealityDriverResult, MAX_HANDSHAKE_BUFFER_BYTES,
};
pub use handshake::{
    extract_client_hello_session_id, extract_server_hello_random, patch_client_hello_session_id,
    patch_server_hello_random, TlsWireError, RANDOM_LEN,
};
pub use listener::{RealityHandshaker, RealityListener};
pub use server_driver::{
    build_reality_server_config, drive_server_handshake, make_self_signed_cert, AcceptedClient,
    RealityServerDriverError, RealityServerDriverResult,
    MAX_HANDSHAKE_BUFFER_BYTES as REALITY_SERVER_MAX_HANDSHAKE_BUFFER_BYTES,
};
pub use server_stream::RealityServerConnection;
pub use stream::RealityConnection;
pub use transport::RealityTransport;
