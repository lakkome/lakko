// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The one error type of the crate.

use thiserror::Error;

/// Why a circle operation was refused. Every decoder and every derivation
/// fails closed with one of these and never panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CircleError {
    /// The bytes are shorter or longer than their layout says.
    #[error("wrong length")]
    Length,
    /// A format byte this crate does not read.
    #[error("unknown format")]
    UnknownFormat,
    /// A field is out of its bound, or the encoding is not the canonical one.
    #[error("field out of bounds")]
    Malformed,
    /// An AEAD tag did not verify: the wrong key, or tampered bytes.
    #[error("authentication failed")]
    Authentication,
    /// A cipher failure on seal.
    #[error("seal failed")]
    Seal,
    /// The object names another epoch than the key in hand.
    #[error("another epoch")]
    WrongEpoch,
    /// The path did not lead to the read key the head names: the header is
    /// stale or incomplete, or this member was removed.
    #[error("the header does not match this member")]
    HeaderMismatch,
    /// A leaf outside the tree, or not assigned to anyone.
    #[error("not a member")]
    NotAMember,
    /// The tree holds its maximum number of leaves.
    #[error("the tree is full")]
    TreeFull,
    /// The operation needs at least one element.
    #[error("nothing to do")]
    Empty,
    /// A write that moves the epoch is already pending.
    #[error("a write is pending")]
    WritePending,
    /// A counter reached its maximum.
    #[error("counter overflow")]
    Overflow,
    /// A filter could not be built within the size limit.
    #[error("the filter does not fit")]
    FilterTooLarge,
    /// A card longer than a row carries.
    #[error("the card is too long")]
    CardTooLong,
    /// A row index outside the directory.
    #[error("row out of range")]
    RowOutOfRange,
}

impl CircleError {
    /// Stable, language-neutral reason code for the FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Length => "length",
            Self::UnknownFormat => "unknownFormat",
            Self::Malformed => "malformed",
            Self::Authentication => "authentication",
            Self::Seal => "seal",
            Self::WrongEpoch => "wrongEpoch",
            Self::HeaderMismatch => "headerMismatch",
            Self::NotAMember => "notAMember",
            Self::TreeFull => "treeFull",
            Self::Empty => "empty",
            Self::WritePending => "writePending",
            Self::Overflow => "overflow",
            Self::FilterTooLarge => "filterTooLarge",
            Self::CardTooLong => "cardTooLong",
            Self::RowOutOfRange => "rowOutOfRange",
        }
    }
}
