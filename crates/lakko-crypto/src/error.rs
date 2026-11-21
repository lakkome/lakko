// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-crypto`.

use thiserror::Error;

/// Errors produced by `lakko-crypto` primitives.
///
/// Variants are deliberately coarse-grained: callers should never branch on
/// crypto failures by inspecting the cause, only on the variant. This avoids
/// leaking detail through error matching.
#[derive(Debug, Error)]
pub enum Error {
    /// AEAD encryption / decryption failed (tag mismatch, wrong nonce, etc.).
    #[error("aead operation failed")]
    Aead,

    /// Signature verification failed.
    #[error("signature verification failed")]
    SignatureVerification,

    /// Input length did not match the expected fixed length.
    #[error("invalid length: expected {expected} bytes, got {actual}")]
    InvalidLength {
        /// Expected length in bytes.
        expected: usize,
        /// Actual length in bytes.
        actual: usize,
    },

    /// HKDF expand requested too many bytes (>255 * HashLen).
    #[error("hkdf expand requested too many bytes: {0}")]
    HkdfTooLong(usize),

    /// Argon2id password hashing failed.
    #[error("password hashing failed")]
    PasswordHashing,

    /// X25519 produced an all-zero shared secret: the peer's public key is a
    /// low-order point, so the result does not depend on this side's secret.
    #[error("key agreement is not contributory")]
    NonContributory,
}

/// Convenience alias for `Result<T, lakko_crypto::Error>`.
pub type Result<T> = core::result::Result<T, Error>;
