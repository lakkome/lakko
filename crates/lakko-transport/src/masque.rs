// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MASQUE / CONNECT-UDP (RFC 9298) — Phase 5 G5-2 scaffold
//! per [ADR-0004](../../../docs/adr/0004-anti-censorship.md).
//!
//! Tunnels QUIC inside an H3 CONNECT exchange so the outer
//! bytes look like ordinary HTTPS. The under-layer is the
//! same quinn-backed QUIC the rest of `lakko-transport`
//! uses; this module wraps it with the CONNECT-UDP framing
//! the proxy expects.
//!
//! Scaffold-only — real implementation lands in `S-5-masque`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::transport::{Connection, Transport};

/// Bootstrap-config descriptor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MasqueConfig {
    /// H3 proxy URL — the CONNECT-UDP target.
    pub proxy_url: String,
    /// Inner UDP host:port the proxy should connect to on
    /// the client's behalf.
    pub inner_target: String,
    /// Optional bearer token for the proxy's authorisation.
    /// `None` for open / IP-allowlisted proxies.
    pub bearer: Option<String>,
}

/// Scaffold transport — fails at `connect` until G5-2 ships.
pub struct MasqueTransport {
    #[allow(dead_code)]
    cfg: MasqueConfig,
}

impl MasqueTransport {
    /// Build the (still-stub) transport from a parsed config.
    #[must_use]
    pub fn new(cfg: MasqueConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Transport for MasqueTransport {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        Err(Error::NotImplemented {
            feature: "masque",
            sprint: "S-5-masque",
        })
    }
}
