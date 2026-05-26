// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! REALITY (xtls/REALITY) — Phase 5 G5-1 scaffold per
//! [ADR-0004](../../../docs/adr/0004-anti-censorship.md).
//!
//! TLS-in-TLS under an attacker-chosen SNI; defeats GFW /
//! TSPU active probing + SNI-blocking. The Lakko-side
//! implementation reuses the upstream xtls / VLESS protocol
//! framing — this module is a stub that fails fast at
//! construction time until G5-1 lands the real handshake.
//!
//! ## What is here
//!
//! - [`RealityConfig`] — the serialisable descriptor that
//!   the rendezvous bootstrap config carries to the client.
//!   Fields match the upstream REALITY config (target SNI,
//!   short ID, public key, server names).
//! - [`RealityTransport`] — a no-op `Transport` impl whose
//!   `connect` returns
//!   `crate::error::Error::NotImplemented` so a misconfigured
//!   build path surfaces loudly rather than silently
//!   falling through.
//!
//! Real implementation rides on
//! `S-5-reality` in
//! [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../../../docs/ROADMAP-PHASE3-CLOSEOUT.md).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::transport::{Connection, Transport};

/// Bootstrap-config descriptor for a REALITY endpoint. JSON
/// shape pinned because the rendezvous bootstrap config the
/// client downloads carries this serialised; a renaming
/// breaks every previously-pinned config.
///
/// Field names mirror upstream xtls/REALITY for trivial
/// interop with existing tooling.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RealityConfig {
    /// Hostname presented in the outer TLS ClientHello —
    /// typically a high-traffic site (e.g. `microsoft.com`)
    /// the censor will not block by SNI alone.
    pub sni: String,
    /// Server-side static x25519 public key, base64.
    pub server_pubkey_b64: String,
    /// Short id (max 8 bytes), hex-encoded.
    pub short_id_hex: String,
    /// Server names the inner TLS may legitimately serve.
    /// Used by the server side to gate which inner targets
    /// it forwards traffic to.
    pub allowed_inner_servers: Vec<String>,
    /// Network endpoint (host:port) of the REALITY server.
    pub remote: String,
}

/// Scaffold transport — fails at `connect` until G5-1 ships.
pub struct RealityTransport {
    #[allow(dead_code)]
    cfg: RealityConfig,
}

impl RealityTransport {
    /// Build the (still-stub) transport from a parsed config.
    #[must_use]
    pub fn new(cfg: RealityConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Transport for RealityTransport {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        Err(Error::NotImplemented {
            feature: "reality",
            sprint: "S-5-reality",
        })
    }
}
