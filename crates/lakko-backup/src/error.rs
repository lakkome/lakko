// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error vocabulary for the backup envelope + payload codec.

use thiserror::Error;

/// Backup-side error surface. Distinct from `lakko_crypto::Error`
/// so callers can tell "the AEAD failed" (wrong phrase / tampered
/// blob) apart from "the wire bytes are malformed".
#[derive(Debug, Error)]
pub enum BackupError {
    /// Wire bytes do not decode as a valid backup envelope —
    /// wrong schema version, truncated, length-prefix overflow.
    #[error("invalid backup encoding: {0}")]
    InvalidEncoding(String),

    /// Payload wire bytes (the AEAD plaintext) do not decode as
    /// a valid [`crate::BackupPayload`] — the AEAD opened
    /// successfully but the inner shape is wrong (corrupted
    /// state, post-format-change downgrade, etc.).
    #[error("invalid backup payload: {0}")]
    InvalidPayload(String),

    /// The AEAD `open` failed. Almost always "wrong recovery
    /// phrase" or "blob has been tampered with"; both fold here
    /// because we do not want a status-code-style oracle that
    /// distinguishes them.
    #[error("backup decryption failed (wrong recovery phrase or tampered blob)")]
    DecryptFailed,

    /// Underlying crypto primitive surface threw an error. In
    /// practice only Argon2id parameter validation can produce
    /// this; the fixed-params API on `lakko-crypto::kdf` returns
    /// it as `Err(Error::PasswordHashing)` and we re-wrap.
    #[error("crypto primitive failed: {0}")]
    Crypto(#[from] lakko_crypto::error::Error),
}

/// Convenience `Result` alias for this crate.
pub type Result<T> = std::result::Result<T, BackupError>;
