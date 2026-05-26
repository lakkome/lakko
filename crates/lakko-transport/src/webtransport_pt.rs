// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! WebTransport — Phase 5 G5-4 scaffold per
//! [ADR-0004](../../../docs/adr/0004-anti-censorship.md).
//!
//! Web-client first-class transport. The browser's native
//! WebTransport API rides over HTTP/3 (QUIC under-layer), so
//! this module is the bridge between the Lakko `Transport`
//! trait and the browser's `new WebTransport(...)` JS class.
//!
//! Native targets (iOS / Android / desktop) talk to a
//! WebTransport server through quinn directly; the wire shape
//! is identical, only the entry-point class differs.
//!
//! Scaffold-only — real implementation lands in
//! `S-5-webtransport`. The module name is suffixed `_pt`
//! to disambiguate from any future `webtransport` server-
//! side module.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::transport::{Connection, Transport};

/// Bootstrap-config descriptor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebTransportConfig {
    /// WebTransport URL (e.g. `https://rendezvous.lakko.me/wt`).
    pub url: String,
    /// Optional pinned-cert SHA-256 hashes; native client
    /// passes these through `quinn::Endpoint`'s pinning
    /// verifier. Web client cannot consume these — the
    /// browser owns trust — and ignores them.
    pub spki_pins_b64: Vec<String>,
}

/// Scaffold transport — fails at `connect` until G5-4 ships.
pub struct WebTransportPt {
    #[allow(dead_code)]
    cfg: WebTransportConfig,
}

impl WebTransportPt {
    /// Build the (still-stub) transport from a parsed config.
    #[must_use]
    pub fn new(cfg: WebTransportConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Transport for WebTransportPt {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        Err(Error::NotImplemented {
            feature: "webtransport",
            sprint: "S-5-webtransport",
        })
    }
}
