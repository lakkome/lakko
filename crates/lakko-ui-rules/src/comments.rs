// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! What follows the first two comments under a collapsed channel post
//! (`docs/ux/group-and-channel-privacy.md` section 13.2).
//!
//! The channels service keeps a comment count per post (ADR-0087 phase B).
//! It counts every stored comment, including those this reader hides: a
//! banned author's (section 13.3: "Their comments disappear for everyone and
//! new ones are not shown") and one that does not open. So the count never
//! stands alone (base walk 2026-11-13: a post read "Show 2 more comments"
//! over a banned author's two comments, and opening it showed none):
//!
//! * before the first page of comments is read nothing is said, except that
//!   a count of none is `Comment`;
//! * the rows read are judged by themselves: `visible` is what this reader
//!   shows of them;
//! * only the rows not read yet come from the service:
//!   `server_count - rows_read`, where `rows_read` counts every row the
//!   service returned, shown or not;
//! * once every row is read, the rows alone decide.

/// The number of comments shown under a collapsed post.
pub const COLLAPSED_SHOWN: u32 = 2;

/// What follows the first comments of a collapsed thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollapsedComments {
    /// Nothing about the count: the first page is not read yet, or every
    /// comment already shows.
    Nothing,
    /// `Show {n} more comments`, `n >= 1`.
    Count(u32),
    /// `Show more comments`: rows remain unread and the service gave no
    /// count.
    Unknown,
    /// The quiet `Comment` link: there are none to show.
    Comment,
}

/// The rule in the module docs.
///
/// * `visible` — the comments read that this reader shows (not banned, opened).
/// * `rows_read` — every row the service returned so far, shown or not.
/// * `first_page_read` — the first page was read (or there was nothing to read).
/// * `more_rows` — the last page was full: more rows may exist.
/// * `server_count` — the service's count, `None` before it answered.
#[must_use]
pub fn collapsed_comments(
    visible: u32,
    rows_read: u32,
    first_page_read: bool,
    more_rows: bool,
    server_count: Option<u64>,
) -> CollapsedComments {
    if !first_page_read {
        return if server_count == Some(0) {
            CollapsedComments::Comment
        } else {
            CollapsedComments::Nothing
        };
    }
    let unread: u32 = if !more_rows {
        0
    } else if let Some(count) = server_count {
        let left = count.saturating_sub(u64::from(rows_read));
        u32::try_from(left).unwrap_or(u32::MAX)
    } else {
        return CollapsedComments::Unknown;
    };
    let rest = visible
        .saturating_sub(visible.min(COLLAPSED_SHOWN))
        .saturating_add(unread);
    if visible == 0 && rest == 0 {
        CollapsedComments::Comment
    } else if rest > 0 {
        CollapsedComments::Count(rest)
    } else {
        CollapsedComments::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_banned_authors_comments_the_service_still_counts_are_not_counted() {
        // Two rows read, both by a banned author; the service says 2.
        assert_eq!(
            collapsed_comments(0, 2, true, false, Some(2)),
            CollapsedComments::Comment
        );
    }

    #[test]
    fn a_comment_that_did_not_open_is_not_counted() {
        assert_eq!(
            collapsed_comments(3, 4, true, false, Some(4)),
            CollapsedComments::Count(1)
        );
    }

    #[test]
    fn nothing_is_said_before_the_first_page_except_none() {
        assert_eq!(
            collapsed_comments(0, 0, false, false, Some(2)),
            CollapsedComments::Nothing
        );
        assert_eq!(
            collapsed_comments(0, 0, false, false, None),
            CollapsedComments::Nothing
        );
        assert_eq!(
            collapsed_comments(0, 0, false, false, Some(0)),
            CollapsedComments::Comment
        );
    }

    #[test]
    fn only_rows_not_read_yet_come_from_the_service() {
        assert_eq!(
            collapsed_comments(5, 20, true, true, Some(30)),
            CollapsedComments::Count(13)
        );
        // A stale count never goes below what was read.
        assert_eq!(
            collapsed_comments(5, 20, true, true, Some(10)),
            CollapsedComments::Count(3)
        );
        assert_eq!(
            collapsed_comments(5, 20, true, true, None),
            CollapsedComments::Unknown
        );
    }

    #[test]
    fn two_or_fewer_shown_say_nothing_more() {
        assert_eq!(
            collapsed_comments(2, 2, true, false, Some(2)),
            CollapsedComments::Nothing
        );
        assert_eq!(
            collapsed_comments(1, 1, true, false, Some(1)),
            CollapsedComments::Nothing
        );
    }

    proptest! {
        /// Once every row is read the service's count changes nothing, and a
        /// count is never more than the comments this reader could show.
        #[test]
        fn a_finished_thread_counts_only_what_it_shows(
            visible in 0u32..10_000,
            hidden in 0u32..10_000,
            server in proptest::option::of(any::<u64>()),
        ) {
            let rows = visible + hidden;
            let got = collapsed_comments(visible, rows, true, false, server);
            prop_assert_eq!(got, collapsed_comments(visible, rows, true, false, None));
            match got {
                CollapsedComments::Count(n) => {
                    prop_assert!(n >= 1);
                    prop_assert_eq!(n, visible - COLLAPSED_SHOWN);
                }
                CollapsedComments::Comment => prop_assert_eq!(visible, 0),
                CollapsedComments::Nothing => prop_assert!((1..=COLLAPSED_SHOWN).contains(&visible)),
                CollapsedComments::Unknown => prop_assert!(false, "a finished thread is never unknown"),
            }
        }

        /// Hidden rows already read never add to the count, whatever the
        /// service says, and the rule never panics.
        #[test]
        fn hidden_rows_read_never_add(
            visible in 0u32..1_000,
            hidden in 0u32..1_000,
            more in any::<bool>(),
            server in proptest::option::of(any::<u64>()),
        ) {
            let with_hidden = collapsed_comments(visible, visible + hidden, true, more, server);
            let without = collapsed_comments(visible, visible, true, more, server);
            let n = |c: CollapsedComments| match c {
                CollapsedComments::Count(n) => n,
                _ => 0,
            };
            prop_assert!(n(with_hidden) <= n(without));
        }
    }
}
