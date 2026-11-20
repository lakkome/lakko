// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The tally of an anonymous poll (ADR-0092 sections 3.5 and 3.6).
//!
//! For each tag, the last valid ballot in DS order before the close counts;
//! its choices are added up, and an empty choice list is a retracted vote.
//! A ballot past the close (a row after the close's DS `messageId`, or with a
//! DS `created_at` at or after `close_at_ms`) is ignored. A ballot whose
//! choices do not fit the poll (an index at or past the option count, or more
//! than one choice in a single-choice poll) is dropped and replaces nothing.
//!
//! The fold sorts its input by `(message_id, created_at_ms, tag, choices)`
//! before folding, so the result does not depend on the order ballots were
//! verified or handed in. DS message ids are unique per journal; the rest of
//! the key only makes the order total for hostile duplicate input.

use std::collections::BTreeMap;

use crate::ballot::{Choices, MAX_OPTIONS};
use crate::point::POINT_LEN;

/// What a client knows about the poll the tally is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollRules {
    /// The number of options, 2..=10.
    pub option_count: u8,
    /// Whether several options may be chosen.
    pub multiple: bool,
    /// The DS `messageId` of the accepted close, if any.
    pub close_message_id: Option<i64>,
    /// The poll's `close_at_ms`, if it has one.
    pub close_at_ms: Option<i64>,
}

/// A verified ballot with its row's DS position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountedBallot {
    /// The DS `messageId` of the row.
    pub message_id: i64,
    /// The DS `created_at` of the row, milliseconds.
    pub created_at_ms: i64,
    /// The voter's tag.
    pub tag: [u8; POINT_LEN],
    /// The choices.
    pub choices: Choices,
}

/// The result of a tally.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Tally {
    /// Votes per option, `option_count` entries.
    pub counts: Vec<u32>,
    /// Tags whose last counted ballot chose something.
    pub voters: u32,
}

impl PollRules {
    fn after_close(&self, ballot: &CountedBallot) -> bool {
        self.close_message_id
            .is_some_and(|close| ballot.message_id > close)
            || self
                .close_at_ms
                .is_some_and(|at| ballot.created_at_ms >= at)
    }

    fn choices_fit(&self, choices: &Choices) -> bool {
        let indices = choices.indices();
        (self.multiple || indices.len() <= 1) && indices.iter().all(|&i| i < self.option_count)
    }
}

/// Fold verified ballots into a tally. Total and order-independent.
#[must_use]
pub fn fold(rules: &PollRules, ballots: &[CountedBallot]) -> Tally {
    let options = usize::from(rules.option_count).min(MAX_OPTIONS);
    let mut ordered: Vec<&CountedBallot> = ballots.iter().collect();
    ordered.sort_by(|a, b| {
        (a.message_id, a.created_at_ms, &a.tag, &a.choices).cmp(&(
            b.message_id,
            b.created_at_ms,
            &b.tag,
            &b.choices,
        ))
    });
    let mut last: BTreeMap<[u8; POINT_LEN], &Choices> = BTreeMap::new();
    for ballot in ordered {
        if rules.after_close(ballot) || !rules.choices_fit(&ballot.choices) {
            continue;
        }
        last.insert(ballot.tag, &ballot.choices);
    }
    let mut counts = vec![0u32; options];
    let mut voters = 0u32;
    for choices in last.values() {
        if choices.indices().is_empty() {
            continue;
        }
        voters = voters.saturating_add(1);
        for &i in choices.indices() {
            if let Some(slot) = counts.get_mut(usize::from(i)) {
                *slot = slot.saturating_add(1);
            }
        }
    }
    Tally { counts, voters }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(id: i64, tag: u8, choices: &[u8]) -> CountedBallot {
        CountedBallot {
            message_id: id,
            created_at_ms: id * 10,
            tag: [tag; 32],
            choices: Choices::new(choices).unwrap(),
        }
    }

    fn rules() -> PollRules {
        PollRules {
            option_count: 3,
            multiple: false,
            close_message_id: None,
            close_at_ms: None,
        }
    }

    #[test]
    fn last_ballot_per_tag_counts_and_empty_retracts() {
        let t = fold(
            &rules(),
            &[b(1, 1, &[0]), b(2, 2, &[1]), b(3, 1, &[2]), b(4, 2, &[])],
        );
        assert_eq!(
            t,
            Tally {
                counts: vec![0, 0, 1],
                voters: 1
            }
        );
    }

    #[test]
    fn invalid_choices_replace_nothing() {
        let t = fold(&rules(), &[b(1, 1, &[0]), b(2, 1, &[0, 1]), b(3, 1, &[5])]);
        assert_eq!(t.counts, vec![1, 0, 0]);
    }

    #[test]
    fn close_by_message_and_by_time() {
        let mut r = rules();
        r.close_message_id = Some(2);
        assert_eq!(
            fold(&r, &[b(1, 1, &[0]), b(3, 1, &[1])]).counts,
            vec![1, 0, 0]
        );
        r.close_message_id = None;
        r.close_at_ms = Some(30);
        assert_eq!(
            fold(&r, &[b(1, 1, &[0]), b(3, 1, &[1])]).counts,
            vec![1, 0, 0]
        );
    }
}
