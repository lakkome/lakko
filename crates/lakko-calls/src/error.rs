// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error types for the calls crate.

use thiserror::Error;

/// `Result` alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can flow out of the public `lakko-calls`
/// surface. Each variant maps onto a distinct failure-mode
/// the caller might want to react to (retry vs surface vs
/// crash).
#[derive(Debug, Error)]
pub enum Error {
    /// MLS-side failure on the exporter — usually means the
    /// group's epoch wasn't ready (caller raced the call
    /// keying against a fresh add/remove commit).
    #[error("mls exporter failed: {0}")]
    Exporter(String),

    /// HKDF / AEAD failure — practically unreachable for the
    /// algorithms we ship with valid inputs, but the public
    /// API exposes it so callers don't have to assume.
    #[error("crypto primitive failed: {0}")]
    Crypto(String),

    /// Frame counter exhausted — 32-bit per RFC 9605 §4.4.
    /// A media stream that hit 2^32 frames over a single
    /// epoch should re-key by triggering a fresh MLS commit;
    /// the caller surfaces this to the SFU/UI.
    #[error("sframe counter exhausted")]
    CounterExhausted,
}

impl From<lakko_mls::Error> for Error {
    fn from(err: lakko_mls::Error) -> Self {
        Error::Exporter(format!("{err:?}"))
    }
}
