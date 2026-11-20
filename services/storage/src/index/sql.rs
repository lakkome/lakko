// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The MariaDB blob index (`storage_blobs`, migrations `0001` and `0003`;
//! the ADR-0089 tombstones and moderation bookkeeping, migration `0004`).

use async_trait::async_trait;
use sqlx::{MySqlPool, QueryBuilder, Row as _};

use super::{
    BlobId, BlobIndex, BlobRow, IndexError, ModerationDelete, ModerationOutcome, UploadOutcome,
};

/// Rows per multi-row `INSERT IGNORE` of the import.
const IMPORT_BATCH: usize = 500;

/// The blob index over the service's own database.
pub(crate) struct SqlIndex {
    pool: MySqlPool,
}

impl SqlIndex {
    /// Over `pool`, which must carry the service's migrations.
    pub(crate) const fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}

/// The last access day before which a row without its own idle life is
/// expired: `last_access + lifetime < today`, i.e. `last_access < today -
/// lifetime`. Saturates at 0 (nothing is expired).
const fn default_cutoff(today: u32, default_lifetime_days: u32) -> u32 {
    today.saturating_sub(default_lifetime_days)
}

fn row_from(r: &sqlx::mysql::MySqlRow) -> Result<BlobRow, IndexError> {
    let id: Vec<u8> = r.try_get("blob_id")?;
    let id: BlobId = id
        .as_slice()
        .try_into()
        .map_err(|_| IndexError::Corrupt(0))?;
    let key: Option<Vec<u8>> = r.try_get("delete_pk")?;
    let delete_pk = match key {
        None => None,
        Some(k) => Some(<[u8; 32]>::try_from(k.as_slice()).map_err(|_| IndexError::Corrupt(0))?),
    };
    Ok(BlobRow {
        id,
        size: r.try_get("size_bytes")?,
        node: r.try_get("node")?,
        created_day: r.try_get("created_day")?,
        last_access_day: r.try_get("last_access_day")?,
        delete_pk,
        idle_days: r.try_get("idle_days")?,
    })
}

#[async_trait]
impl BlobIndex for SqlIndex {
    async fn get(&self, id: &BlobId) -> Result<Option<BlobRow>, IndexError> {
        let row = sqlx::query(
            "SELECT blob_id, size_bytes, node, created_day, last_access_day, delete_pk, \
             idle_days FROM storage_blobs WHERE blob_id = ?",
        )
        .bind(&id[..])
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_from).transpose()
    }

    async fn record_upload(&self, row: BlobRow) -> Result<UploadOutcome, IndexError> {
        let key = row.delete_pk.map(|k| k.to_vec());
        // No row for a tombstoned id: the condition is in the insert itself,
        // so a delete applied between a check and the insert cannot leave one.
        let inserted = sqlx::query(
            "INSERT IGNORE INTO storage_blobs \
             (blob_id, size_bytes, node, created_day, last_access_day, delete_pk, idle_days) \
             SELECT ?, ?, ?, ?, ?, ?, ? FROM DUAL WHERE NOT EXISTS \
               (SELECT 1 FROM storage_blob_tombstones WHERE blob_id = ?)",
        )
        .bind(&row.id[..])
        .bind(row.size)
        .bind(&row.node)
        .bind(row.created_day)
        .bind(row.last_access_day)
        .bind(key)
        .bind(row.idle_days)
        .bind(&row.id[..])
        .execute(&self.pool)
        .await?
        .rows_affected();
        if inserted == 1 {
            return Ok(UploadOutcome::Created);
        }
        if self.is_tombstoned(&row.id).await? {
            return Ok(UploadOutcome::Removed);
        }
        self.touch(&[row.id], row.last_access_day).await?;
        Ok(UploadOutcome::Existed)
    }

    async fn is_tombstoned(&self, id: &BlobId) -> Result<bool, IndexError> {
        let row = sqlx::query("SELECT 1 FROM storage_blob_tombstones WHERE blob_id = ?")
            .bind(&id[..])
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    async fn apply_moderation_delete(
        &self,
        statement: &ModerationDelete,
    ) -> Result<ModerationOutcome, IndexError> {
        let mut tx = self.pool.begin().await?;
        // The stored list version, under its row lock: one statement applies
        // at a time.
        let stored: Option<u64> = sqlx::query_scalar(
            "SELECT list_version FROM moderation_list_state WHERE id = 1 FOR UPDATE",
        )
        .fetch_optional(&mut *tx)
        .await?;
        if stored.is_some_and(|v| statement.list_version < v) {
            return Ok(ModerationOutcome::StaleList);
        }
        let repeat = sqlx::query(
            "SELECT 1 FROM moderation_applied_statements WHERE statement_id = ? FOR UPDATE",
        )
        .bind(&statement.statement_id[..])
        .fetch_optional(&mut *tx)
        .await?;
        if repeat.is_some() {
            return Ok(ModerationOutcome::Repeat);
        }
        let tombstoned =
            sqlx::query("SELECT 1 FROM storage_blob_tombstones WHERE blob_id = ? FOR UPDATE")
                .bind(&statement.id[..])
                .fetch_optional(&mut *tx)
                .await?;
        let outcome = if tombstoned.is_some() {
            ModerationOutcome::AlreadyRemoved
        } else {
            let node: Option<String> =
                sqlx::query_scalar("SELECT node FROM storage_blobs WHERE blob_id = ? FOR UPDATE")
                    .bind(&statement.id[..])
                    .fetch_optional(&mut *tx)
                    .await?;
            sqlx::query("INSERT INTO storage_blob_tombstones (blob_id, at_day) VALUES (?, ?)")
                .bind(&statement.id[..])
                .bind(statement.day)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM storage_blobs WHERE blob_id = ?")
                .bind(&statement.id[..])
                .execute(&mut *tx)
                .await?;
            ModerationOutcome::Removed { node }
        };
        sqlx::query(
            "INSERT INTO moderation_applied_statements (statement_id, applied_at_ms) \
             VALUES (?, ?)",
        )
        .bind(&statement.statement_id[..])
        .bind(statement.now_ms)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO moderation_list_state (id, list_version) VALUES (1, ?) \
             ON DUPLICATE KEY UPDATE list_version = GREATEST(list_version, VALUES(list_version))",
        )
        .bind(statement.list_version)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(outcome)
    }

    async fn raise_moderation_list_floor(&self, list_version: u64) -> Result<bool, IndexError> {
        let mut tx = self.pool.begin().await?;
        let stored: Option<u64> = sqlx::query_scalar(
            "SELECT list_version FROM moderation_list_state WHERE id = 1 FOR UPDATE",
        )
        .fetch_optional(&mut *tx)
        .await?;
        match lakko_moderation::apply::list_floor(stored, list_version) {
            Err(_) => return Ok(false),
            Ok(lakko_moderation::apply::FloorStep::Keep) => {},
            Ok(lakko_moderation::apply::FloorStep::Raise) => {
                sqlx::query(
                    "INSERT INTO moderation_list_state (id, list_version) VALUES (1, ?) \
                     ON DUPLICATE KEY UPDATE \
                        list_version = GREATEST(list_version, VALUES(list_version))",
                )
                .bind(list_version)
                .execute(&mut *tx)
                .await?;
            },
        }
        tx.commit().await?;
        Ok(true)
    }

    async fn touch(&self, ids: &[BlobId], day: u32) -> Result<(), IndexError> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut q = QueryBuilder::new("UPDATE storage_blobs SET last_access_day = ");
        q.push_bind(day);
        q.push(" WHERE last_access_day < ");
        q.push_bind(day);
        q.push(" AND blob_id IN (");
        let mut sep = q.separated(", ");
        for id in ids {
            sep.push_bind(id.to_vec());
        }
        q.push(")");
        q.build().execute(&self.pool).await?;
        Ok(())
    }

    async fn remove(&self, id: &BlobId) -> Result<bool, IndexError> {
        let n = sqlx::query("DELETE FROM storage_blobs WHERE blob_id = ?")
            .bind(&id[..])
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(n == 1)
    }

    async fn expired(
        &self,
        node: &str,
        today: u32,
        default_lifetime_days: u32,
        limit: usize,
    ) -> Result<Vec<BlobId>, IndexError> {
        let limit = u64::try_from(limit).unwrap_or(u64::MAX);
        // Two arms so the default arm keeps the `last_access_day` index; the
        // own-life arm is bounded by `last_access_day < today` (a life is at
        // least one day). The `+` is on unsigned columns widened to BIGINT.
        let rows = sqlx::query(
            "SELECT blob_id FROM storage_blobs \
             WHERE node = ? AND ( \
                 (idle_days IS NULL AND last_access_day < ?) \
              OR (idle_days IS NOT NULL AND last_access_day < ? \
                  AND CAST(last_access_day AS UNSIGNED) + idle_days < ?) \
             ) ORDER BY last_access_day LIMIT ?",
        )
        .bind(node)
        .bind(default_cutoff(today, default_lifetime_days))
        .bind(today)
        .bind(today)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let id: Vec<u8> = r.try_get("blob_id")?;
                BlobId::try_from(id.as_slice()).map_err(|_| IndexError::Corrupt(0))
            })
            .collect()
    }

    async fn remove_if_expired(
        &self,
        id: &BlobId,
        today: u32,
        default_lifetime_days: u32,
    ) -> Result<bool, IndexError> {
        let n = sqlx::query(
            "DELETE FROM storage_blobs WHERE blob_id = ? AND ( \
                 (idle_days IS NULL AND last_access_day < ?) \
              OR (idle_days IS NOT NULL \
                  AND CAST(last_access_day AS UNSIGNED) + idle_days < ?) \
             )",
        )
        .bind(&id[..])
        .bind(default_cutoff(today, default_lifetime_days))
        .bind(today)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    async fn import_done(&self, node: &str) -> Result<bool, IndexError> {
        let row = sqlx::query("SELECT done_day FROM storage_blob_imports WHERE node = ?")
            .bind(node)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    async fn import(&self, rows: Vec<BlobRow>, node: &str, day: u32) -> Result<usize, IndexError> {
        let mut inserted = 0u64;
        for chunk in rows.chunks(IMPORT_BATCH) {
            let mut q = QueryBuilder::new(
                "INSERT IGNORE INTO storage_blobs \
                 (blob_id, size_bytes, node, created_day, last_access_day, delete_pk, \
                 idle_days) ",
            );
            q.push_values(chunk, |mut b, r| {
                b.push_bind(r.id.to_vec())
                    .push_bind(r.size)
                    .push_bind(r.node.clone())
                    .push_bind(r.created_day)
                    .push_bind(r.last_access_day)
                    .push_bind(r.delete_pk.map(|k| k.to_vec()))
                    .push_bind(r.idle_days);
            });
            inserted += q.build().execute(&self.pool).await?.rows_affected();
        }
        sqlx::query("INSERT IGNORE INTO storage_blob_imports (node, done_day) VALUES (?, ?)")
            .bind(node)
            .bind(day)
            .execute(&self.pool)
            .await?;
        Ok(usize::try_from(inserted).unwrap_or(usize::MAX))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sqlx::mysql::MySqlPoolOptions;

    /// A pool on `LAKKO_STORAGE_TEST_DATABASE_URL`, migrated once per test
    /// process; `None` (the test skips) when the variable is unset.
    pub(crate) async fn test_pool(name: &str) -> Option<MySqlPool> {
        let Ok(url) = std::env::var("LAKKO_STORAGE_TEST_DATABASE_URL") else {
            eprintln!("skipping {name}: LAKKO_STORAGE_TEST_DATABASE_URL unset");
            return None;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        MIGRATED
            .get_or_init(|| async {
                crate::MIGRATIONS.run(&pool).await.expect("run migrations");
            })
            .await;
        Some(pool)
    }

    static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

    /// A random id byte so reruns against a persistent database do not
    /// collide.
    fn random_base() -> u8 {
        let mut b = [0u8; 1];
        lakko_crypto::random::fill(&mut b);
        b[0] & 0xF0
    }

    #[tokio::test]
    async fn the_sql_index_meets_the_contract() {
        let Some(pool) = test_pool("the_sql_index_meets_the_contract").await else {
            return;
        };
        let index = SqlIndex::new(pool);
        super::super::contract::run(&index, random_base()).await;
    }

    #[tokio::test]
    async fn the_sql_index_keeps_moderator_tombstones() {
        let Some(pool) = test_pool("the_sql_index_keeps_moderator_tombstones").await else {
            return;
        };
        let index = SqlIndex::new(pool);
        let mut ids = [[0u8; 32]; 2];
        lakko_crypto::random::fill(&mut ids[0]);
        lakko_crypto::random::fill(&mut ids[1]);
        let mut statement = [0u8; 16];
        lakko_crypto::random::fill(&mut statement);
        // The stored list version only grows and the database is shared:
        // every run uses the highest version there is.
        super::super::contract::run_moderation(&index, ids, statement, u64::MAX).await;
    }

    #[tokio::test]
    async fn the_sql_import_inserts_missing_rows_and_marks_the_node() {
        let Some(pool) = test_pool("the_sql_import_inserts_missing_rows_and_marks_the_node").await
        else {
            return;
        };
        let index = SqlIndex::new(pool);
        let mut node_bytes = [0u8; 8];
        lakko_crypto::random::fill(&mut node_bytes);
        let node = hex::encode(node_bytes);
        let mut id = [0u8; 32];
        lakko_crypto::random::fill(&mut id);
        let row = BlobRow {
            id,
            size: 5,
            node: node.clone(),
            created_day: 10,
            last_access_day: 10,
            delete_pk: None,
            idle_days: None,
        };
        assert!(!index.import_done(&node).await.unwrap());
        assert_eq!(index.import(vec![row.clone()], &node, 10).await.unwrap(), 1);
        assert!(index.import_done(&node).await.unwrap());
        assert_eq!(index.import(vec![row], &node, 10).await.unwrap(), 0);
        assert!(index.remove(&id).await.unwrap());
    }

    #[test]
    fn the_spent_set_migration_is_the_shared_crate_schema_byte_for_byte() {
        assert_eq!(
            include_str!("../../migrations/0002_anon_token_spent.sql"),
            lakko_anon_token_server::SPENT_SET_MIGRATION_SQL
        );
    }
}
