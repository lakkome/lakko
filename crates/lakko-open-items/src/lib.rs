// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The typed half of the open-items hand-over (ADR-0092 section 3.9).
//!
//! The wire codec, the row planner and the part assembler live in
//! `lakko_contacts::open_items` and `lakko_contacts::poll_ballots`, where the
//! bodies, ring and votes are bytes. This crate is the part that needs the
//! types of `lakko-attachments` and `lakko-ballot`:
//!
//! * [`build`] — from a committer's held poll or checklist (signed body,
//!   ring, votes, folded checklist state) to a `lakko_contacts::OpenItem`;
//! * [`adopt`] — from an assembled item to the records a newcomer stores:
//!   the author signature checked, the body decoded, the ring decoded and its
//!   version 0 matched against the signed `ring0_hash`, every vote and tick
//!   checked against the body.
//!
//! It is its own crate because `lakko-attachments` depends on
//! `lakko-contacts` (so `lakko-contacts` cannot decode a poll body), and the
//! iOS notification extension links `lakko-contacts` but must not carry
//! `lakko-ballot`. Only the full FFI surfaces link this crate.

#![forbid(unsafe_code)]

pub mod adopt;
pub mod build;

pub use adopt::{adopt_open_item, AdoptError, AdoptedItem};
pub use build::{checklist_open_item, poll_open_item, BuildError};

#[cfg(test)]
mod tests;
