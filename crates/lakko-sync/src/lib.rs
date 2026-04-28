// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Multi-device synchronization for Lakko.
//!
//! Implements the user-level DeviceGroup (each device a separate MLS leaf)
//! used as the substrate for read receipts, drafts, conversation order, and
//! catch-up history bundles. Conflict resolution uses hybrid logical clocks
//! per group; edits and deletes propagate as application messages with
//! monotonic counters and CRDT semantics.

#![forbid(unsafe_code)]
