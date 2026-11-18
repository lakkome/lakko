// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The MariaDB half of channel poll votes ([ADR-0090] section 4.3;
//! migration `0013_poll_votes.sql`). Called from
//! [`crate::SqlChannelStore`]'s `ChannelStore` impl.
//!
//! One transaction per request: lock the tag's row (created as a tombstone
//! at `at_ms = 0` when absent), refuse a request that is not newer, adjust
//! the per-label counters and the voter count in ascending label order (so
//! two transactions on one post take the counter locks in one order), then
//! write the row.
//!
//! [ADR-0090]: ../../../docs/adr/0090-attachment-kinds.md

use std::collections::BTreeMap;

use sqlx::{MySqlPool, Row};

use crate::feedback::{top_labels, Label, MAX_LABELS_PER_POST, REACTION_LABEL_LEN};
use crate::storage::{ChannelId, StorageError};
use crate::votes::{PostVoteCounts, Tag, VoteOutcome};

fn db_err(e: sqlx::Error) -> StorageError {
    StorageError::Database(e.to_string())
}

fn parse_labels(bytes: &[u8]) -> Result<Vec<Label>, StorageError> {
    if bytes.len() % REACTION_LABEL_LEN != 0 {
        return Err(StorageError::Database(
            "stored vote labels are not a multiple of 16 bytes".into(),
        ));
    }
    Ok(bytes
        .chunks_exact(REACTION_LABEL_LEN)
        .map(|c| {
            let mut l = [0u8; REACTION_LABEL_LEN];
            l.copy_from_slice(c);
            l
        })
        .collect())
}

/// Move the per-label counters from `old` to `new` in ascending label order,
/// and the voter count when the row gains or loses its labels.
async fn adjust_counters(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    ch: &[u8],
    post_cursor: u64,
    old: &[Label],
    labels: &[Label],
) -> Result<(), StorageError> {
    // Every counter change, in ascending label order.
    let mut deltas: BTreeMap<Label, i64> = BTreeMap::new();
    for l in old {
        *deltas.entry(*l).or_default() -= 1;
    }
    for l in labels {
        *deltas.entry(*l).or_default() += 1;
    }
    for (label, delta) in deltas {
        if delta > 0 {
            sqlx::query(
                "INSERT INTO channel_poll_vote_counts (channel_id, post_cursor, label, votes) \
                 VALUES (?, ?, ?, 1) ON DUPLICATE KEY UPDATE votes = votes + 1",
            )
            .bind(ch)
            .bind(post_cursor)
            .bind(label.as_slice())
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
        } else if delta < 0 {
            sqlx::query(
                "UPDATE channel_poll_vote_counts SET votes = votes - 1 \
                 WHERE channel_id = ? AND post_cursor = ? AND label = ? AND votes > 0",
            )
            .bind(ch)
            .bind(post_cursor)
            .bind(label.as_slice())
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
        }
    }

    let had = !old.is_empty();
    let has = !labels.is_empty();
    if has && !had {
        sqlx::query(
            "INSERT INTO channel_poll_voter_counts (channel_id, post_cursor, voters) \
             VALUES (?, ?, 1) ON DUPLICATE KEY UPDATE voters = voters + 1",
        )
        .bind(ch)
        .bind(post_cursor)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    } else if had && !has {
        sqlx::query(
            "UPDATE channel_poll_voter_counts SET voters = voters - 1 \
             WHERE channel_id = ? AND post_cursor = ? AND voters > 0",
        )
        .bind(ch)
        .bind(post_cursor)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    }

    Ok(())
}

pub(crate) async fn apply_vote(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursor: u64,
    tag: Tag,
    labels: &[Label],
    at_ms: u64,
) -> Result<VoteOutcome, StorageError> {
    let ch = channel.as_bytes().as_slice();
    let mut tx = pool.begin().await.map_err(db_err)?;
    // A missing parent post turns this into a warning and no row under
    // MariaDB's INSERT IGNORE; the SELECT below then finds nothing.
    sqlx::query(
        "INSERT IGNORE INTO channel_poll_votes (channel_id, post_cursor, tag, labels, at_ms) \
         VALUES (?, ?, ?, '', 0)",
    )
    .bind(ch)
    .bind(post_cursor)
    .bind(tag.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;
    let row = sqlx::query(
        "SELECT labels, at_ms FROM channel_poll_votes \
         WHERE channel_id = ? AND post_cursor = ? AND tag = ? FOR UPDATE",
    )
    .bind(ch)
    .bind(post_cursor)
    .bind(tag.as_slice())
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_err)?;
    let Some(row) = row else {
        return Err(StorageError::UnknownPost);
    };
    let stored_labels: Vec<u8> = row.get("labels");
    let stored_at_ms: u64 = row.get("at_ms");
    if at_ms <= stored_at_ms {
        return Ok(VoteOutcome::Stale { stored_at_ms });
    }
    let old = parse_labels(&stored_labels)?;

    adjust_counters(&mut tx, ch, post_cursor, &old, labels).await?;
    let had = !old.is_empty();
    let has = !labels.is_empty();

    let mut packed = Vec::with_capacity(labels.len() * REACTION_LABEL_LEN);
    for l in labels {
        packed.extend_from_slice(l);
    }
    sqlx::query(
        "UPDATE channel_poll_votes SET labels = ?, at_ms = ? \
         WHERE channel_id = ? AND post_cursor = ? AND tag = ?",
    )
    .bind(packed)
    .bind(at_ms)
    .bind(ch)
    .bind(post_cursor)
    .bind(tag.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;
    tx.commit().await.map_err(db_err)?;
    Ok(if has {
        VoteOutcome::Stored { replaced: had }
    } else {
        VoteOutcome::Retracted { existed: had }
    })
}

pub(crate) async fn vote_counts(
    pool: &MySqlPool,
    channel: ChannelId,
    post_cursors: &[u64],
) -> Result<Vec<PostVoteCounts>, StorageError> {
    let ch = channel.as_bytes().as_slice();
    let limit = i64::try_from(MAX_LABELS_PER_POST).unwrap_or(64);
    let mut out = Vec::with_capacity(post_cursors.len());
    for &post_cursor in post_cursors {
        let voters: Option<u64> = sqlx::query_scalar(
            "SELECT voters FROM channel_poll_voter_counts WHERE channel_id = ? AND post_cursor = ?",
        )
        .bind(ch)
        .bind(post_cursor)
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;
        let rows = sqlx::query(
            "SELECT label, votes FROM channel_poll_vote_counts \
             WHERE channel_id = ? AND post_cursor = ? AND votes > 0 \
             ORDER BY votes DESC, label ASC LIMIT ?",
        )
        .bind(ch)
        .bind(post_cursor)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
        let mut labels = Vec::with_capacity(rows.len());
        for r in rows {
            let label: Vec<u8> = r.get("label");
            let n: u64 = r.get("votes");
            let label = <[u8; REACTION_LABEL_LEN]>::try_from(label.as_slice())
                .map_err(|_| StorageError::Database("stored vote label is not 16 bytes".into()))?;
            labels.push((label, n));
        }
        out.push(PostVoteCounts {
            post_cursor,
            voters: voters.unwrap_or(0),
            labels: top_labels(labels),
        });
    }
    Ok(out)
}
