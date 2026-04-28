// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Transport layer for Lakko.
//!
//! Provides `Transport` and `Listener` traits implemented over QUIC (`quinn`),
//! WebSocket-over-TLS, and WebTransport. Pluggable obfuscation transports
//! (REALITY, Shadowsocks-2022, MASQUE, obfs4) are wired in as adapters
//! satisfying the same trait, and selected at runtime via happy-eyeballs racing.
//!
//! See `docs/adr/0004-anti-censorship.md`.

#![forbid(unsafe_code)]
