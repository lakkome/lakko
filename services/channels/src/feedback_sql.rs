// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The MariaDB half of the comments and reactions store ([ADR-0087] section
//! 13a; migration `0011_comments_and_reactions.sql`). Called from
//! [`crate::SqlChannelStore`]'s `ChannelStore` impl.
//!
//! Both row tables cascade from `channel_posts`, so a pruned post takes its
//! rows with it and no separate pruner is needed for them.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use sqlx::{MySqlPool, Row};

use crate::feedback::{
    top_labels, CommentReactionCounts, Label, PostReactionCounts, StoredComment, WriterKey,
    MAX_LABELS_PER_POST, WRITE_NONCE_LEN,
};
use crate::storage::{ChannelId, StorageError};

fn db_err(e: sqlx::Error) -> StorageError {
    StorageError::Database(e.to_string())
}

fn fixed<const N: usize>(bytes: &[u8]) -> Result<[u8; N], StorageError> {
    bytes
        .try_into()
        .map_err(|_| StorageError::Database(format!("stored value is not {N} bytes")))
}

pub(crate) async fn has_post(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
) -> Result<bool, StorageError> {
    let row: Option<(u64,)> =
        sqlx::query_as("SELECT `cursor` FROM channel_posts WHERE channel_id = ? AND `cursor` = ?")
            .bind(channel.as_bytes().as_slice())
            .bind(post_cursor)
            .fetch_optional(pool)
            .await
            .map_err(db_err)?;
    Ok(row.is_some())
}

/// A foreign-key failure on insert: the post was pruned between the lookup
/// and the insert.
fn is_fk_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.is_foreign_key_violation())
}

pub(crate) async fn add_reaction(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    writer_key: WriterKey,
    label: Label,
    now_ms: u64,
) -> Result<bool, StorageError> {
    if !has_post(pool, channel, post_cursor).await? {
        return Err(StorageError::UnknownPost);
    }
    let res = sqlx::query(
        "INSERT IGNORE INTO channel_reactions \
         (channel_id, post_cursor, writer_key, label, created_at_ms) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(channel.as_bytes().as_slice())
    .bind(post_cursor)
    .bind(writer_key.as_slice())
    .bind(label.as_slice())
    .bind(now_ms)
    .execute(pool)
    .await;
    match res {
        Ok(r) if r.rows_affected() == 1 => Ok(true),
        // MariaDB's INSERT IGNORE turns a missing parent into a warning and
        // inserts nothing; check which of the two "nothing" cases this is.
        Ok(_) => {
            if has_post(pool, channel, post_cursor).await? {
                Ok(false)
            } else {
                Err(StorageError::UnknownPost)
            }
        },
        Err(e) if is_fk_violation(&e) => Err(StorageError::UnknownPost),
        Err(e) => Err(db_err(e)),
    }
}

pub(crate) async fn delete_reaction(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    writer_key: WriterKey,
    label: Label,
) -> Result<bool, StorageError> {
    let res = sqlx::query(
        "DELETE FROM channel_reactions \
         WHERE channel_id = ? AND post_cursor = ? AND writer_key = ? AND label = ?",
    )
    .bind(channel.as_bytes().as_slice())
    .bind(post_cursor)
    .bind(writer_key.as_slice())
    .bind(label.as_slice())
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(res.rows_affected() > 0)
}

pub(crate) async fn reaction_counts(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursors: &[u64],
) -> Result<Vec<PostReactionCounts>, StorageError> {
    let mut out = Vec::with_capacity(post_cursors.len());
    let limit = i64::try_from(MAX_LABELS_PER_POST).unwrap_or(64);
    for &post_cursor in post_cursors {
        // One indexed query per post (idx_channel_reactions_count); the
        // caller bounds how many posts one read names.
        let rows = sqlx::query(
            "SELECT label, COUNT(*) AS n FROM channel_reactions \
             WHERE channel_id = ? AND post_cursor = ? \
             GROUP BY label ORDER BY n DESC, label ASC LIMIT ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(post_cursor)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
        let mut labels = Vec::with_capacity(rows.len());
        for r in rows {
            let label: Vec<u8> = r.get("label");
            let n: i64 = r.get("n");
            labels.push((fixed(&label)?, u64::try_from(n).unwrap_or(0)));
        }
        out.push(PostReactionCounts {
            post_cursor,
            labels: top_labels(labels),
        });
    }
    Ok(out)
}

pub(crate) async fn add_comment(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    writer_key: WriterKey,
    sealed: Vec<u8>,
    now_ms: u64,
) -> Result<u64, StorageError> {
    if !has_post(pool, channel, post_cursor).await? {
        return Err(StorageError::UnknownPost);
    }
    // The row and the post's maintained count move together (migration
    // 0012), so the count a reader gets never runs ahead of the rows.
    let mut tx = pool.begin().await.map_err(db_err)?;
    let res = sqlx::query(
        "INSERT INTO channel_comments \
         (channel_id, post_cursor, writer_key, sealed, created_at_ms) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(channel.as_bytes().as_slice())
    .bind(post_cursor)
    .bind(writer_key.as_slice())
    .bind(sealed)
    .bind(now_ms)
    .execute(&mut *tx)
    .await;
    let id = match res {
        Ok(r) => r.last_insert_id(),
        Err(e) if is_fk_violation(&e) => return Err(StorageError::UnknownPost),
        Err(e) => return Err(db_err(e)),
    };
    let counted = sqlx::query(
        "INSERT INTO channel_comment_counts (channel_id, post_cursor, comments) VALUES (?, ?, 1) \
         ON DUPLICATE KEY UPDATE comments = comments + 1",
    )
    .bind(channel.as_bytes().as_slice())
    .bind(post_cursor)
    .execute(&mut *tx)
    .await;
    match counted {
        Ok(_) => {},
        Err(e) if is_fk_violation(&e) => return Err(StorageError::UnknownPost),
        Err(e) => return Err(db_err(e)),
    }
    tx.commit().await.map_err(db_err)?;
    Ok(id)
}

pub(crate) async fn comment_writer(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    comment_cursor: u64,
) -> Result<Option<WriterKey>, StorageError> {
    let row: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT writer_key FROM channel_comments \
         WHERE id = ? AND channel_id = ? AND post_cursor = ?",
    )
    .bind(comment_cursor)
    .bind(channel.as_bytes().as_slice())
    .bind(post_cursor)
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;
    row.map(|(k,)| fixed(&k)).transpose()
}

pub(crate) async fn delete_comment(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    comment_cursor: u64,
) -> Result<bool, StorageError> {
    let mut tx = pool.begin().await.map_err(db_err)?;
    let res = sqlx::query(
        "DELETE FROM channel_comments WHERE id = ? AND channel_id = ? AND post_cursor = ?",
    )
    .bind(comment_cursor)
    .bind(channel.as_bytes().as_slice())
    .bind(post_cursor)
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;
    let removed = res.rows_affected() > 0;
    if removed {
        sqlx::query(
            "UPDATE channel_comment_counts SET comments = comments - 1 \
             WHERE channel_id = ? AND post_cursor = ? AND comments > 0",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(post_cursor)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
    }
    tx.commit().await.map_err(db_err)?;
    Ok(removed)
}

pub(crate) async fn comment_counts(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursors: &[u64],
) -> Result<Vec<(u64, u64)>, StorageError> {
    let mut out = Vec::with_capacity(post_cursors.len());
    for &post_cursor in post_cursors {
        // One primary-key lookup per post; the caller bounds how many.
        let row: Option<(u64,)> = sqlx::query_as(
            "SELECT comments FROM channel_comment_counts WHERE channel_id = ? AND post_cursor = ?",
        )
        .bind(channel.as_bytes().as_slice())
        .bind(post_cursor)
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;
        out.push((post_cursor, row.map_or(0, |(n,)| n)));
    }
    Ok(out)
}

pub(crate) async fn add_comment_reaction(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    comment_cursor: u64,
    writer_key: WriterKey,
    label: Label,
    now_ms: u64,
) -> Result<bool, StorageError> {
    if comment_writer(pool, channel, post_cursor, comment_cursor)
        .await?
        .is_none()
    {
        return Err(StorageError::UnknownComment);
    }
    let res = sqlx::query(
        "INSERT IGNORE INTO channel_comment_reactions \
         (comment_id, writer_key, label, created_at_ms) VALUES (?, ?, ?, ?)",
    )
    .bind(comment_cursor)
    .bind(writer_key.as_slice())
    .bind(label.as_slice())
    .bind(now_ms)
    .execute(pool)
    .await;
    match res {
        Ok(r) if r.rows_affected() == 1 => Ok(true),
        // As for a post: INSERT IGNORE turns a missing parent into a warning.
        Ok(_) => {
            if comment_writer(pool, channel, post_cursor, comment_cursor)
                .await?
                .is_some()
            {
                Ok(false)
            } else {
                Err(StorageError::UnknownComment)
            }
        },
        Err(e) if is_fk_violation(&e) => Err(StorageError::UnknownComment),
        Err(e) => Err(db_err(e)),
    }
}

pub(crate) async fn delete_comment_reaction(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    comment_cursor: u64,
    writer_key: WriterKey,
    label: Label,
) -> Result<bool, StorageError> {
    // The comment id is global; the join keeps a request naming another
    // channel's comment from touching its rows.
    let res = sqlx::query(
        "DELETE r FROM channel_comment_reactions r \
         JOIN channel_comments c ON c.id = r.comment_id \
         WHERE r.comment_id = ? AND r.writer_key = ? AND r.label = ? \
           AND c.channel_id = ? AND c.post_cursor = ?",
    )
    .bind(comment_cursor)
    .bind(writer_key.as_slice())
    .bind(label.as_slice())
    .bind(channel.as_bytes().as_slice())
    .bind(post_cursor)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(res.rows_affected() > 0)
}

pub(crate) async fn comment_reaction_counts(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    comment_cursors: &[u64],
) -> Result<Vec<CommentReactionCounts>, StorageError> {
    let mut out = Vec::with_capacity(comment_cursors.len());
    let limit = i64::try_from(MAX_LABELS_PER_POST).unwrap_or(64);
    for &comment_cursor in comment_cursors {
        let rows = sqlx::query(
            "SELECT r.label, COUNT(*) AS n FROM channel_comment_reactions r \
             JOIN channel_comments c ON c.id = r.comment_id \
             WHERE r.comment_id = ? AND c.channel_id = ? AND c.post_cursor = ? \
             GROUP BY r.label ORDER BY n DESC, r.label ASC LIMIT ?",
        )
        .bind(comment_cursor)
        .bind(channel.as_bytes().as_slice())
        .bind(post_cursor)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
        let mut labels = Vec::with_capacity(rows.len());
        for r in rows {
            let label: Vec<u8> = r.get("label");
            let n: i64 = r.get("n");
            labels.push((fixed(&label)?, u64::try_from(n).unwrap_or(0)));
        }
        out.push(CommentReactionCounts {
            comment_cursor,
            labels: top_labels(labels),
        });
    }
    Ok(out)
}

pub(crate) async fn list_comments(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    after: u64,
    limit: usize,
) -> Result<Vec<StoredComment>, StorageError> {
    let rows = sqlx::query(
        "SELECT id, writer_key, sealed, created_at_ms FROM channel_comments \
         WHERE channel_id = ? AND post_cursor = ? AND id > ? ORDER BY id ASC LIMIT ?",
    )
    .bind(channel.as_bytes().as_slice())
    .bind(post_cursor)
    .bind(after)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(db_err)?;
    rows.into_iter()
        .map(|r| {
            let key: Vec<u8> = r.get("writer_key");
            Ok(StoredComment {
                cursor: r.get::<u64, _>("id"),
                writer_key: fixed(&key)?,
                sealed: r.get::<Vec<u8>, _>("sealed"),
                server_ts_ms: r.get::<u64, _>("created_at_ms"),
            })
        })
        .collect()
}

pub(crate) async fn consume_write_nonce(
    pool: &MySqlPool,
    nonce: [u8; WRITE_NONCE_LEN],
    now_ms: u64,
) -> Result<bool, StorageError> {
    // One INSERT on the primary key: of two concurrent uses exactly one wins.
    let res = sqlx::query("INSERT INTO channel_write_nonces (nonce, seen_at_ms) VALUES (?, ?)")
        .bind(nonce.as_slice())
        .bind(now_ms)
        .execute(pool)
        .await;
    match res {
        Ok(_) => Ok(true),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(false),
        Err(e) => Err(db_err(e)),
    }
}

pub(crate) async fn prune_write_nonces_older_than(
    pool: &MySqlPool,
    cutoff_ms: u64,
) -> Result<u64, StorageError> {
    let res = sqlx::query("DELETE FROM channel_write_nonces WHERE seen_at_ms < ?")
        .bind(cutoff_ms)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(res.rows_affected())
}
