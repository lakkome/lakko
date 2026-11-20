// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Where the moderation log is kept: MariaDB in deployment, memory for tests
//! and a database-less dev run (the [`crate::store`] pattern).
//!
//! The store is dumb: it keeps leaves by index, the operator seed, the last
//! witnessed head and the per-witness high-waters. Ordering, deduplication
//! and the Merkle tree are [`crate::log::ModerationLog`]'s, which serialises
//! every append.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use lakko_transparency::{TreeHash, WITNESS_COSIG_LEN};
use rand::RngCore as _;
use sqlx::MySqlPool;

/// A stored leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLeaf {
    /// Its index.
    pub idx: u64,
    /// The idempotency key.
    pub dedup_key: [u8; 32],
    /// `kind ‖ bytes`.
    pub leaf: Vec<u8>,
}

/// The last witnessed head and its co-signature bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Witnessed {
    /// The co-signed tree root.
    pub tree_root: TreeHash,
    /// The co-signed size.
    pub log_size: u64,
    /// One co-signature per witness that co-signed it.
    pub cosigs: Vec<Vec<u8>>,
}

/// A store failure. Never carries leaf bytes.
#[derive(Debug, thiserror::Error)]
pub enum LogStoreError {
    /// The database failed.
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    /// A stored value has the wrong shape (corruption).
    #[error("corrupt: {0}")]
    Corrupt(&'static str),
    /// The index or the dedup key is already taken.
    #[error("leaf conflict")]
    Conflict,
}

/// Moderation-log storage.
#[async_trait]
pub trait LogStore: Send + Sync {
    /// The operator seed, generated and kept on first call.
    async fn operator_seed(&self) -> Result<[u8; 32], LogStoreError>;
    /// Every leaf, ordered by index.
    async fn leaves(&self) -> Result<Vec<StoredLeaf>, LogStoreError>;
    /// Insert a leaf at `leaf.idx`. [`LogStoreError::Conflict`] when the
    /// index or the dedup key exists.
    async fn insert(&self, leaf: &StoredLeaf, kind: u8, now_ms: u64) -> Result<(), LogStoreError>;
    /// The last witnessed head, if any.
    async fn witnessed(&self) -> Result<Option<Witnessed>, LogStoreError>;
    /// Replace the last witnessed head.
    async fn set_witnessed(&self, witnessed: &Witnessed) -> Result<(), LogStoreError>;
    /// The size each of the first `count` witnesses last co-signed (0 for a
    /// witness never seen).
    async fn witness_high_waters(&self, count: usize) -> Result<Vec<u64>, LogStoreError>;
    /// Record that witness `index` co-signed `(tree_root, log_size)`.
    async fn set_witness_high_water(
        &self,
        index: u32,
        tree_root: &TreeHash,
        log_size: u64,
    ) -> Result<(), LogStoreError>;
}

/// MariaDB store over migration `0003`.
#[derive(Debug, Clone)]
pub struct SqlLogStore {
    pool: MySqlPool,
}

impl SqlLogStore {
    /// Wrap a pool whose migrations have run.
    #[must_use]
    pub const fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}

fn hash32(bytes: &[u8], what: &'static str) -> Result<[u8; 32], LogStoreError> {
    bytes.try_into().map_err(|_| LogStoreError::Corrupt(what))
}

#[async_trait]
impl LogStore for SqlLogStore {
    async fn operator_seed(&self) -> Result<[u8; 32], LogStoreError> {
        let row: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT seed FROM moderation_log_key WHERE id = 1")
                .fetch_optional(&self.pool)
                .await?;
        if let Some(seed) = row {
            return hash32(&seed, "operator seed");
        }
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        sqlx::query("INSERT IGNORE INTO moderation_log_key (id, seed) VALUES (1, ?)")
            .bind(&seed[..])
            .execute(&self.pool)
            .await?;
        // Re-read so a racing first boot ends with one seed.
        let seed: Vec<u8> = sqlx::query_scalar("SELECT seed FROM moderation_log_key WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        hash32(&seed, "operator seed")
    }

    async fn leaves(&self) -> Result<Vec<StoredLeaf>, LogStoreError> {
        let rows: Vec<(u64, Vec<u8>, Vec<u8>)> =
            sqlx::query_as("SELECT idx, dedup_key, leaf FROM moderation_log_leaves ORDER BY idx")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|(idx, key, leaf)| {
                Ok(StoredLeaf {
                    idx,
                    dedup_key: hash32(&key, "dedup key")?,
                    leaf,
                })
            })
            .collect()
    }

    async fn insert(&self, leaf: &StoredLeaf, kind: u8, now_ms: u64) -> Result<(), LogStoreError> {
        let result = sqlx::query(
            "INSERT INTO moderation_log_leaves (idx, kind, dedup_key, leaf, appended_at_ms) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(leaf.idx)
        .bind(kind)
        .bind(&leaf.dedup_key[..])
        .bind(&leaf.leaf)
        .bind(now_ms)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(LogStoreError::Conflict)
            },
            Err(e) => Err(LogStoreError::Db(e)),
        }
    }

    async fn witnessed(&self) -> Result<Option<Witnessed>, LogStoreError> {
        let row: Option<(Vec<u8>, u64, Vec<u8>)> = sqlx::query_as(
            "SELECT tree_root, log_size, cosigs FROM moderation_log_witnessed WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some((root, log_size, cosigs)) = row else {
            return Ok(None);
        };
        if cosigs.len() % WITNESS_COSIG_LEN != 0 {
            return Err(LogStoreError::Corrupt("cosig bundle"));
        }
        Ok(Some(Witnessed {
            tree_root: hash32(&root, "witnessed root")?,
            log_size,
            cosigs: cosigs
                .chunks_exact(WITNESS_COSIG_LEN)
                .map(<[u8]>::to_vec)
                .collect(),
        }))
    }

    async fn set_witnessed(&self, w: &Witnessed) -> Result<(), LogStoreError> {
        let bundle: Vec<u8> = w.cosigs.concat();
        sqlx::query(
            "INSERT INTO moderation_log_witnessed (id, tree_root, log_size, cosigs) \
             VALUES (1, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE tree_root = VALUES(tree_root), \
             log_size = VALUES(log_size), cosigs = VALUES(cosigs)",
        )
        .bind(&w.tree_root[..])
        .bind(w.log_size)
        .bind(bundle)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn witness_high_waters(&self, count: usize) -> Result<Vec<u64>, LogStoreError> {
        let rows: Vec<(u32, u64)> =
            sqlx::query_as("SELECT witness_index, log_size FROM moderation_log_witness_high_water")
                .fetch_all(&self.pool)
                .await?;
        let mut out = vec![0u64; count];
        for (index, size) in rows {
            if let Some(slot) = out.get_mut(index as usize) {
                *slot = size;
            }
        }
        Ok(out)
    }

    async fn set_witness_high_water(
        &self,
        index: u32,
        tree_root: &TreeHash,
        log_size: u64,
    ) -> Result<(), LogStoreError> {
        sqlx::query(
            "INSERT INTO moderation_log_witness_high_water (witness_index, tree_root, log_size) \
             VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE tree_root = VALUES(tree_root), log_size = VALUES(log_size)",
        )
        .bind(index)
        .bind(&tree_root[..])
        .bind(log_size)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[derive(Debug, Default)]
struct MemoryInner {
    seed: Option<[u8; 32]>,
    leaves: Vec<StoredLeaf>,
    witnessed: Option<Witnessed>,
    high_water: HashMap<u32, u64>,
}

/// In-memory store (tests, and a database-less dev run).
#[derive(Debug, Default)]
pub struct MemoryLogStore {
    inner: Mutex<MemoryInner>,
}

impl MemoryLogStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl LogStore for MemoryLogStore {
    async fn operator_seed(&self) -> Result<[u8; 32], LogStoreError> {
        let mut inner = self.lock();
        if let Some(seed) = inner.seed {
            return Ok(seed);
        }
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        inner.seed = Some(seed);
        Ok(seed)
    }

    async fn leaves(&self) -> Result<Vec<StoredLeaf>, LogStoreError> {
        Ok(self.lock().leaves.clone())
    }

    async fn insert(
        &self,
        leaf: &StoredLeaf,
        _kind: u8,
        _now_ms: u64,
    ) -> Result<(), LogStoreError> {
        let mut inner = self.lock();
        if leaf.idx != inner.leaves.len() as u64
            || inner.leaves.iter().any(|l| l.dedup_key == leaf.dedup_key)
        {
            return Err(LogStoreError::Conflict);
        }
        inner.leaves.push(leaf.clone());
        Ok(())
    }

    async fn witnessed(&self) -> Result<Option<Witnessed>, LogStoreError> {
        Ok(self.lock().witnessed.clone())
    }

    async fn set_witnessed(&self, witnessed: &Witnessed) -> Result<(), LogStoreError> {
        self.lock().witnessed = Some(witnessed.clone());
        Ok(())
    }

    async fn witness_high_waters(&self, count: usize) -> Result<Vec<u64>, LogStoreError> {
        let inner = self.lock();
        Ok((0..count)
            .map(|i| {
                u32::try_from(i)
                    .ok()
                    .and_then(|i| inner.high_water.get(&i).copied())
                    .unwrap_or(0)
            })
            .collect())
    }

    async fn set_witness_high_water(
        &self,
        index: u32,
        _tree_root: &TreeHash,
        log_size: u64,
    ) -> Result<(), LogStoreError> {
        self.lock().high_water.insert(index, log_size);
        Ok(())
    }
}

#[cfg(test)]
mod sql_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::log::{list_key, statement_key, Appended, ModerationLog, LEAF_STATEMENT};
    use std::sync::Arc;

    /// The MariaDB store end to end: first-boot key, appends, a reopen, the
    /// witnessed head and the high-waters. Gated on
    /// `LAKKO_MODERATION_TEST_DATABASE_URL`; skips without it.
    #[tokio::test]
    async fn the_sql_store_keeps_the_log_across_a_reopen() {
        let Ok(url) = std::env::var("LAKKO_MODERATION_TEST_DATABASE_URL") else {
            eprintln!("skipping: LAKKO_MODERATION_TEST_DATABASE_URL unset");
            return;
        };
        let pool = crate::store_tests::migrated_pool(&url).await;
        for statement in [
            "DELETE FROM moderation_log_leaves",
            "DELETE FROM moderation_log_key",
            "DELETE FROM moderation_log_witnessed",
            "DELETE FROM moderation_log_witness_high_water",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        let store: Arc<dyn LogStore> = Arc::new(SqlLogStore::new(pool.clone()));
        let log = ModerationLog::open(store.clone(), Vec::new())
            .await
            .unwrap();
        log.append(2, list_key(b"list"), b"list", 1).await.unwrap();
        log.append(LEAF_STATEMENT, statement_key(&[1; 16]), b"s1", 2)
            .await
            .unwrap();
        let (root, _) = log.root(7).await;

        let reopened = ModerationLog::open(store.clone(), Vec::new())
            .await
            .unwrap();
        assert_eq!(reopened.operator_public(), log.operator_public());
        assert_eq!(reopened.size().await, 2);
        assert_eq!(reopened.root(7).await.0.tree_root, root.tree_root);
        assert_eq!(
            reopened
                .append(LEAF_STATEMENT, statement_key(&[1; 16]), b"s1", 3)
                .await
                .unwrap(),
            Appended {
                index: 1,
                new: false
            }
        );
        // The store itself refuses a taken index or key.
        let dup = StoredLeaf {
            idx: 2,
            dedup_key: statement_key(&[1; 16]),
            leaf: b"x".to_vec(),
        };
        assert!(matches!(
            store.insert(&dup, 1, 4).await,
            Err(LogStoreError::Conflict)
        ));

        let w = Witnessed {
            tree_root: root.tree_root,
            log_size: 2,
            cosigs: vec![vec![1; WITNESS_COSIG_LEN], vec![2; WITNESS_COSIG_LEN]],
        };
        store.set_witnessed(&w).await.unwrap();
        assert_eq!(store.witnessed().await.unwrap(), Some(w));
        store
            .set_witness_high_water(1, &root.tree_root, 2)
            .await
            .unwrap();
        assert_eq!(store.witness_high_waters(3).await.unwrap(), vec![0, 2, 0]);
        pool.close().await;
    }
}
