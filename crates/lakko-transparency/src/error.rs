// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-transparency`.

use thiserror::Error;

/// Errors produced by the key-transparency log and its proof verifiers.
#[derive(Debug, Error)]
pub enum Error {
    /// Tried to query an entry beyond the log's current size.
    #[error("leaf index {index} out of bounds (log size {size})")]
    IndexOutOfBounds {
        /// Requested leaf index.
        index: u64,
        /// Current log size in leaves.
        size: u64,
    },

    /// An inclusion proof failed verification.
    #[error("inclusion proof did not verify against the given root")]
    InclusionProofMismatch,

    /// A consistency proof failed verification.
    #[error("consistency proof did not verify between the two roots")]
    ConsistencyProofMismatch,

    /// Signed root signature failed verification.
    #[error("signed root signature is invalid")]
    SignedRootInvalid,

    /// Signed root size is older than what the client has previously seen
    /// (rollback attempt).
    #[error("signed root rollback: new size {new} < seen size {seen}")]
    SignedRootRollback {
        /// Size of the newly received signed root.
        new: u64,
        /// Size of the previously trusted root.
        seen: u64,
    },

    /// Crypto-layer failure surfaced from `lakko-crypto`.
    #[error(transparent)]
    Crypto(#[from] lakko_crypto::Error),

    /// A supersession proof was not the expected wire length (G-4).
    #[error("supersession proof has wrong length: {got} bytes")]
    SupersessionBadLength {
        /// The actual length received.
        got: usize,
    },

    /// A supersession proof carried an unsupported version byte (G-4).
    #[error("unsupported supersession proof version: {0}")]
    SupersessionBadVersion(u8),

    /// A supersession proof's Ed25519 signature did not verify against the
    /// old key it names (G-4).
    #[error("supersession proof signature is invalid")]
    SupersessionBadSignature,

    /// A supersession proof's old key did not match the key the caller
    /// already trusts (G-4).
    #[error("supersession proof old key does not match the trusted key")]
    SupersessionOldKeyMismatch,

    /// A supersession proof has expired relative to `now` (G-4).
    #[error("supersession proof has expired")]
    SupersessionExpired,

    /// A supersession proof's validity window is invalid — either
    /// `expires_at < issued_at`, or it exceeds the 30-day maximum (G-4,
    /// invariant #6: a leaked old key cannot indefinitely re-authorise).
    #[error("supersession proof validity window is invalid")]
    SupersessionInvalidValidityWindow,
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, Error>;
