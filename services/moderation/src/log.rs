// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The moderation log (ADR-0099 section 5).
//!
//! An append-only RFC 6962 Merkle log, the KT log's construction
//! (`lakko_transparency::merkle`), holding every moderator statement and
//! every moderator list:
//!
//! ```text
//! leaf = kind(1) ‖ bytes
//!   kind 1  a signed statement (`SignedStatement::encode`, assertion included;
//!           the reason only as its salted commitment)
//!   kind 2  a root-signed moderator list, the bytes as served
//!   kind 3  a suspension snapshot root (`lakko_moderation::snapshot`,
//!           step 4); the dedup key is BLAKE3 of the root
//! ```
//!
//! No leaf holds report content or anything about a reporter.
//!
//! The head is signed by this service's own operator key (a
//! [`lakko_transparency::SignedRoot`], generated on first boot and kept in
//! the store) and co-signed by the configured witnesses under
//! [`LogId::Moderation`]. A witness that is down contributes nothing, and a
//! cached bundle is served only for the head it binds.
//!
//! Appends are serialised by one lock and are idempotent by a dedup key: a
//! statement's id padded with zeros to 32 bytes, or BLAKE3 of a list. The
//! leaves are held in memory (moderation actions are rare); a service
//! instance is the log's only writer.

use std::collections::HashMap;
use std::sync::Arc;

use lakko_crypto::signing::SigningKey;
use lakko_transparency::merkle::{audit_path, hash_leaf, root_from_hashes};
use lakko_transparency::{consistency_proof, SignedRoot, TreeHash, WITNESS_COSIG_LEN};
use tokio::sync::Mutex;

use crate::log_store::{LogStore, LogStoreError, StoredLeaf, Witnessed};
use crate::witness_client::WitnessClient;

/// Leaf kind: a signed moderator statement.
pub const LEAF_STATEMENT: u8 = lakko_moderation::snapshot::LOG_LEAF_STATEMENT;
/// Leaf kind: a root-signed moderator list.
pub const LEAF_MODERATOR_LIST: u8 = 2;
/// Leaf kind: a suspension snapshot root (ADR-0099 step 4).
pub const LEAF_SNAPSHOT_ROOT: u8 = lakko_moderation::snapshot::LOG_LEAF_SNAPSHOT_ROOT;

/// Why a log operation failed.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The store failed.
    #[error("store: {0}")]
    Store(#[from] LogStoreError),
    /// An index or size outside the log.
    #[error("out of range")]
    OutOfRange,
    /// The dedup key is taken by a different leaf.
    #[error("dedup key holds a different leaf")]
    DedupConflict,
}

/// The result of an append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appended {
    /// The leaf's index.
    pub index: u64,
    /// `false` when the identical leaf was already in the log.
    pub new: bool,
}

/// One leaf with its inclusion proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `kind ‖ bytes`.
    pub leaf: Vec<u8>,
    /// The audit path, leaf-most sibling first.
    pub audit_path: Vec<TreeHash>,
    /// The size of the tree the path is for.
    pub log_size: u64,
}

#[derive(Debug, Default)]
struct LogState {
    leaves: Vec<Vec<u8>>,
    hashes: Vec<TreeHash>,
    by_key: HashMap<[u8; 32], u64>,
}

/// The moderation log.
pub struct ModerationLog {
    store: Arc<dyn LogStore>,
    operator: SigningKey,
    state: Mutex<LogState>,
    witnessed: Mutex<Option<Witnessed>>,
    witnesses: Vec<WitnessClient>,
}

impl std::fmt::Debug for ModerationLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModerationLog")
            .field("witnesses", &self.witnesses.len())
            .finish_non_exhaustive()
    }
}

/// The dedup key of a statement: its id padded with zeros.
#[must_use]
pub fn statement_key(statement_id: &[u8; 16]) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(statement_id);
    key
}

/// The dedup key of a moderator list: BLAKE3 of its bytes.
#[must_use]
pub fn list_key(list_bytes: &[u8]) -> [u8; 32] {
    lakko_crypto::hash::blake3(list_bytes)
}

impl ModerationLog {
    /// Open the log kept in `store`: load the operator key (generated on
    /// first boot), every leaf and the last witnessed head.
    ///
    /// # Errors
    ///
    /// [`LogError::Store`], including leaves that are not numbered `0..n`.
    pub async fn open(
        store: Arc<dyn LogStore>,
        witnesses: Vec<WitnessClient>,
    ) -> Result<Self, LogError> {
        let operator = SigningKey::from_seed(&store.operator_seed().await?);
        let mut state = LogState::default();
        for (expected, stored) in store.leaves().await?.into_iter().enumerate() {
            if stored.idx != expected as u64 {
                return Err(LogStoreError::Corrupt("leaf indexes are not contiguous").into());
            }
            state.hashes.push(hash_leaf(&stored.leaf));
            state.by_key.insert(stored.dedup_key, stored.idx);
            state.leaves.push(stored.leaf);
        }
        let last_witnessed = store.witnessed().await?;
        Ok(Self {
            store,
            operator,
            state: Mutex::new(state),
            witnessed: Mutex::new(last_witnessed),
            witnesses,
        })
    }

    /// The operator's verifying key.
    #[must_use]
    pub fn operator_public(&self) -> [u8; 32] {
        self.operator.verifying_key().to_bytes()
    }

    /// How many witnesses are configured.
    #[must_use]
    pub fn witness_count(&self) -> usize {
        self.witnesses.len()
    }

    /// The current size.
    pub async fn size(&self) -> u64 {
        self.state.lock().await.leaves.len() as u64
    }

    /// A copy of every leaf, in order.
    pub async fn leaves(&self) -> Vec<Vec<u8>> {
        self.state.lock().await.leaves.clone()
    }

    /// Append `kind ‖ payload` under `dedup_key`. A repeat of the identical
    /// leaf returns its index with `new == false`.
    ///
    /// # Errors
    ///
    /// [`LogError::DedupConflict`] when the key holds a different leaf;
    /// [`LogError::Store`].
    pub async fn append(
        &self,
        kind: u8,
        dedup_key: [u8; 32],
        payload: &[u8],
        now_ms: u64,
    ) -> Result<Appended, LogError> {
        self.append_inner(kind, dedup_key, payload, None, now_ms)
            .await
            // Without `at` the inner append never declines.
            .and_then(|a| a.ok_or(LogError::OutOfRange))
    }

    /// Append as [`Self::append`] does, but only at index `at`: when the log
    /// has grown past `at` since the caller read it, nothing is appended and
    /// `None` is returned. A leaf derived from the first `at` leaves (a
    /// snapshot root) is then always preceded by exactly those.
    ///
    /// # Errors
    ///
    /// As [`Self::append`].
    pub async fn append_at(
        &self,
        kind: u8,
        dedup_key: [u8; 32],
        payload: &[u8],
        at: u64,
        now_ms: u64,
    ) -> Result<Option<Appended>, LogError> {
        self.append_inner(kind, dedup_key, payload, Some(at), now_ms)
            .await
    }

    async fn append_inner(
        &self,
        kind: u8,
        dedup_key: [u8; 32],
        payload: &[u8],
        at: Option<u64>,
        now_ms: u64,
    ) -> Result<Option<Appended>, LogError> {
        let mut leaf = Vec::with_capacity(1 + payload.len());
        leaf.push(kind);
        leaf.extend_from_slice(payload);

        let mut state = self.state.lock().await;
        if let Some(&index) = state.by_key.get(&dedup_key) {
            let same = usize::try_from(index)
                .ok()
                .and_then(|i| state.leaves.get(i))
                .is_some_and(|existing| *existing == leaf);
            return if same {
                Ok(Some(Appended { index, new: false }))
            } else {
                Err(LogError::DedupConflict)
            };
        }
        let index = state.leaves.len() as u64;
        if at.is_some_and(|at| at != index) {
            return Ok(None);
        }
        let stored = StoredLeaf {
            idx: index,
            dedup_key,
            leaf,
        };
        self.store.insert(&stored, kind, now_ms).await?;
        state.hashes.push(hash_leaf(&stored.leaf));
        state.by_key.insert(dedup_key, index);
        state.leaves.push(stored.leaf);
        Ok(Some(Appended { index, new: true }))
    }

    /// The leaf at `index` with its audit path in the tree of `size` leaves
    /// (the current size when `None`).
    ///
    /// # Errors
    ///
    /// [`LogError::OutOfRange`] unless `index < size <= current size`.
    pub async fn entry(&self, index: u64, size: Option<u64>) -> Result<Entry, LogError> {
        let state = self.state.lock().await;
        let current = state.leaves.len() as u64;
        let size = size.unwrap_or(current);
        if index >= size || size > current {
            return Err(LogError::OutOfRange);
        }
        let (Ok(i), Ok(n)) = (usize::try_from(index), usize::try_from(size)) else {
            return Err(LogError::OutOfRange);
        };
        Ok(Entry {
            leaf: state.leaves[i].clone(),
            audit_path: audit_path(&state.leaves[..n], i),
            log_size: size,
        })
    }

    /// The consistency proof from the tree of `from` leaves to that of `to`.
    ///
    /// # Errors
    ///
    /// [`LogError::OutOfRange`] unless `from <= to <= current size`.
    pub async fn consistency(&self, from: u64, to: u64) -> Result<Vec<TreeHash>, LogError> {
        let state = self.state.lock().await;
        if from > to || to > state.leaves.len() as u64 {
            return Err(LogError::OutOfRange);
        }
        let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) else {
            return Err(LogError::OutOfRange);
        };
        Ok(consistency_proof(from, &state.leaves[..to]))
    }

    /// Sign the current head at `timestamp_unix` and attach the witness
    /// co-signature bundle: the cached one when the head has not moved,
    /// otherwise a fresh best-effort fan-out. An empty bundle is served as
    /// is; a bundle is never served for a head it does not bind.
    pub async fn root(&self, timestamp_unix: u64) -> (SignedRoot, Vec<Vec<u8>>) {
        let (head, size, leaves) = {
            let state = self.state.lock().await;
            let head = self.sign(
                root_from_hashes(&state.hashes),
                state.hashes.len() as u64,
                timestamp_unix,
            );
            (head, state.hashes.len() as u64, state.leaves.clone())
        };

        if let Some(w) = self.witnessed.lock().await.as_ref() {
            if w.log_size == size && w.tree_root == head.tree_root {
                return (head, w.cosigs.clone());
            }
        }
        if self.witnesses.is_empty() {
            return (head, Vec::new());
        }

        let bases = match self.store.witness_high_waters(self.witnesses.len()).await {
            Ok(bases) => bases,
            Err(e) => {
                tracing::error!(error = %e, "moderation log: loading witness high-waters failed");
                return (head, Vec::new());
            },
        };
        let proofs: Vec<Vec<TreeHash>> = bases
            .iter()
            .map(|&base| {
                if base > 0 && base < size {
                    usize::try_from(base)
                        .map_or_else(|_| Vec::new(), |b| consistency_proof(b, &leaves))
                } else {
                    Vec::new()
                }
            })
            .collect();

        let results = futures::future::join_all(
            self.witnesses
                .iter()
                .zip(&proofs)
                .map(|(witness, proof)| witness.cosign(&head, proof)),
        )
        .await;

        let mut cosigs = Vec::new();
        for (index, result) in results.into_iter().enumerate() {
            match result {
                Ok(cosig) if cosig.len() == WITNESS_COSIG_LEN => {
                    cosigs.push(cosig);
                    let index = u32::try_from(index).unwrap_or(u32::MAX);
                    if let Err(e) = self
                        .store
                        .set_witness_high_water(index, &head.tree_root, size)
                        .await
                    {
                        tracing::warn!(error = %e, "moderation log: persisting a witness high-water failed");
                    }
                },
                Ok(_) => tracing::warn!(
                    index,
                    "moderation log: a witness returned a malformed cosig"
                ),
                Err(e) => {
                    tracing::warn!(index, error = %e, "moderation log: a witness did not co-sign");
                },
            }
        }
        if cosigs.is_empty() {
            return (head, Vec::new());
        }
        let witnessed = Witnessed {
            tree_root: head.tree_root,
            log_size: size,
            cosigs: cosigs.clone(),
        };
        if let Err(e) = self.store.set_witnessed(&witnessed).await {
            tracing::error!(error = %e, "moderation log: persisting the witnessed head failed");
        }
        *self.witnessed.lock().await = Some(witnessed);
        (head, cosigs)
    }

    fn sign(&self, tree_root: TreeHash, log_size: u64, timestamp_unix: u64) -> SignedRoot {
        let payload = SignedRoot::signing_payload(&tree_root, log_size, timestamp_unix);
        SignedRoot {
            tree_root,
            log_size,
            timestamp_unix,
            signature: self.operator.sign(&payload),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::log_store::MemoryLogStore;
    use lakko_crypto::signing::VerifyingKey;
    use lakko_transparency::merkle::verify_inclusion;
    use lakko_transparency::verify_consistency;

    async fn log() -> (ModerationLog, Arc<MemoryLogStore>) {
        let store = Arc::new(MemoryLogStore::new());
        (
            ModerationLog::open(store.clone(), Vec::new())
                .await
                .unwrap(),
            store,
        )
    }

    #[tokio::test]
    async fn appends_prove_inclusion_and_consistency() {
        let (log, _) = log().await;
        for i in 0u8..5 {
            let a = log
                .append(LEAF_STATEMENT, statement_key(&[i; 16]), &[i; 40], 1)
                .await
                .unwrap();
            assert_eq!(
                a,
                Appended {
                    index: u64::from(i),
                    new: true
                }
            );
        }
        let (root, cosigs) = log.root(100).await;
        assert!(cosigs.is_empty());
        assert_eq!(root.log_size, 5);
        root.verify(&VerifyingKey::from_bytes(&log.operator_public()).unwrap())
            .unwrap();
        for i in 0..5 {
            let e = log.entry(i, None).await.unwrap();
            assert_eq!(e.leaf[0], LEAF_STATEMENT);
            assert!(verify_inclusion(
                &e.leaf,
                i,
                5,
                &e.audit_path,
                &root.tree_root
            ));
        }
        // An entry against an older size verifies against that size's root.
        let e = log.entry(1, Some(3)).await.unwrap();
        let old_root = root_from_hashes(
            &(0u8..3)
                .map(|i| {
                    let mut l = vec![LEAF_STATEMENT];
                    l.extend_from_slice(&[i; 40]);
                    hash_leaf(&l)
                })
                .collect::<Vec<_>>(),
        );
        assert!(verify_inclusion(&e.leaf, 1, 3, &e.audit_path, &old_root));
        let proof = log.consistency(3, 5).await.unwrap();
        assert!(verify_consistency(3, &old_root, 5, &root.tree_root, &proof));
        assert!(matches!(
            log.entry(5, None).await,
            Err(LogError::OutOfRange)
        ));
        assert!(matches!(
            log.entry(2, Some(2)).await,
            Err(LogError::OutOfRange)
        ));
        assert!(matches!(
            log.entry(0, Some(6)).await,
            Err(LogError::OutOfRange)
        ));
        assert!(matches!(
            log.consistency(4, 3).await,
            Err(LogError::OutOfRange)
        ));
        assert!(matches!(
            log.consistency(0, 6).await,
            Err(LogError::OutOfRange)
        ));
    }

    #[tokio::test]
    async fn a_repeat_is_idempotent_and_a_different_leaf_conflicts() {
        let (log, _) = log().await;
        let key = statement_key(&[1; 16]);
        log.append(LEAF_STATEMENT, key, b"a", 1).await.unwrap();
        let again = log.append(LEAF_STATEMENT, key, b"a", 2).await.unwrap();
        assert_eq!(
            again,
            Appended {
                index: 0,
                new: false
            }
        );
        assert_eq!(log.size().await, 1);
        assert!(matches!(
            log.append(LEAF_STATEMENT, key, b"b", 3).await,
            Err(LogError::DedupConflict)
        ));
    }

    #[tokio::test]
    async fn a_reopened_log_keeps_its_key_and_leaves() {
        let (log, store) = log().await;
        log.append(LEAF_MODERATOR_LIST, list_key(b"list"), b"list", 1)
            .await
            .unwrap();
        let (root, _) = log.root(5).await;
        let reopened = ModerationLog::open(store, Vec::new()).await.unwrap();
        assert_eq!(reopened.operator_public(), log.operator_public());
        let (root2, _) = reopened.root(5).await;
        assert_eq!(root2.tree_root, root.tree_root);
        assert_eq!(
            reopened
                .append(LEAF_MODERATOR_LIST, list_key(b"list"), b"list", 2)
                .await
                .unwrap(),
            Appended {
                index: 0,
                new: false
            }
        );
    }
}
