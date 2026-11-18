// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel poll votes: the stored shapes, the ordering rule, the in-memory
//! store and the per-tag replace cap ([ADR-0092] section 4.3).
//!
//! A vote row is `(post, tag, labels, at_ms)`. The tag is the anonymous
//! credential's per-poll tag (`lakko-anon-cred`): the same for one account
//! and one poll, unlinkable across polls. Labels are opaque 16-byte option
//! labels; a retract leaves the row with no labels (a tombstone) so its
//! `at_ms` still orders later requests. Counters per label and a voter count
//! (rows with labels) are kept in the same transaction as the row. The routes
//! are in [`crate::votes_api`]; the SQL store is `crate::votes_sql`.
//!
//! **Ordering.** A request carries `at_ms`, the client's clock, bound into
//! the presentation. It is accepted only when strictly greater than the row's
//! stored `at_ms`, so a captured request cannot be replayed to undo or
//! restore a vote once a newer one landed; and only within
//! [`VOTE_WINDOW_MS`] of the service clock, so a captured request that never
//! landed expires.
//!
//! [ADR-0092]: ../../../docs/adr/0092-attachment-kinds.md

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::feedback::{top_labels, Label};
use crate::storage::ChannelId;

/// Length of a vote tag (a Ristretto255 point).
pub const TAG_LEN: usize = lakko_anon_cred::TAG_LEN;

/// A vote tag.
pub type Tag = [u8; TAG_LEN];

/// How far a vote's `at_ms` may be from the service clock, either way.
/// Chosen, not measured: the write-proof window of the other subscriber
/// writes (`lakko_channels::WRITE_PROOF_WINDOW_MS`).
pub const VOTE_WINDOW_MS: u64 = lakko_channels::WRITE_PROOF_WINDOW_MS;

/// Accepted requests (put or delete) per tag per [`VOTE_CAP_WINDOW`].
/// Chosen, not measured: far above a person changing their mind.
pub const VOTE_REPLACES_PER_TAG: u32 = 20;

/// The window of [`VOTE_REPLACES_PER_TAG`].
pub const VOTE_CAP_WINDOW: Duration = Duration::from_secs(3600);

/// The most tags the cap remembers; past it, expired entries are dropped,
/// then the whole map. Chosen, not measured (a few MiB).
pub const VOTE_CAP_MAX_TAGS: usize = 100_000;

/// What storing a vote did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteOutcome {
    /// A put: `replaced` when the tag had a counted vote before.
    Stored {
        /// The tag had a vote with labels before.
        replaced: bool,
    },
    /// A delete: `existed` when the tag had a counted vote before.
    Retracted {
        /// The tag had a vote with labels before.
        existed: bool,
    },
    /// `at_ms` is not greater than the stored one; nothing changed.
    Stale {
        /// The `at_ms` stored for the tag.
        stored_at_ms: u64,
    },
}

/// The vote counts of one post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostVoteCounts {
    /// The post.
    pub post_cursor: u64,
    /// Tags with a counted vote.
    pub voters: u64,
    /// `(label, count)`, the most counted first, at most 64.
    pub labels: Vec<(Label, u64)>,
}

/// One stored vote row.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VoteRow {
    labels: Vec<Label>,
    at_ms: u64,
}

/// The in-memory votes, held inside the in-memory store's feedback.
#[derive(Default)]
pub(crate) struct MemoryVotes {
    rows: HashMap<(ChannelId, u64), BTreeMap<Tag, VoteRow>>,
}

impl MemoryVotes {
    /// Store, replace (non-empty `labels`) or retract (empty) the tag's vote
    /// when `at_ms` is newer than the stored one.
    pub(crate) fn apply(
        &mut self,
        channel: ChannelId,
        post_cursor: u64,
        tag: Tag,
        labels: Vec<Label>,
        at_ms: u64,
    ) -> VoteOutcome {
        let rows = self.rows.entry((channel, post_cursor)).or_default();
        let had = match rows.get(&tag) {
            Some(row) if at_ms <= row.at_ms => {
                return VoteOutcome::Stale {
                    stored_at_ms: row.at_ms,
                }
            },
            Some(row) => !row.labels.is_empty(),
            None => false,
        };
        let put = !labels.is_empty();
        rows.insert(tag, VoteRow { labels, at_ms });
        if put {
            VoteOutcome::Stored { replaced: had }
        } else {
            VoteOutcome::Retracted { existed: had }
        }
    }

    pub(crate) fn counts(&self, channel: ChannelId, post_cursors: &[u64]) -> Vec<PostVoteCounts> {
        post_cursors
            .iter()
            .map(|&post_cursor| {
                let mut voters = 0u64;
                let mut counts: HashMap<Label, u64> = HashMap::new();
                if let Some(rows) = self.rows.get(&(channel, post_cursor)) {
                    for row in rows.values().filter(|r| !r.labels.is_empty()) {
                        voters += 1;
                        for l in &row.labels {
                            *counts.entry(*l).or_default() += 1;
                        }
                    }
                }
                PostVoteCounts {
                    post_cursor,
                    voters,
                    labels: top_labels(counts.into_iter().collect()),
                }
            })
            .collect()
    }

    pub(crate) fn retain_posts(&mut self, mut keep: impl FnMut(ChannelId, u64) -> bool) {
        self.rows.retain(|(c, p), _| keep(*c, *p));
    }
}

/// One cap entry: requests counted and the start of their window.
type CapEntries = HashMap<(ChannelId, u64, Tag), (u32, Instant)>;

/// The in-memory cap on accepted requests per tag (ADR-0092 4.3: "repeated
/// replaces under one tag are capped in memory per tag"). Per process; a
/// restart resets it.
#[derive(Default)]
pub struct VoteCap {
    entries: Mutex<CapEntries>,
}

impl VoteCap {
    /// Count one accepted request for the tag; `false` when the tag spent
    /// its window.
    pub fn admit(&self, channel: ChannelId, post_cursor: u64, tag: Tag) -> bool {
        self.admit_at(channel, post_cursor, tag, Instant::now())
    }

    fn admit_at(&self, channel: ChannelId, post_cursor: u64, tag: Tag, now: Instant) -> bool {
        let Ok(mut map) = self.entries.lock() else {
            // A poisoned lock means a panic elsewhere held it; the cap is an
            // anti-abuse bound, not a security gate, so admit.
            return true;
        };
        if map.len() >= VOTE_CAP_MAX_TAGS && !map.contains_key(&(channel, post_cursor, tag)) {
            map.retain(|_, (_, start)| now.duration_since(*start) < VOTE_CAP_WINDOW);
            if map.len() >= VOTE_CAP_MAX_TAGS {
                tracing::warn!("vote cap at its size bound; cleared");
                map.clear();
            }
        }
        let entry = map.entry((channel, post_cursor, tag)).or_insert((0, now));
        if now.duration_since(entry.1) >= VOTE_CAP_WINDOW {
            *entry = (0, now);
        }
        if entry.0 >= VOTE_REPLACES_PER_TAG {
            return false;
        }
        entry.0 += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch() -> ChannelId {
        ChannelId::from_hex("0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c").unwrap()
    }

    #[test]
    fn newer_requests_replace_and_older_ones_are_stale() {
        let mut v = MemoryVotes::default();
        let tag = [1u8; 32];
        assert_eq!(
            v.apply(ch(), 1, tag, vec![[1; 16]], 10),
            VoteOutcome::Stored { replaced: false }
        );
        assert_eq!(
            v.apply(ch(), 1, tag, vec![[2; 16]], 10),
            VoteOutcome::Stale { stored_at_ms: 10 }
        );
        assert_eq!(
            v.apply(ch(), 1, tag, vec![[2; 16], [3; 16]], 11),
            VoteOutcome::Stored { replaced: true }
        );
        let c = v.counts(ch(), &[1]);
        assert_eq!(c[0].voters, 1);
        assert_eq!(c[0].labels, vec![([2; 16], 1), ([3; 16], 1)]);
        // Retract leaves a tombstone that still orders.
        assert_eq!(
            v.apply(ch(), 1, tag, vec![], 12),
            VoteOutcome::Retracted { existed: true }
        );
        assert_eq!(v.counts(ch(), &[1])[0].voters, 0);
        assert_eq!(
            v.apply(ch(), 1, tag, vec![[1; 16]], 11),
            VoteOutcome::Stale { stored_at_ms: 12 }
        );
        assert_eq!(
            v.apply(ch(), 1, [2; 32], vec![], 5),
            VoteOutcome::Retracted { existed: false }
        );
    }

    #[test]
    fn the_cap_admits_twenty_per_tag_per_window() {
        let cap = VoteCap::default();
        let t0 = Instant::now();
        for _ in 0..VOTE_REPLACES_PER_TAG {
            assert!(cap.admit_at(ch(), 1, [1; 32], t0));
        }
        assert!(!cap.admit_at(ch(), 1, [1; 32], t0));
        assert!(cap.admit_at(ch(), 1, [2; 32], t0));
        assert!(cap.admit_at(ch(), 1, [1; 32], t0 + VOTE_CAP_WINDOW));
    }
}
