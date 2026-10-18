// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the DeviceGroup journal readability rule
//! (ADR-0079, order of 2026-10-17): a claim or link-founded note row counts
//! only if the epoch it was sealed at (`epochOfMessage`) equals the journal's
//! epoch at its position. Mirror of
//! `crates/lakko-ffi-uniffi/src/journal_epoch.rs`; the rule lives in
//! `lakko_contacts::journal_epoch`.

use lakko_contacts::journal_epoch::JournalEpochs as RustJournalEpochs;
use wasm_bindgen::prelude::*;

/// The journal's epochs as this device has read them. Persist it with
/// `base`, `commitRowIds` and `commitEpochsAfter`; rebuild with `fromParts`.
#[wasm_bindgen(js_name = JournalEpochs)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmJournalEpochs {
    inner: RustJournalEpochs,
}

#[wasm_bindgen(js_class = JournalEpochs)]
impl WasmJournalEpochs {
    /// A record starting at `base` (the group's epoch when nothing is held).
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(base: u64) -> Self {
        Self {
            inner: RustJournalEpochs::starting_at(base),
        }
    }

    /// Rebuild a persisted record.
    ///
    /// # Errors
    ///
    /// `"bounds: ..."` when the two lists differ in length.
    #[wasm_bindgen(js_name = fromParts)]
    pub fn from_parts(
        base: u64,
        commit_row_ids: &[u64],
        commit_epochs_after: &[u64],
    ) -> Result<WasmJournalEpochs, String> {
        if commit_row_ids.len() != commit_epochs_after.len() {
            return Err("bounds: one epoch per commit row".to_string());
        }
        let mut commits: Vec<(u64, u64)> = commit_row_ids
            .iter()
            .copied()
            .zip(commit_epochs_after.iter().copied())
            .collect();
        commits.sort_by_key(|(id, _)| *id);
        Ok(Self {
            inner: RustJournalEpochs { base, commits },
        })
    }

    /// The journal's epoch before the oldest remembered commit.
    #[wasm_bindgen(getter)]
    pub fn base(&self) -> u64 {
        self.inner.base
    }

    /// DS message ids of the effective commits, ascending.
    #[wasm_bindgen(js_name = commitRowIds)]
    pub fn commit_row_ids(&self) -> Vec<u64> {
        self.inner.commits.iter().map(|(id, _)| *id).collect()
    }

    /// The epoch after each of those commits.
    #[wasm_bindgen(js_name = commitEpochsAfter)]
    pub fn commit_epochs_after(&self) -> Vec<u64> {
        self.inner.commits.iter().map(|(_, after)| *after).collect()
    }

    /// Whether a claim or note row at `rowId`, sealed at `sealedEpoch`, is
    /// readable by every device.
    #[wasm_bindgen(js_name = rowCounts)]
    pub fn row_counts(&self, row_id: u64, sealed_epoch: u64) -> bool {
        self.inner.row_counts(row_id, sealed_epoch)
    }

    /// Take in a row after the device processed it (or skipped it as its
    /// own). `true` when it was a commit that took effect (the record grew).
    pub fn observe(
        &mut self,
        row_id: u64,
        sealed_epoch: u64,
        epoch_after: u64,
        group_epoch_after: u64,
    ) -> bool {
        self.inner
            .observe(row_id, sealed_epoch, epoch_after, group_epoch_after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_straddled_row_does_not_count_and_the_parts_round_trip() {
        let mut j = WasmJournalEpochs::new(5);
        assert!(j.row_counts(10, 5));
        assert!(j.observe(11, 5, 6, 6));
        assert!(!j.row_counts(12, 5));
        let back =
            WasmJournalEpochs::from_parts(j.base(), &j.commit_row_ids(), &j.commit_epochs_after())
                .unwrap();
        assert_eq!(back, j);
        assert!(WasmJournalEpochs::from_parts(5, &[1], &[]).is_err());
    }
}
