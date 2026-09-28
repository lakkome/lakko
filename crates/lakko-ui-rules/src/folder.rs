// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! What a chat folder shows, and in what order (ADR-0058 section 6,
//! `docs/ux/chat-folders.md` section 3.2).
//!
//! A folder is a saved view of the one merged list. The rule deciding which
//! rows it holds is written here once, so three clients cannot disagree about
//! which chats are in "Work". The client computes a [`FolderRule`] from the
//! synced folder (`lakko_sync::SyncedState::folders`), mapping each
//! conversation reference to the id of the row it already draws.

use crate::conversation::{compare_unpinned, order_by, ConversationKind, ConversationRow};

/// `include_kinds` bit: one-to-one conversations. Same bit as the wire.
pub const INCLUDE_ONE_TO_ONE: u32 = 1 << 0;
/// `include_kinds` bit: groups.
pub const INCLUDE_GROUPS: u32 = 1 << 1;
/// `include_kinds` bit: channels.
pub const INCLUDE_CHANNELS: u32 = 1 << 2;
/// `exclude_flags` bit: muted conversations.
pub const EXCLUDE_MUTED: u32 = 1 << 0;
/// `exclude_flags` bit: conversations with nothing unread.
pub const EXCLUDE_READ: u32 = 1 << 1;
/// `exclude_flags` bit: archived conversations.
pub const EXCLUDE_ARCHIVED: u32 = 1 << 2;

/// A folder's rule, in row terms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderRule {
    /// `INCLUDE_*` bits, exactly as the folder's meta register holds them.
    pub include_kinds: u32,
    /// `EXCLUDE_*` bits, exactly as the folder's meta register holds them.
    pub exclude_flags: u32,
    /// Ids of the rows added by hand ([`ConversationRow::id`]).
    pub included: Vec<String>,
    /// Ids of the rows excluded by hand.
    pub excluded: Vec<String>,
}

const fn kind_bit(kind: ConversationKind) -> u32 {
    match kind {
        ConversationKind::Chat => INCLUDE_ONE_TO_ONE,
        ConversationKind::Group => INCLUDE_GROUPS,
        ConversationKind::Channel => INCLUDE_CHANNELS,
    }
}

/// Whether `row` is in the folder.
///
/// 1. Excluded by hand beats everything - the one precedence rule, which
///    lets "all channels except these two" be built in two steps.
/// 2. It must be included: its kind toggled on, or added by hand.
/// 3. It must not be muted while Muted is on, fully read while Read is on,
///    or archived while Archived is on. These apply to a chat added by hand
///    too: the toggles describe the folder, not how a chat got into it.
///
/// A folder with an unknown bit set is never produced by the decoder
/// (refused on the wire), so unknown bits are simply not consulted here.
#[must_use]
pub fn folder_matches(folder: &FolderRule, row: &ConversationRow) -> bool {
    if folder.excluded.contains(&row.id) {
        return false;
    }
    let included =
        folder.include_kinds & kind_bit(row.kind) != 0 || folder.included.contains(&row.id);
    if !included {
        return false;
    }
    if row.muted && folder.exclude_flags & EXCLUDE_MUTED != 0 {
        return false;
    }
    if !row.has_unread && folder.exclude_flags & EXCLUDE_READ != 0 {
        return false;
    }
    if row.archived && folder.exclude_flags & EXCLUDE_ARCHIVED != 0 {
        return false;
    }
    true
}

/// Order a folder's rows: the folder's own pins first, in their stored
/// order, then everything else by the [`crate::conversation::order_conversations`]
/// rule with the global pin ignored (inside a folder, the global pin is not
/// what "pinned" means).
///
/// `folder_pins` are row ids; one that names no row is skipped, and a repeat
/// counts at its first position. Returns input indices, like
/// `order_conversations`, every row exactly once. The caller passes the rows
/// that match the folder ([`folder_matches`]); a pinned row that does not
/// match is not the caller's to show.
#[must_use]
pub fn order_conversations_in_folder(rows: &[ConversationRow], folder_pins: &[String]) -> Vec<u32> {
    let pin_rank = |row: &ConversationRow| folder_pins.iter().position(|p| *p == row.id);
    let mut pinned: Vec<(usize, u32)> = Vec::new();
    let mut rest: Vec<ConversationRow> = Vec::new();
    let mut rest_index: Vec<u32> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let idx = u32::try_from(i).unwrap_or(u32::MAX);
        if let Some(rank) = pin_rank(row) {
            pinned.push((rank, idx));
        } else {
            rest.push(row.clone());
            rest_index.push(idx);
        }
    }
    // Two rows with one id (never produced by a client) keep input order.
    pinned.sort_by_key(|&(rank, idx)| (rank, idx));
    let mut out: Vec<u32> = pinned.into_iter().map(|(_, idx)| idx).collect();
    out.extend(
        order_by(&rest, compare_unpinned)
            .into_iter()
            .filter_map(|i| rest_index.get(i as usize).copied()),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, kind: ConversationKind, at: i64) -> ConversationRow {
        ConversationRow {
            id: id.into(),
            kind,
            title: id.into(),
            last_activity_ms: Some(at),
            pinned: false,
            archived: false,
            muted: false,
            has_unread: true,
        }
    }

    fn rule(include_kinds: u32, exclude_flags: u32) -> FolderRule {
        FolderRule {
            include_kinds,
            exclude_flags,
            included: vec![],
            excluded: vec![],
        }
    }

    #[test]
    fn the_bits_are_the_wire_bits() {
        use lakko_sync::synced as w;
        assert_eq!(INCLUDE_ONE_TO_ONE, w::INCLUDE_ONE_TO_ONE);
        assert_eq!(INCLUDE_GROUPS, w::INCLUDE_GROUPS);
        assert_eq!(INCLUDE_CHANNELS, w::INCLUDE_CHANNELS);
        assert_eq!(EXCLUDE_MUTED, w::EXCLUDE_MUTED);
        assert_eq!(EXCLUDE_READ, w::EXCLUDE_READ);
        assert_eq!(EXCLUDE_ARCHIVED, w::EXCLUDE_ARCHIVED);
    }

    #[test]
    fn a_kind_toggle_includes_every_row_of_that_kind() {
        let r = rule(INCLUDE_CHANNELS, 0);
        assert!(folder_matches(&r, &row("c", ConversationKind::Channel, 1)));
        assert!(!folder_matches(&r, &row("g", ConversationKind::Group, 1)));
        assert!(!folder_matches(&r, &row("d", ConversationKind::Chat, 1)));
    }

    #[test]
    fn added_by_hand_is_included_without_its_kind() {
        let mut r = rule(0, 0);
        r.included.push("g".into());
        assert!(folder_matches(&r, &row("g", ConversationKind::Group, 1)));
        assert!(!folder_matches(&r, &row("h", ConversationKind::Group, 1)));
    }

    #[test]
    fn excluded_by_hand_beats_included_by_hand_and_by_kind() {
        let mut r = rule(INCLUDE_CHANNELS, 0);
        r.included.push("c".into());
        r.excluded.push("c".into());
        assert!(!folder_matches(&r, &row("c", ConversationKind::Channel, 1)));
    }

    #[test]
    fn exclude_toggles_apply_to_hand_added_rows_too() {
        let mut r = rule(0, EXCLUDE_MUTED | EXCLUDE_READ | EXCLUDE_ARCHIVED);
        r.included.push("x".into());
        let mut muted = row("x", ConversationKind::Chat, 1);
        muted.muted = true;
        assert!(!folder_matches(&r, &muted));
        let mut read = row("x", ConversationKind::Chat, 1);
        read.has_unread = false;
        assert!(!folder_matches(&r, &read));
        let mut archived = row("x", ConversationKind::Chat, 1);
        archived.archived = true;
        assert!(!folder_matches(&r, &archived));
        assert!(folder_matches(&r, &row("x", ConversationKind::Chat, 1)));
    }

    #[test]
    fn toggles_off_let_muted_read_and_archived_rows_through() {
        let r = rule(INCLUDE_ONE_TO_ONE, 0);
        let mut x = row("x", ConversationKind::Chat, 1);
        x.muted = true;
        x.has_unread = false;
        x.archived = true;
        assert!(folder_matches(&r, &x));
    }

    fn ids(rows: &[ConversationRow], pins: &[&str]) -> Vec<String> {
        let pins: Vec<String> = pins.iter().map(|p| (*p).to_string()).collect();
        order_conversations_in_folder(rows, &pins)
            .into_iter()
            .map(|i| rows[i as usize].id.clone())
            .collect()
    }

    #[test]
    fn folder_pins_come_first_in_stored_order_then_newest() {
        let rows = vec![
            row("a", ConversationKind::Chat, 10),
            row("b", ConversationKind::Chat, 30),
            row("c", ConversationKind::Chat, 20),
            row("d", ConversationKind::Chat, 40),
        ];
        assert_eq!(ids(&rows, &["c", "a", "zz", "c"]), ["c", "a", "d", "b"]);
    }

    #[test]
    fn the_global_pin_is_ignored_inside_a_folder() {
        let mut globally_pinned = row("a", ConversationKind::Chat, 1);
        globally_pinned.pinned = true;
        let rows = vec![globally_pinned, row("b", ConversationKind::Chat, 2)];
        assert_eq!(ids(&rows, &[]), ["b", "a"]);
    }

    #[test]
    fn archived_still_sinks_inside_a_folder() {
        let mut archived = row("a", ConversationKind::Chat, 99);
        archived.archived = true;
        let rows = vec![archived, row("b", ConversationKind::Chat, 1)];
        assert_eq!(ids(&rows, &[]), ["b", "a"]);
    }

    #[test]
    fn every_row_appears_exactly_once() {
        let rows: Vec<ConversationRow> = (0..30)
            .map(|i| row(&i.to_string(), ConversationKind::Group, i64::from(i % 5)))
            .collect();
        let mut seen = order_conversations_in_folder(&rows, &["3".into(), "7".into(), "3".into()]);
        assert_eq!(&seen[..2], &[3, 7]);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), rows.len());
    }
}
