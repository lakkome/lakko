// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Whether a DeviceGroup journal row is readable by every device —
//! ADR-0079 (order of 2026-10-17).
//!
//! A DeviceGroup message is sealed at the epoch its author holds. If another
//! device's commit lands on the journal between the author's seal and its
//! row, every other device has moved past that epoch when it reaches the row
//! and cannot open it (OpenMLS keeps no past epochs). Its author cannot open
//! its own row either, so it must work the answer out from the journal: a
//! row COUNTS only if the epoch it was sealed at (`epoch_of_message`, in the
//! public framing) equals the journal's epoch at the row's position — the
//! epoch after the last commit before it that took effect.
//!
//! A claim or a link-founded note that does not count is treated as never
//! posted, by every device: its author posts it again at the current epoch.
//!
//! **Which commits took effect.** A commit row takes effect when it was
//! sealed at the journal's epoch at its position AND the device's group is
//! past that epoch after the row: true for another device's commit this
//! device applied, and for this device's own commit (already merged
//! locally); false for a commit every device refuses (its epoch does not
//! move) and for a commit that lost to an earlier one at the same epoch.
//! The device's group epoch alone is NOT the journal's epoch at a row: a
//! device that merged its own commit locally is ahead of the journal until
//! the loop reaches that commit's row, which is why the journal epoch is
//! tracked from the commit rows instead.
//!
//! [`JournalEpochs`] is the record every device keeps: the epoch before the
//! oldest commit it remembers, and the effective commits by row id (the
//! newest [`JOURNAL_EPOCHS_MAX_COMMITS`]). The answer for a row depends only
//! on the commits before it, so re-reading part of the journal after a
//! restart gives the same answers.

/// How many effective commits a [`JournalEpochs`] remembers. Chosen, not
/// measured: a row is judged within a few passes of its publish, long before
/// as many commits follow it.
pub const JOURNAL_EPOCHS_MAX_COMMITS: usize = 64;

/// The journal's epochs as one device has read them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalEpochs {
    /// The journal's epoch before the oldest commit in `commits`.
    pub base: u64,
    /// Effective commits: `(row id, epoch after it)`, ascending by row id.
    pub commits: Vec<(u64, u64)>,
}

impl JournalEpochs {
    /// A record starting at `base` with no commit seen.
    #[must_use]
    pub const fn starting_at(base: u64) -> Self {
        Self {
            base,
            commits: Vec::new(),
        }
    }

    /// The journal's epoch at row `row_id`: the epoch after the last
    /// effective commit before it, else [`Self::base`].
    #[must_use]
    pub fn epoch_at(&self, row_id: u64) -> u64 {
        self.commits
            .iter()
            .filter(|(id, _)| *id < row_id)
            .map(|(_, after)| *after)
            .max()
            .unwrap_or(self.base)
    }

    /// Whether a non-commit row (a claim, a note) at `row_id`, sealed at
    /// `sealed_epoch`, is readable by every device.
    #[must_use]
    pub fn row_counts(&self, row_id: u64, sealed_epoch: u64) -> bool {
        sealed_epoch == self.epoch_at(row_id)
    }

    /// Take in a row at `row_id` after the device processed it (or skipped
    /// it as its own): `sealed_epoch` and `epoch_after` from its public
    /// framing (`epoch_of_message` / `epoch_after_message`), `group_epoch_after`
    /// the device's group epoch after the row. Records the row when it is a
    /// commit that took effect (see the module doc); `true` when it did.
    pub fn observe(
        &mut self,
        row_id: u64,
        sealed_epoch: u64,
        epoch_after: u64,
        group_epoch_after: u64,
    ) -> bool {
        let is_commit = epoch_after > sealed_epoch;
        if !is_commit || sealed_epoch != self.epoch_at(row_id) || group_epoch_after <= sealed_epoch
        {
            return false;
        }
        if self.commits.iter().any(|(id, _)| *id == row_id) {
            return true;
        }
        self.commits.push((row_id, epoch_after));
        self.commits.sort_by_key(|(id, _)| *id);
        if self.commits.len() > JOURNAL_EPOCHS_MAX_COMMITS {
            let drop = self.commits.len() - JOURNAL_EPOCHS_MAX_COMMITS;
            for (_, after) in self.commits.drain(..drop) {
                self.base = self.base.max(after);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_claim::{link_claim_outcome, LinkClaimOutcome, ObservedLinkClaim};
    use proptest::prelude::*;

    #[test]
    fn a_row_counts_only_at_the_journal_epoch_at_its_position() {
        let mut j = JournalEpochs::starting_at(5);
        assert!(j.row_counts(10, 5));
        // Another device's commit at epoch 5 lands at row 11.
        assert!(j.observe(11, 5, 6, 6));
        // A claim sealed at 5 that landed at row 12 is straddled; one at 6 counts.
        assert!(!j.row_counts(12, 5));
        assert!(j.row_counts(12, 6));
        // A row sealed AHEAD of the journal does not count either.
        assert!(!j.row_counts(12, 7));
        // Re-reading the earlier row after a restart gives the same answer.
        assert!(j.row_counts(10, 5));
    }

    #[test]
    fn an_own_commit_merged_early_does_not_straddle_the_claim_before_its_row() {
        // The device sealed a claim at 5 (row 20), then made its own commit
        // at 5 and merged it locally (group at 6) before its loop reached row
        // 20. The claim counts: the journal is at 5 until row 21.
        let mut j = JournalEpochs::starting_at(5);
        assert!(j.row_counts(20, 5));
        assert!(j.observe(21, 5, 6, 6));
        assert_eq!(j.epoch_at(22), 6);
    }

    #[test]
    fn a_refused_or_losing_commit_does_not_move_the_journal() {
        let mut j = JournalEpochs::starting_at(5);
        // Every device refused it: the group stays at 5.
        assert!(!j.observe(3, 5, 6, 5));
        assert_eq!(j.epoch_at(4), 5);
        // A second commit at 5 after an effective one loses.
        assert!(j.observe(4, 5, 6, 6));
        assert!(!j.observe(5, 5, 6, 6));
        assert_eq!(j.epoch_at(6), 6);
        // Not a commit at all.
        assert!(!j.observe(7, 6, 6, 6));
    }

    #[test]
    fn the_record_is_bounded_and_keeps_the_newest_commits() {
        let mut j = JournalEpochs::starting_at(0);
        for e in 0..(JOURNAL_EPOCHS_MAX_COMMITS as u64 + 10) {
            assert!(j.observe(e + 1, e, e + 1, e + 1));
        }
        assert_eq!(j.commits.len(), JOURNAL_EPOCHS_MAX_COMMITS);
        assert_eq!(j.base, 10);
        assert_eq!(j.epoch_at(u64::MAX), JOURNAL_EPOCHS_MAX_COMMITS as u64 + 10);
    }

    /// One row of a simulated DeviceGroup journal.
    #[derive(Debug, Clone)]
    enum Row {
        /// A commit by the committer device (never a claimer), sealed at the
        /// journal's epoch at its position.
        Commit,
        /// A claim by `author`, sealed `lag` epochs behind the journal's
        /// epoch at its position (0: counts; 1: a commit landed between its
        /// seal and its row).
        Claim { author: usize, lag: u64 },
    }

    fn rows() -> impl Strategy<Value = Vec<Row>> {
        prop::collection::vec(
            prop_oneof![
                Just(Row::Commit),
                (0usize..3, 0u64..2).prop_map(|(author, lag)| Row::Claim { author, lag }),
            ],
            1..24,
        )
    }

    proptest! {
        /// Across any interleaving of claims and commits: every claimer holds
        /// the same set of counting claims (another device's claim only when
        /// it could open it, its own only by `row_counts`), a straddled claim
        /// never wins, and exactly one device builds when any claim counts.
        #[test]
        fn a_straddled_claim_never_wins_and_one_device_builds(rows in rows(), start in 0u64..1000) {
            let keys: Vec<[u8; 32]> = (0..3u8).map(|d| [d + 1; 32]).collect();
            let mut epoch = start;
            // The truth: (row id, author, sealed, epoch at the row).
            let mut claims = Vec::new();
            let mut journal = JournalEpochs::starting_at(start);
            for (i, row) in rows.iter().enumerate() {
                let id = i as u64 + 1;
                match row {
                    Row::Commit => {
                        prop_assert!(journal.observe(id, epoch, epoch + 1, epoch + 1));
                        epoch += 1;
                    }
                    Row::Claim { author, lag } => {
                        let sealed = epoch.saturating_sub(*lag);
                        claims.push((id, *author, sealed, epoch));
                    }
                }
            }
            let truly_counting: Vec<_> = claims.iter().filter(|c| c.2 == c.3).collect();
            let mut winners = 0;
            let mut seen_winner = None;
            for device in 0..3usize {
                let held: Vec<ObservedLinkClaim> = claims
                    .iter()
                    .filter(|(id, author, sealed, at)| {
                        if *author == device {
                            journal.row_counts(*id, *sealed)
                        } else {
                            sealed == at // another device's row opens only at its epoch
                        }
                    })
                    .map(|(id, author, _, _)| ObservedLinkClaim { message_id: *id, author_key: keys[*author] })
                    .collect();
                // Every device holds exactly the truly counting claims.
                prop_assert_eq!(held.len(), truly_counting.len());
                match link_claim_outcome(&keys[device], &held, &keys) {
                    LinkClaimOutcome::Won { message_id } => {
                        winners += 1;
                        let c = claims.iter().find(|c| c.0 == message_id).unwrap();
                        prop_assert_eq!(c.2, c.3, "a straddled claim won");
                        seen_winner = Some(message_id);
                    }
                    LinkClaimOutcome::Lost { winner_message_id } => {
                        if let Some(w) = seen_winner {
                            prop_assert_eq!(w, winner_message_id);
                        }
                    }
                    LinkClaimOutcome::NoWinner => prop_assert!(truly_counting.is_empty()),
                }
            }
            prop_assert_eq!(winners, usize::from(!truly_counting.is_empty()));
        }

        /// A note (any non-commit row) sealed before a commit that lands
        /// ahead of it never counts, and one sealed at the journal's epoch at
        /// its position always does.
        #[test]
        fn a_straddled_note_never_counts(commits_before in 0u64..8, start in 0u64..1000, behind in 1u64..4) {
            let mut j = JournalEpochs::starting_at(start);
            for i in 0..commits_before {
                prop_assert!(j.observe(i + 1, start + i, start + i + 1, start + i + 1));
            }
            let at = commits_before + 1;
            let current = start + commits_before;
            prop_assert!(j.row_counts(at, current));
            if current >= behind {
                prop_assert!(!j.row_counts(at, current - behind));
            }
            prop_assert!(!j.row_counts(at, current + 1));
        }
    }
}
