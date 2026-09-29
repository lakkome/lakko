// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `SqlChannelStore` against a real MariaDB (ADR-0059 S2 surfaces).
//!
//! Gated on `LAKKO_CHANNELS_TEST_DATABASE_URL`; each test skips (and says so
//! on stderr) when it is unset. The database persists across runs, so every
//! test works on fresh random channel ids.

use lakko_svc_channels::{
    ChannelId, ChannelStore, DirectoryQuery, ModerationAction, SqlChannelStore, StorageError,
    StoredListing, SubscriberId, MIGRATIONS,
};
use sqlx::mysql::MySqlPoolOptions;

const OWNER: [u8; 32] = [7; 32];
const DAY_MS: u64 = 24 * 3600 * 1000;

#[test]
fn the_spent_set_migration_is_the_shared_crate_schema_byte_for_byte() {
    assert_eq!(
        include_str!("../migrations/0006_anon_token_spent.sql"),
        lakko_anon_token_server::SPENT_SET_MIGRATION_SQL
    );
}

/// Concurrent first runs of the migrator race on `_sqlx_migrations`; run it
/// once per test process.
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn store(test: &str) -> Option<SqlChannelStore> {
    let Ok(url) = std::env::var("LAKKO_CHANNELS_TEST_DATABASE_URL") else {
        // The test passes vacuously; say so under `--nocapture`.
        tracing::warn!(test, "LAKKO_CHANNELS_TEST_DATABASE_URL unset; skipping");
        return None;
    };
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect test DB");
    MIGRATED
        .get_or_init(|| async { MIGRATIONS.run(&pool).await.expect("run migrations") })
        .await;
    Some(SqlChannelStore::new(pool))
}

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    lakko_crypto::random::fill(&mut out);
    out
}

fn channel() -> ChannelId {
    ChannelId::from_bytes(&random::<16>()).unwrap()
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

fn listing(language: &str, name: &str, updated_at_ms: u64) -> StoredListing {
    StoredListing {
        topic: 5,
        language: language.to_string(),
        handle: "handle_x".to_string(),
        name: name.to_string(),
        description: "About 100% of things_".to_string(),
        bytes: name.as_bytes().to_vec(),
        updated_at_ms,
    }
}

#[tokio::test]
async fn create_keeps_the_first_owner_key() {
    let Some(s) = store("create_keeps_the_first_owner_key").await else {
        return;
    };
    let c = channel();
    s.create(c, OWNER).await.unwrap();
    s.create(c, OWNER).await.unwrap();
    assert!(matches!(
        s.create(c, [8; 32]).await,
        Err(StorageError::OwnerMismatch)
    ));
    assert_eq!(s.owner_pubkey(c).await.unwrap(), Some(OWNER));
    assert_eq!(s.owner_pubkey(channel()).await.unwrap(), None);
}

#[tokio::test]
async fn an_owner_nonce_is_accepted_once_and_pruned() {
    let Some(s) = store("an_owner_nonce_is_accepted_once_and_pruned").await else {
        return;
    };
    let n = random::<16>();
    // Far in the past so the prune below takes only rows of this test's era.
    let seen = 1_000;
    assert!(s.consume_owner_nonce(n, seen).await.unwrap());
    assert!(!s.consume_owner_nonce(n, seen).await.unwrap());
    assert!(s.prune_owner_nonces_older_than(seen + 1).await.unwrap() >= 1);
    assert!(s.consume_owner_nonce(n, now_ms()).await.unwrap());
}

#[tokio::test]
async fn a_listing_must_be_newer_survives_nothing_when_blocked_and_keeps_its_first_time() {
    let Some(s) = store("listing_rules").await else {
        return;
    };
    let c = channel();
    assert!(matches!(
        s.put_listing(c, listing("en", "A", 1), 10).await,
        Err(StorageError::UnknownChannel)
    ));
    s.create(c, OWNER).await.unwrap();
    s.put_listing(c, listing("en", "A", 5), 10 * DAY_MS)
        .await
        .unwrap();
    assert!(matches!(
        s.put_listing(c, listing("en", "B", 5), 11 * DAY_MS).await,
        Err(StorageError::StaleListing)
    ));
    s.put_listing(c, listing("en", "B", 6), 11 * DAY_MS)
        .await
        .unwrap();
    let entry = s.listing(c, 12 * DAY_MS).await.unwrap().unwrap();
    assert_eq!(entry.listing, b"B");
    // The first-listed time is the first listing's, and survives withdrawal.
    assert_eq!(entry.first_listed_at_ms, 10 * DAY_MS);
    assert!(s.withdraw_listing(c).await.unwrap());
    assert!(!s.withdraw_listing(c).await.unwrap());
    assert!(s.listing(c, 12 * DAY_MS).await.unwrap().is_none());
    s.put_listing(c, listing("en", "C", 1), 20 * DAY_MS)
        .await
        .unwrap();
    assert_eq!(
        s.listing(c, 21 * DAY_MS)
            .await
            .unwrap()
            .unwrap()
            .first_listed_at_ms,
        10 * DAY_MS
    );

    // Block removes the listing and refuses a new one; a removal keeps the
    // block; unblock lifts it.
    s.moderate(c, ModerationAction::Block, "spam", 1)
        .await
        .unwrap();
    assert!(s.listing(c, 21 * DAY_MS).await.unwrap().is_none());
    assert!(matches!(
        s.put_listing(c, listing("en", "D", 2), 21 * DAY_MS).await,
        Err(StorageError::ListingBlocked)
    ));
    s.moderate(c, ModerationAction::Remove, "dup", 2)
        .await
        .unwrap();
    assert!(matches!(
        s.put_listing(c, listing("en", "D", 2), 21 * DAY_MS).await,
        Err(StorageError::ListingBlocked)
    ));
    s.moderate(c, ModerationAction::Unblock, "appeal", 3)
        .await
        .unwrap();
    s.put_listing(c, listing("en", "D", 2), 21 * DAY_MS)
        .await
        .unwrap();
    // A block may precede the channel's creation.
    s.moderate(channel(), ModerationAction::Block, "pre-emptive", 4)
        .await
        .unwrap();
}

#[tokio::test]
async fn candidates_carry_counts_and_recent_registrations() {
    let Some(s) = store("candidates").await else {
        return;
    };
    // A language no other test run uses, so the bucket holds only ours.
    let lang: String = random::<3>()
        .iter()
        .map(|b| char::from(b'a' + b % 26))
        .collect();
    let c = channel();
    s.create(c, OWNER).await.unwrap();
    let now = now_ms();
    // Two old registrations (10 days ago), three recent ones.
    for i in 0..5u8 {
        let at = if i < 2 {
            now - 10 * DAY_MS
        } else {
            now - DAY_MS
        };
        let sub = SubscriberId::from_bytes(&random::<32>()).unwrap();
        s.subscribe(c, sub, at).await.unwrap();
        // A re-subscribe keeps the first registration time.
        s.subscribe(c, sub, now).await.unwrap();
    }
    let mut stored = listing(&lang, "Zeta News", 1);
    stored.handle = format!("zeta_{lang}");
    s.put_listing(c, stored, now).await.unwrap();

    let bucket = s
        .directory_candidates(
            &DirectoryQuery::Bucket {
                topic: 5,
                language: lang.clone(),
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(bucket.len(), 1);
    assert_eq!(bucket[0].channel, c);
    assert_eq!(bucket[0].subscribers, 5);
    assert_eq!(bucket[0].recent_registrations, 3);

    // Search is a case-insensitive substring over name, handle, description,
    // and LIKE wildcards in the needle are literal.
    let hit = |needle: &str| {
        let s = s.clone();
        let needle = needle.to_string();
        async move {
            s.directory_candidates(&DirectoryQuery::Search { needle }, now)
                .await
                .unwrap()
                .iter()
                .any(|e| e.channel == c)
        }
    };
    assert!(hit(&format!("zeta_{lang}")).await);
    assert!(hit("zeta news").await);
    assert!(hit("100% of things_").await);
    assert!(!hit(&format!("zeta%{lang}")).await);
}

#[tokio::test]
async fn copublisher_keys_are_appointed_bounded_and_revoked() {
    let Some(s) = store("copublisher_keys_are_appointed_bounded_and_revoked").await else {
        return;
    };
    let c = channel();
    let key: [u8; 32] = random();
    assert!(matches!(
        s.appoint_copublisher(c, key, 2, now_ms()).await,
        Err(StorageError::UnknownChannel)
    ));
    s.create(c, random()).await.unwrap();
    assert!(!s.is_copublisher(c, key).await.unwrap());
    s.appoint_copublisher(c, key, 2, now_ms()).await.unwrap();
    s.appoint_copublisher(c, key, 2, now_ms()).await.unwrap();
    assert!(s.is_copublisher(c, key).await.unwrap());
    s.appoint_copublisher(c, random(), 2, now_ms())
        .await
        .unwrap();
    assert!(matches!(
        s.appoint_copublisher(c, random(), 2, now_ms()).await,
        Err(StorageError::TooManyCoPublishers)
    ));
    // Another channel's appointments are its own.
    assert!(!s.is_copublisher(channel(), key).await.unwrap());
    assert!(s.revoke_copublisher(c, key).await.unwrap());
    assert!(!s.revoke_copublisher(c, key).await.unwrap());
    assert!(!s.is_copublisher(c, key).await.unwrap());
}
