// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sync primitives for Lakko: hybrid logical clock + edit chain +
//! drafts. Phase 1.12 ships the **single-device** variants — every
//! op is local, the [`HybridLogicalClock`] is per-process, drafts
//! live in `lakko-storage`. The structures are intentionally chosen
//! so Phase 2's DeviceGroup multi-device sync (per ADR-0001 and
//! plan §7) is a layer **above** these types, not a redesign of them.
//!
//! ## Module map
//!
//! - [`hlc`] — Hybrid Logical Clock (Kulkarni–Demirbas–Browne 2014).
//!   Strictly-monotonic stamps that combine wall-clock milliseconds
//!   with a logical counter so events ordered within and across
//!   devices.
//! - [`edit`] — append-only edit chain per message, last-write-wins
//!   by [`Hlc`]. Includes Update + Delete (tombstone) ops; the
//!   resolver returns [`Resolved::Visible`], [`Resolved::Deleted`]
//!   or [`Resolved::Empty`].
//! - [`draft`] — per-group [`Draft`] with the same LWW rule. One
//!   draft per group, replaceable by a strictly-greater HLC.
//!
//! ## Why no character-level CRDT
//!
//! In Lakko's threat / UX model only the original sender ever edits
//! their own message — there are no concurrent multi-author edits
//! to merge, so the "merge two simultaneous typing sessions"
//! complexity of Yjs / Automerge is overkill. Flat
//! `Vec<EditOp>` + HLC ordering gives the right answer for every
//! realistic scenario at a fraction of the wire-format and
//! complexity cost.
//!
//! ## Out of scope (Phase 2+)
//!
//! - **DeviceGroup MLS substrate** — each device a separate leaf in
//!   a personal MLS group, used as the carrier for sync events
//!   (Phase 2.3).
//! - **Cross-device sync** of edits, read receipts, drafts, and
//!   conversation order through DeviceGroup application messages
//!   (Phase 2.4).
//! - **History catch-up bundle** — HPKE-envelope from an old device
//!   to a freshly-paired new one carrying the last N messages
//!   (Phase 2.5).
//! - **Read receipts** wire format — same-shape LWW state machine
//!   on top of [`Hlc`], plumbed through DeviceGroup (Phase 2.4).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod draft;
pub mod edit;
pub mod hlc;

pub use draft::{merge_drafts, Draft};
pub use edit::{EditChain, EditOp, Resolved};
pub use hlc::{Hlc, HybridLogicalClock};
