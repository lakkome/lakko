// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The MariaDB vault index (`storage_vaults`, `storage_vault_objects`,
//! migration `0004`).
//!
//! Every reservation and commit runs in one transaction that first locks the
//! vault row (`SELECT ... FOR UPDATE`), so writes to one vault are serialised
//! across instances and the byte and rate checks see one state. The object
//! rows are then read without a locking read: the vault lock already
//! serialises every writer of them, and a locking read of an absent row
//! takes a gap lock that a concurrent insert into ANOTHER vault can deadlock
//! against (seen in the parallel route tests before this was removed).

use async_trait::async_trait;
use sqlx::{MySql, Row as _, Transaction};

use super::SqlIndex;
use crate::index::{
    decide_reserve, hour_of, new_token, ClaimOutcome, CommitOutcome, IndexError, ObjectAddr,
    ObjectRow, ReservationToken, ReserveFacts, ReserveOutcome, VaultId, VaultIndex, VaultLimits,
    VaultRow, WriteRequest,
};

/// Attempts of one reservation or commit transaction rolled back as a
/// deadlock victim. Chosen: the transaction is short and rolled back whole,
/// so a retry is safe.
const DEADLOCK_ATTEMPTS: usize = 3;

/// SQLSTATE of a transaction rolled back as a deadlock victim.
const SQLSTATE_DEADLOCK: &str = "40001";

fn is_deadlock(e: &IndexError) -> bool {
    match e {
        IndexError::Db(sqlx::Error::Database(db)) => {
            db.code().as_deref() == Some(SQLSTATE_DEADLOCK)
                || db.message().contains("Deadlock found")
        },
        _ => false,
    }
}

fn fixed<const N: usize>(v: &[u8]) -> Result<[u8; N], IndexError> {
    <[u8; N]>::try_from(v).map_err(|_| IndexError::Corrupt(0))
}

/// The vault row locked for the rest of the transaction: bytes used and the
/// write window.
struct LockedVault {
    bytes_used: u64,
    window_hour: u64,
    window_writes: u32,
}

async fn lock_vault(
    tx: &mut Transaction<'_, MySql>,
    id: &VaultId,
) -> Result<Option<LockedVault>, IndexError> {
    let row = sqlx::query(
        "SELECT bytes_used, window_hour, window_writes FROM storage_vaults \
         WHERE vault_id = ? FOR UPDATE",
    )
    .bind(&id[..])
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|r| {
        Ok(LockedVault {
            bytes_used: r.try_get("bytes_used")?,
            window_hour: r.try_get("window_hour")?,
            window_writes: r.try_get("window_writes")?,
        })
    })
    .transpose()
}

impl SqlIndex {
    async fn reserve_once(
        &self,
        id: &VaultId,
        req: WriteRequest,
        limits: &VaultLimits,
    ) -> Result<ReserveOutcome, IndexError> {
        let mut tx = self.pool.begin().await?;
        let Some(vault) = lock_vault(&mut tx, id).await? else {
            return Ok(ReserveOutcome::NoVault);
        };
        let kind = req.addr.kind_byte();
        let chunk_no = req.addr.chunk_no();
        let object = sqlx::query(
            "SELECT version, size_bytes, reserved_token, reserved_until_ms \
             FROM storage_vault_objects WHERE vault_id = ? AND kind = ? AND chunk_no = ?",
        )
        .bind(&id[..])
        .bind(kind)
        .bind(chunk_no)
        .fetch_optional(&mut *tx)
        .await?;
        let mut facts = ReserveFacts {
            bytes_used: vault.bytes_used,
            writes_this_hour: if vault.window_hour == hour_of(req.now_ms) {
                vault.window_writes
            } else {
                0
            },
            ..ReserveFacts::default()
        };
        if let Some(r) = &object {
            let reserved: Option<Vec<u8>> = r.try_get("reserved_token")?;
            let until: u64 = r.try_get("reserved_until_ms")?;
            facts.object = Some((r.try_get("version")?, r.try_get("size_bytes")?));
            facts.object_reserved = reserved.is_some() && until > req.now_ms;
        }
        let counts = sqlx::query(
            "SELECT \
               CAST(COALESCE(SUM(kind = 2), 0) AS UNSIGNED) AS chunk_rows, \
               CAST(COALESCE(SUM(CASE WHEN reserved_token IS NOT NULL \
                    AND reserved_until_ms > ? AND NOT (kind = ? AND chunk_no = ?) \
                    THEN reserved_size ELSE 0 END), 0) AS UNSIGNED) AS other_reserved \
             FROM storage_vault_objects WHERE vault_id = ?",
        )
        .bind(req.now_ms)
        .bind(kind)
        .bind(chunk_no)
        .bind(&id[..])
        .fetch_one(&mut *tx)
        .await?;
        facts.chunk_rows = counts.try_get("chunk_rows")?;
        facts.other_reserved_bytes = counts.try_get("other_reserved")?;

        if let Err(refusal) = decide_reserve(&req, &facts, limits) {
            tx.rollback().await?;
            return Ok(refusal);
        }
        let token = new_token();
        let until = req.now_ms.saturating_add(limits.reservation_ms);
        sqlx::query(
            "INSERT INTO storage_vault_objects \
             (vault_id, kind, chunk_no, reserved_version, reserved_token, reserved_size, \
              reserved_until_ms) VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE reserved_version = VALUES(reserved_version), \
             reserved_token = VALUES(reserved_token), reserved_size = VALUES(reserved_size), \
             reserved_until_ms = VALUES(reserved_until_ms)",
        )
        .bind(&id[..])
        .bind(kind)
        .bind(chunk_no)
        .bind(req.version)
        .bind(&token[..])
        .bind(req.size)
        .bind(until)
        .execute(&mut *tx)
        .await?;
        let hour = hour_of(req.now_ms);
        sqlx::query(
            "UPDATE storage_vaults SET window_hour = ?, window_writes = ? WHERE vault_id = ?",
        )
        .bind(hour)
        .bind(facts.writes_this_hour.saturating_add(1))
        .bind(&id[..])
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ReserveOutcome::Reserved(token))
    }

    async fn commit_once(
        &self,
        id: &VaultId,
        addr: ObjectAddr,
        token: &ReservationToken,
        size: u64,
    ) -> Result<CommitOutcome, IndexError> {
        let mut tx = self.pool.begin().await?;
        if lock_vault(&mut tx, id).await?.is_none() {
            return Ok(CommitOutcome::Lost);
        }
        let row = sqlx::query(
            "SELECT version, token, size_bytes, reserved_version FROM storage_vault_objects \
             WHERE vault_id = ? AND kind = ? AND chunk_no = ? AND reserved_token = ?",
        )
        .bind(&id[..])
        .bind(addr.kind_byte())
        .bind(addr.chunk_no())
        .bind(&token[..])
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(CommitOutcome::Lost);
        };
        let old_version: u64 = row.try_get("version")?;
        let old_token: Option<Vec<u8>> = row.try_get("token")?;
        let old_size: u64 = row.try_get("size_bytes")?;
        let new_version: Option<u64> = row.try_get("reserved_version")?;
        let new_version = new_version.ok_or(IndexError::Corrupt(0))?;
        sqlx::query(
            "UPDATE storage_vault_objects SET version = ?, token = ?, size_bytes = ?, \
             reserved_version = NULL, reserved_token = NULL, reserved_size = 0, \
             reserved_until_ms = 0 WHERE vault_id = ? AND kind = ? AND chunk_no = ?",
        )
        .bind(new_version)
        .bind(&token[..])
        .bind(size)
        .bind(&id[..])
        .bind(addr.kind_byte())
        .bind(addr.chunk_no())
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE storage_vaults SET bytes_used = bytes_used + ? - ? WHERE vault_id = ?")
            .bind(size)
            .bind(old_size)
            .bind(&id[..])
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let replaced = match (old_version, old_token) {
            (0, _) | (_, None) => None,
            (v, Some(t)) => Some((v, fixed(&t)?)),
        };
        Ok(CommitOutcome::Committed { replaced })
    }
}

#[async_trait]
impl VaultIndex for SqlIndex {
    async fn vault(&self, id: &VaultId) -> Result<Option<VaultRow>, IndexError> {
        let row = sqlx::query(
            "SELECT write_pk, node, created_day, bytes_used FROM storage_vaults \
             WHERE vault_id = ?",
        )
        .bind(&id[..])
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let pk: Vec<u8> = r.try_get("write_pk")?;
            Ok(VaultRow {
                id: *id,
                write_pk: fixed(&pk)?,
                node: r.try_get("node")?,
                created_day: r.try_get("created_day")?,
                bytes_used: r.try_get("bytes_used")?,
            })
        })
        .transpose()
    }

    async fn claim(&self, row: VaultRow) -> Result<ClaimOutcome, IndexError> {
        let inserted = sqlx::query(
            "INSERT IGNORE INTO storage_vaults (vault_id, write_pk, node, created_day) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&row.id[..])
        .bind(&row.write_pk[..])
        .bind(&row.node)
        .bind(row.created_day)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if inserted == 1 {
            return Ok(ClaimOutcome::Created);
        }
        match self.vault(&row.id).await? {
            Some(existing) if existing.write_pk == row.write_pk => Ok(ClaimOutcome::SameKey),
            Some(_) => Ok(ClaimOutcome::OtherKey),
            // Deleted between the insert and the read: claim again once.
            None => {
                let again = sqlx::query(
                    "INSERT IGNORE INTO storage_vaults (vault_id, write_pk, node, created_day) \
                     VALUES (?, ?, ?, ?)",
                )
                .bind(&row.id[..])
                .bind(&row.write_pk[..])
                .bind(&row.node)
                .bind(row.created_day)
                .execute(&self.pool)
                .await?
                .rows_affected();
                Ok(if again == 1 {
                    ClaimOutcome::Created
                } else {
                    ClaimOutcome::OtherKey
                })
            },
        }
    }

    async fn unclaim(&self, id: &VaultId) -> Result<bool, IndexError> {
        let n = sqlx::query(
            "DELETE FROM storage_vaults WHERE vault_id = ? AND NOT EXISTS \
             (SELECT 1 FROM storage_vault_objects o WHERE o.vault_id = ?)",
        )
        .bind(&id[..])
        .bind(&id[..])
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    async fn object(
        &self,
        id: &VaultId,
        addr: ObjectAddr,
    ) -> Result<Option<ObjectRow>, IndexError> {
        let row = sqlx::query(
            "SELECT version, token, size_bytes FROM storage_vault_objects \
             WHERE vault_id = ? AND kind = ? AND chunk_no = ? AND version > 0",
        )
        .bind(&id[..])
        .bind(addr.kind_byte())
        .bind(addr.chunk_no())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let token: Option<Vec<u8>> = r.try_get("token")?;
            let token = token.ok_or(IndexError::Corrupt(0))?;
            Ok(ObjectRow {
                version: r.try_get("version")?,
                token: fixed(&token)?,
                size: r.try_get("size_bytes")?,
            })
        })
        .transpose()
    }

    async fn reserve(
        &self,
        id: &VaultId,
        req: WriteRequest,
        limits: &VaultLimits,
    ) -> Result<ReserveOutcome, IndexError> {
        let mut attempt = 1;
        loop {
            match self.reserve_once(id, req, limits).await {
                Err(e) if attempt < DEADLOCK_ATTEMPTS && is_deadlock(&e) => attempt += 1,
                other => return other,
            }
        }
    }

    async fn commit(
        &self,
        id: &VaultId,
        addr: ObjectAddr,
        token: &ReservationToken,
        size: u64,
    ) -> Result<CommitOutcome, IndexError> {
        let mut attempt = 1;
        loop {
            match self.commit_once(id, addr, token, size).await {
                Err(e) if attempt < DEADLOCK_ATTEMPTS && is_deadlock(&e) => attempt += 1,
                other => return other,
            }
        }
    }

    async fn release(
        &self,
        id: &VaultId,
        addr: ObjectAddr,
        token: &ReservationToken,
    ) -> Result<(), IndexError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM storage_vault_objects WHERE vault_id = ? AND kind = ? AND chunk_no = ? \
             AND reserved_token = ? AND version = 0",
        )
        .bind(&id[..])
        .bind(addr.kind_byte())
        .bind(addr.chunk_no())
        .bind(&token[..])
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE storage_vault_objects SET reserved_version = NULL, reserved_token = NULL, \
             reserved_size = 0, reserved_until_ms = 0 \
             WHERE vault_id = ? AND kind = ? AND chunk_no = ? AND reserved_token = ?",
        )
        .bind(&id[..])
        .bind(addr.kind_byte())
        .bind(addr.chunk_no())
        .bind(&token[..])
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn delete_vault(&self, id: &VaultId) -> Result<bool, IndexError> {
        // The object rows go with the vault row (ON DELETE CASCADE).
        let n = sqlx::query("DELETE FROM storage_vaults WHERE vault_id = ?")
            .bind(&id[..])
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(n == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_pool;
    use super::*;

    #[tokio::test]
    async fn the_sql_vault_index_meets_the_contract() {
        let Some(pool) = test_pool("the_sql_vault_index_meets_the_contract").await else {
            return;
        };
        let index = SqlIndex::new(pool);
        crate::index::contract::run_vault(&index).await;
    }

    #[tokio::test]
    async fn concurrent_reservations_of_one_version_yield_one_winner() {
        let Some(pool) = test_pool("concurrent_reservations_of_one_version_yield_one_winner").await
        else {
            return;
        };
        let index = std::sync::Arc::new(SqlIndex::new(pool));
        crate::index::contract::run_vault_race(index).await;
    }
}
