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
//!   [`authenticator::verify`] — the ECDH + HKDF + HMAC
//!   primitive that produces / accepts the 32-byte
//!   session_id slot. Reproducible across implementations
//!   via the pinned label string.
//! - [`RealityTransport`] still returns
//!   `Error::NotImplemented` on `connect`, but with a
//!   pointed message — the TLS-handshake injection layer
//!   (rustls custom-ClientHello hooks) is the next
//!   sub-sprint.
//!
//! ## What is NOT here yet
//!
//! - **rustls ClientHello injection.** rustls 0.23's
//!   public API does not expose a session_id override
//!   hook. The wire-up sprint either (a) uses a custom
//!   TLS client built on `rustls::client::ClientConfig`
//!   plus the `dangerous_configuration` feature to clone
//!   and patch the ClientHello payload, or (b) waits on
//!   the `rustls-hooks` feature flag (tracking upstream).
//! - **Wire compatibility with upstream xtls/REALITY.**
//!   The authenticator landed here is REALITY-shaped
//!   (same primitives, same audit posture) but pins its
//!   own canonical bytes via the `"lakko/reality/v1"`
//!   HKDF label. Wire interop with go-xray REALITY
//!   servers is a separate sprint that aligns the exact
//!   byte layout to upstream Go source.
//! - **Server side.** This module only ships the client-
//!   side primitive. A REALITY server would land in a
//!   future SFU-frontend sprint.
//!
//! ## Pinned audit invariant
//!
//! - **T-13** — REALITY authenticator is ECDH + HKDF +
//!   HMAC-SHA-256 keyed by the X25519 shared secret;
//!   timestamp window bounded; tag verified before any
//!   downstream byte is trusted. See
//!   `docs/security/transport.md`.

pub mod authenticator;
pub mod config;
pub mod handshake;
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
pub use handshake::{
    extract_server_hello_random, patch_client_hello_session_id, TlsWireError, RANDOM_LEN,
};
pub use transport::RealityTransport;
