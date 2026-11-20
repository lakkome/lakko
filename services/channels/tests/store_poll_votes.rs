// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The channel poll vote store (ADR-0092 section 4.3), the same scenario
//! against both `ChannelStore` impls: the in-memory store always, and
//! `SqlChannelStore` against a real MariaDB when
//! `LAKKO_CHANNELS_TEST_DATABASE_URL` is set (else that half skips and says
//! so on stderr). Every run works on fresh random channel ids.

use lakko_svc_channels::votes::VoteOutcome;
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

// Short names keep the vote table readable: store, channel, three labels.
#[allow(clippy::many_single_char_names)]
async fn exercise(s: &dyn ChannelStore) {
    let c = ChannelId::from_bytes(&random::<16>()).unwrap();
    s.create(c, random::<32>()).await.unwrap();
    assert_eq!(
        s.put(c, vec![1; 8], 1024, now_ms()).await.unwrap().cursor,
        1
    );
    let old = s
        .put(c, vec![2; 8], 1024, now_ms() - 40 * DAY_MS)
        .await
        .unwrap();
    assert_eq!(old.cursor, 2);
    let (t1, t2) = (random::<32>(), random::<32>());
    let (a, b, z) = ([1u8; 16], [2u8; 16], [3u8; 16]);

    // An unknown post is refused, for a put and a retract.
    assert!(matches!(
        s.apply_vote(c, 99, t1, vec![a], 10).await,
        Err(StorageError::UnknownPost)
    ));
    assert!(matches!(
        s.apply_vote(c, 99, t1, vec![], 10).await,
        Err(StorageError::UnknownPost)
    ));

    assert_eq!(
        s.apply_vote(c, 1, t1, vec![a], 10).await.unwrap(),
        VoteOutcome::Stored { replaced: false }
    );
    assert_eq!(
        s.apply_vote(c, 1, t2, vec![a, b], 10).await.unwrap(),
        VoteOutcome::Stored { replaced: false }
    );
    // Equal or older time: refused, nothing moves.
    assert_eq!(
        s.apply_vote(c, 1, t1, vec![z], 10).await.unwrap(),
        VoteOutcome::Stale { stored_at_ms: 10 }
    );
    let counts = s.vote_counts(c, &[1, 2]).await.unwrap();
    assert_eq!(counts[0].voters, 2);
    assert_eq!(counts[0].labels, vec![(a, 2), (b, 1)]);
    assert_eq!(counts[1].voters, 0);
    assert!(counts[1].labels.is_empty());

    // A replace moves the counters, the voters stay.
    assert_eq!(
        s.apply_vote(c, 1, t1, vec![z], 11).await.unwrap(),
        VoteOutcome::Stored { replaced: true }
    );
    let counts = s.vote_counts(c, &[1]).await.unwrap();
    assert_eq!(counts[0].voters, 2);
    assert_eq!(counts[0].labels, vec![(a, 1), (b, 1), (z, 1)]);

    // A retract keeps a tombstone that still orders.
    assert_eq!(
        s.apply_vote(c, 1, t2, vec![], 12).await.unwrap(),
        VoteOutcome::Retracted { existed: true }
    );
    assert_eq!(
        s.apply_vote(c, 1, t2, vec![a, b], 11).await.unwrap(),
        VoteOutcome::Stale { stored_at_ms: 12 }
    );
    let counts = s.vote_counts(c, &[1]).await.unwrap();
    assert_eq!(counts[0].voters, 1);
    assert_eq!(counts[0].labels, vec![(z, 1)]);
    // A retract of a tag that never voted records its time.
    assert_eq!(
        s.apply_vote(c, 1, random::<32>(), vec![], 5).await.unwrap(),
        VoteOutcome::Retracted { existed: false }
    );
    assert_eq!(s.vote_counts(c, &[1]).await.unwrap()[0].voters, 1);

    // A vote under the old post goes when the post is pruned.
    s.apply_vote(c, 2, t1, vec![a], 10).await.unwrap();
    assert_eq!(s.vote_counts(c, &[2]).await.unwrap()[0].voters, 1);
    s.prune_posts_older_than(now_ms() - 30 * DAY_MS)
        .await
        .unwrap();
    let counts = s.vote_counts(c, &[2]).await.unwrap();
    assert_eq!(counts[0].voters, 0);
    assert!(counts[0].labels.is_empty());
}

#[tokio::test]
async fn in_memory_store_counts_votes() {
    exercise(&InMemoryChannelStore::new()).await;
}

#[tokio::test]
async fn sql_store_counts_votes() {
    if let Some(s) = sql_store().await {
        exercise(&s).await;
    }
}
