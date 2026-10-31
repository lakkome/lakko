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
pub mod byte_stream;
pub mod doh_fetch;
pub mod ech;
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
pub mod selector;
pub mod shadowsocks;
pub mod transport;
pub mod tunnel_http;
pub mod webtransport_pt;
pub mod ws_tunnel;

pub use aead_stream::{
    AeadStreamError, AeadStreamReader, AeadStreamWriter, ALG as AEAD_STREAM_ALG,
    KEY_LEN_PUB as AEAD_STREAM_KEY_LEN, LENGTH_PREFIX_LEN, MAX_CHUNK as AEAD_STREAM_MAX_CHUNK,
    NONCE_LEN_PUB as AEAD_STREAM_NONCE_LEN, TAG_LEN_PUB as AEAD_STREAM_TAG_LEN,
};
pub use byte_stream::ByteStreamConnection;
pub use doh_fetch::{DohError, DohFetcher, DOH_ENDPOINTS, PER_LEG_TIMEOUT};
pub use ech::{
    parse_ech_config_list, EchConfig, EchConfigList, EchError, HpkeSymmetricCipherSuite,
    ECH_VERSION_DRAFT_13, MAX_ECH_CONFIG_LIST_LEN, MAX_INNER_CONFIGS, MAX_INNER_CONFIG_LEN,
};
pub use endpoint::Endpoint;
pub use error::{Error, Result};
pub use http::{HttpError, HttpResult, HttpTransport, HttpTransportConfig};
pub use masque::{
    build_connect_udp_request, decode_udp_payload, encode_udp_payload, ConnectUdpRequest,
    DatagramError, MasqueBearer, MasqueConfig, MasqueConfigError, MasqueInnerTarget,
    MasqueListener, MasqueProxyUrl, MasqueTransport, CONNECT_UDP_CONTEXT_ID, MASQUE_PATH_PREFIX,
    MAX_DATAGRAM_PAYLOAD, PROTOCOL_CONNECT_UDP,
};
pub use pinning::{PinError, PinnedVerifier, SpkiHash, SpkiPinSet};
pub use pluggable::{
    race_select, HappyEyeballs, PluggableCandidate, PluggableError, PluggableKind, PluggableResult,
    PluggableTransport,
};
pub use reality::{
    build_reality_server_config, compute_authenticator as compute_reality_authenticator,
    compute_server_response, drive_server_handshake, extract_client_hello_session_id,
    extract_server_hello_random, make_self_signed_cert, patch_client_hello_session_id,
    patch_server_hello_random, verify_authenticator as verify_reality_authenticator,
    verify_server_response, AcceptedClient, RealityAuthError, RealityAuthenticator, RealityConfig,
    RealityConfigError, RealityConnection, RealityListener, RealityServerConnection,
    RealityServerDriverError, RealityServerDriverResult, RealityShortId, RealityTagSecret,
    RealityTimestamp, RealityTransport, AUTHENTICATOR_LEN as REALITY_AUTHENTICATOR_LEN,
    HKDF_LABEL as REALITY_HKDF_LABEL, SERVER_RESPONSE_LEN as REALITY_SERVER_RESPONSE_LEN,
    SHORT_ID_LEN as REALITY_SHORT_ID_LEN, TAG_SECRET_LEN as REALITY_TAG_SECRET_LEN,
    TIMESTAMP_LEN as REALITY_TIMESTAMP_LEN,
};
pub use rendezvous::{
    BootstrapConfig, RendezvousError, RendezvousResult, TransportSpec, ISSUED_AT_CLOCK_SKEW_SECS,
    SCHEMA_VERSION,
};
pub use selector::{BootstrapDialer, SelectorError};
pub use shadowsocks::{
    accept_inbound as shadowsocks_accept_inbound, psk_from_b64 as shadowsocks_psk_from_b64,
    ReplayWindow as ShadowsocksReplayWindow, ShadowsocksConfig, ShadowsocksConnection,
    ShadowsocksHeaderError, ShadowsocksListener, ShadowsocksTransport,
};
pub use tokio_tungstenite::tungstenite;
pub use transport::{Connection, Listener, Transport, MAX_FRAME_BYTES};
pub use tunnel_http::{
    tunneled_request, tunneled_request_via_dialer, TunnelHttpError, TunneledRequest,
    TunneledResponse, MAX_RESPONSE_BYTES,
};
pub use webtransport_pt::{
    ParsedConfig as WebTransportParsedConfig, WebTransportConfig, WebTransportConfigError,
    WebTransportListener, WebTransportPt,
};
pub use ws_tunnel::{tunneled_websocket, TunneledWebSocket, WsTunnelError};
