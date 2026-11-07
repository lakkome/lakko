// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The comments and reactions store (ADR-0087 section 13a), the same
//! scenario against both `ChannelStore` impls: the in-memory store always,
//! and `SqlChannelStore` against a real MariaDB when
//! `LAKKO_CHANNELS_TEST_DATABASE_URL` is set (else that half skips and says
//! so on stderr). The database persists across runs, so every run works on
//! fresh random channel ids.

use lakko_svc_channels::{
    ChannelId, ChannelStore, InMemoryChannelStore, SqlChannelStore, StorageError, MIGRATIONS,
};
use sqlx::mysql::MySqlPoolOptions;

const DAY_MS: u64 = 24 * 3600 * 1000;

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    lakko_crypto::random::fill(&mut out);
    out
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

async fn sql_store() -> Option<SqlChannelStore> {
    let Ok(url) = std::env::var("LAKKO_CHANNELS_TEST_DATABASE_URL") else {
        eprintln!("skipping the SQL half: LAKKO_CHANNELS_TEST_DATABASE_URL unset");
        return None;
    };
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect test DB");
    MIGRATIONS.run(&pool).await.expect("run migrations");
    eprintln!("running the SQL half against the test database");
    Some(SqlChannelStore::new(pool))
}

/// One channel with two posts: cursor 1 stored now, cursor 2 stored 40 days
/// ago (so a prune at 30 days removes it).
async fn channel_with_posts(s: &dyn ChannelStore) -> ChannelId {
    let c = ChannelId::from_bytes(&random::<16>()).unwrap();
    s.create(c, random::<32>()).await.unwrap();
    let fresh = s.put(c, vec![1; 8], 1024, now_ms()).await.unwrap();
    assert_eq!(fresh.cursor, 1);
    let old = s
        .put(c, vec![2; 8], 1024, now_ms() - 40 * DAY_MS)
        .await
        .unwrap();
    assert_eq!(old.cursor, 2);
    c
}

async fn exercise(s: &dyn ChannelStore) {
    let c = channel_with_posts(s).await;
    let now = now_ms();
    let (k1, k2, k3) = (random::<32>(), random::<32>(), random::<32>());
    let (thumbs, heart) = ([1u8; 16], [2u8; 16]);

    assert!(s.post_exists(c, 1).await.unwrap());
    assert!(s.post_exists(c, 2).await.unwrap());
    assert!(!s.post_exists(c, 99).await.unwrap());

    // Reactions: a row per (post, key, label); a repeat is not a new row.
    assert!(s.add_reaction(c, 1, k1, thumbs, now).await.unwrap());
    assert!(!s.add_reaction(c, 1, k1, thumbs, now).await.unwrap());
    assert!(s.add_reaction(c, 1, k2, thumbs, now).await.unwrap());
    assert!(s.add_reaction(c, 1, k3, heart, now).await.unwrap());
    assert!(s.add_reaction(c, 2, k3, heart, now).await.unwrap());
    assert!(matches!(
        s.add_reaction(c, 99, k1, thumbs, now).await,
        Err(StorageError::UnknownPost)
    ));
    let counts = s.reaction_counts(c, &[1, 2, 99]).await.unwrap();
    assert_eq!(counts.len(), 3);
    assert_eq!(counts[0].post_cursor, 1);
    assert_eq!(counts[0].labels, vec![(thumbs, 2), (heart, 1)]);
    assert_eq!(counts[1].labels, vec![(heart, 1)]);
    assert!(counts[2].labels.is_empty());
    assert!(s.delete_reaction(c, 1, k2, thumbs).await.unwrap());
    assert!(!s.delete_reaction(c, 1, k2, thumbs).await.unwrap());
    // A key deletes only its own row.
    assert!(!s.delete_reaction(c, 1, k2, heart).await.unwrap());
    let counts = s.reaction_counts(c, &[1]).await.unwrap();
    // An equal count is ordered by label.
    assert_eq!(counts[0].labels, vec![(thumbs, 1), (heart, 1)]);

    // At most 64 labels, the most counted first.
    for i in 0u8..70 {
        let mut label = [0xA0; 16];
        label[15] = i;
        s.add_reaction(c, 1, random::<32>(), label, now)
            .await
            .unwrap();
    }
    for _ in 0..3 {
        s.add_reaction(c, 1, random::<32>(), heart, now)
            .await
            .unwrap();
    }
    let counts = s.reaction_counts(c, &[1]).await.unwrap();
    assert_eq!(counts[0].labels.len(), 64);
    assert_eq!(counts[0].labels[0], (heart, 4));

    // Comments: ascending cursors, pages after a cursor, the row's key.
    let mut cursors = Vec::new();
    for i in 0u8..25 {
        let key = [i; 32];
        cursors.push(s.add_comment(c, 1, key, vec![i; 200], now).await.unwrap());
    }
    assert!(cursors.windows(2).all(|w| w[0] < w[1]));
    assert!(matches!(
        s.add_comment(c, 99, k1, vec![1; 200], now).await,
        Err(StorageError::UnknownPost)
    ));
    let page = s.list_comments(c, 1, 0, 20).await.unwrap();
    assert_eq!(page.len(), 20);
    assert_eq!(page[0].cursor, cursors[0]);
    assert_eq!(page[0].writer_key, [0; 32]);
    assert_eq!(page[0].sealed, vec![0; 200]);
    let rest = s.list_comments(c, 1, page[19].cursor, 20).await.unwrap();
    assert_eq!(rest.len(), 5);
    assert_eq!(
        s.comment_writer(c, 1, cursors[3]).await.unwrap(),
        Some([3; 32])
    );
    // Another post does not name it.
    assert_eq!(s.comment_writer(c, 2, cursors[3]).await.unwrap(), None);
    assert!(!s.delete_comment(c, 2, cursors[3]).await.unwrap());
    assert!(s.delete_comment(c, 1, cursors[3]).await.unwrap());
    assert!(!s.delete_comment(c, 1, cursors[3]).await.unwrap());
    assert_eq!(s.comment_writer(c, 1, cursors[3]).await.unwrap(), None);
    let old_comment = s.add_comment(c, 2, k1, vec![9; 200], now).await.unwrap();

    // Nonces: once each.
    let n = random::<16>();
    assert!(s.consume_write_nonce(n, now).await.unwrap());
    assert!(!s.consume_write_nonce(n, now).await.unwrap());
    assert!(s.prune_write_nonces_older_than(now + 1).await.unwrap() >= 1);
    assert!(s.consume_write_nonce(n, now).await.unwrap());

    // Pruning the old post takes its rows with it; the fresh post keeps its.
    s.prune_posts_older_than(now - 30 * DAY_MS).await.unwrap();
    assert!(s.reaction_counts(c, &[2]).await.unwrap()[0]
        .labels
        .is_empty());
    assert_eq!(s.comment_writer(c, 2, old_comment).await.unwrap(), None);
    assert!(!s.post_exists(c, 2).await.unwrap());
    assert_eq!(s.list_comments(c, 1, 0, 20).await.unwrap().len(), 20);
    assert!(!s.reaction_counts(c, &[1]).await.unwrap()[0]
        .labels
        .is_empty());
}

/// 2026-11-05: comment counts, reactions on comments, and deleting a
/// channel, the same scenario against both stores.
async fn exercise_phase_b(s: &dyn ChannelStore) {
    let c = channel_with_posts(s).await;
    let now = now_ms();
    let (k1, k2) = (random::<32>(), random::<32>());
    let (thumbs, heart) = ([1u8; 16], [2u8; 16]);

    assert_eq!(
        s.comment_counts(c, &[1, 2, 99]).await.unwrap(),
        vec![(1, 0), (2, 0), (99, 0)]
    );
    let a = s.add_comment(c, 1, k1, vec![1; 200], now).await.unwrap();
    let b = s.add_comment(c, 1, k2, vec![2; 200], now).await.unwrap();
    let other = s.add_comment(c, 2, k2, vec![3; 200], now).await.unwrap();
    assert_eq!(
        s.comment_counts(c, &[1, 2]).await.unwrap(),
        vec![(1, 2), (2, 1)]
    );

    // Reactions on a comment: a row per (comment, key, label).
    assert!(s
        .add_comment_reaction(c, 1, a, k1, thumbs, now)
        .await
        .unwrap());
    assert!(!s
        .add_comment_reaction(c, 1, a, k1, thumbs, now)
        .await
        .unwrap());
    assert!(s
        .add_comment_reaction(c, 1, a, k2, thumbs, now)
        .await
        .unwrap());
    assert!(s
        .add_comment_reaction(c, 1, b, k1, heart, now)
        .await
        .unwrap());
    // A comment of another post, or none at all, is not there.
    assert!(matches!(
        s.add_comment_reaction(c, 1, other, k1, thumbs, now).await,
        Err(StorageError::UnknownComment)
    ));
    assert!(matches!(
        s.add_comment_reaction(c, 1, 999_999_999, k1, thumbs, now)
            .await,
        Err(StorageError::UnknownComment)
    ));
    let counts = s
        .comment_reaction_counts(c, 1, &[a, b, other])
        .await
        .unwrap();
    assert_eq!(counts[0].comment_cursor, a);
    assert_eq!(counts[0].labels, vec![(thumbs, 2)]);
    assert_eq!(counts[1].labels, vec![(heart, 1)]);
    assert!(
        counts[2].labels.is_empty(),
        "another post's comment counts nothing here"
    );
    // Taking one off; a row of another post's comment is not touched.
    assert!(s
        .delete_comment_reaction(c, 1, a, k2, thumbs)
        .await
        .unwrap());
    assert!(!s
        .delete_comment_reaction(c, 1, a, k2, thumbs)
        .await
        .unwrap());
    assert!(!s
        .delete_comment_reaction(c, 2, a, k1, thumbs)
        .await
        .unwrap());
    assert_eq!(
        s.comment_reaction_counts(c, 1, &[a]).await.unwrap()[0].labels,
        vec![(thumbs, 1)]
    );
    // Deleting a comment takes its reactions and lowers the count.
    assert!(s.delete_comment(c, 1, a).await.unwrap());
    assert!(s.comment_reaction_counts(c, 1, &[a]).await.unwrap()[0]
        .labels
        .is_empty());
    assert_eq!(s.comment_counts(c, &[1]).await.unwrap(), vec![(1, 1)]);

    // Deleting the channel: everything goes, the id is never registered
    // again, a second delete finds nothing, another channel is untouched.
    let keep = channel_with_posts(s).await;
    assert!(!s.is_deleted(c).await.unwrap());
    assert!(s.delete_channel(c, now).await.unwrap());
    assert!(s.is_deleted(c).await.unwrap());
    assert!(!s.exists(c).await);
    assert!(!s.post_exists(c, 1).await.unwrap());
    assert_eq!(s.comment_counts(c, &[1]).await.unwrap(), vec![(1, 0)]);
    assert!(matches!(
        s.fetch(c, 0, 10).await,
        Err(StorageError::UnknownChannel)
    ));
    assert!(matches!(
        s.create(c, random::<32>()).await,
        Err(StorageError::ChannelDeleted)
    ));
    assert!(!s.delete_channel(c, now).await.unwrap());
    assert!(s.is_deleted(c).await.unwrap());
    assert!(!s.is_deleted(keep).await.unwrap());
    assert!(s.post_exists(keep, 1).await.unwrap());
}

#[tokio::test]
async fn the_in_memory_store() {
    exercise(&InMemoryChannelStore::new()).await;
}

#[tokio::test]
async fn the_in_memory_store_phase_b() {
    exercise_phase_b(&InMemoryChannelStore::new()).await;
}

#[tokio::test]
async fn the_sql_store_phase_b() {
    let Some(s) = sql_store().await else {
        return;
    };
    exercise_phase_b(&s).await;
}

#[tokio::test]
async fn the_sql_store() {
    let Some(s) = sql_store().await else {
        return;
    };
    exercise(&s).await;
}
