// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-transport`.

use thiserror::Error;

/// Errors produced by transports, listeners, and connections.
///
/// Variants are coarse to avoid leaking timing or implementation detail to
/// the caller; the underlying source is captured for logs only.
#[derive(Debug, Error)]
pub enum Error {
    /// Failed to bind a listener or connect to a remote endpoint.
    #[error("transport setup failed: {0}")]
    Setup(String),

    /// I/O failure on a live connection (read, write, accept).
    #[error("transport i/o failed: {0}")]
    Io(String),

    /// The peer closed the connection.
    #[error("connection closed by peer")]
    PeerClosed,

    /// The local end was already closed.
    #[error("connection already closed")]
    LocalClosed,

    /// Frame too large for the transport to carry.
    #[error("frame too large: {actual} > {limit}")]
    FrameTooLarge {
        /// Actual frame size in bytes.
        actual: usize,
        /// Configured per-frame limit in bytes.
        limit: usize,
    },

    /// TLS / certificate setup failed.
    #[error("tls error: {0}")]
    Tls(String),

    /// Generic configuration error (invalid endpoint, bad address, etc.).
    #[error("invalid configuration: {0}")]
    Config(String),

    /// A scaffold-only transport variant (REALITY / Shadowsocks-2022 /
    /// MASQUE / WebTransport) was reached before its implementation
    /// sprint landed. Carries the sprint identifier so the operator
    /// log line points to the open ROADMAP item.
    #[error("transport {feature} is scaffold-only — implementation lands in {sprint}")]
    NotImplemented {
        /// Short identifier of the missing feature (the
        /// `PluggableKind::as_str()` of the corresponding
        /// kind).
        feature: &'static str,
        /// Sprint label in `docs/ROADMAP-PHASE3-CLOSEOUT.md`.
        sprint: &'static str,
    },
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, Error>;
