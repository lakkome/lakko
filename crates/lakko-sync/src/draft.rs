// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Per-group draft — what the user is currently typing but hasn't
//! sent.
//!
//! Phase 1.12 keeps this entirely **single-device**: drafts live in
//! local storage (`lakko-storage`), keyed by `group_id`. Phase 2's
//! DeviceGroup will sync drafts across paired devices using the
//! same [`Hlc`] last-write-wins rule that already governs edits —
//! the structure here is therefore designed so the multi-device
//! upgrade is "wrap a sync layer around the existing [`Draft`]
//! type", not a redesign.
//!
//! Empty body = "user cleared the draft" = stored as a [`Draft`]
//! with `body == ""`. The application UI can treat that as
//! "no draft" if it prefers, but the persistence layer keeps the
//! HLC so a later draft from another device cannot accidentally
//! resurrect the stale text.

use serde::{Deserialize, Serialize};

use crate::hlc::Hlc;

/// One per-group draft. Replaceable by another [`Draft`] for the
/// same `group_id` with a strictly greater [`Hlc`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    /// 32-byte group pseudonym this draft belongs to.
    pub group_id: [u8; 32],
    /// Current draft text. UTF-8.
    pub body: String,
    /// HLC at which the user last edited this draft. Used to
    /// resolve concurrent edits in Phase 2 multi-device sync.
    pub hlc: Hlc,
}

impl Draft {
    /// Construct a fresh draft.
    #[must_use]
    pub const fn new(group_id: [u8; 32], body: String, hlc: Hlc) -> Self {
        Self {
            group_id,
            body,
            hlc,
        }
    }

    /// Should `incoming` replace `self`?
    ///
    /// Rule: replace iff the `group_id` matches AND `incoming.hlc`
    /// is strictly greater. Equal-HLC events never replace — that
    /// would let a network retransmit clobber genuine local
    /// progress.
    #[must_use]
    pub fn should_replace_with(&self, incoming: &Draft) -> bool {
        self.group_id == incoming.group_id && incoming.hlc > self.hlc
    }
}

/// Apply `incoming` to `current` using the LWW rule. Returns the
/// resolved draft. Useful as a building block when consuming a
/// draft-update event from local storage or (future) DeviceGroup.
///
/// Keeps `current` if the incoming HLC is older or equal; otherwise
/// adopts `incoming`. Mismatched group IDs panic in debug builds —
/// they're a caller bug (drafts belong to one group and one group
/// only).
#[must_use]
pub fn merge_drafts(current: Draft, incoming: Draft) -> Draft {
    debug_assert_eq!(
        current.group_id, incoming.group_id,
        "merge_drafts requires same group_id",
    );
    if incoming.hlc > current.hlc {
        incoming
    } else {
        current
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn group(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn newer_hlc_replaces_older() {
        let cur = Draft::new(group(0xAA), "old".into(), Hlc::new(100, 0));
        let new = Draft::new(group(0xAA), "new".into(), Hlc::new(200, 0));
        assert!(cur.should_replace_with(&new));
        let merged = merge_drafts(cur, new.clone());
        assert_eq!(merged, new);
    }

    #[test]
    fn equal_hlc_does_not_replace() {
        let stamp = Hlc::new(100, 5);
        let cur = Draft::new(group(0xAA), "first".into(), stamp);
        let dup = Draft::new(group(0xAA), "second".into(), stamp);
        assert!(!cur.should_replace_with(&dup));
        let merged = merge_drafts(cur.clone(), dup);
        assert_eq!(merged, cur);
    }

    #[test]
    fn older_hlc_does_not_replace() {
        let cur = Draft::new(group(0xAA), "current".into(), Hlc::new(200, 0));
        let stale = Draft::new(group(0xAA), "stale".into(), Hlc::new(100, 0));
        assert!(!cur.should_replace_with(&stale));
        let merged = merge_drafts(cur.clone(), stale);
        assert_eq!(merged, cur);
    }

    #[test]
    fn lc_breaks_pt_ties_in_replacement() {
        let cur = Draft::new(group(0xAA), "older lc".into(), Hlc::new(100, 5));
        let new = Draft::new(group(0xAA), "newer lc".into(), Hlc::new(100, 6));
        assert!(cur.should_replace_with(&new));
    }

    #[test]
    fn empty_body_is_a_valid_draft() {
        let cleared = Draft::new(group(0xAA), String::new(), Hlc::new(100, 0));
        assert_eq!(cleared.body, "");
    }

    #[test]
    fn different_group_does_not_replace() {
        let cur = Draft::new(group(0xAA), "a-group".into(), Hlc::new(100, 0));
        let other = Draft::new(group(0xBB), "b-group".into(), Hlc::new(999, 0));
        assert!(!cur.should_replace_with(&other));
    }
}
