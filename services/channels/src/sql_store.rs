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
use crate::directory::{DirectoryEntry, GROWTH_WINDOW_MS};
use crate::feedback::{
    CommentReactionCounts, Label, PostReactionCounts, StoredComment, WriterKey, WRITE_NONCE_LEN,
};
use crate::feedback_sql;
use crate::storage::{
    ChannelId, ChannelModeration, ChannelStore, DirectoryQuery, InviteKeyRow, ModerationOutcome,
    ModerationRow, ModerationStatement, PublicSeedOutcome, SealedTicket, StorageError,
    StoredListing, StoredPost, SubscriberEntry, SubscriberId, INVITE_KEY_LEN, OWNER_NONCE_LEN,
    OWNER_PUBKEY_LEN, PUBLIC_SEED_LEN, SEALED_TICKET_LEN,
};

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

/// Lock the channel row inside `tx` and read `(private, current_epoch)`;
/// `None` for an unknown channel. Serialises every write that moves the
/// subscriber counter or checks the flag.
async fn lock_channel(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    channel: ChannelId,
) -> Result<Option<(bool, u32)>, StorageError> {
    let row: Option<(i8, u32)> =
        sqlx::query_as("SELECT private, current_epoch FROM channels WHERE id = ? FOR UPDATE")
            .bind(channel.as_bytes().as_slice())
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_err)?;
    Ok(row.map(|(private, epoch)| (private != 0, epoch)))
}

/// Delete `(channel, subscriber)` and its staged envelopes inside `tx`
/// (envelopes and the member card go through the FK cascades) and move the
/// counter. The channel row must be locked. Returns whether a row went.
async fn delete_subscriber_row(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    channel: ChannelId,
    subscriber: SubscriberId,
) -> Result<bool, StorageError> {
    let n =
        sqlx::query("DELETE FROM channel_subscribers WHERE channel_id = ? AND subscriber_id = ?")
            .bind(channel.as_bytes().as_slice())
            .bind(subscriber.as_bytes().as_slice())
            .execute(&mut **tx)
            .await
            .map_err(db_err)?
            .rows_affected();
    sqlx::query("DELETE FROM channel_staged_envelopes WHERE channel_id = ? AND subscriber_id = ?")
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    decrement_count(tx, channel, n).await?;
    Ok(n > 0)
}

/// `subscriber_count -= n`, never below zero.
async fn decrement_count(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    channel: ChannelId,
    n: u64,
) -> Result<(), StorageError> {
    if n == 0 {
        return Ok(());
    }
    sqlx::query(
        "UPDATE channels SET subscriber_count = subscriber_count - LEAST(subscriber_count, ?) \
         WHERE id = ?",
    )
    .bind(n)
    .bind(channel.as_bytes().as_slice())
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    Ok(())
}

fn ticket_from(bytes: Option<Vec<u8>>) -> Result<Option<SealedTicket>, StorageError> {
    bytes
        .map(|b| {
            <[u8; SEALED_TICKET_LEN]>::try_from(b.as_slice())
                .map_err(|_| StorageError::Database("sealed ticket of the wrong length".into()))
        })
        .transpose()
}

fn invite_row(
    (pk, expiry_ms, max_uses, used_count): (Vec<u8>, u64, u32, u32),
) -> Result<InviteKeyRow, StorageError> {
    Ok(InviteKeyRow {
        invite_pk: <[u8; INVITE_KEY_LEN]>::try_from(pk.as_slice())
            .map_err(|_| StorageError::Database("invite key of the wrong length".into()))?,
        expiry_ms,
        max_uses,
        used_count,
    })
}

/// Escape a search needle for `LIKE ... ESCAPE '!'` and wrap it in `%`.
fn like_pattern(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 2);
    out.push('%');
    for c in needle.chars() {
        if matches!(c, '!' | '%' | '_') {
            out.push('!');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// One directory row as the candidate queries select it.
type DirectoryRow = (Vec<u8>, Vec<u8>, Option<u64>, i64, i64);

/// The select list + joins every directory read shares, as a literal so each
/// query stays a static string (`concat!`). The two `COUNT(*)` subqueries run
/// over `channel_subscribers`' primary key and its `(channel_id,
/// registered_at)` index.
macro_rules! directory_select {
    () => {
        "SELECT l.channel_id, l.listing, c.first_listed_at_ms, \
       (SELECT COUNT(*) FROM channel_subscribers s \
         WHERE s.channel_id = l.channel_id) AS subs, \
       (SELECT COUNT(*) FROM channel_subscribers s \
         WHERE s.channel_id = l.channel_id \
           AND s.registered_at >= FROM_UNIXTIME(? / 1000.0)) AS recent \
     FROM channel_listings l JOIN channels c ON c.id = l.channel_id "
    };
}

fn directory_entry(row: DirectoryRow, now_ms: u64) -> Result<DirectoryEntry, StorageError> {
    let (channel, listing, first_listed, subs, recent) = row;
    Ok(DirectoryEntry {
        channel: ChannelId::from_bytes(&channel)?,
        listing,
        subscribers: u64::try_from(subs).unwrap_or(0),
        recent_registrations: u64::try_from(recent).unwrap_or(0),
        first_listed_at_ms: first_listed.unwrap_or(now_ms),
    })
}

#[async_trait]
impl ChannelStore for SqlChannelStore {
    async fn create_with_access(
        &self,
        channel: ChannelId,
        owner_pubkey: [u8; OWNER_PUBKEY_LEN],
        private: bool,
    ) -> Result<(), StorageError> {
        if self.is_deleted(channel).await? {
            return Err(StorageError::ChannelDeleted);
        }
        // INSERT IGNORE keeps the call idempotent; the owner key of the row
        // that won is then compared, so a second create with another key
        // changes nothing and is refused (ADR-0059 section 1). The flag is
        // written with the row only (ADR-0087).
        sqlx::query(
            "INSERT IGNORE INTO channels (id, current_epoch, next_cursor, owner_pubkey, private) \
             VALUES (?, 0, 1, ?, ?)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(owner_pubkey.as_slice())
        .bind(i8::from(private))
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        match self.owner_pubkey(channel).await? {
            Some(stored) if stored == owner_pubkey => Ok(()),
            Some(_) => Err(StorageError::OwnerMismatch),
            None => Err(StorageError::Database(
                "channel row missing after insert".to_string(),
            )),
        }
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

    async fn subscribe_with_ticket(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        server_ts_ms: u64,
        sealed_ticket: Option<SealedTicket>,
    ) -> Result<bool, StorageError> {
        // One transaction under the channel row lock: whether the row is new
        // decides the counter, so the check and the write are one step. A
        // re-subscribe counts as activity; the first registration time and
        // row number are kept, since the directory's 7-day growth signal
        // counts new subscribers, not re-registrations (ADR-0059).
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        if lock_channel(&mut tx, channel).await?.is_none() {
            return Err(StorageError::UnknownChannel);
        }
        let ticket = sealed_ticket.map(|t| t.to_vec());
        // A plain read, not a locking one: every insert and delete of this
        // channel's rows holds the channel row lock taken above, so the read
        // cannot race one, and a locking read of an absent row would take a
        // gap lock that deadlocks against inserts of neighbouring channels.
        let existing: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM channel_subscribers \
             WHERE channel_id = ? AND subscriber_id = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        let is_new = existing.is_none();
        if is_new {
            sqlx::query(
                "INSERT INTO channel_subscribers \
                     (channel_id, subscriber_id, registered_at, last_active_at, sealed_ticket) \
                 VALUES (?, ?, FROM_UNIXTIME(? / 1000.0), FROM_UNIXTIME(? / 1000.0), ?)",
            )
            .bind(channel.as_bytes().as_slice())
            .bind(subscriber.as_bytes().as_slice())
            .bind(server_ts_ms)
            .bind(server_ts_ms)
            .bind(&ticket)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            sqlx::query("UPDATE channels SET subscriber_count = subscriber_count + 1 WHERE id = ?")
                .bind(channel.as_bytes().as_slice())
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        } else {
            sqlx::query(
                "UPDATE channel_subscribers \
                 SET last_active_at = FROM_UNIXTIME(? / 1000.0), \
                     sealed_ticket = COALESCE(?, sealed_ticket) \
                 WHERE channel_id = ? AND subscriber_id = ?",
            )
            .bind(server_ts_ms)
            .bind(&ticket)
            .bind(channel.as_bytes().as_slice())
            .bind(subscriber.as_bytes().as_slice())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        sqlx::query(
            "DELETE FROM channel_removed_subscribers WHERE channel_id = ? AND subscriber_id = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(is_new)
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
        // Per channel, under its row lock, so each channel's counter moves
        // by exactly the rows its DELETE removed (ADR-0087 section 15: a
        // maintained counter). FK ON DELETE CASCADE on `channel_envelopes`
        // and the member cards cleans up the matching rows. The
        // (last_active_at) index finds the channels.
        let channels: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT DISTINCT channel_id FROM channel_subscribers \
             WHERE last_active_at < FROM_UNIXTIME(? / 1000.0)",
        )
        .bind(cutoff_ms)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        let mut removed: u64 = 0;
        for (id,) in channels {
            let channel = ChannelId::from_bytes(&id)?;
            let mut tx = self.pool.begin().await.map_err(db_err)?;
            if lock_channel(&mut tx, channel).await?.is_none() {
                continue;
            }
            let n = sqlx::query(
                "DELETE FROM channel_subscribers \
                 WHERE channel_id = ? AND last_active_at < FROM_UNIXTIME(? / 1000.0)",
            )
            .bind(channel.as_bytes().as_slice())
            .bind(cutoff_ms)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
            .rows_affected();
            decrement_count(&mut tx, channel, n).await?;
            tx.commit().await.map_err(db_err)?;
            removed = removed.saturating_add(n);
        }
        sqlx::query("DELETE FROM channel_removed_subscribers WHERE removed_at_ms < ?")
            .bind(cutoff_ms)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(removed)
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

    async fn owner_pubkey(
        &self,
        channel: ChannelId,
    ) -> Result<Option<[u8; OWNER_PUBKEY_LEN]>, StorageError> {
        let row: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT owner_pubkey FROM channels WHERE id = ?")
                .bind(channel.as_bytes().as_slice())
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        row.map(|(bytes,)| {
            <[u8; OWNER_PUBKEY_LEN]>::try_from(bytes.as_slice())
                .map_err(|_| StorageError::Database("owner_pubkey is not 32 bytes".to_string()))
        })
        .transpose()
    }

    async fn replace_owner(
        &self,
        channel: ChannelId,
        current_owner: [u8; OWNER_PUBKEY_LEN],
        new_owner: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<bool, StorageError> {
        // One conditional UPDATE: of two handovers verified under the same
        // key, the row lock lets one through and the other matches no row.
        let replaced =
            sqlx::query("UPDATE channels SET owner_pubkey = ? WHERE id = ? AND owner_pubkey = ?")
                .bind(new_owner.as_slice())
                .bind(channel.as_bytes().as_slice())
                .bind(current_owner.as_slice())
                .execute(&self.pool)
                .await
                .map_err(db_err)?
                .rows_affected();
        if replaced == 1 {
            return Ok(true);
        }
        // No row matched: an unknown channel, or a key that is not (or no
        // longer) the owner's. (`new == current` matches a row and changes
        // nothing; MariaDB then reports 0 affected rows, which is also the
        // "nothing changed" answer.)
        match self.owner_pubkey(channel).await? {
            None => Err(StorageError::UnknownChannel),
            Some(stored) => Ok(stored == new_owner && current_owner == new_owner),
        }
    }

    async fn consume_owner_nonce(
        &self,
        nonce: [u8; OWNER_NONCE_LEN],
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        // One INSERT on the primary key: of two concurrent uses of one nonce
        // exactly one succeeds.
        let res = sqlx::query("INSERT INTO channel_owner_nonces (nonce, seen_at_ms) VALUES (?, ?)")
            .bind(nonce.as_slice())
            .bind(now_ms)
            .execute(&self.pool)
            .await;
        match res {
            Ok(_) => Ok(true),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(false),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn prune_owner_nonces_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
        let res = sqlx::query("DELETE FROM channel_owner_nonces WHERE seen_at_ms < ?")
            .bind(cutoff_ms)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected())
    }

    async fn appoint_copublisher(
        &self,
        channel: ChannelId,
        key: [u8; OWNER_PUBKEY_LEN],
        max: usize,
        now_ms: u64,
    ) -> Result<(), StorageError> {
        // One transaction under the channel row's lock, so two concurrent
        // appointments cannot both pass the count and exceed the cap.
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let exists: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT id FROM channels WHERE id = ? FOR UPDATE")
                .bind(channel.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if exists.is_none() {
            return Err(StorageError::UnknownChannel);
        }
        let already: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM channel_copublishers WHERE channel_id = ? AND pubkey = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(key.as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if already.is_some() {
            tx.commit().await.map_err(db_err)?;
            return Ok(());
        }
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM channel_copublishers WHERE channel_id = ?")
                .bind(channel.as_bytes().as_slice())
                .fetch_one(&mut *tx)
                .await
                .map_err(db_err)?;
        if usize::try_from(count).unwrap_or(usize::MAX) >= max {
            return Err(StorageError::TooManyCoPublishers);
        }
        sqlx::query(
            "INSERT INTO channel_copublishers (channel_id, pubkey, appointed_at_ms) \
             VALUES (?, ?, ?)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(key.as_slice())
        .bind(now_ms)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn revoke_copublisher(
        &self,
        channel: ChannelId,
        key: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<bool, StorageError> {
        let res =
            sqlx::query("DELETE FROM channel_copublishers WHERE channel_id = ? AND pubkey = ?")
                .bind(channel.as_bytes().as_slice())
                .bind(key.as_slice())
                .execute(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(res.rows_affected() > 0)
    }

    async fn is_copublisher(
        &self,
        channel: ChannelId,
        key: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<bool, StorageError> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM channel_copublishers WHERE channel_id = ? AND pubkey = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(key.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.is_some())
    }

    async fn put_listing(
        &self,
        channel: ChannelId,
        listing: StoredListing,
        now_ms: u64,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Lock the channel row: serialises concurrent listings of one channel
        // so the newer-than check and the write are one step.
        match lock_channel(&mut tx, channel).await? {
            None => return Err(StorageError::UnknownChannel),
            Some((true, _)) => return Err(StorageError::ChannelPrivate),
            Some((false, _)) => {},
        }
        let moderation: Option<(i8, i8, i8)> = sqlx::query_as(
            "SELECT blocked, delisted, removed FROM channel_moderation WHERE channel_id = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        match moderation {
            Some((blocked, _, _)) if blocked != 0 => return Err(StorageError::ListingBlocked),
            Some((_, delisted, removed)) if delisted != 0 || removed != 0 => {
                return Err(StorageError::ListingDelisted);
            },
            _ => {},
        }
        let stored: Option<(u64,)> =
            sqlx::query_as("SELECT updated_at_ms FROM channel_listings WHERE channel_id = ?")
                .bind(channel.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if let Some((stored,)) = stored {
            if !lakko_channels::listing::listing_is_newer(listing.updated_at_ms, stored) {
                return Err(StorageError::StaleListing);
            }
        }
        sqlx::query(
            "INSERT INTO channel_listings \
                 (channel_id, topic, language, handle, name, description, listing, updated_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE \
                 topic = VALUES(topic), language = VALUES(language), \
                 handle = VALUES(handle), name = VALUES(name), \
                 description = VALUES(description), listing = VALUES(listing), \
                 updated_at_ms = VALUES(updated_at_ms)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(listing.topic)
        .bind(&listing.language)
        .bind(&listing.handle)
        .bind(&listing.name)
        .bind(&listing.description)
        .bind(&listing.bytes)
        .bind(listing.updated_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "UPDATE channels SET first_listed_at_ms = COALESCE(first_listed_at_ms, ?) \
             WHERE id = ?",
        )
        .bind(now_ms)
        .bind(channel.as_bytes().as_slice())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn withdraw_listing(&self, channel: ChannelId) -> Result<bool, StorageError> {
        let res = sqlx::query("DELETE FROM channel_listings WHERE channel_id = ?")
            .bind(channel.as_bytes().as_slice())
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected() > 0)
    }

    async fn listing(
        &self,
        channel: ChannelId,
        now_ms: u64,
    ) -> Result<Option<DirectoryEntry>, StorageError> {
        let row: Option<DirectoryRow> =
            sqlx::query_as(concat!(directory_select!(), "WHERE l.channel_id = ?"))
                .bind(now_ms.saturating_sub(GROWTH_WINDOW_MS))
                .bind(channel.as_bytes().as_slice())
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        row.map(|r| directory_entry(r, now_ms)).transpose()
    }

    async fn directory_candidates(
        &self,
        query: &DirectoryQuery,
        now_ms: u64,
    ) -> Result<Vec<DirectoryEntry>, StorageError> {
        let since = now_ms.saturating_sub(GROWTH_WINDOW_MS);
        let rows: Vec<DirectoryRow> = match query {
            DirectoryQuery::Bucket { topic, language } => {
                sqlx::query_as(concat!(
                    directory_select!(),
                    "WHERE l.topic = ? AND l.language = ?"
                ))
                .bind(since)
                .bind(*topic)
                .bind(language)
                .fetch_all(&self.pool)
                .await
            },
            DirectoryQuery::Search { needle } => {
                // `LIKE` under the table's case-insensitive collation; the
                // needle arrives lowercased and is escaped here.
                let pattern = like_pattern(needle);
                sqlx::query_as(concat!(
                    directory_select!(),
                    "WHERE l.name LIKE ? ESCAPE '!' \
                       OR l.handle LIKE ? ESCAPE '!' \
                       OR l.description LIKE ? ESCAPE '!'"
                ))
                .bind(since)
                .bind(&pattern)
                .bind(&pattern)
                .bind(&pattern)
                .fetch_all(&self.pool)
                .await
            },
        }
        .map_err(db_err)?;
        rows.into_iter()
            .map(|r| directory_entry(r, now_ms))
            .collect()
    }

    async fn apply_moderation(
        &self,
        statement: &ModerationStatement,
    ) -> Result<ModerationOutcome, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // The stored list version, under its row lock: one statement applies
        // at a time.
        let stored: Option<(u64,)> = sqlx::query_as(
            "SELECT list_version FROM moderation_list_state WHERE id = 1 FOR UPDATE",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if stored.is_some_and(|(v,)| statement.list_version < v) {
            return Ok(ModerationOutcome::StaleList);
        }
        let repeat: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM moderation_applied_statements WHERE statement_id = ? FOR UPDATE",
        )
        .bind(&statement.statement_id[..])
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if repeat.is_some() {
            return Ok(ModerationOutcome::Repeat);
        }
        let channel = statement.channel.as_bytes();
        let row: Option<(i8, i8, i8, Option<u64>, u64)> = sqlx::query_as(
            "SELECT blocked, delisted, removed, log_index, issued_at_ms \
             FROM channel_moderation WHERE channel_id = ? FOR UPDATE",
        )
        .bind(channel.as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        let current = row.map_or_else(ModerationRow::default, |(b, d, r, log_index, at)| {
            ModerationRow {
                blocked: b != 0,
                delisted: d != 0,
                removed: r != 0,
                log_index,
                issued_at_ms: at,
            }
        });
        let outcome = if statement.issued_at_ms < current.issued_at_ms {
            ModerationOutcome::Superseded
        } else {
            let next = current.after(statement.action, statement.log_index);
            sqlx::query(
                "INSERT INTO channel_moderation \
                    (channel_id, blocked, delisted, removed, reason, action_at_ms, log_index, \
                     issued_at_ms) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON DUPLICATE KEY UPDATE blocked = VALUES(blocked), \
                    delisted = VALUES(delisted), removed = VALUES(removed), \
                    reason = VALUES(reason), action_at_ms = VALUES(action_at_ms), \
                    log_index = VALUES(log_index), issued_at_ms = VALUES(issued_at_ms)",
            )
            .bind(channel.as_slice())
            .bind(next.blocked)
            .bind(next.delisted)
            .bind(next.removed)
            .bind(statement.reason)
            .bind(statement.now_ms)
            .bind(statement.log_index)
            .bind(statement.issued_at_ms)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            if statement.action != ChannelModeration::Restore {
                sqlx::query("DELETE FROM channel_listings WHERE channel_id = ?")
                    .bind(channel.as_slice())
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            }
            ModerationOutcome::Changed
        };
        sqlx::query(
            "INSERT INTO moderation_applied_statements (statement_id, applied_at_ms) \
             VALUES (?, ?)",
        )
        .bind(&statement.statement_id[..])
        .bind(statement.now_ms)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO moderation_list_state (id, list_version) VALUES (1, ?) \
             ON DUPLICATE KEY UPDATE list_version = GREATEST(list_version, VALUES(list_version))",
        )
        .bind(statement.list_version)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(outcome)
    }

    async fn removed_channel(&self, channel: ChannelId) -> Result<Option<u64>, StorageError> {
        let row: Option<(Option<u64>,)> = sqlx::query_as(
            "SELECT log_index FROM channel_moderation WHERE channel_id = ? AND removed = 1",
        )
        .bind(channel.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(i,)| i.unwrap_or(0)))
    }

    async fn raise_moderation_list_floor(&self, list_version: u64) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let stored: Option<(u64,)> = sqlx::query_as(
            "SELECT list_version FROM moderation_list_state WHERE id = 1 FOR UPDATE",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        match lakko_moderation::apply::list_floor(stored.map(|(v,)| v), list_version) {
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
                .await
                .map_err(db_err)?;
            },
        }
        tx.commit().await.map_err(db_err)?;
        Ok(true)
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

    async fn unsubscribe(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<(), StorageError> {
        // One transaction under the channel row lock: the subscriber row
        // (its envelopes and member card go with it through the FK cascades),
        // its staged envelopes (no FK to the subscriber, so removed
        // explicitly) and the counter.
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        if lock_channel(&mut tx, channel).await?.is_none() {
            return Err(StorageError::UnknownChannel);
        }
        delete_subscriber_row(&mut tx, channel, subscriber).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn post_exists(
        &self,
        channel: ChannelId,
        post_cursor: u64,
    ) -> Result<bool, StorageError> {
        feedback_sql::has_post(&self.pool, channel, post_cursor).await
    }

    async fn add_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        label: Label,
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        feedback_sql::add_reaction(&self.pool, channel, post_cursor, writer_key, label, now_ms)
            .await
    }

    async fn delete_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> Result<bool, StorageError> {
        feedback_sql::delete_reaction(&self.pool, channel, post_cursor, writer_key, label).await
    }

    async fn reaction_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Result<Vec<PostReactionCounts>, StorageError> {
        feedback_sql::reaction_counts(&self.pool, channel, post_cursors).await
    }

    async fn add_comment(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        sealed: Vec<u8>,
        now_ms: u64,
    ) -> Result<u64, StorageError> {
        feedback_sql::add_comment(&self.pool, channel, post_cursor, writer_key, sealed, now_ms)
            .await
    }

    async fn comment_writer(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> Result<Option<WriterKey>, StorageError> {
        feedback_sql::comment_writer(&self.pool, channel, post_cursor, comment_cursor).await
    }

    async fn delete_comment(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> Result<bool, StorageError> {
        feedback_sql::delete_comment(&self.pool, channel, post_cursor, comment_cursor).await
    }

    async fn list_comments(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        after: u64,
        limit: usize,
    ) -> Result<Vec<StoredComment>, StorageError> {
        feedback_sql::list_comments(&self.pool, channel, post_cursor, after, limit).await
    }

    async fn consume_write_nonce(
        &self,
        nonce: [u8; WRITE_NONCE_LEN],
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        feedback_sql::consume_write_nonce(&self.pool, nonce, now_ms).await
    }

    async fn prune_write_nonces_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
        feedback_sql::prune_write_nonces_older_than(&self.pool, cutoff_ms).await
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

    async fn is_private(&self, channel: ChannelId) -> Result<Option<bool>, StorageError> {
        let row: Option<(i8,)> = sqlx::query_as("SELECT private FROM channels WHERE id = ?")
            .bind(channel.as_bytes().as_slice())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.map(|(p,)| p != 0))
    }

    async fn set_private(&self, channel: ChannelId, private: bool) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let Some((was, _)) = lock_channel(&mut tx, channel).await? else {
            return Err(StorageError::UnknownChannel);
        };
        sqlx::query("UPDATE channels SET private = ? WHERE id = ?")
            .bind(i8::from(private))
            .bind(channel.as_bytes().as_slice())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if private {
            for query in [
                "DELETE FROM channel_listings WHERE channel_id = ?",
                "DELETE FROM channel_public_seeds WHERE channel_id = ?",
            ] {
                sqlx::query(query)
                    .bind(channel.as_bytes().as_slice())
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            }
        }
        tx.commit().await.map_err(db_err)?;
        Ok(was != private)
    }

    async fn put_public_seed(
        &self,
        channel: ChannelId,
        epoch: u32,
        envelope: [u8; PUBLIC_SEED_LEN],
        now_ms: u64,
    ) -> Result<PublicSeedOutcome, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let current = match lock_channel(&mut tx, channel).await? {
            None => return Err(StorageError::UnknownChannel),
            Some((true, _)) => return Err(StorageError::ChannelPrivate),
            Some((false, current)) => current,
        };
        if epoch > current.saturating_add(1) {
            return Err(StorageError::PublicSeedEpochInFuture {
                current,
                got: epoch,
            });
        }
        let stored: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT envelope FROM channel_public_seeds WHERE channel_id = ? AND epoch = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(epoch)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if let Some((stored,)) = stored {
            return if stored.as_slice() == envelope.as_slice() {
                Ok(PublicSeedOutcome::AlreadyStored)
            } else {
                Err(StorageError::PublicSeedConflict)
            };
        }
        // A seed stored after a higher epoch's is superseded at once.
        let higher: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM channel_public_seeds WHERE channel_id = ? AND epoch > ? LIMIT 1",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(epoch)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO channel_public_seeds \
                 (channel_id, epoch, envelope, stored_at_ms, superseded_at_ms) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(epoch)
        .bind(envelope.as_slice())
        .bind(now_ms)
        .bind(higher.map(|_| now_ms))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "UPDATE channel_public_seeds SET superseded_at_ms = ? \
             WHERE channel_id = ? AND epoch < ? AND superseded_at_ms IS NULL",
        )
        .bind(now_ms)
        .bind(channel.as_bytes().as_slice())
        .bind(epoch)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(PublicSeedOutcome::Stored)
    }

    async fn public_seed(
        &self,
        channel: ChannelId,
        epoch: u32,
    ) -> Result<Option<[u8; PUBLIC_SEED_LEN]>, StorageError> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT envelope FROM channel_public_seeds WHERE channel_id = ? AND epoch = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(epoch)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.map(|(b,)| {
            <[u8; PUBLIC_SEED_LEN]>::try_from(b.as_slice())
                .map_err(|_| StorageError::Database("public seed of the wrong length".into()))
        })
        .transpose()
    }

    async fn prune_public_seeds_superseded_before(
        &self,
        cutoff_ms: u64,
    ) -> Result<u64, StorageError> {
        Ok(
            sqlx::query("DELETE FROM channel_public_seeds WHERE superseded_at_ms < ?")
                .bind(cutoff_ms)
                .execute(&self.pool)
                .await
                .map_err(db_err)?
                .rows_affected(),
        )
    }

    async fn put_invite_key(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
        expiry_ms: u64,
        max_uses: u32,
        max_keys: usize,
        now_ms: u64,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        if lock_channel(&mut tx, channel).await?.is_none() {
            return Err(StorageError::UnknownChannel);
        }
        let updated = sqlx::query(
            "UPDATE channel_invite_keys SET expiry_ms = ?, max_uses = ? \
             WHERE channel_id = ? AND invite_pk = ?",
        )
        .bind(expiry_ms)
        .bind(max_uses)
        .bind(channel.as_bytes().as_slice())
        .bind(invite_pk.as_slice())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        let exists: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM channel_invite_keys WHERE channel_id = ? AND invite_pk = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(invite_pk.as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if updated.rows_affected() == 0 && exists.is_none() {
            let (count,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM channel_invite_keys WHERE channel_id = ?")
                    .bind(channel.as_bytes().as_slice())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_err)?;
            if usize::try_from(count).unwrap_or(usize::MAX) >= max_keys {
                return Err(StorageError::TooManyInviteKeys);
            }
            sqlx::query(
                "INSERT INTO channel_invite_keys \
                     (channel_id, invite_pk, expiry_ms, max_uses, used_count, registered_at_ms) \
                 VALUES (?, ?, ?, ?, 0, ?)",
            )
            .bind(channel.as_bytes().as_slice())
            .bind(invite_pk.as_slice())
            .bind(expiry_ms)
            .bind(max_uses)
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn delete_invite_key(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
    ) -> Result<bool, StorageError> {
        Ok(
            sqlx::query("DELETE FROM channel_invite_keys WHERE channel_id = ? AND invite_pk = ?")
                .bind(channel.as_bytes().as_slice())
                .bind(invite_pk.as_slice())
                .execute(&self.pool)
                .await
                .map_err(db_err)?
                .rows_affected()
                > 0,
        )
    }

    async fn invite_key(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
    ) -> Result<Option<InviteKeyRow>, StorageError> {
        let row: Option<(Vec<u8>, u64, u32, u32)> = sqlx::query_as(
            "SELECT invite_pk, expiry_ms, max_uses, used_count FROM channel_invite_keys \
             WHERE channel_id = ? AND invite_pk = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(invite_pk.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.map(invite_row).transpose()
    }

    async fn list_invite_keys(
        &self,
        channel: ChannelId,
    ) -> Result<Vec<InviteKeyRow>, StorageError> {
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        let rows: Vec<(Vec<u8>, u64, u32, u32)> = sqlx::query_as(
            "SELECT invite_pk, expiry_ms, max_uses, used_count FROM channel_invite_keys \
             WHERE channel_id = ? ORDER BY invite_pk ASC",
        )
        .bind(channel.as_bytes().as_slice())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.into_iter().map(invite_row).collect()
    }

    async fn claim_invite_use(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        // One conditional UPDATE: live (unexpired) and below its ceiling
        // (`0` is one use, `u32::MAX` never runs out), so two concurrent
        // subscribes cannot both take a key's last use.
        let n = sqlx::query(
            "UPDATE channel_invite_keys SET used_count = used_count + 1 \
             WHERE channel_id = ? AND invite_pk = ? \
               AND (expiry_ms = 0 OR expiry_ms > ?) \
               AND used_count < CASE WHEN max_uses = 0 THEN 1 ELSE max_uses END",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(invite_pk.as_slice())
        .bind(now_ms)
        .execute(&self.pool)
        .await
        .map_err(db_err)?
        .rows_affected();
        Ok(n == 1)
    }

    async fn release_invite_use(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE channel_invite_keys SET used_count = used_count - 1 \
             WHERE channel_id = ? AND invite_pk = ? AND used_count > 0",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(invite_pk.as_slice())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn prune_invite_keys_expired_before(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
        Ok(
            sqlx::query("DELETE FROM channel_invite_keys WHERE expiry_ms <> 0 AND expiry_ms < ?")
                .bind(cutoff_ms)
                .execute(&self.pool)
                .await
                .map_err(db_err)?
                .rows_affected(),
        )
    }

    async fn list_subscriber_page(
        &self,
        channel: ChannelId,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<SubscriberEntry>, StorageError> {
        if !self.exists(channel).await {
            return Err(StorageError::UnknownChannel);
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows: Vec<(u64, Vec<u8>, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT seq, subscriber_id, sealed_ticket FROM channel_subscribers \
             WHERE channel_id = ? AND seq > ? ORDER BY seq ASC LIMIT ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(after_seq)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.into_iter()
            .map(|(seq, sub, ticket)| {
                Ok(SubscriberEntry {
                    seq,
                    subscriber: SubscriberId::from_bytes(&sub)?,
                    sealed_ticket: ticket_from(ticket)?,
                })
            })
            .collect()
    }

    async fn subscriber_count(&self, channel: ChannelId) -> Result<u64, StorageError> {
        let row: Option<(u64,)> =
            sqlx::query_as("SELECT subscriber_count FROM channels WHERE id = ?")
                .bind(channel.as_bytes().as_slice())
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        row.map(|(n,)| n).ok_or(StorageError::UnknownChannel)
    }

    async fn remove_subscriber(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        match lock_channel(&mut tx, channel).await? {
            None => return Err(StorageError::UnknownChannel),
            Some((false, _)) => return Err(StorageError::ChannelPublic),
            Some((true, _)) => {},
        }
        let removed = delete_subscriber_row(&mut tx, channel, subscriber).await?;
        sqlx::query(
            "INSERT INTO channel_removed_subscribers (channel_id, subscriber_id, removed_at_ms) \
             VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE removed_at_ms = VALUES(removed_at_ms)",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .bind(now_ms)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(removed)
    }

    async fn is_removed(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<bool, StorageError> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM channel_removed_subscribers WHERE channel_id = ? AND subscriber_id = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(subscriber.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.is_some())
    }

    async fn comment_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Result<Vec<(u64, u64)>, StorageError> {
        feedback_sql::comment_counts(&self.pool, channel, post_cursors).await
    }

    async fn add_comment_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
        writer_key: WriterKey,
        label: Label,
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        feedback_sql::add_comment_reaction(
            &self.pool,
            channel,
            post_cursor,
            comment_cursor,
            writer_key,
            label,
            now_ms,
        )
        .await
    }

    async fn delete_comment_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> Result<bool, StorageError> {
        feedback_sql::delete_comment_reaction(
            &self.pool,
            channel,
            post_cursor,
            comment_cursor,
            writer_key,
            label,
        )
        .await
    }

    async fn comment_reaction_counts(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursors: &[u64],
    ) -> Result<Vec<CommentReactionCounts>, StorageError> {
        feedback_sql::comment_reaction_counts(&self.pool, channel, post_cursor, comment_cursors)
            .await
    }

    async fn delete_channel(&self, channel: ChannelId, now_ms: u64) -> Result<bool, StorageError> {
        // The tombstone and the delete in one transaction: no moment where the
        // id is free and unmarked. Everything else cascades from `channels`
        // (one statement, proportional to what the channel held).
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query("INSERT IGNORE INTO channel_deleted (channel_id, deleted_at_ms) VALUES (?, ?)")
            .bind(channel.as_bytes().as_slice())
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        let res = sqlx::query("DELETE FROM channels WHERE id = ?")
            .bind(channel.as_bytes().as_slice())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(res.rows_affected() > 0)
    }

    async fn is_deleted(&self, channel: ChannelId) -> Result<bool, StorageError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM channel_deleted WHERE channel_id = ?")
                .bind(channel.as_bytes().as_slice())
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(row.is_some())
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
        // Parallel tests share one database and two concurrent migrator runs
        // on an empty one collide, so migrate once per process.
        MIGRATED
            .get_or_init(|| async {
                sqlx::migrate!("./migrations")
                    .run(&pool)
                    .await
                    .expect("migrate");
            })
            .await;
        Some(SqlChannelStore::new(pool))
    }

    static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

    /// The server clock the store is handed: since ADR-0059 the SQL store
    /// files the subscriber's `registered_at` / `last_active_at` from it, and
    /// MariaDB refuses the epoch itself as a `TIMESTAMP`.
    fn now_ms() -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap()
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
        s.create(channel, [7; 32]).await.unwrap();
        let mut subs: Vec<SubscriberId> = (0..3)
            .map(|_| SubscriberId::from_bytes(&random::<32>()).unwrap())
            .collect();
        subs.sort_unstable();
        for sub in &subs {
            s.subscribe(channel, *sub, now_ms()).await.unwrap();
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

    #[tokio::test]
    async fn unsubscribe_takes_the_row_envelopes_card_and_staged_rows() {
        let Some(s) = store().await else {
            return;
        };
        let channel = ChannelId::from_bytes(&random::<16>()).unwrap();
        s.create(channel, [7; 32]).await.unwrap();
        let sub = SubscriberId::from_bytes(&random::<32>()).unwrap();
        let keep = SubscriberId::from_bytes(&random::<32>()).unwrap();
        for id in [sub, keep] {
            s.subscribe(channel, id, now_ms()).await.unwrap();
            s.deliver_envelope(channel, id, None, vec![7; 80], 80)
                .await
                .unwrap();
            s.put_member_card(channel, id, 0, vec![1; 40])
                .await
                .unwrap();
        }
        s.stage_envelopes(
            channel,
            1,
            vec![(sub, vec![8; 80]), (keep, vec![8; 80])],
            80,
        )
        .await
        .unwrap();

        s.unsubscribe(channel, sub).await.unwrap();
        s.unsubscribe(channel, sub).await.unwrap();
        assert_eq!(s.list_subscribers(channel).await.unwrap(), vec![keep]);
        assert_eq!(
            s.list_member_cards(channel, None, 10)
                .await
                .unwrap()
                .into_iter()
                .map(|(k, _)| k)
                .collect::<Vec<_>>(),
            vec![keep]
        );
        let (envelopes,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM channel_envelopes WHERE channel_id = ? AND subscriber_id = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(sub.as_bytes().as_slice())
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(envelopes, 0);
        let (staged,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM channel_staged_envelopes WHERE channel_id = ?")
                .bind(channel.as_bytes().as_slice())
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(staged, 1, "only the remaining subscriber's staged row");
        assert!(matches!(
            s.unsubscribe(ChannelId::from_bytes(&random::<16>()).unwrap(), sub)
                .await,
            Err(StorageError::UnknownChannel)
        ));
    }

    #[test]
    fn the_member_card_column_fits_the_longest_card() {
        let migration = include_str!("../migrations/0006_member_card_binding_width.sql");
        assert!(migration.contains(&format!(
            "VARBINARY({})",
            lakko_channels::MEMBER_CARD_MAX_LEN
        )));
    }
}
