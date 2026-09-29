// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MariaDB-backed `ChannelStore` implementation (G4-1-sql).
//!
//! Mirror of [`crate::storage::InMemoryChannelStore`] —
//! same trait, same opacity contract. The four tables
//! (`channels`, `channel_posts`, `channel_subscribers`,
//! `channel_envelopes`) live in `migrations/0001_init.sql`; the ADR-0060
//! member cards in `migrations/0005_member_cards.sql`.
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

use crate::config::DEFAULT_ENVELOPE_RETENTION_EPOCHS;
use crate::storage::{ChannelId, ChannelStore, StorageError, StoredPost, SubscriberId};

/// MariaDB-backed `ChannelStore`. Construct with a pool
/// that has been migrated via `sqlx::migrate!` from the
/// `migrations/` directory.
#[derive(Clone)]
pub struct SqlChannelStore {
    pool: MySqlPool,
    /// The most-recent N seed envelopes retained per
    /// `(channel, subscriber)` (ADR-0031). Always `>= 1` —
    /// [`SqlChannelStore::with_envelope_retention`] clamps it
    /// and the default is [`DEFAULT_ENVELOPE_RETENTION_EPOCHS`].
    envelope_retention_epochs: usize,
}

impl SqlChannelStore {
    /// Wrap an existing `MySqlPool`. The caller is
    /// responsible for running migrations before any
    /// handler hits this store; the binary's `main()`
    /// does so via `sqlx::migrate!("./migrations").run(&pool)`.
    ///
    /// Envelope retention defaults to
    /// [`DEFAULT_ENVELOPE_RETENTION_EPOCHS`] (4).
    #[must_use]
    pub fn new(pool: MySqlPool) -> Self {
        Self {
            pool,
            envelope_retention_epochs: DEFAULT_ENVELOPE_RETENTION_EPOCHS,
        }
    }

    /// Override the per-subscriber envelope retention window
    /// (ADR-0031). `n` is clamped to `>= 1` — a retention of 0
    /// would prune every envelope on the next write.
    #[must_use]
    pub fn with_envelope_retention(mut self, n: usize) -> Self {
        self.envelope_retention_epochs = n.max(1);
        self
    }
}

/// Prune a subscriber's retained envelopes down to the
/// highest `keep_n` epochs, deleting the lower epochs
/// (ADR-0031). Runs inside the caller's transaction so the
/// insert + prune are atomic (C-42). The derived-table
/// wrapper is required because MariaDB refuses to modify a
/// table that also appears in the `NOT IN` subquery directly.
async fn prune_envelopes<'e, E>(
    executor: E,
    channel: ChannelId,
    subscriber: SubscriberId,
    keep_n: usize,
) -> Result<(), StorageError>
where
    E: sqlx::Executor<'e, Database = sqlx::MySql>,
{
    // `keep_n` is `>= 1` (builder-clamped); the `.max(1)` is
    // belt-and-braces so a stray 0 never deletes everything.
    let keep = i64::try_from(keep_n).unwrap_or(i64::MAX).max(1);
    sqlx::query(
        "DELETE FROM channel_envelopes \
         WHERE channel_id = ? AND subscriber_id = ? \
           AND epoch NOT IN ( \
               SELECT epoch FROM ( \
                   SELECT epoch FROM channel_envelopes \
                   WHERE channel_id = ? AND subscriber_id = ? \
                   ORDER BY epoch DESC LIMIT ? \
               ) t \
           )",
    )
    .bind(channel.as_bytes().as_slice())
    .bind(subscriber.as_bytes().as_slice())
    .bind(channel.as_bytes().as_slice())
    .bind(subscriber.as_bytes().as_slice())
    .bind(keep)
    .execute(executor)
    .await
    .map_err(db_err)?;
    Ok(())
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
        sqlx::query_scalar::<_, i64>("SELECT 1 FROM channels WHERE id = ? LIMIT 1")
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
        let existing: Option<(u64,)> =
            sqlx::query_as("SELECT next_cursor FROM channels WHERE id = ? FOR UPDATE")
                .bind(channel.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        let Some((cursor,)) = existing else {
            return Err(StorageError::UnknownChannel);
        };
        sqlx::query("UPDATE channels SET next_cursor = next_cursor + 1 WHERE id = ?")
            .bind(channel.as_bytes().as_slice())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO channel_posts \
                 (channel_id, `cursor`, server_ts_ms, payload) \
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
            "SELECT `cursor`, server_ts_ms, payload \
             FROM channel_posts \
             WHERE channel_id = ? AND `cursor` > ? \
             ORDER BY `cursor` ASC \
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

    async fn prune_posts_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
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

    async fn is_subscribed(&self, channel: ChannelId, subscriber: SubscriberId) -> bool {
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

    async fn prune_subscribers_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
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
        epoch: Option<u32>,
        envelope: Vec<u8>,
        expected_len: usize,
    ) -> Result<(), StorageError> {
        if envelope.len() != expected_len {
            return Err(StorageError::BadEnvelopeLen {
                expected: expected_len,
                got: envelope.len(),
            });
        }
        // Transaction: lock the channel row, resolve the
        // target epoch, refuse a future epoch, UPSERT at that
        // epoch, then prune to the retained N — all atomic so
        // a concurrent rotation can't interleave (C-42).
        let mut tx = self.pool.begin().await.map_err(db_err)?;
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
        let registered: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM channel_subscribers \
             WHERE channel_id = ? AND subscriber_id = ? \
             LIMIT 1",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if registered.is_none() {
            return Err(StorageError::UnknownSubscriber);
        }
        let ep = epoch.unwrap_or(current);
        if ep > current {
            return Err(StorageError::EnvelopeEpochInFuture { current, got: ep });
        }
        // UPSERT at the concrete epoch (ADR-0031).
        sqlx::query(
            "INSERT INTO channel_envelopes \
                 (channel_id, subscriber_id, epoch, envelope) \
             VALUES (?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE \
                 envelope = VALUES(envelope), \
                 delivered_at = CURRENT_TIMESTAMP(3)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .bind(ep)
        .bind(&envelope)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        prune_envelopes(
            &mut *tx,
            channel,
            subscriber,
            self.envelope_retention_epochs,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;
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
        // Highest retained epoch — the current seed (ADR-0031).
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT envelope FROM channel_envelopes \
             WHERE channel_id = ? AND subscriber_id = ? \
             ORDER BY epoch DESC LIMIT 1",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| b))
    }

    async fn fetch_envelopes(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<Vec<(u32, Vec<u8>)>, StorageError> {
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        if !self.is_subscribed(channel, subscriber).await {
            return Err(StorageError::UnknownSubscriber);
        }
        // The whole retained set, ascending by epoch (ADR-0031).
        let rows: Vec<(u32, Vec<u8>)> = sqlx::query_as(
            "SELECT epoch, envelope FROM channel_envelopes \
             WHERE channel_id = ? AND subscriber_id = ? \
             ORDER BY epoch ASC",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows)
    }

    async fn total_subscribers(&self) -> usize {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channel_subscribers")
            .fetch_one(&self.pool)
            .await
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0)
    }

    async fn current_epoch(&self, channel: ChannelId) -> Result<u32, StorageError> {
        let row: Option<(u32,)> = sqlx::query_as("SELECT current_epoch FROM channels WHERE id = ?")
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

        // Validations passed — file each envelope at the new
        // epoch, prune the subscriber to the retained N, and
        // bump the epoch, all in this one transaction (C-42 +
        // ADR-0031).
        let new_epoch = current.saturating_add(1);
        for (sub, env) in batch {
            sqlx::query(
                "INSERT INTO channel_envelopes \
                     (channel_id, subscriber_id, epoch, envelope) \
                 VALUES (?, ?, ?, ?) \
                 ON DUPLICATE KEY UPDATE \
                     envelope = VALUES(envelope), \
                     delivered_at = CURRENT_TIMESTAMP(3)",
            )
            .bind(channel.as_bytes().as_slice())
            .bind(sub.as_bytes().as_slice())
            .bind(new_epoch)
            .bind(&env)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            prune_envelopes(&mut *tx, channel, sub, self.envelope_retention_epochs).await?;
        }
        sqlx::query("UPDATE channels SET current_epoch = ? WHERE id = ?")
            .bind(new_epoch)
            .bind(channel.as_bytes().as_slice())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(new_epoch)
    }

    async fn stage_envelopes(
        &self,
        channel: ChannelId,
        target_epoch: u32,
        batch: Vec<(SubscriberId, Vec<u8>)>,
        expected_envelope_len: usize,
    ) -> Result<(), StorageError> {
        // First pass: validate batch entry lengths before
        // opening a transaction. Matches the single-shot
        // rotate's either-all-or-none contract.
        for (_sub, env) in &batch {
            if env.len() != expected_envelope_len {
                return Err(StorageError::BadEnvelopeLen {
                    expected: expected_envelope_len,
                    got: env.len(),
                });
            }
        }
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        // Lock the channel row + read its live epoch. FOR
        // UPDATE serializes a concurrent stage/commit on the
        // same channel through the row lock.
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

        // Monotone target: refuse anything but current + 1.
        let expected = current.saturating_add(1);
        if target_epoch != expected {
            return Err(StorageError::StaleTargetEpoch {
                expected,
                got: target_epoch,
            });
        }

        // Cross-check every subscriber id against the
        // registry before any staged row lands.
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

        // Validations passed — upsert the staged rows. The
        // (channel, target_epoch, subscriber) primary key
        // makes a re-sent chunk an idempotent overwrite.
        for (sub, env) in batch {
            sqlx::query(
                "INSERT INTO channel_staged_envelopes \
                     (channel_id, target_epoch, subscriber_id, envelope) \
                 VALUES (?, ?, ?, ?) \
                 ON DUPLICATE KEY UPDATE \
                     envelope = VALUES(envelope), \
                     staged_at = CURRENT_TIMESTAMP(3)",
            )
            .bind(channel.as_bytes().as_slice())
            .bind(target_epoch)
            .bind(sub.as_bytes().as_slice())
            .bind(&env)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn commit_rotation(
        &self,
        channel: ChannelId,
        target_epoch: u32,
    ) -> Result<u32, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        // Lock the channel row + read its live epoch.
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

        // Monotone target: refuse anything but current + 1.
        let expected = current.saturating_add(1);
        if target_epoch != expected {
            return Err(StorageError::StaleTargetEpoch {
                expected,
                got: target_epoch,
            });
        }

        // Snapshot the staged subscribers for this target so
        // we can prune each after promotion (ADR-0031). Read
        // before the promote/delete so the ids survive.
        let staged_subs: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT subscriber_id FROM channel_staged_envelopes \
             WHERE channel_id = ? AND target_epoch = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(target_epoch)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_err)?;

        // Promote the staged rows for this target into the
        // live set at `target_epoch` — but only for subscribers
        // that are still registered (partial-coverage policy;
        // also honours the composite FK on channel_envelopes so
        // a pruned-mid-prepare subscriber cannot abort the
        // commit). A registered subscriber with no staged row
        // keeps their prior-epoch envelope.
        sqlx::query(
            "INSERT INTO channel_envelopes \
                 (channel_id, subscriber_id, epoch, envelope) \
             SELECT s.channel_id, s.subscriber_id, ?, s.envelope \
             FROM channel_staged_envelopes s \
             WHERE s.channel_id = ? AND s.target_epoch = ? \
               AND EXISTS ( \
                   SELECT 1 FROM channel_subscribers cs \
                   WHERE cs.channel_id = s.channel_id \
                     AND cs.subscriber_id = s.subscriber_id \
               ) \
             ON DUPLICATE KEY UPDATE \
                 envelope = VALUES(envelope), \
                 delivered_at = CURRENT_TIMESTAMP(3)",
        )
        .bind(target_epoch)
        .bind(channel.as_bytes().as_slice())
        .bind(target_epoch)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        // Prune each promoted subscriber to the retained N,
        // inside this same transaction (C-42 + ADR-0031).
        // Pruning a staged-but-unregistered subscriber whose
        // envelope was not promoted is a harmless no-op.
        for (bytes,) in &staged_subs {
            let sub = SubscriberId::from_bytes(bytes)?;
            prune_envelopes(&mut *tx, channel, sub, self.envelope_retention_epochs).await?;
        }

        // Single atomic transition: bump the epoch to the
        // target, then clear every staged row for the channel
        // (this target and any abandoned older prepare).
        sqlx::query("UPDATE channels SET current_epoch = ? WHERE id = ?")
            .bind(target_epoch)
            .bind(channel.as_bytes().as_slice())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("DELETE FROM channel_staged_envelopes WHERE channel_id = ?")
            .bind(channel.as_bytes().as_slice())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(target_epoch)
    }

    async fn put_member_card(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        epoch: u32,
        card: Vec<u8>,
    ) -> Result<(), StorageError> {
        // Lock the channel row so the epoch check and the write cannot
        // interleave with a rotation, then refuse a future or a regressed
        // epoch.
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let existing: Option<(u32,)> =
            sqlx::query_as("SELECT current_epoch FROM channels WHERE id = ? FOR UPDATE")
                .bind(channel.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        let Some((current,)) = existing else {
            return Err(StorageError::UnknownChannel);
        };
        let registered: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM channel_subscribers \
             WHERE channel_id = ? AND subscriber_id = ? LIMIT 1",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if registered.is_none() {
            return Err(StorageError::UnknownSubscriber);
        }
        if epoch > current {
            return Err(StorageError::MemberCardEpochInFuture {
                current,
                got: epoch,
            });
        }
        let stored: Option<(u32,)> = sqlx::query_as(
            "SELECT epoch FROM channel_member_cards \
             WHERE channel_id = ? AND subscriber_id = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if let Some((stored,)) = stored {
            if epoch < stored {
                return Err(StorageError::MemberCardStale { stored, got: epoch });
            }
        }
        sqlx::query(
            "INSERT INTO channel_member_cards (channel_id, subscriber_id, card, epoch) \
             VALUES (?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE \
                 card = VALUES(card), epoch = VALUES(epoch), \
                 updated_at = CURRENT_TIMESTAMP(3)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .bind(&card)
        .bind(epoch)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn delete_member_card(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<(), StorageError> {
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        sqlx::query("DELETE FROM channel_member_cards WHERE channel_id = ? AND subscriber_id = ?")
            .bind(channel.as_bytes().as_slice())
            .bind(subscriber.as_bytes().as_slice())
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn list_member_cards(
        &self,
        channel: ChannelId,
        after: Option<SubscriberId>,
        limit: usize,
    ) -> Result<Vec<(SubscriberId, Vec<u8>)>, StorageError> {
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        // BINARY(32) compares bytewise, the same order as `SubscriberId`'s
        // derived `Ord`, so the page cursor means the same in both stores.
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows: Vec<(Vec<u8>, Vec<u8>)> = match after {
            None => {
                sqlx::query_as(
                    "SELECT subscriber_id, card FROM channel_member_cards \
                     WHERE channel_id = ? ORDER BY subscriber_id ASC LIMIT ?",
                )
                .bind(channel.as_bytes().as_slice())
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            },
            Some(after) => {
                sqlx::query_as(
                    "SELECT subscriber_id, card FROM channel_member_cards \
                     WHERE channel_id = ? AND subscriber_id > ? \
                     ORDER BY subscriber_id ASC LIMIT ?",
                )
                .bind(channel.as_bytes().as_slice())
                .bind(after.as_bytes().as_slice())
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            },
        }
        .map_err(db_err)?;
        rows.into_iter()
            .map(|(sub, card)| Ok((SubscriberId::from_bytes(&sub)?, card)))
            .collect()
    }
}

// ============================================================================
// Tests - the ADR-0060 member-card paths against MariaDB, gated on
// `LAKKO_CHANNELS_TEST_DATABASE_URL` and skipped silently without it.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> Option<SqlChannelStore> {
        let Ok(url) = std::env::var("LAKKO_CHANNELS_TEST_DATABASE_URL") else {
            eprintln!("skipping: LAKKO_CHANNELS_TEST_DATABASE_URL unset");
            return None;
        };
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test db");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrate");
        Some(SqlChannelStore::new(pool))
    }

    fn random<const N: usize>() -> [u8; N] {
        let mut out = [0u8; N];
        lakko_crypto::random::fill(&mut out);
        out
    }

    #[tokio::test]
    async fn member_cards_are_epoch_bounded_paged_and_pruned_with_the_subscriber() {
        let Some(s) = store().await else {
            return;
        };
        let channel = ChannelId::from_bytes(&random::<16>()).unwrap();
        s.create(channel).await.unwrap();
        let mut subs: Vec<SubscriberId> = (0..3)
            .map(|_| SubscriberId::from_bytes(&random::<32>()).unwrap())
            .collect();
        subs.sort_unstable();
        for sub in &subs {
            s.subscribe(channel, *sub, 0).await.unwrap();
        }

        assert!(matches!(
            s.put_member_card(channel, subs[0], 1, vec![1; 40]).await,
            Err(StorageError::MemberCardEpochInFuture { current: 0, got: 1 })
        ));
        s.rotate(channel, vec![], 80).await.unwrap();
        for sub in &subs {
            s.put_member_card(channel, *sub, 1, sub.as_bytes().to_vec())
                .await
                .unwrap();
        }
        assert!(matches!(
            s.put_member_card(channel, subs[0], 0, vec![0; 40]).await,
            Err(StorageError::MemberCardStale { stored: 1, got: 0 })
        ));
        assert!(matches!(
            s.put_member_card(
                channel,
                SubscriberId::from_bytes(&random::<32>()).unwrap(),
                0,
                vec![0; 40]
            )
            .await,
            Err(StorageError::UnknownSubscriber)
        ));

        let first = s.list_member_cards(channel, None, 2).await.unwrap();
        assert_eq!(
            first.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            subs[..2].to_vec()
        );
        let rest = s
            .list_member_cards(channel, Some(subs[1]), 2)
            .await
            .unwrap();
        assert_eq!(rest, vec![(subs[2], subs[2].as_bytes().to_vec())]);

        s.delete_member_card(channel, subs[0]).await.unwrap();
        s.delete_member_card(channel, subs[0]).await.unwrap();
        assert_eq!(
            s.list_member_cards(channel, None, 10).await.unwrap().len(),
            2
        );

        // The stale-subscriber prune takes the card through the FK.
        sqlx::query(
            "UPDATE channel_subscribers SET last_active_at = FROM_UNIXTIME(1) \
             WHERE channel_id = ? AND subscriber_id = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subs[1].as_bytes().as_slice())
        .execute(&s.pool)
        .await
        .unwrap();
        s.prune_subscribers_older_than(10_000).await.unwrap();
        assert_eq!(
            s.list_member_cards(channel, None, 10).await.unwrap(),
            vec![(subs[2], subs[2].as_bytes().to_vec())]
        );
    }
}
