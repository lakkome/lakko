// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-storage`.

use thiserror::Error;

/// Operations on the local store can fail along three axes:
///
///   * **Backend** — the underlying KV store reported an I/O / SQL error.
///   * **Cipher**  — AEAD seal/open failed (corrupt ciphertext, wrong key,
///     AAD mismatch, …).
///   * **Codec**   — value de/serialisation failed (e.g. truncated row).
///
/// Higher layers don't usually need to discriminate further than that;
/// the `source` chain carries the underlying detail for logs.
#[derive(Error, Debug)]
pub enum StorageError {
    /// Underlying SQL / I/O error from the backend.
    #[error("storage backend error")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// AEAD seal / open failed. Open failure means the ciphertext was
    /// tampered with, the wrong key was used, or the AAD changed
    /// (e.g. the row was relocated to a different table — which we
    /// reject by design).
    #[error("local store cipher failure: {0}")]
    Cipher(String),

    /// Value de/serialisation failed.
    #[error("local store codec error: {0}")]
    Codec(String),

    /// Caller passed bad arguments.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, StorageError>;

impl From<rusqlite::Error> for StorageError {
    fn from(e: rusqlite::Error) -> Self {
        StorageError::Backend(Box::new(e))
    }
}
