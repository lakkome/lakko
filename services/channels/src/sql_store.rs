// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MariaDB-backed `ChannelStore` implementation (G4-1-sql).
//!
//! Mirror of [`crate::storage::InMemoryChannelStore`] —
//! same trait, same opacity contract. The four tables
//! (`channels`, `channel_posts`, `channel_subscribers`,
//! `channel_envelopes`) live in `migrations/0001_init.sql`.
//!
//! ## Opacity contract — pinned
//!
//! Every byte we hand to MariaDB stays opaque on the way
//! in and on the way out. `payload`, `envelope`,
//! `subscriber_id` are all stored as raw `BLOB` /
//! `VARBINARY` — no JSON, no UTF-8 validation, no
//! decoding. The SQL impl never imports
//! `lakko-channels` and never derives a key or a nonce.
//! Audit-pack invariant C-40 (publisher path) + C-41
//! (subscribe handshake) both rely on this property
//! continuing to hold across the in-memory → SQL swap.
//!
//! ## Atomicity
//!
//! `put` and `rotate` are the two methods where atomicity
//! matters across multiple table writes. Both run inside
//! a `BEGIN` / `COMMIT` transaction so a concurrent reader
//! cannot observe a half-applied state.

use async_trait::async_trait;
use sqlx::MySqlPool;
use sqlx::Row;

use crate::storage::{
    ChannelId, ChannelStore, StorageError, StoredPost, SubscriberId,
};

/// MariaDB-backed `ChannelStore`. Construct with a pool
/// that has been migrated via `sqlx::migrate!` from the
/// `migrations/` directory.
#[derive(Clone)]
pub struct SqlChannelStore {
    pool: MySqlPool,
}

impl SqlChannelStore {
    /// Wrap an existing `MySqlPool`. The caller is
    /// responsible for running migrations before any
    /// handler hits this store; the binary's `main()`
    /// does so via `sqlx::migrate!("./migrations").run(&pool)`.
    #[must_use]
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}

/// Map any `sqlx::Error` into the storage layer's domain
/// error type. We surface a single `StorageError::Database`
/// variant — handlers that need to differentiate (unknown
/// channel vs. row not found) check `Option<...>` shape
/// before this conversion runs.
fn db_err(e: sqlx::Error) -> StorageError {
    StorageError::Database(e.to_string())
}

#[async_trait]
impl ChannelStore for SqlChannelStore {
    async fn create(&self, channel: ChannelId) -> Result<(), StorageError> {
        // INSERT IGNORE keeps the call idempotent — a second
        // call with the same id is a no-op (matches the
        // in-memory impl's contract).
        sqlx::query(
            "INSERT IGNORE INTO channels (id, current_epoch, next_cursor) \
             VALUES (?, 0, 1)",
        )
        .bind(channel.as_bytes().as_slice())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn exists(&self, channel: ChannelId) -> bool {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM channels WHERE id = ? LIMIT 1",
        )
        .bind(channel.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .is_some()
    }

    async fn put(
        &self,
        channel: ChannelId,
        payload: Vec<u8>,
        max_bytes: usize,
        server_ts_ms: u64,
    ) -> Result<StoredPost, StorageError> {
        if payload.is_empty() {
            return Err(StorageError::EmptyPost);
        }
        if payload.len() > max_bytes {
            return Err(StorageError::PostTooLarge {
                max: max_bytes,
                got: payload.len(),
            });
        }
        // Transaction: bump next_cursor, read the value
        // back, insert the post under that cursor. A
        // concurrent put on the same channel serializes
        // through the row-level lock on `channels.id`.
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Bump and capture in one round-trip via UPDATE
        // ... RETURNING-equivalent (MariaDB lacks RETURNING
        // on UPDATE; emulate with SELECT FOR UPDATE then
        // UPDATE).
        let existing: Option<(u64,)> = sqlx::query_as(
            "SELECT next_cursor FROM channels WHERE id = ? FOR UPDATE",
        )
        .bind(channel.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        let Some((cursor,)) = existing else {
            return Err(StorageError::UnknownChannel);
        };
        sqlx::query(
            "UPDATE channels SET next_cursor = next_cursor + 1 WHERE id = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO channel_posts \
                 (channel_id, cursor, server_ts_ms, payload) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(cursor)
        .bind(server_ts_ms)
        .bind(&payload)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(StoredPost {
            cursor,
            server_ts_ms,
            payload,
        })
    }

    async fn fetch(
        &self,
        channel: ChannelId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<StoredPost>, StorageError> {
        // Confirm the channel exists before returning an
        // empty result set — the in-memory impl distinguishes
        // "unknown channel" from "no new posts", and the
        // API layer surfaces both as different status codes.
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(
            "SELECT cursor, server_ts_ms, payload \
             FROM channel_posts \
             WHERE channel_id = ? AND cursor > ? \
             ORDER BY cursor ASC \
             LIMIT ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(after)
        .bind(limit_i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|r| StoredPost {
                cursor: r.get::<u64, _>("cursor"),
                server_ts_ms: r.get::<u64, _>("server_ts_ms"),
                payload: r.get::<Vec<u8>, _>("payload"),
            })
            .collect())
    }

    async fn total_posts(&self) -> usize {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channel_posts")
            .fetch_one(&self.pool)
            .await
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0)
    }

    async fn prune_posts_older_than(
        &self,
        cutoff_ms: u64,
    ) -> Result<u64, StorageError> {
        // Single bulk DELETE — the per-channel indexes
        // (`PRIMARY KEY (channel_id, cursor)` +
        // `INDEX idx_channel_cursor`) keep this fast
        // even against a multi-million-row table because
        // the cutoff column is monotone-ish per channel
        // (server timestamps grow with cursors). A
        // future optimization would partition by month;
        // out of scope for the MVP retention sprint.
        //
        // `channels.next_cursor` is deliberately NOT
        // rolled back — cursors stay monotone across
        // pruning so a paginating subscriber's resume
        // point keeps pointing at the right survivor.
        let result = sqlx::query("DELETE FROM channel_posts WHERE server_ts_ms < ?")
            .bind(cutoff_ms)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected())
    }

    async fn subscribe(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        _server_ts_ms: u64,
    ) -> Result<(), StorageError> {
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        // INSERT ... ON DUPLICATE KEY UPDATE keeps the call
        // idempotent. `registered_at` is bumped on re-subscribe
        // to mirror the in-memory impl's behaviour.
        sqlx::query(
            "INSERT INTO channel_subscribers \
                 (channel_id, subscriber_id) \
             VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE registered_at = CURRENT_TIMESTAMP(3)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn is_subscribed(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> bool {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM channel_subscribers \
             WHERE channel_id = ? AND subscriber_id = ? \
             LIMIT 1",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .is_some()
    }

    async fn touch_subscriber_activity(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        now_ms: u64,
    ) -> Result<(), StorageError> {
        // Single UPDATE — `last_active_at` is a
        // TIMESTAMP(3) column so we feed it the
        // millisecond value via FROM_UNIXTIME. Affecting
        // zero rows when the subscriber is not
        // registered is intentional (no-op semantics).
        sqlx::query(
            "UPDATE channel_subscribers \
             SET last_active_at = FROM_UNIXTIME(? / 1000.0) \
             WHERE channel_id = ? AND subscriber_id = ?",
        )
        .bind(now_ms)
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn prune_subscribers_older_than(
        &self,
        cutoff_ms: u64,
    ) -> Result<u64, StorageError> {
        // Single bulk DELETE. FK ON DELETE CASCADE on
        // `channel_envelopes` cleans up the matching
        // envelope rows automatically — no second query
        // needed. The (last_active_at) index ferries
        // this query past a full table scan even when
        // the surviving subscriber set is the majority.
        let result = sqlx::query(
            "DELETE FROM channel_subscribers \
             WHERE last_active_at < FROM_UNIXTIME(? / 1000.0)",
        )
        .bind(cutoff_ms)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(result.rows_affected())
    }

    async fn list_subscribers(
        &self,
        channel: ChannelId,
    ) -> Result<Vec<SubscriberId>, StorageError> {
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        let rows: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT subscriber_id FROM channel_subscribers \
             WHERE channel_id = ? \
             ORDER BY registered_at ASC, subscriber_id ASC",
        )
        .bind(channel.as_bytes().as_slice())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for (bytes,) in rows {
            out.push(SubscriberId::from_bytes(&bytes)?);
        }
        Ok(out)
    }

    async fn deliver_envelope(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        envelope: Vec<u8>,
        expected_len: usize,
    ) -> Result<(), StorageError> {
        if envelope.len() != expected_len {
            return Err(StorageError::BadEnvelopeLen {
                expected: expected_len,
                got: envelope.len(),
            });
        }
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        if !self.is_subscribed(channel, subscriber).await {
            return Err(StorageError::UnknownSubscriber);
        }
        // UPSERT — latest-wins per ADR-0010 N=1 retention.
        sqlx::query(
            "INSERT INTO channel_envelopes \
                 (channel_id, subscriber_id, envelope) \
             VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE \
                 envelope = VALUES(envelope), \
                 delivered_at = CURRENT_TIMESTAMP(3)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .bind(&envelope)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn fetch_envelope(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        if !self.is_subscribed(channel, subscriber).await {
            return Err(StorageError::UnknownSubscriber);
        }
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT envelope FROM channel_envelopes \
             WHERE channel_id = ? AND subscriber_id = ? \
             LIMIT 1",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| b))
    }

    async fn total_subscribers(&self) -> usize {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM channel_subscribers",
        )
        .fetch_one(&self.pool)
        .await
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0)
    }

    async fn current_epoch(
        &self,
        channel: ChannelId,
    ) -> Result<u32, StorageError> {
        let row: Option<(u32,)> = sqlx::query_as(
            "SELECT current_epoch FROM channels WHERE id = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        let Some((e,)) = row else {
            return Err(StorageError::UnknownChannel);
        };
        Ok(e)
    }

    async fn rotate(
        &self,
        channel: ChannelId,
        batch: Vec<(SubscriberId, Vec<u8>)>,
        expected_envelope_len: usize,
    ) -> Result<u32, StorageError> {
        // First pass: validate batch entry lengths
        // before opening a transaction. Matches the
        // in-memory impl's either-all-or-none contract.
        for (_sub, env) in &batch {
            if env.len() != expected_envelope_len {
                return Err(StorageError::BadEnvelopeLen {
                    expected: expected_envelope_len,
                    got: env.len(),
                });
            }
        }
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        // Lock the channel row + verify it exists.
        let existing: Option<(u32,)> = sqlx::query_as(
            "SELECT current_epoch FROM channels \
             WHERE id = ? FOR UPDATE",
        )
        .bind(channel.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        let Some((current,)) = existing else {
            return Err(StorageError::UnknownChannel);
        };

        // Cross-check every subscriber id against the
        // registry before any mutation lands.
        for (sub, _env) in &batch {
            let registered: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM channel_subscribers \
                 WHERE channel_id = ? AND subscriber_id = ? \
                 LIMIT 1",
            )
            .bind(channel.as_bytes().as_slice())
            .bind(sub.as_bytes().as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
            if registered.is_none() {
                return Err(StorageError::UnknownSubscriber);
            }
        }

        // Validations passed — commit the envelope
        // upserts + bump the epoch.
        for (sub, env) in batch {
            sqlx::query(
                "INSERT INTO channel_envelopes \
                     (channel_id, subscriber_id, envelope) \
                 VALUES (?, ?, ?) \
                 ON DUPLICATE KEY UPDATE \
                     envelope = VALUES(envelope), \
                     delivered_at = CURRENT_TIMESTAMP(3)",
            )
            .bind(channel.as_bytes().as_slice())
            .bind(sub.as_bytes().as_slice())
            .bind(&env)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        let new_epoch = current.saturating_add(1);
        sqlx::query(
            "UPDATE channels SET current_epoch = ? WHERE id = ?",
        )
        .bind(new_epoch)
        .bind(channel.as_bytes().as_slice())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(new_epoch)
    }
}
