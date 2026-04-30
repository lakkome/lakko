// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Transport layer for Lakko.
//!
//! Provides:
//!
//! - [`Transport`], [`Listener`], [`Connection`] message-oriented traits
//! - [`Endpoint`] type for direct + hostname targets
//! - [`in_memory`] backend (mpsc) for tests and local IPC
//! - [`quic`] backend (`quinn`) for production traffic
//!
//! Pluggable obfuscation transports (REALITY, Shadowsocks-2022, MASQUE,
//! obfs4) plug in as adapters satisfying [`Transport`] and are selected at
//! runtime via happy-eyeballs racing — wired in Phase 5 per ADR-0004.

#![forbid(unsafe_code)]

pub mod endpoint;
pub mod error;
pub mod in_memory;
pub mod quic;
pub mod transport;

pub use endpoint::Endpoint;
pub use error::{Error, Result};
pub use transport::{Connection, Listener, Transport, MAX_FRAME_BYTES};
