// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Errors of the ballot crate.
//!
//! Every variant has a stable kebab-case code ([`BallotError::code`]) for the
//! FFI surfaces, so no client parses the English message.

use thiserror::Error;

/// Everything that can go wrong building, opening or verifying a ballot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BallotError {
    /// A byte string has the wrong length or layout for what it claims to be.
    #[error("malformed encoding")]
    Malformed,
    /// A version byte this build does not read.
    #[error("unknown version")]
    UnknownVersion,
    /// A 32-byte string is not a canonical encoding of a point in the
    /// prime-order subgroup (or is the identity where that is refused).
    #[error("invalid point")]
    InvalidPoint,
    /// A 32-byte string is not a canonical scalar.
    #[error("invalid scalar")]
    InvalidScalar,
    /// A ring is empty, larger than the bound, not sorted where it must be,
    /// or holds a key twice.
    #[error("invalid ring")]
    InvalidRing,
    /// A ring extension would grow the ring past [`crate::ring::MAX_RING_LEN`].
    #[error("ring too large")]
    RingTooLarge,
    /// The ring version is not held, or a key the anonymity set selects is not
    /// a valid point.
    #[error("ring version not usable")]
    RingVersionUnusable,
    /// A ballot names a ring version (by hash) this device does not hold.
    #[error("unknown ring version")]
    UnknownRingVersion,
    /// The voter's key is not in the ring version being proven against.
    #[error("voter not in ring")]
    NotInRing,
    /// The voter's secret does not match the ring key at its index.
    #[error("witness does not match the ring key")]
    WitnessMismatch,
    /// The choices are not sorted, repeat an index, or are out of range.
    #[error("invalid choices")]
    InvalidChoices,
    /// The group id is empty or longer than [`crate::ballot::MAX_GROUP_ID_LEN`].
    #[error("invalid group id")]
    InvalidGroupId,
    /// The row did not open under the poll's seal key.
    #[error("row did not open")]
    RowDidNotOpen,
    /// The ballot names another poll.
    #[error("ballot names another poll")]
    WrongPoll,
    /// The proof does not verify.
    #[error("proof does not verify")]
    ProofRejected,
    /// Hash-to-point found no point (probability about 2^-256; never seen).
    #[error("hash to point failed")]
    HashToPoint,
    /// A key derivation failed.
    #[error("key derivation failed")]
    Kdf,
    /// An anonymity set is empty, longer than 256, not strictly ascending,
    /// not `min(256, L_k)` long for its version, or reaches past it.
    #[error("invalid anonymity set")]
    InvalidSet,
}

impl BallotError {
    /// A stable kebab-case code for the FFI surfaces.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnknownVersion => "unknown-version",
            Self::InvalidPoint => "invalid-point",
            Self::InvalidScalar => "invalid-scalar",
            Self::InvalidRing => "invalid-ring",
            Self::RingTooLarge => "ring-too-large",
            Self::RingVersionUnusable => "ring-version-unusable",
            Self::UnknownRingVersion => "unknown-ring-version",
            Self::NotInRing => "not-in-ring",
            Self::WitnessMismatch => "witness-mismatch",
            Self::InvalidChoices => "invalid-choices",
            Self::InvalidGroupId => "invalid-group-id",
            Self::RowDidNotOpen => "row-did-not-open",
            Self::WrongPoll => "wrong-poll",
            Self::ProofRejected => "proof-rejected",
            Self::HashToPoint => "hash-to-point",
            Self::Kdf => "kdf",
            Self::InvalidSet => "invalid-set",
        }
    }
}

/// Result alias for this crate.
pub type Result<T> = core::result::Result<T, BallotError>;
