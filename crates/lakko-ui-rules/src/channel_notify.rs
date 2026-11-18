// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Whether a channel post shows a notification on this device
//! (`docs/ux/channel-notifications.md` section 2).
//!
//! There is no push for channels yet: a device notices a post when it reads
//! the channel (section 3). The client reads a batch of new records, asks
//! [`channel_posts_to_notify`] which of them notify, shows the newest of
//! those (one notification per channel, the newest replacing the older), and
//! stores the returned last-notified cursor, per device and not synced.
//!
//! A post does not notify when:
//!
//! * the channel is muted;
//! * the post is this account's own (the client decides what "own" is);
//! * the channel is open and visible on this device;
//! * it is at or below the channel's read marker, or at or below the cursor
//!   this device last notified, so no post is notified twice;
//! * the read is a refetch of the channel's history (the first read after
//!   subscribing, after joining as a publisher, after the post log was
//!   dropped or restored): those posts are history, not news. The one
//!   exception: on the first read after SUBSCRIBING, the newest post may
//!   notify when it is less than [`FIRST_READ_FRESH_MS`] old;
//! * the record is not a post (a metadata record, a ban list, a rotation).
//!
//! Every record read moves the last-notified cursor forward, notified or
//! not: a post decided once is never decided again, so unmuting a channel
//! or closing it never announces what was already read.

/// How old the newest post may be to notify on the first read after
/// subscribing: one hour (`docs/ux/channel-notifications.md` section 2).
pub const FIRST_READ_FRESH_MS: u64 = 3_600_000;

/// What kind of read produced the records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelReadKind {
    /// An ordinary read of posts that arrived since the last one.
    Ongoing,
    /// The first read of the channel after subscribing to it: history, except
    /// that its newest post may notify while it is fresh.
    FirstAfterSubscribe,
    /// Any other refetch of the history (after joining as a publisher, a
    /// dropped or restored post log): nothing notifies.
    History,
}

/// The channel's state as this device holds it when the records were read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelNotifyContext {
    /// The channel is muted (the synced flag).
    pub muted: bool,
    /// The channel is open and visible on this device (a focused window on
    /// desktop and web, the foreground screen on a phone).
    pub open_and_visible: bool,
    /// The newest post cursor read; `0` when none.
    pub read_marker: u64,
    /// The newest cursor this device already decided about; `0` when none.
    pub last_notified: u64,
    /// What kind of read this is.
    pub read_kind: ChannelReadKind,
    /// Now, unix milliseconds.
    pub now_ms: u64,
}

/// One record read from the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelPostCandidate {
    /// The server's per-channel cursor.
    pub cursor: u64,
    /// The record is a post (not a metadata record, ban list or rotation,
    /// and it opened).
    pub is_post: bool,
    /// The post is this account's own.
    pub own: bool,
    /// The server's receive time, unix milliseconds.
    pub server_ts_ms: u64,
}

/// Which records of a batch notify, and the cursor to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelNotifyBatch {
    /// Indices into the records passed, ascending by cursor, each cursor at
    /// most once. The client shows the last one.
    pub notify: Vec<u32>,
    /// The new last-notified cursor: never below the one passed, and at
    /// least the newest cursor read.
    pub last_notified: u64,
}

/// The rule for one record. `newest_of_read` says whether it is the newest
/// post of the read it came in (it matters on the first read after
/// subscribing only).
#[must_use]
pub fn channel_post_notifies(
    context: &ChannelNotifyContext,
    post: &ChannelPostCandidate,
    newest_of_read: bool,
) -> bool {
    if !post.is_post || post.own || context.muted || context.open_and_visible {
        return false;
    }
    if post.cursor <= context.read_marker || post.cursor <= context.last_notified {
        return false;
    }
    match context.read_kind {
        ChannelReadKind::Ongoing => true,
        ChannelReadKind::History => false,
        ChannelReadKind::FirstAfterSubscribe => {
            newest_of_read && context.now_ms.saturating_sub(post.server_ts_ms) < FIRST_READ_FRESH_MS
        },
    }
}

/// The rule over a batch of records read together: which notify, and the
/// last-notified cursor to store afterwards.
#[must_use]
pub fn channel_posts_to_notify(
    context: &ChannelNotifyContext,
    posts: &[ChannelPostCandidate],
) -> ChannelNotifyBatch {
    let newest_post = posts.iter().filter(|p| p.is_post).map(|p| p.cursor).max();
    let mut order: Vec<usize> = (0..posts.len()).collect();
    order.sort_by_key(|&i| (posts[i].cursor, i));
    let mut notify = Vec::new();
    let mut decided = context.last_notified;
    let mut at = 0;
    while at < order.len() {
        // The records sharing one cursor are judged together, against the
        // cursor decided before them, and give at most one notification.
        let cursor = posts[order[at]].cursor;
        let mut end = at;
        while end < order.len() && posts[order[end]].cursor == cursor {
            end += 1;
        }
        let ctx = ChannelNotifyContext {
            last_notified: decided,
            ..*context
        };
        let newest = newest_post == Some(cursor);
        if let Some(&i) = order[at..end]
            .iter()
            .find(|&&i| channel_post_notifies(&ctx, &posts[i], newest))
        {
            if let Ok(index) = u32::try_from(i) {
                notify.push(index);
            }
        }
        decided = decided.max(cursor);
        at = end;
    }
    ChannelNotifyBatch {
        notify,
        last_notified: decided,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const NOW: u64 = 1_800_000_000_000;

    fn ctx() -> ChannelNotifyContext {
        ChannelNotifyContext {
            muted: false,
            open_and_visible: false,
            read_marker: 10,
            last_notified: 10,
            read_kind: ChannelReadKind::Ongoing,
            now_ms: NOW,
        }
    }

    fn post(cursor: u64) -> ChannelPostCandidate {
        ChannelPostCandidate {
            cursor,
            is_post: true,
            own: false,
            server_ts_ms: NOW - 1_000,
        }
    }

    #[test]
    fn a_new_post_notifies() {
        assert!(channel_post_notifies(&ctx(), &post(11), true));
    }

    #[test]
    fn muted_own_open_and_non_posts_never_notify() {
        let muted = ChannelNotifyContext {
            muted: true,
            ..ctx()
        };
        assert!(!channel_post_notifies(&muted, &post(11), true));
        let open = ChannelNotifyContext {
            open_and_visible: true,
            ..ctx()
        };
        assert!(!channel_post_notifies(&open, &post(11), true));
        let own = ChannelPostCandidate {
            own: true,
            ..post(11)
        };
        assert!(!channel_post_notifies(&ctx(), &own, true));
        let record = ChannelPostCandidate {
            is_post: false,
            ..post(11)
        };
        assert!(!channel_post_notifies(&ctx(), &record, true));
    }

    #[test]
    fn a_post_read_or_already_notified_does_not_notify() {
        let read = ChannelNotifyContext {
            read_marker: 12,
            ..ctx()
        };
        assert!(!channel_post_notifies(&read, &post(12), true));
        let notified = ChannelNotifyContext {
            last_notified: 12,
            ..ctx()
        };
        assert!(!channel_post_notifies(&notified, &post(12), true));
        assert!(channel_post_notifies(&notified, &post(13), true));
    }

    #[test]
    fn a_history_refetch_notifies_nothing() {
        let history = ChannelNotifyContext {
            read_kind: ChannelReadKind::History,
            read_marker: 0,
            last_notified: 0,
            ..ctx()
        };
        let batch = channel_posts_to_notify(&history, &[post(1), post(2), post(3)]);
        assert!(batch.notify.is_empty());
        assert_eq!(batch.last_notified, 3);
    }

    #[test]
    fn the_first_read_after_subscribing_notifies_the_newest_fresh_post_only() {
        let first = ChannelNotifyContext {
            read_kind: ChannelReadKind::FirstAfterSubscribe,
            read_marker: 0,
            last_notified: 0,
            ..ctx()
        };
        let batch = channel_posts_to_notify(&first, &[post(3), post(1), post(2)]);
        assert_eq!(batch.notify, vec![0]);
        assert_eq!(batch.last_notified, 3);

        // An hour or older: nothing.
        let stale = ChannelPostCandidate {
            server_ts_ms: NOW - FIRST_READ_FRESH_MS,
            ..post(3)
        };
        let batch = channel_posts_to_notify(&first, &[post(1), stale]);
        assert!(batch.notify.is_empty());

        // The newest is a record, not a post: the newest POST counts.
        let record = ChannelPostCandidate {
            is_post: false,
            ..post(4)
        };
        let batch = channel_posts_to_notify(&first, &[post(2), record]);
        assert_eq!(batch.notify, vec![0]);
        assert_eq!(batch.last_notified, 4);
    }

    #[test]
    fn every_record_read_moves_the_cursor_even_when_nothing_notifies() {
        let muted = ChannelNotifyContext {
            muted: true,
            ..ctx()
        };
        let batch = channel_posts_to_notify(&muted, &[post(11), post(14)]);
        assert!(batch.notify.is_empty());
        assert_eq!(batch.last_notified, 14);
        // Unmuting later announces none of them.
        let after = ChannelNotifyContext {
            last_notified: batch.last_notified,
            ..ctx()
        };
        assert!(channel_posts_to_notify(&after, &[post(11), post(14)])
            .notify
            .is_empty());
    }

    #[test]
    fn a_cursor_seen_twice_in_one_batch_notifies_once() {
        let batch = channel_posts_to_notify(&ctx(), &[post(12), post(11), post(12)]);
        assert_eq!(batch.notify, vec![1, 0]);
        assert_eq!(batch.last_notified, 12);
    }

    #[test]
    fn the_cursor_never_moves_back() {
        let ahead = ChannelNotifyContext {
            last_notified: 50,
            ..ctx()
        };
        let batch = channel_posts_to_notify(&ahead, &[post(20)]);
        assert!(batch.notify.is_empty());
        assert_eq!(batch.last_notified, 50);
        assert_eq!(channel_posts_to_notify(&ahead, &[]).last_notified, 50);
    }

    fn arb_kind() -> impl Strategy<Value = ChannelReadKind> {
        prop_oneof![
            Just(ChannelReadKind::Ongoing),
            Just(ChannelReadKind::FirstAfterSubscribe),
            Just(ChannelReadKind::History),
        ]
    }

    fn arb_post() -> impl Strategy<Value = ChannelPostCandidate> {
        (
            0u64..60,
            any::<bool>(),
            any::<bool>(),
            0u64..(2 * FIRST_READ_FRESH_MS),
        )
            .prop_map(|(cursor, is_post, own, age)| ChannelPostCandidate {
                cursor,
                is_post,
                own,
                server_ts_ms: NOW - age,
            })
    }

    proptest! {
        /// Over any sequence of reads, carrying the stored cursor from one to
        /// the next: no cursor is notified twice, none at or below the read
        /// marker, none of a muted channel, none own, none that is not a
        /// post, and the stored cursor never moves back.
        #[test]
        fn reads_never_notify_twice_or_below_the_marker(
            reads in proptest::collection::vec(
                (
                    proptest::collection::vec(arb_post(), 0..12),
                    any::<bool>(),
                    any::<bool>(),
                    0u64..40,
                    arb_kind(),
                ),
                0..8,
            ),
        ) {
            let mut stored = 0u64;
            let mut notified = std::collections::BTreeSet::new();
            for (posts, muted, open, marker, kind) in reads {
                let context = ChannelNotifyContext {
                    muted,
                    open_and_visible: open,
                    read_marker: marker,
                    last_notified: stored,
                    read_kind: kind,
                    now_ms: NOW,
                };
                let batch = channel_posts_to_notify(&context, &posts);
                prop_assert!(batch.last_notified >= stored);
                for &i in &batch.notify {
                    let p = posts[i as usize];
                    prop_assert!(!muted && !open);
                    prop_assert!(p.is_post && !p.own);
                    prop_assert!(p.cursor > marker);
                    prop_assert!(p.cursor > stored);
                    prop_assert!(kind != ChannelReadKind::History);
                    prop_assert!(notified.insert(p.cursor), "cursor {} notified twice", p.cursor);
                }
                if kind == ChannelReadKind::FirstAfterSubscribe {
                    prop_assert!(batch.notify.len() <= 1);
                }
                if let Some(max) = posts.iter().map(|p| p.cursor).max() {
                    prop_assert!(batch.last_notified >= max);
                }
                stored = batch.last_notified;
            }
        }

        /// The order the records come in changes nothing but the indices.
        #[test]
        fn the_input_order_does_not_change_which_cursors_notify(
            posts in proptest::collection::vec(arb_post(), 0..12),
            kind in arb_kind(),
        ) {
            let context = ChannelNotifyContext { read_kind: kind, read_marker: 5, last_notified: 5, ..ctx() };
            let forward = channel_posts_to_notify(&context, &posts);
            let mut reversed = posts.clone();
            reversed.reverse();
            let backward = channel_posts_to_notify(&context, &reversed);
            let cursors = |b: &ChannelNotifyBatch, ps: &[ChannelPostCandidate]| {
                b.notify.iter().map(|&i| ps[i as usize].cursor).collect::<Vec<_>>()
            };
            prop_assert_eq!(cursors(&forward, &posts), cursors(&backward, &reversed));
            prop_assert_eq!(forward.last_notified, backward.last_notified);
        }
    }
}
