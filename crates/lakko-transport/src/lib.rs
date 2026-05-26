// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Transport layer for Lakko.
//!
//! Two distinct surfaces, intentionally separate:
//!
//! ## Mesh / message-oriented (Phase 0.3)
//!
//! Datagram-style framing for direct client-to-client / client-to-DS
//! traffic that carries MLS application messages. Implementations:
//!
//! - [`Transport`], [`Listener`], [`Connection`] — abstract traits
//! - [`Endpoint`] — direct (sock-addr) + hostname targets
//! - [`in_memory`] — mpsc loopback for tests and local IPC
//! - [`quic`] — `quinn`-backed production transport
//!
//! Pluggable obfuscation transports (REALITY, Shadowsocks-2022,
//! MASQUE, obfs4) plug in as adapters satisfying [`Transport`] and
//! are selected at runtime via happy-eyeballs racing — wired in
//! Phase 5 per ADR-0004.
//!
//! ## HTTP REST (Phase 1.8)
//!
//! REST client for the back-end services (`auth`, `ds`, `storage`,
//! `discovery`, `push-gw`). Built on `reqwest` with a custom rustls
//! TLS config that does **SPKI pinning instead of system trust**:
//!
//! - [`http::HttpTransport`] — `post_json` / `get_json` /
//!   `get_bytes` over HTTPS with HTTP/2 negotiation.
//! - [`pinning::SpkiPinSet`] / [`pinning::PinnedVerifier`] — the pin
//!   set and the rustls verifier that gates every TLS handshake on
//!   it. Phase 5 will deliver pin updates as TUF-signed data through
//!   the bootstrap rendezvous service.

#![forbid(unsafe_code)]

pub mod aead_stream;
pub mod doh_fetch;
pub mod endpoint;
pub mod error;
pub mod http;
pub mod in_memory;
pub mod masque;
pub mod pinning;
pub mod pluggable;
pub mod quic;
pub mod reality;
pub mod rendezvous;
pub mod shadowsocks;
pub mod transport;
pub mod webtransport_pt;

pub use aead_stream::{
    AeadStreamError, AeadStreamReader, AeadStreamWriter, ALG as AEAD_STREAM_ALG,
    KEY_LEN_PUB as AEAD_STREAM_KEY_LEN, LENGTH_PREFIX_LEN, MAX_CHUNK as AEAD_STREAM_MAX_CHUNK,
    NONCE_LEN_PUB as AEAD_STREAM_NONCE_LEN, TAG_LEN_PUB as AEAD_STREAM_TAG_LEN,
};
pub use doh_fetch::{DohError, DohFetcher, DOH_ENDPOINTS, PER_LEG_TIMEOUT};
pub use endpoint::Endpoint;
pub use error::{Error, Result};
pub use http::{HttpError, HttpResult, HttpTransport, HttpTransportConfig};
pub use masque::{MasqueConfig, MasqueTransport};
pub use pinning::{PinError, PinnedVerifier, SpkiHash, SpkiPinSet};
pub use pluggable::{
    race_select, HappyEyeballs, PluggableCandidate, PluggableError, PluggableKind,
    PluggableResult, PluggableTransport,
};
pub use reality::{RealityConfig, RealityTransport};
pub use rendezvous::{
    BootstrapConfig, RendezvousError, RendezvousResult, TransportSpec,
    ISSUED_AT_CLOCK_SKEW_SECS, SCHEMA_VERSION,
};
pub use shadowsocks::{
    accept_inbound as shadowsocks_accept_inbound, psk_from_b64 as shadowsocks_psk_from_b64,
    ReplayWindow as ShadowsocksReplayWindow, ShadowsocksConfig, ShadowsocksConnection,
    ShadowsocksHeaderError, ShadowsocksListener, ShadowsocksTransport,
};
pub use transport::{Connection, Listener, Transport, MAX_FRAME_BYTES};
pub use webtransport_pt::{WebTransportConfig, WebTransportPt};
