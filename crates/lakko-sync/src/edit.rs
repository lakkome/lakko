// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Edit chain — append-only log of message edits, last-write-wins by
//! [`Hlc`].
//!
//! ## Why a simple LWW chain instead of a character-level CRDT
//!
//! Lakko's threat / UX model: only the original sender ever edits
//! their own message. There are no concurrent multi-author edits to
//! resolve, so the "merge two simultaneous typing sessions"
//! complexity of Yjs / Automerge is overkill here. A flat
//! `Vec<EditOp>` with HLC-ordered last-write-wins gives:
//!
//! - **Observably correct UX** — readers always see the freshest
//!   text the sender produced, regardless of message-arrival
//!   order.
//! - **Deterministic conflict resolution** if a sender edits from
//!   two paired devices simultaneously (Phase 2 DeviceGroup)
//!   the higher [`Hlc`] wins. HLC's monotonicity guarantee makes
//!   the result identical on every observer.
//! - **Cheap on the wire** — 16 bytes for the HLC plus the body,
//!   no complex op-graph.
//!
//! ## Tombstones
//!
//! [`EditOp::Delete`] is just an edit with no body. It still
//! carries an HLC and competes with other edits via the same
//! last-write-wins rule. This means a fast edit can resurrect a
//! deleted message — that's intentional and matches what users
//! expect from "I deleted then changed my mind". The application
//! layer can refuse to display anything after a delete if it
//! wants stricter semantics.

use serde::{Deserialize, Serialize};

use crate::hlc::Hlc;

/// Single edit operation in a message's edit chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EditOp {
    /// Replace the message body with this UTF-8 text. The original
    /// "send" event is itself an `Update` with the message's
    /// initial body — the chain has no special "create" variant,
    /// keeping the merge function uniform.
    Update {
        /// HLC at which the sender produced this revision.
        hlc: Hlc,
        /// New body. Empty string is allowed (clears the message
        /// without tombstoning it).
        body: String,
    },
    /// Tombstone — sender deleted the message.
    Delete {
        /// HLC at which the sender produced the deletion.
        hlc: Hlc,
    },
}

impl EditOp {
    /// HLC of this op.
    #[must_use]
    pub const fn hlc(&self) -> Hlc {
        match self {
            EditOp::Update { hlc, .. } | EditOp::Delete { hlc } => *hlc,
        }
    }
}

/// Resolved view of a message after applying every op in the chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolved {
    /// Message currently has visible text — the body of the latest
    /// `Update`.
    Visible(String),
    /// Message was tombstoned by the latest op.
    Deleted,
    /// No ops have been applied — caller never appended to this
    /// chain. The "send" itself is supposed to be the first op, so
    /// hitting `Empty` means a freshly-constructed chain that hasn't
    /// received its first event.
    Empty,
}

/// Append-only edit log for one message. Order of insertion does
/// not matter — `current()` always returns the op with the
/// highest [`Hlc`].
///
/// The chain is intentionally naive about deduplication. Repeated
/// inserts of the same `EditOp` are stored verbatim; resolution is
/// idempotent because LWW only looks at the highest HLC. If a
/// caller cares about wire-size they should dedup before persisting,
/// not here.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EditChain {
    ops: Vec<EditOp>,
}

impl EditChain {
    /// Empty chain.
    #[must_use]
    pub const fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Construct from existing ops (e.g. loaded from `lakko-storage`).
    /// Order in the input vector is irrelevant; resolution is HLC-driven.
    #[must_use]
    pub fn from_ops(ops: Vec<EditOp>) -> Self {
        Self { ops }
    }

    /// Append a new op. Cheap (`Vec::push`).
    pub fn append(&mut self, op: EditOp) {
        self.ops.push(op);
    }

    /// Number of stored ops. Mostly useful for tests + ops debugging.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// `true` if no ops have been appended yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Borrow the underlying op vector. Order matches insertion.
    #[must_use]
    pub fn ops(&self) -> &[EditOp] {
        &self.ops
    }

    /// Resolve the chain to the user-visible state.
    ///
    /// Picks the op with the highest [`Hlc`]. Tie-break (which can
    /// only happen if two devices independently produced the same
    /// physical-time + logical counter, which the HLC algorithm is
    /// designed to prevent in practice) falls back to position in
    /// the underlying vector — both arms are deterministic.
    #[must_use]
    pub fn current(&self) -> Resolved {
        let Some(latest) = self.ops.iter().max_by_key(|op| op.hlc()) else {
            return Resolved::Empty;
        };
        match latest {
            EditOp::Update { body, .. } => Resolved::Visible(body.clone()),
            EditOp::Delete { .. } => Resolved::Deleted,
        }
    }

    /// Merge another chain into this one. Equivalent to appending
    /// every op from `other` — works because resolution is
    /// commutative under HLC. Useful when joining two devices'
    /// views of the same message during DeviceGroup catch-up.
    pub fn merge(&mut self, other: &EditChain) {
        self.ops.extend(other.ops.iter().cloned());
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn upd(pt: u64, lc: u64, body: &str) -> EditOp {
        EditOp::Update {
            hlc: Hlc::new(pt, lc),
            body: body.to_string(),
        }
    }

    fn del(pt: u64, lc: u64) -> EditOp {
        EditOp::Delete {
            hlc: Hlc::new(pt, lc),
        }
    }

    #[test]
    fn empty_chain_resolves_to_empty() {
        let chain = EditChain::new();
        assert_eq!(chain.current(), Resolved::Empty);
        assert!(chain.is_empty());
    }

    #[test]
    fn single_update_is_visible() {
        let mut chain = EditChain::new();
        chain.append(upd(100, 0, "hello"));
        assert_eq!(chain.current(), Resolved::Visible("hello".to_string()));
    }

    #[test]
    fn later_update_wins_regardless_of_insertion_order() {
        let mut chain = EditChain::new();
        chain.append(upd(200, 0, "later"));
        chain.append(upd(100, 0, "earlier"));
        assert_eq!(chain.current(), Resolved::Visible("later".to_string()));
    }

    #[test]
    fn lc_breaks_pt_ties() {
        let mut chain = EditChain::new();
        chain.append(upd(100, 5, "lc=5"));
        chain.append(upd(100, 7, "lc=7"));
        chain.append(upd(100, 6, "lc=6"));
        assert_eq!(chain.current(), Resolved::Visible("lc=7".to_string()));
    }

    #[test]
    fn delete_tombstones_when_latest() {
        let mut chain = EditChain::new();
        chain.append(upd(100, 0, "hi"));
        chain.append(del(200, 0));
        assert_eq!(chain.current(), Resolved::Deleted);
    }

    #[test]
    fn later_update_resurrects_after_delete() {
        let mut chain = EditChain::new();
        chain.append(upd(100, 0, "hi"));
        chain.append(del(200, 0));
        chain.append(upd(300, 0, "actually nvm"));
        assert_eq!(
            chain.current(),
            Resolved::Visible("actually nvm".to_string()),
        );
    }

    #[test]
    fn earlier_delete_does_not_clobber_later_update() {
        // Out-of-order arrival: delete arrives first but has older HLC.
        let mut chain = EditChain::new();
        chain.append(del(100, 0));
        chain.append(upd(200, 0, "hi"));
        assert_eq!(chain.current(), Resolved::Visible("hi".to_string()));
    }

    #[test]
    fn merge_is_commutative() {
        let mut a = EditChain::from_ops(vec![upd(100, 0, "a"), upd(300, 0, "c")]);
        let b = EditChain::from_ops(vec![upd(200, 0, "b")]);
        a.merge(&b);
        assert_eq!(a.current(), Resolved::Visible("c".to_string()));
        assert_eq!(a.len(), 3);
    }

    #[test]
    fn duplicate_ops_are_idempotent_in_resolution() {
        let mut chain = EditChain::new();
        let dup = upd(500, 0, "the body");
        chain.append(dup.clone());
        chain.append(dup);
        // len = 2 (chain doesn't dedup), but current is the same.
        assert_eq!(chain.len(), 2);
        assert_eq!(chain.current(), Resolved::Visible("the body".to_string()));
    }

    #[test]
    fn editop_hlc_accessor_returns_correct_value() {
        let u = upd(42, 7, "x");
        let d = del(99, 3);
        assert_eq!(u.hlc(), Hlc::new(42, 7));
        assert_eq!(d.hlc(), Hlc::new(99, 3));
    }

    #[test]
    fn from_ops_preserves_input_vector_order() {
        let ops = vec![upd(100, 0, "a"), upd(200, 0, "b")];
        let chain = EditChain::from_ops(ops.clone());
        assert_eq!(chain.ops(), ops.as_slice());
    }

    #[test]
    fn empty_string_update_is_visible_not_tombstone() {
        // Confirm the API distinguishes "user typed empty" from
        // "user deleted".
        let mut chain = EditChain::new();
        chain.append(upd(100, 0, ""));
        assert_eq!(chain.current(), Resolved::Visible(String::new()));
    }
}
