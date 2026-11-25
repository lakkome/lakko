// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Where the archived conversations sit in the list
//! (`docs/ux/navigation.md` section 2.2, step 3 and its note of 2026-11-25).
//!
//! The ORDER is not decided here: it is [`crate::conversation`] (All chats)
//! or [`crate::folder`] (a folder), which already put archived
//! conversations last. This rule takes the list in that order and says what
//! the conversation panel shows:
//!
//! - the chats view: the conversations that are not archived, and, when any
//!   are archived, one "Archived" bar fixed to the bottom of the panel;
//! - the archive view (the person tapped the bar): the archived
//!   conversations alone, in the same panel, with a way back;
//! - while something is typed in search, the chats view lists the archived
//!   matches under a divider instead of hiding them behind the bar.
//!
//! On every width and for any number of archived conversations: archived
//! conversations are always behind the bar.
//!
//! A conversation that is pinned in the view shown stays with the pins even
//! when it is archived (the shared order puts it there), so the archived part
//! is the trailing run of rows that are archived and not pinned. Inside a
//! folder "pinned" is the folder's own pin; the caller says which.
//!
//! The answer is counts over the ordered input, not rows: the client keeps
//! its rich rows and slices them, the way [`crate::conversation`] hands back
//! indices.

/// The two flags of one ordered row this rule reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArchivedSectionRow {
    /// The conversation is archived.
    pub archived: bool,
    /// The conversation is pinned in the view shown (the folder's own pin
    /// inside a folder, the global pin in All chats and in search).
    pub pinned: bool,
}

/// Which of the two lists the conversation panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchivedView {
    /// The conversations that are not archived.
    Chats,
    /// The archived conversations, opened from the bar.
    Archive,
}

/// What the panel draws, as counts over the ordered input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchivedSection {
    /// The list shown.
    pub view: ArchivedView,
    /// Rows `0..top_count` are the conversations that are not archived (with
    /// the pins); rows `top_count..` are the archived ones.
    pub top_count: u32,
    /// How many rows are archived (`rows.len() - top_count`).
    pub archived_count: u32,
    /// Chats view only: the "Archived" bar stands at the bottom of the panel,
    /// carrying `archived_count`. Never while searching and never in the
    /// archive view.
    pub bar: bool,
    /// Chats view only, while searching: the archived matches are listed
    /// under a divider after the other rows.
    pub divider: bool,
}

/// Split the ordered list around its archived conversations.
///
/// `searching`: something is typed in the search field (the rows given are
/// then the matches). `archive_open`: the person opened the archive from the
/// bar and has not gone back. The archive view closes by itself once it holds
/// nothing and nothing is typed, so unarchiving the last conversation returns
/// to the chats view; while searching it stays, since an empty result is not
/// an empty archive.
#[must_use]
pub fn archived_section(
    rows: &[ArchivedSectionRow],
    searching: bool,
    archive_open: bool,
) -> ArchivedSection {
    let archived = rows
        .iter()
        .rev()
        .take_while(|r| r.archived && !r.pinned)
        .count();
    let top_count = saturate(rows.len() - archived);
    let archived_count = saturate(archived);
    let view = if archive_open && (archived > 0 || searching) {
        ArchivedView::Archive
    } else {
        ArchivedView::Chats
    };
    let chats = view == ArchivedView::Chats;
    ArchivedSection {
        view,
        top_count,
        archived_count,
        bar: chats && !searching && archived > 0,
        divider: chats && searching && archived > 0,
    }
}

/// A list longer than `u32::MAX` cannot be on a screen; clamp rather than
/// wrap.
fn saturate(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const LIVE: ArchivedSectionRow = ArchivedSectionRow {
        archived: false,
        pinned: false,
    };
    const GONE: ArchivedSectionRow = ArchivedSectionRow {
        archived: true,
        pinned: false,
    };
    const PINNED_GONE: ArchivedSectionRow = ArchivedSectionRow {
        archived: true,
        pinned: true,
    };

    fn rows(live: usize, gone: usize) -> Vec<ArchivedSectionRow> {
        let mut v = vec![LIVE; live];
        v.extend(std::iter::repeat_n(GONE, gone));
        v
    }

    #[test]
    fn no_bar_when_nothing_is_archived() {
        let s = archived_section(&rows(2, 0), false, false);
        assert_eq!(
            s,
            ArchivedSection {
                view: ArchivedView::Chats,
                top_count: 2,
                archived_count: 0,
                bar: false,
                divider: false,
            }
        );
        assert!(!archived_section(&[], false, false).bar);
    }

    #[test]
    fn one_archived_conversation_is_behind_the_bar() {
        let s = archived_section(&rows(3, 1), false, false);
        assert_eq!(s.view, ArchivedView::Chats);
        assert_eq!(s.top_count, 3);
        assert_eq!(s.archived_count, 1);
        assert!(s.bar);
        assert!(!s.divider);
    }

    #[test]
    fn everything_archived_still_shows_the_bar() {
        let s = archived_section(&rows(0, 4), false, false);
        assert_eq!(s.top_count, 0);
        assert!(s.bar);
    }

    #[test]
    fn a_pinned_archived_conversation_stays_with_the_pins() {
        let s = archived_section(&[PINNED_GONE, LIVE, GONE], false, false);
        assert_eq!(s.top_count, 2);
        assert_eq!(s.archived_count, 1);
        // Pinned and archived alone: no bar.
        assert!(!archived_section(&[PINNED_GONE], false, false).bar);
    }

    #[test]
    fn opening_the_bar_shows_the_archive() {
        let s = archived_section(&rows(2, 3), false, true);
        assert_eq!(s.view, ArchivedView::Archive);
        assert_eq!(s.top_count, 2);
        assert_eq!(s.archived_count, 3);
        assert!(!s.bar);
        assert!(!s.divider);
    }

    #[test]
    fn an_emptied_archive_closes_by_itself() {
        let s = archived_section(&rows(2, 0), false, true);
        assert_eq!(s.view, ArchivedView::Chats);
        assert!(!s.bar);
    }

    #[test]
    fn a_search_with_no_match_keeps_the_archive_open() {
        let s = archived_section(&rows(0, 0), true, true);
        assert_eq!(s.view, ArchivedView::Archive);
    }

    #[test]
    fn search_lists_archived_matches_under_a_divider() {
        let s = archived_section(&rows(1, 2), true, false);
        assert_eq!(s.view, ArchivedView::Chats);
        assert!(s.divider);
        assert!(!s.bar);
        assert_eq!(s.archived_count, 2);
    }

    fn row_strategy() -> impl Strategy<Value = ArchivedSectionRow> {
        (any::<bool>(), any::<bool>())
            .prop_map(|(archived, pinned)| ArchivedSectionRow { archived, pinned })
    }

    proptest! {
        #[test]
        fn the_split_is_consistent(
            input in proptest::collection::vec(row_strategy(), 0..40),
            searching in any::<bool>(),
            archive_open in any::<bool>(),
        ) {
            let s = archived_section(&input, searching, archive_open);
            let top = s.top_count as usize;
            // Every row is accounted for.
            prop_assert!(top <= input.len());
            prop_assert_eq!(s.archived_count as usize, input.len() - top);
            // The archived part is exactly the trailing run of archived,
            // unpinned rows.
            prop_assert!(input[top..].iter().all(|r| r.archived && !r.pinned));
            if top > 0 {
                let last = input[top - 1];
                prop_assert!(!last.archived || last.pinned);
            }
            let any_archived = s.archived_count > 0;
            // The archive view only when opened, and never empty unless a
            // search is typed.
            prop_assert_eq!(
                s.view == ArchivedView::Archive,
                archive_open && (any_archived || searching)
            );
            // The bar and the divider never together, only in the chats view,
            // and exactly one of them whenever that view has archived rows.
            prop_assert!(!(s.bar && s.divider));
            if s.view == ArchivedView::Archive {
                prop_assert!(!s.bar && !s.divider);
            } else {
                prop_assert_eq!(s.bar, any_archived && !searching);
                prop_assert_eq!(s.divider, any_archived && searching);
            }
        }
    }
}
