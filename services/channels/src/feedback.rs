// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Comments and reactions under a post: the stored shapes, the bounds, and
//! the in-memory store ([ADR-0087] section 13a).
//!
//! The service stores rows it cannot read. A reaction row is `(post,
//! writer_key, label)`; a comment row is `(post, cursor, writer_key,
//! sealed)`. Every writer key is a key for that one write
//! (`lakko_channels::write_key`), so no row links to another. The routes are
//! in [`crate::feedback_api`]; the SQL store is [`crate::feedback_sql`].
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::storage::ChannelId;

/// Length of a write key.
pub const WRITER_KEY_LEN: usize = lakko_channels::WRITE_KEY_LEN;
/// Length of a reaction label.
pub const REACTION_LABEL_LEN: usize = lakko_channels::REACTION_LABEL_LEN;
/// Length of a write-proof nonce.
pub const WRITE_NONCE_LEN: usize = 16;
/// The most labels a reader gets for one post (ADR-0087 section 13a), the
/// most-counted first.
pub const MAX_LABELS_PER_POST: usize = 64;
/// A comment page (ADR-0087 section 13a).
pub const COMMENT_PAGE_LEN: usize = 20;
/// The most posts one counts read names. Chosen, not measured: more than a
/// screen of posts.
pub const MAX_COUNT_POSTS: usize = 50;
/// The most bytes a comment row holds (`lakko_channels::COMMENT_SEALED_MAX_LEN`).
pub const COMMENT_SEALED_MAX_LEN: usize = lakko_channels::COMMENT_SEALED_MAX_LEN;
/// The fewest bytes a sealed comment can have.
pub const COMMENT_SEALED_MIN_LEN: usize = lakko_channels::COMMENT_SEALED_MIN_LEN;

/// A write key.
pub type WriterKey = [u8; WRITER_KEY_LEN];
/// A reaction label.
pub type Label = [u8; REACTION_LABEL_LEN];

/// One stored comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredComment {
    /// The comment cursor: ascending, unique across the service.
    pub cursor: u64,
    /// The key of this one comment.
    pub writer_key: WriterKey,
    /// The sealed comment, opaque.
    pub sealed: Vec<u8>,
    /// When the row was stored, unix milliseconds.
    pub server_ts_ms: u64,
}

/// The label counts of one post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostReactionCounts {
    /// The post.
    pub post_cursor: u64,
    /// At most [`MAX_LABELS_PER_POST`] `(label, count)`, the most counted
    /// first, ties by label ascending.
    pub labels: Vec<(Label, u64)>,
}

/// The label counts of one comment (reactions on comments, UX 13.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentReactionCounts {
    /// The comment cursor.
    pub comment_cursor: u64,
    /// At most [`MAX_LABELS_PER_POST`] `(label, count)`, ranked as a post's.
    pub labels: Vec<(Label, u64)>,
}

/// The most comments one comment-reaction counts read names: a page of
/// comments and more. Chosen, not measured.
pub const MAX_COUNT_COMMENTS: usize = 50;

/// Rank `(label, count)` pairs the way both stores answer: count descending,
/// label ascending, at most [`MAX_LABELS_PER_POST`].
pub(crate) fn top_labels(mut labels: Vec<(Label, u64)>) -> Vec<(Label, u64)> {
    labels.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    labels.truncate(MAX_LABELS_PER_POST);
    labels
}

/// The in-memory half of the comments and reactions store, held inside
/// [`crate::InMemoryChannelStore`]'s state.
#[derive(Default)]
pub(crate) struct MemoryFeedback {
    reactions: HashMap<(ChannelId, u64), BTreeSet<(WriterKey, Label)>>,
    comments: HashMap<(ChannelId, u64), BTreeMap<u64, StoredComment>>,
    /// (channel, post, comment) -> reaction rows on that comment.
    comment_reactions: HashMap<(ChannelId, u64, u64), BTreeSet<(WriterKey, Label)>>,
    last_comment_id: u64,
    nonces: HashMap<[u8; WRITE_NONCE_LEN], u64>,
    /// Channel poll votes (ADR-0092 section 4.3).
    pub(crate) votes: crate::votes::MemoryVotes,
}

impl MemoryFeedback {
    pub(crate) fn add_reaction(
        &mut self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> bool {
        self.reactions
            .entry((channel, post_cursor))
            .or_default()
            .insert((writer_key, label))
    }

    pub(crate) fn delete_reaction(
        &mut self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> bool {
        self.reactions
            .get_mut(&(channel, post_cursor))
            .is_some_and(|rows| rows.remove(&(writer_key, label)))
    }

    pub(crate) fn reaction_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Vec<PostReactionCounts> {
        post_cursors
            .iter()
            .map(|&post_cursor| {
                let mut counts: HashMap<Label, u64> = HashMap::new();
                if let Some(rows) = self.reactions.get(&(channel, post_cursor)) {
                    for (_, label) in rows {
                        *counts.entry(*label).or_default() += 1;
                    }
                }
                PostReactionCounts {
                    post_cursor,
                    labels: top_labels(counts.into_iter().collect()),
                }
            })
            .collect()
    }

    pub(crate) fn add_comment(
        &mut self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        sealed: Vec<u8>,
        now_ms: u64,
    ) -> u64 {
        self.last_comment_id = self.last_comment_id.saturating_add(1);
        let cursor = self.last_comment_id;
        self.comments
            .entry((channel, post_cursor))
            .or_default()
            .insert(
                cursor,
                StoredComment {
                    cursor,
                    writer_key,
                    sealed,
                    server_ts_ms: now_ms,
                },
            );
        cursor
    }

    pub(crate) fn comment_writer(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> Option<WriterKey> {
        self.comments
            .get(&(channel, post_cursor))
            .and_then(|page| page.get(&comment_cursor))
            .map(|c| c.writer_key)
    }

    pub(crate) fn delete_comment(
        &mut self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> bool {
        let removed = self
            .comments
            .get_mut(&(channel, post_cursor))
            .is_some_and(|page| page.remove(&comment_cursor).is_some());
        if removed {
            // The SQL store's cascade from `channel_comments`.
            self.comment_reactions
                .remove(&(channel, post_cursor, comment_cursor));
        }
        removed
    }

    /// The number of comments under each post, in the order asked.
    pub(crate) fn comment_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Vec<(u64, u64)> {
        post_cursors
            .iter()
            .map(|&p| {
                let n = self.comments.get(&(channel, p)).map_or(0, BTreeMap::len);
                (p, n as u64)
            })
            .collect()
    }

    pub(crate) fn has_comment(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> bool {
        self.comments
            .get(&(channel, post_cursor))
            .is_some_and(|page| page.contains_key(&comment_cursor))
    }

    pub(crate) fn add_comment_reaction(
        &mut self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> bool {
        self.comment_reactions
            .entry((channel, post_cursor, comment_cursor))
            .or_default()
            .insert((writer_key, label))
    }

    pub(crate) fn delete_comment_reaction(
        &mut self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> bool {
        self.comment_reactions
            .get_mut(&(channel, post_cursor, comment_cursor))
            .is_some_and(|rows| rows.remove(&(writer_key, label)))
    }

    pub(crate) fn comment_reaction_counts(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursors: &[u64],
    ) -> Vec<CommentReactionCounts> {
        comment_cursors
            .iter()
            .map(|&comment_cursor| {
                let mut counts: HashMap<Label, u64> = HashMap::new();
                if let Some(rows) =
                    self.comment_reactions
                        .get(&(channel, post_cursor, comment_cursor))
                {
                    for (_, label) in rows {
                        *counts.entry(*label).or_default() += 1;
                    }
                }
                CommentReactionCounts {
                    comment_cursor,
                    labels: top_labels(counts.into_iter().collect()),
                }
            })
            .collect()
    }

    pub(crate) fn list_comments(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        after: u64,
        limit: usize,
    ) -> Vec<StoredComment> {
        self.comments
            .get(&(channel, post_cursor))
            .map(|page| {
                page.range(after.saturating_add(1)..)
                    .take(limit)
                    .map(|(_, c)| c.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn consume_nonce(&mut self, nonce: [u8; WRITE_NONCE_LEN], now_ms: u64) -> bool {
        if self.nonces.contains_key(&nonce) {
            return false;
        }
        self.nonces.insert(nonce, now_ms);
        true
    }

    pub(crate) fn prune_nonces_older_than(&mut self, cutoff_ms: u64) -> u64 {
        let before = self.nonces.len();
        self.nonces.retain(|_, seen| *seen >= cutoff_ms);
        (before - self.nonces.len()) as u64
    }

    /// Drop the rows of every post `keep` says is gone: the SQL store's
    /// cascade from `channel_posts`.
    pub(crate) fn retain_posts(&mut self, mut keep: impl FnMut(ChannelId, u64) -> bool) {
        self.reactions.retain(|(c, p), _| keep(*c, *p));
        self.comments.retain(|(c, p), _| keep(*c, *p));
        self.comment_reactions.retain(|(c, p, _), _| keep(*c, *p));
        self.votes.retain_posts(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_rank_by_count_then_label_and_stop_at_64() {
        let mut labels: Vec<(Label, u64)> = (0u8..70).map(|i| ([i; 16], 1)).collect();
        labels.push(([200; 16], 5));
        labels.push(([100; 16], 5));
        let top = top_labels(labels);
        assert_eq!(top.len(), MAX_LABELS_PER_POST);
        assert_eq!(top[0], ([100; 16], 5));
        assert_eq!(top[1], ([200; 16], 5));
        assert_eq!(top[2], ([0; 16], 1));
    }
}
