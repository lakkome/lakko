// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-mls`.

use thiserror::Error;

/// Errors produced by Lakko's MLS wrapper.
///
/// Variants are deliberately coarse: callers should not branch on internal
/// OpenMLS failure detail. Where additional context is useful for logs, a
/// short string is captured.
#[derive(Debug, Error)]
pub enum Error {
    /// Identity creation, key generation, or credential setup failed.
    #[error("identity / key generation failed: {0}")]
    Identity(String),

    /// KeyPackage creation or validation failed.
    #[error("key package operation failed: {0}")]
    KeyPackage(String),

    /// Group creation, joining, or commit processing failed.
    #[error("group operation failed: {0}")]
    Group(String),

    /// Application-message creation or processing failed.
    #[error("application message failed: {0}")]
    Message(String),

    /// MLS exporter failed.
    #[error("exporter failed: {0}")]
    Exporter(String),

    /// Wire-format (de)serialization failed.
    #[error("tls codec error: {0}")]
    Codec(String),

    /// Received an unsupported / unexpected message kind.
    #[error("unsupported message: {0}")]
    Unsupported(&'static str),
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, Error>;
