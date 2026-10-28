// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the DeviceGroup journal readability rule (ADR-0079,
//! order of 2026-10-17): a claim or link-founded note row counts only if the
//! epoch it was sealed at (`epochOfMessage`) equals the journal's epoch at
//! its position. The record ([`FfiJournalEpochs`]) is kept by the client and
//! passed in and out; the rule lives in `lakko_contacts::journal_epoch`.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/journal_epoch.rs`.

use lakko_contacts::journal_epoch::JournalEpochs;

/// The journal's epochs as this device has read them: the epoch before the
/// oldest remembered commit, and the effective commits (row id, epoch after
/// it), parallel lists ascending by row id.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiJournalEpochs {
    /// The journal's epoch before the oldest commit below.
    pub base: u64,
    /// DS message ids of the effective commits.
    pub commit_row_ids: Vec<u64>,
    /// The epoch after each of those commits.
    pub commit_epochs_after: Vec<u64>,
}

impl From<&FfiJournalEpochs> for JournalEpochs {
    fn from(f: &FfiJournalEpochs) -> Self {
        Self {
            base: f.base,
            commits: f
                .commit_row_ids
                .iter()
                .copied()
                .zip(f.commit_epochs_after.iter().copied())
                .collect(),
        }
    }
}

impl From<JournalEpochs> for FfiJournalEpochs {
    fn from(j: JournalEpochs) -> Self {
        Self {
            base: j.base,
            commit_row_ids: j.commits.iter().map(|(id, _)| *id).collect(),
            commit_epochs_after: j.commits.iter().map(|(_, after)| *after).collect(),
        }
    }
}

/// A record starting at `base` (the group's epoch when nothing is held).
#[uniffi::export]
#[must_use]
pub fn journal_epochs_starting_at(base: u64) -> FfiJournalEpochs {
    JournalEpochs::starting_at(base).into()
}

/// Whether a claim or note row at `row_id`, sealed at `sealed_epoch`, is
/// readable by every device.
#[uniffi::export]
#[must_use]
pub fn journal_row_counts(epochs: FfiJournalEpochs, row_id: u64, sealed_epoch: u64) -> bool {
    JournalEpochs::from(&epochs).row_counts(row_id, sealed_epoch)
}

/// Take in a row after the device processed it (or skipped it as its own):
/// its framed `sealed_epoch` / `epoch_after` and the device's group epoch
/// after it. Returns the record, with the row added when it is a commit that
/// took effect.
#[uniffi::export]
#[must_use]
pub fn journal_epochs_observe(
    epochs: FfiJournalEpochs,
    row_id: u64,
    sealed_epoch: u64,
    epoch_after: u64,
    group_epoch_after: u64,
) -> FfiJournalEpochs {
    let mut j = JournalEpochs::from(&epochs);
    j.observe(row_id, sealed_epoch, epoch_after, group_epoch_after);
    j.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_straddled_row_does_not_count_and_the_record_round_trips() {
        let j = journal_epochs_starting_at(5);
        assert!(journal_row_counts(j.clone(), 10, 5));
        let j = journal_epochs_observe(j, 11, 5, 6, 6);
        assert_eq!(j.commit_row_ids, vec![11]);
        assert_eq!(j.commit_epochs_after, vec![6]);
        assert!(!journal_row_counts(j.clone(), 12, 5));
        assert!(journal_row_counts(j.clone(), 12, 6));
        // A refused commit leaves it unchanged.
        assert_eq!(journal_epochs_observe(j.clone(), 13, 6, 7, 6), j);
    }
}
