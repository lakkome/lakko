// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The order of the one conversation list.
//!
//! `docs/ux/navigation.md` section 2.2. Chats, groups and channels are one
//! list, sorted by when something last happened in them - which is the whole
//! reason they had to become one list, since a list sorted by WHEN cannot be
//! three lists.
//!
//! The rule is here rather than in each client because it is a rule: no
//! screen, no store, no network. A client maps whatever it holds into
//! [`ConversationRow`] and gets back the order.

/// Which screen a row opens.
///
/// Carried so a client can route a tap, and for nothing else. The list says
/// nothing about which kind a row is - no badge, no label, no differing
/// avatar - because a person who subscribed to a channel knows they
/// subscribed to a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConversationKind {
    /// A one-to-one conversation.
    Chat,
    /// A group conversation.
    Group,
    /// A channel this person subscribes to.
    Channel,
}

/// One row of the merged list, reduced to what the order reads.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ConversationRow {
    /// Stable within a kind; `(kind, id)` is unique in the list.
    pub id: String,
    /// Which screen the row opens.
    pub kind: ConversationKind,
    /// What the row is called. Read only to break a tie.
    pub title: String,
    /// When the last thing a person can SEE happened, in unix milliseconds,
    /// or `None` for a conversation nothing has happened in yet.
    ///
    /// A row that draws nothing does not move a conversation to the top: a
    /// signal nobody sees is not activity.
    pub last_activity_ms: Option<i64>,
    /// Pinned by this person. Beats everything, including something newer.
    pub pinned: bool,
    /// Archived by this person. Sinks below everything, including something
    /// newer.
    pub archived: bool,
    /// Muted by this person. Read only by a folder's "Muted" exclude toggle
    /// (ADR-0058); the list order ignores it.
    pub muted: bool,
    /// Something unread in it, or a manual "mark as unread" on it - what the
    /// unread badge means. Read only by a folder's "Read" exclude toggle.
    pub has_unread: bool,
}

/// Order the list: pinned first in the person's order, archived last, newest
/// between.
///
/// Returns the input INDICES in display order rather than the rows
/// themselves. A client's real row carries far more than this - an avatar, a
/// preview, an unread count, a handle to open - and handing all of it across
/// an FFI boundary twice, in both directions, to learn an order would be
/// waste. It reorders what it already holds.
///
/// `pin_order` is the account's synced conversation order
/// (`docs/ux/navigation.md` section 2.2.1): row ids, top first, as the client
/// maps the `ConversationOrder` register's entries to the ids of the rows it
/// draws. It orders the PINNED rows only, and nothing else:
///
/// - a pinned row the order names sits at its place in the order (a repeat
///   counts at its first position), whatever happened in it since - a new
///   message never moves a pin;
/// - a pinned row the order does not name (pinned on another device while
///   this one reordered, or pinned before the order existed) sits ABOVE the
///   named ones, newest activity first, the way a fresh pin lands on top;
/// - an id that names no row, or a row that is not pinned, is ignored, so an
///   unpinned conversation always sorts by activity.
///
/// Pinning beats archiving. Pinning is an explicit act and archiving is a
/// tidy-up, so a conversation that is both stays where the person put it;
/// otherwise pinning something and then archiving it would look as though
/// the pin had been ignored.
///
/// The order is TOTAL. Two conversations with no activity yet compare equal
/// on every interesting field, and without the title and `(kind, id)` tie
/// breaks they would sit in whatever order the client's store happened to
/// yield - so an unrelated refresh would reshuffle the list, which reads as
/// a bug and is impossible to report.
#[must_use]
pub fn order_conversations(rows: &[ConversationRow], pin_order: &[String]) -> Vec<u32> {
    let rank = |row: &ConversationRow| -> Option<usize> {
        if row.pinned {
            pin_order.iter().position(|p| *p == row.id)
        } else {
            None
        }
    };
    let ranks: Vec<Option<usize>> = rows.iter().map(rank).collect();
    let mut order: Vec<u32> = (0..rows.len())
        .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
        .collect();
    order.sort_by(|&a, &b| {
        let (l, r) = (&rows[a as usize], &rows[b as usize]);
        compare_with_pins(l, ranks[a as usize], r, ranks[b as usize]).then_with(|| a.cmp(&b))
    });
    order
}

fn compare_with_pins(
    l: &ConversationRow,
    l_rank: Option<usize>,
    r: &ConversationRow,
    r_rank: Option<usize>,
) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    match (l.pinned, r.pinned) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        (false, false) => return compare_unpinned(l, r),
        (true, true) => {},
    }
    match (l_rank, r_rank) {
        // Both named: the person's order.
        (Some(a), Some(b)) if a != b => a.cmp(&b),
        // Unnamed pins above the named ones.
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        // Both unnamed (or one id named twice): by activity, then the ties.
        _ => compare_unpinned(l, r),
    }
}

pub(crate) fn order_by(
    rows: &[ConversationRow],
    compare: fn(&ConversationRow, &ConversationRow) -> core::cmp::Ordering,
) -> Vec<u32> {
    let mut order: Vec<u32> = (0..rows.len())
        .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
        .collect();
    order.sort_by(|&a, &b| {
        let (l, r) = (&rows[a as usize], &rows[b as usize]);
        compare(l, r).then_with(|| a.cmp(&b))
    });
    order
}

/// The order without the global pin: archived last, newest first, then the
/// title and `(kind, id)` tie breaks. Shared with the folder order, which
/// replaces the global pin with the folder's own pins.
pub(crate) fn compare_unpinned(l: &ConversationRow, r: &ConversationRow) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    match (l.archived, r.archived) {
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {},
    }
    // Newest first, and "never" is the oldest there is.
    let by_activity = r
        .last_activity_ms
        .unwrap_or(i64::MIN)
        .cmp(&l.last_activity_ms.unwrap_or(i64::MIN));
    if by_activity != Ordering::Equal {
        return by_activity;
    }
    // Case-folded so "anna" and "Anna" do not straddle every other title.
    let by_title = l.title.to_lowercase().cmp(&r.title.to_lowercase());
    if by_title != Ordering::Equal {
        return by_title;
    }
    l.kind.cmp(&r.kind).then_with(|| l.id.cmp(&r.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, kind: ConversationKind, title: &str, at: Option<i64>) -> ConversationRow {
        ConversationRow {
            id: id.to_string(),
            kind,
            title: title.to_string(),
            last_activity_ms: at,
            pinned: false,
            archived: false,
            muted: false,
            has_unread: false,
        }
    }

    fn ids(rows: &[ConversationRow]) -> Vec<&str> {
        ids_with(rows, &[])
    }

    fn ids_with<'a>(rows: &'a [ConversationRow], pins: &[&str]) -> Vec<&'a str> {
        let pins: Vec<String> = pins.iter().map(|p| (*p).to_string()).collect();
        order_conversations(rows, &pins)
            .into_iter()
            .map(|i| rows[i as usize].id.as_str())
            .collect()
    }

    #[test]
    fn newest_first_whatever_kind_it_is() {
        // The point of merging: the newest thing that happened does not care
        // which container it arrived in.
        let rows = vec![
            row("chat", ConversationKind::Chat, "Anna", Some(10)),
            row("channel", ConversationKind::Channel, "News", Some(30)),
            row("group", ConversationKind::Group, "Climbing", Some(20)),
        ];
        assert_eq!(ids(&rows), ["channel", "group", "chat"]);
    }

    #[test]
    fn pinned_beats_something_newer() {
        let mut pinned = row("pinned", ConversationKind::Chat, "Anna", Some(1));
        pinned.pinned = true;
        let rows = vec![
            row("new", ConversationKind::Chat, "Boris", Some(999)),
            pinned,
        ];
        assert_eq!(ids(&rows)[0], "pinned");
    }

    #[test]
    fn archived_sinks_below_something_newer() {
        let mut archived = row("archived", ConversationKind::Chat, "Anna", Some(999));
        archived.archived = true;
        let rows = vec![
            archived,
            row("live", ConversationKind::Chat, "Boris", Some(1)),
        ];
        assert_eq!(ids(&rows), ["live", "archived"]);
    }

    #[test]
    fn pinning_beats_archiving_so_a_pin_never_looks_ignored() {
        let mut both = row("both", ConversationKind::Chat, "Anna", Some(1));
        both.pinned = true;
        both.archived = true;
        let rows = vec![
            row("live", ConversationKind::Chat, "Boris", Some(500)),
            both,
        ];
        assert_eq!(ids(&rows)[0], "both");
    }

    #[test]
    fn a_conversation_nothing_has_happened_in_is_the_oldest() {
        let rows = vec![
            row("empty", ConversationKind::Chat, "Zoe", None),
            row("old", ConversationKind::Chat, "Anna", Some(1)),
        ];
        assert_eq!(ids(&rows), ["old", "empty"]);
    }

    #[test]
    fn two_empty_conversations_order_by_title_rather_than_by_luck() {
        // Both are `None`, so without the tie break they sit in whatever
        // order the store yielded and a refresh reshuffles the list.
        let zoe = row("1", ConversationKind::Chat, "Zoe", None);
        let anna = row("2", ConversationKind::Chat, "Anna", None);
        assert_eq!(ids(&[zoe.clone(), anna.clone()]), ["2", "1"]);
        assert_eq!(ids(&[anna, zoe]), ["2", "1"]);
    }

    #[test]
    fn the_title_tie_break_ignores_case() {
        let rows = vec![
            row("1", ConversationKind::Chat, "zoe", None),
            row("2", ConversationKind::Chat, "Anna", None),
        ];
        assert_eq!(ids(&rows), ["2", "1"]);
    }

    #[test]
    fn identical_titles_still_order_the_same_way_every_time() {
        let rows = vec![
            row("b", ConversationKind::Group, "Same", None),
            row("a", ConversationKind::Chat, "Same", None),
        ];
        // Chat sorts before Group, so the kind decides before the id does.
        assert_eq!(ids(&rows), ["a", "b"]);
    }

    #[test]
    fn an_empty_list_orders_to_nothing() {
        assert!(order_conversations(&[], &[]).is_empty());
    }

    #[test]
    fn every_row_appears_exactly_once() {
        // A sort that dropped or duplicated a row would lose a conversation
        // from the list without anything failing.
        let rows: Vec<ConversationRow> = (0..50)
            .map(|i| {
                row(
                    &i.to_string(),
                    ConversationKind::Chat,
                    "Same",
                    if i % 3 == 0 {
                        None
                    } else {
                        Some(i64::from(i % 7))
                    },
                )
            })
            .collect();
        let mut seen = order_conversations(&rows, &["4".into(), "9".into()]);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), rows.len());
    }

    fn pinned(id: &str, at: Option<i64>) -> ConversationRow {
        let mut r = row(id, ConversationKind::Chat, id, at);
        r.pinned = true;
        r
    }

    #[test]
    fn pins_follow_the_stored_order_not_activity() {
        let rows = vec![
            pinned("a", Some(30)),
            pinned("b", Some(10)),
            pinned("c", Some(20)),
            row("x", ConversationKind::Chat, "x", Some(999)),
        ];
        assert_eq!(ids_with(&rows, &["b", "c", "a"]), ["b", "c", "a", "x"]);
    }

    #[test]
    fn a_new_message_does_not_move_a_pin() {
        let mut rows = vec![pinned("a", Some(1)), pinned("b", Some(2))];
        let before = ids_with(&rows, &["a", "b"]).join(",");
        rows[1].last_activity_ms = Some(1_000_000);
        assert_eq!(ids_with(&rows, &["a", "b"]).join(","), before);
    }

    #[test]
    fn an_unnamed_pin_sits_above_the_named_ones_newest_first() {
        let rows = vec![
            pinned("named", Some(999)),
            pinned("old", Some(1)),
            pinned("new", Some(5)),
        ];
        assert_eq!(ids_with(&rows, &["named"]), ["new", "old", "named"]);
    }

    #[test]
    fn an_unpinned_row_the_order_names_sorts_by_activity() {
        let rows = vec![
            row("stale", ConversationKind::Chat, "s", Some(1)),
            row("fresh", ConversationKind::Chat, "f", Some(2)),
        ];
        assert_eq!(ids_with(&rows, &["stale"]), ["fresh", "stale"]);
    }

    #[test]
    fn a_repeated_or_unknown_id_is_harmless() {
        let rows = vec![pinned("a", Some(1)), pinned("b", Some(2))];
        assert_eq!(ids_with(&rows, &["zz", "b", "a", "b"]), ["b", "a"]);
    }

    #[test]
    fn an_archived_pin_keeps_its_place_among_the_pins() {
        let mut both = pinned("both", Some(1));
        both.archived = true;
        let rows = vec![pinned("first", Some(5)), both, pinned("last", Some(9))];
        assert_eq!(
            ids_with(&rows, &["first", "both", "last"]),
            ["first", "both", "last"]
        );
    }

    proptest::proptest! {
        #[test]
        fn named_pins_keep_their_relative_order_and_every_row_appears_once(
            spec in proptest::collection::vec((proptest::bool::ANY, proptest::option::of(0i64..20), proptest::bool::ANY), 0..24),
            order_picks in proptest::collection::vec(0usize..30, 0..30),
        ) {
            let rows: Vec<ConversationRow> = spec
                .iter()
                .enumerate()
                .map(|(i, (p, at, arch))| {
                    let mut r = row(&format!("r{i}"), ConversationKind::Group, "t", *at);
                    r.pinned = *p;
                    r.archived = *arch;
                    r
                })
                .collect();
            let pin_order: Vec<String> = order_picks.iter().map(|i| format!("r{i}")).collect();
            let out = order_conversations(&rows, &pin_order);
            let mut seen = out.clone();
            seen.sort_unstable();
            seen.dedup();
            proptest::prop_assert_eq!(seen.len(), rows.len());
            // Pinned rows form one prefix.
            let flags: Vec<bool> = out.iter().map(|&i| rows[i as usize].pinned).collect();
            proptest::prop_assert!(flags.windows(2).all(|w| w[0] || !w[1]));
            // Named pins appear in the order's first-position order, after every unnamed pin.
            let first_pos = |id: &str| pin_order.iter().position(|p| p == id);
            let pins: Vec<Option<usize>> = out
                .iter()
                .filter(|&&i| rows[i as usize].pinned)
                .map(|&i| first_pos(&rows[i as usize].id))
                .collect();
            let in_order = pins.windows(2).all(|w| match (w[0], w[1]) {
                (Some(a), Some(b)) => a < b,
                (Some(_), None) => false,
                _ => true,
            });
            proptest::prop_assert!(in_order);
            // Unpinned rows ignore the order: same as with no order at all.
            let unpinned = |o: &[u32]| -> Vec<u32> { o.iter().copied().filter(|&i| !rows[i as usize].pinned).collect() };
            proptest::prop_assert_eq!(unpinned(&out), unpinned(&order_conversations(&rows, &[])));
            // Input order does not matter (the order is total).
            let mut rev = rows.clone();
            rev.reverse();
            let rev_ids: Vec<String> = order_conversations(&rev, &pin_order).into_iter().map(|i| rev[i as usize].id.clone()).collect();
            let fwd_ids: Vec<String> = out.into_iter().map(|i| rows[i as usize].id.clone()).collect();
            proptest::prop_assert_eq!(rev_ids, fwd_ids);
        }
    }
}
