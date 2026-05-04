// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Per-group MLS state on top of [`EncryptedStore`].
//!
//! Stores the serialised MLS `GroupContext` (or whatever opaque
//! state-machine snapshot OpenMLS hands us — the actual MLS
//! integration lives in `crates/lakko-mls`). One row per group_id.
//! Atomic put + get; range scans aren't a use case here.

use crate::cipher::EncryptedStore;
use crate::error::Result;
use crate::store::Table;

const PSEUDONYM_LEN: usize = 32;

/// Per-group MLS state facade.
pub struct GroupStateStore {
    inner: EncryptedStore,
}

impl GroupStateStore {
    /// Wrap an existing [`EncryptedStore`].
    #[must_use]
    pub fn new(inner: EncryptedStore) -> Self {
        Self { inner }
    }

    /// Store the serialised MLS state snapshot for a group. Replaces
    /// any previous snapshot — the MLS state machine produces a fresh
    /// snapshot on every commit / proposal application.
    pub fn save(&self, group_id: &[u8; PSEUDONYM_LEN], state: &[u8]) -> Result<()> {
        self.inner.put(Table::GroupState, group_id, state)
    }

    /// Load the most recent snapshot. Returns `Ok(None)` for unknown
    /// groups — caller bootstraps a fresh group in that case.
    pub fn load(&self, group_id: &[u8; PSEUDONYM_LEN]) -> Result<Option<Vec<u8>>> {
        self.inner.get(Table::GroupState, group_id)
    }

    /// Drop the snapshot. Idempotent.
    pub fn forget(&self, group_id: &[u8; PSEUDONYM_LEN]) -> Result<()> {
        self.inner.delete(Table::GroupState, group_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cipher::EncryptedStore;
    use crate::store::{InMemoryStore, Store};
    use std::sync::Arc;

    fn store() -> GroupStateStore {
        let inner: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        GroupStateStore::new(EncryptedStore::new(inner, [0xABu8; 32]))
    }

    #[test]
    fn save_and_load_roundtrip() {
        let s = store();
        let group = [0xAAu8; PSEUDONYM_LEN];
        s.save(&group, b"opaque mls snapshot").unwrap();
        assert_eq!(
            s.load(&group).unwrap().as_deref(),
            Some(&b"opaque mls snapshot"[..]),
        );
    }

    #[test]
    fn load_missing_returns_none() {
        let s = store();
        assert!(s.load(&[0u8; PSEUDONYM_LEN]).unwrap().is_none());
    }

    #[test]
    fn save_overwrites_previous_snapshot() {
        let s = store();
        let group = [0xAAu8; PSEUDONYM_LEN];
        s.save(&group, b"v1").unwrap();
        s.save(&group, b"v2").unwrap();
        assert_eq!(s.load(&group).unwrap().as_deref(), Some(&b"v2"[..]));
    }

    #[test]
    fn forget_removes_snapshot() {
        let s = store();
        let group = [0xAAu8; PSEUDONYM_LEN];
        s.save(&group, b"data").unwrap();
        s.forget(&group).unwrap();
        assert!(s.load(&group).unwrap().is_none());
    }

    #[test]
    fn isolated_per_group() {
        let s = store();
        let g1 = [0x11u8; PSEUDONYM_LEN];
        let g2 = [0x22u8; PSEUDONYM_LEN];
        s.save(&g1, b"alpha").unwrap();
        s.save(&g2, b"beta").unwrap();
        assert_eq!(s.load(&g1).unwrap().as_deref(), Some(&b"alpha"[..]));
        assert_eq!(s.load(&g2).unwrap().as_deref(), Some(&b"beta"[..]));
    }
}
