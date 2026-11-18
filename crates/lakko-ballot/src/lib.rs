// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Anonymous group-poll ballots ([ADR-0090] section 3).
//!
//! A ballot proves, without saying which, that its voter holds the secret of
//! one key in an anonymity set of at most 256 keys of the poll's ring, and
//! carries a tag that is the same for every ballot one key casts in one poll
//! and unrelated across polls. Members count
//! the last ballot per tag; nobody is trusted to count.
//!
//! | Module | What it is |
//! |---|---|
//! | [`proof`] | the linkable one-out-of-many proof (Triptych, base 2) |
//! | [`ring`] | the electorate and its versions, prefixes of one list |
//! | [`ballot`] | the ballot, its tag and the sealed journal row |
//! | [`tally`] | the fold from verified ballots to totals |
//! | [`point`] | point and scalar decoding, hash-to-point, the voter key |
//!
//! The crate does no I/O and holds no state beyond a [`ring::Ring`].
//! Randomness comes from `lakko_crypto::random`. Every decoder is bounded
//! and fail-closed; the fuzz targets are in `fuzz/`.
//!
//! This is new cryptographic code and part of the G-39 external review
//! scope ([ADR-0046]). Sizes and timings are in `src/proof.sc.md`.
//!
//! [ADR-0090]: ../../../docs/adr/0090-attachment-kinds.md
//! [ADR-0046]: ../../../docs/adr/0046-external-crypto-review.md

#![forbid(unsafe_code)]

mod transcript;

pub mod ballot;
pub mod error;
pub mod point;
pub mod proof;
pub mod ring;
pub mod tally;

#[doc(hidden)]
pub mod fuzz_api;

pub use ballot::{
    cast, draw_set, open_and_verify, open_row, own_set_version, verify_ballot, AnonymitySet,
    Ballot, Cast, Choices, PollKeys, VerifiedBallot, ANONYMITY_SET,
};
pub use error::{BallotError, Result};
pub use point::VoterKey;
pub use ring::Ring;
pub use tally::{fold, CountedBallot, PollRules, Tally};
