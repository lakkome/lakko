// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Shadowsocks-2022 (AEAD-2022) — Phase 5 G5-3 scaffold per
//! [ADR-0004](../../../docs/adr/0004-anti-censorship.md).
//!
//! Stream cipher obfs with AEAD-2022 (chacha20-poly1305-2022
//! or aes-256-gcm-2022). Simpler than REALITY (no TLS-in-TLS),
//! useful as a paired-deployment fallback when REALITY is
//! detected by an updated DPI.
//!
//! Scaffold-only — real implementation lands in
//! `S-5-shadowsocks`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::transport::{Connection, Transport};

/// Bootstrap-config descriptor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowsocksConfig {
    /// Host:port of the Shadowsocks server.
    pub remote: String,
    /// AEAD-2022 cipher suite ID. Stable strings:
    /// `"2022-blake3-chacha20-poly1305"`,
    /// `"2022-blake3-aes-256-gcm"`.
    pub method: String,
    /// 32-byte master pre-shared key, base64. AEAD-2022 derives
    /// per-session subkeys via BLAKE3.
    pub psk_b64: String,
}

/// Scaffold transport — fails at `connect` until G5-3 ships.
pub struct ShadowsocksTransport {
    #[allow(dead_code)]
    cfg: ShadowsocksConfig,
}

impl ShadowsocksTransport {
    /// Build the (still-stub) transport from a parsed config.
    #[must_use]
    pub fn new(cfg: ShadowsocksConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Transport for ShadowsocksTransport {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        Err(Error::NotImplemented {
            feature: "shadowsocks-2022",
            sprint: "S-5-shadowsocks",
        })
    }
}
