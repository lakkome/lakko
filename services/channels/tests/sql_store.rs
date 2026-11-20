// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `SqlChannelStore` against a real MariaDB (ADR-0059 S2 surfaces).
//!
//! Gated on `LAKKO_CHANNELS_TEST_DATABASE_URL`; each test skips (and says so
//! on stderr) when it is unset. The database persists across runs, so every
//! test works on fresh random channel ids.

use lakko_svc_channels::{
    ChannelId, ChannelModeration, ChannelStore, DirectoryQuery, ModerationOutcome,
    ModerationStatement, PublicSeedOutcome, SqlChannelStore, StorageError, StoredListing,
    SubscriberId, INVITE_USES_UNLIMITED, MIGRATIONS, PUBLIC_SEED_LEN, SEALED_TICKET_LEN,
};
use sqlx::mysql::MySqlPoolOptions;

const OWNER: [u8; 32] = [7; 32];
const DAY_MS: u64 = 24 * 3600 * 1000;

#[test]
fn the_spent_set_migration_is_the_shared_crate_schema_byte_for_byte() {
    assert_eq!(
        include_str!("../migrations/0008_anon_token_spent.sql"),
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
async fn replace_owner_is_a_compare_and_swap() {
    let Some(s) = store("replace_owner_is_a_compare_and_swap").await else {
        return;
    };
    let c = channel();
    assert!(matches!(
        s.replace_owner(c, OWNER, [8; 32]).await,
        Err(StorageError::UnknownChannel)
    ));
    s.create(c, OWNER).await.unwrap();
    // Not the current key: nothing changes.
    assert!(!s.replace_owner(c, [9; 32], [8; 32]).await.unwrap());
    assert_eq!(s.owner_pubkey(c).await.unwrap(), Some(OWNER));
    // Of two handovers under the same key, exactly one lands.
    let (a, b) = tokio::join!(
        s.replace_owner(c, OWNER, [8; 32]),
        s.replace_owner(c, OWNER, [7; 32])
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(a ^ b, "exactly one handover wins: {a} {b}");
    let now = s.owner_pubkey(c).await.unwrap().unwrap();
    assert_eq!(now, if a { [8; 32] } else { [7; 32] });
    // The displaced key hands nothing over any more, and cannot re-create.
    assert!(!s.replace_owner(c, OWNER, [6; 32]).await.unwrap());
    assert_eq!(s.owner_pubkey(c).await.unwrap(), Some(now));
    assert!(matches!(
        s.create(c, OWNER).await,
        Err(StorageError::OwnerMismatch)
    ));
    // The subscribers, the epoch and the co-publishers are untouched.
    assert_eq!(s.current_epoch(c).await.unwrap(), 0);
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

    // ADR-0089: a block removes the listing and refuses a new one, a delist
    // and a removal accumulate with it, a restore clears all three. The list
    // version only grows and the database is shared, so every statement here
    // is under the highest version there is.
    let statement = |channel, action, at| ModerationStatement {
        statement_id: random(),
        list_version: u64::MAX,
        channel,
        action,
        issued_at_ms: at,
        log_index: at,
        reason: "spam",
        now_ms: at,
    };
    let block = statement(c, ChannelModeration::BlockHandle, 1);
    assert_eq!(
        s.apply_moderation(&block).await.unwrap(),
        ModerationOutcome::Changed
    );
    assert!(s.listing(c, 21 * DAY_MS).await.unwrap().is_none());
    assert!(matches!(
        s.put_listing(c, listing("en", "D", 2), 21 * DAY_MS).await,
        Err(StorageError::ListingBlocked)
    ));
    assert_eq!(
        s.apply_moderation(&block).await.unwrap(),
        ModerationOutcome::Repeat
    );
    s.apply_moderation(&statement(c, ChannelModeration::Remove, 3))
        .await
        .unwrap();
    assert_eq!(s.removed_channel(c).await.unwrap(), Some(3));
    // A Restore the moderator made before the removal changes nothing.
    assert_eq!(
        s.apply_moderation(&statement(c, ChannelModeration::Restore, 2))
            .await
            .unwrap(),
        ModerationOutcome::Superseded
    );
    assert_eq!(s.removed_channel(c).await.unwrap(), Some(3));
    s.apply_moderation(&statement(c, ChannelModeration::Restore, 4))
        .await
        .unwrap();
    assert_eq!(s.removed_channel(c).await.unwrap(), None);
    s.put_listing(c, listing("en", "D", 2), 21 * DAY_MS)
        .await
        .unwrap();
    s.apply_moderation(&statement(c, ChannelModeration::Delist, 5))
        .await
        .unwrap();
    assert!(s.listing(c, 21 * DAY_MS).await.unwrap().is_none());
    assert!(matches!(
        s.put_listing(c, listing("en", "E", 3), 21 * DAY_MS).await,
        Err(StorageError::ListingDelisted)
    ));
    // A list older than one already seen is refused.
    let mut stale = statement(c, ChannelModeration::Restore, 6);
    stale.list_version = u64::MAX - 1;
    assert_eq!(
        s.apply_moderation(&stale).await.unwrap(),
        ModerationOutcome::StaleList
    );
    // The floor alone (ADR-0089 F1): an older list is refused, the floor
    // itself is kept.
    assert!(!s.raise_moderation_list_floor(u64::MAX - 1).await.unwrap());
    assert!(s.raise_moderation_list_floor(u64::MAX).await.unwrap());
    // A statement may precede the channel's creation.
    s.apply_moderation(&statement(channel(), ChannelModeration::BlockHandle, 7))
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

// ----------------------------------------------------------------------------
// ADR-0087 sections 6, 7 and 15: the store contract of public and private
// channels, run against the in-memory store always and against MariaDB when
// `LAKKO_CHANNELS_TEST_DATABASE_URL` is set.
// ----------------------------------------------------------------------------

/// Far-past clock values so the prunes below take only this test's rows from
/// a database other tests share.
const OLD_MS: u64 = 1_000_000;
const PRUNE_CUTOFF_MS: u64 = 2_000_000;

fn sub() -> SubscriberId {
    SubscriberId::from_bytes(&random::<32>()).unwrap()
}

async fn flag_and_public_seeds(s: &dyn ChannelStore) {
    let c = channel();
    assert_eq!(s.is_private(c).await.unwrap(), None);
    s.create_with_access(c, OWNER, true).await.unwrap();
    assert_eq!(s.is_private(c).await.unwrap(), Some(true));
    // Re-creating never changes the flag.
    s.create_with_access(c, OWNER, false).await.unwrap();
    s.create(c, OWNER).await.unwrap();
    assert_eq!(s.is_private(c).await.unwrap(), Some(true));

    // A private channel holds no public seed and no listing.
    assert!(matches!(
        s.put_public_seed(c, 0, [1; PUBLIC_SEED_LEN], OLD_MS).await,
        Err(StorageError::ChannelPrivate)
    ));
    assert!(matches!(
        s.put_listing(c, listing("en", "Private", 1), now_ms())
            .await,
        Err(StorageError::ChannelPrivate)
    ));

    assert!(s.set_private(c, false).await.unwrap());
    assert!(!s.set_private(c, false).await.unwrap());
    assert_eq!(s.is_private(c).await.unwrap(), Some(false));
    s.put_listing(c, listing("en", "Public", 1), now_ms())
        .await
        .unwrap();

    // Immutable per epoch; at most the next epoch.
    assert_eq!(
        s.put_public_seed(c, 0, [1; PUBLIC_SEED_LEN], OLD_MS)
            .await
            .unwrap(),
        PublicSeedOutcome::Stored
    );
    assert_eq!(
        s.put_public_seed(c, 0, [1; PUBLIC_SEED_LEN], OLD_MS)
            .await
            .unwrap(),
        PublicSeedOutcome::AlreadyStored
    );
    assert!(matches!(
        s.put_public_seed(c, 0, [2; PUBLIC_SEED_LEN], OLD_MS).await,
        Err(StorageError::PublicSeedConflict)
    ));
    assert!(matches!(
        s.put_public_seed(c, 2, [2; PUBLIC_SEED_LEN], OLD_MS).await,
        Err(StorageError::PublicSeedEpochInFuture { current: 0, got: 2 })
    ));
    s.put_public_seed(c, 1, [3; PUBLIC_SEED_LEN], OLD_MS)
        .await
        .unwrap();
    assert_eq!(
        s.public_seed(c, 0).await.unwrap(),
        Some([1; PUBLIC_SEED_LEN])
    );
    assert_eq!(
        s.public_seed(c, 1).await.unwrap(),
        Some([3; PUBLIC_SEED_LEN])
    );
    assert_eq!(s.public_seed(c, 7).await.unwrap(), None);

    // Epoch 0 was superseded when epoch 1's seed came; epoch 1 was not.
    assert!(
        s.prune_public_seeds_superseded_before(PRUNE_CUTOFF_MS)
            .await
            .unwrap()
            >= 1
    );
    assert_eq!(s.public_seed(c, 0).await.unwrap(), None);
    assert_eq!(
        s.public_seed(c, 1).await.unwrap(),
        Some([3; PUBLIC_SEED_LEN])
    );

    // Going private withdraws the listing and deletes every public seed.
    assert!(s.set_private(c, true).await.unwrap());
    assert_eq!(s.public_seed(c, 1).await.unwrap(), None);
    assert!(s.listing(c, now_ms()).await.unwrap().is_none());
    assert!(matches!(
        s.set_private(channel(), true).await,
        Err(StorageError::UnknownChannel)
    ));
}

async fn invite_keys(s: &dyn ChannelStore) {
    let c = channel();
    s.create_with_access(c, OWNER, true).await.unwrap();
    let one_use = random::<32>();
    let unlimited = random::<32>();
    let expiring = random::<32>();
    let now = now_ms();
    s.put_invite_key(c, one_use, 0, 0, 3, now).await.unwrap();
    s.put_invite_key(c, unlimited, 0, INVITE_USES_UNLIMITED, 3, now)
        .await
        .unwrap();
    s.put_invite_key(c, expiring, 5_000, 10, 3, now)
        .await
        .unwrap();
    assert!(matches!(
        s.put_invite_key(c, random::<32>(), 0, 1, 3, now).await,
        Err(StorageError::TooManyInviteKeys)
    ));
    assert!(matches!(
        s.put_invite_key(channel(), one_use, 0, 1, 3, now).await,
        Err(StorageError::UnknownChannel)
    ));
    assert_eq!(s.list_invite_keys(c).await.unwrap().len(), 3);

    // `0` is one use.
    assert!(s.claim_invite_use(c, one_use, now).await.unwrap());
    assert!(!s.claim_invite_use(c, one_use, now).await.unwrap());
    s.release_invite_use(c, one_use).await.unwrap();
    assert!(s.claim_invite_use(c, one_use, now).await.unwrap());
    // Updating the terms keeps the count.
    s.put_invite_key(c, one_use, 0, 2, 3, now).await.unwrap();
    let row = s.invite_key(c, one_use).await.unwrap().unwrap();
    assert_eq!((row.max_uses, row.used_count), (2, 1));
    assert!(s.claim_invite_use(c, one_use, now).await.unwrap());
    assert!(!s.claim_invite_use(c, one_use, now).await.unwrap());

    for _ in 0..5 {
        assert!(s.claim_invite_use(c, unlimited, now).await.unwrap());
    }
    // Expired (5 000 ms after the epoch).
    assert!(!s.claim_invite_use(c, expiring, now).await.unwrap());
    assert!(!s.claim_invite_use(c, random::<32>(), now).await.unwrap());

    assert!(
        s.prune_invite_keys_expired_before(PRUNE_CUTOFF_MS)
            .await
            .unwrap()
            >= 1
    );
    assert_eq!(s.invite_key(c, expiring).await.unwrap(), None);
    assert!(s.delete_invite_key(c, unlimited).await.unwrap());
    assert!(!s.delete_invite_key(c, unlimited).await.unwrap());
    let left: Vec<[u8; 32]> = s
        .list_invite_keys(c)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.invite_pk)
        .collect();
    assert_eq!(left, vec![one_use]);
}

async fn subscriber_rows_count_and_removal(s: &dyn ChannelStore) {
    let c = channel();
    s.create_with_access(c, OWNER, true).await.unwrap();
    let (alice, bob, dave) = (sub(), sub(), sub());
    let ticket_a = [0xA1; SEALED_TICKET_LEN];
    let ticket_b = [0xB2; SEALED_TICKET_LEN];
    assert!(s
        .subscribe_with_ticket(c, alice, now_ms(), Some(ticket_a))
        .await
        .unwrap());
    assert!(s
        .subscribe_with_ticket(c, bob, now_ms(), Some(ticket_b))
        .await
        .unwrap());
    assert!(s
        .subscribe_with_ticket(c, dave, now_ms(), None)
        .await
        .unwrap());
    assert_eq!(s.subscriber_count(c).await.unwrap(), 3);
    // A re-subscribe is not a new row and keeps the count; a ticket given
    // replaces the stored one, none keeps it.
    assert!(!s
        .subscribe_with_ticket(c, alice, now_ms(), Some(ticket_b))
        .await
        .unwrap());
    assert!(!s
        .subscribe_with_ticket(c, bob, now_ms(), None)
        .await
        .unwrap());
    assert_eq!(s.subscriber_count(c).await.unwrap(), 3);

    let page = s.list_subscriber_page(c, 0, 2).await.unwrap();
    assert_eq!(
        page.iter().map(|e| e.subscriber).collect::<Vec<_>>(),
        vec![alice, bob]
    );
    assert_eq!(page[0].sealed_ticket, Some(ticket_b));
    assert_eq!(page[1].sealed_ticket, Some(ticket_b));
    assert!(page[0].seq < page[1].seq);
    let rest = s.list_subscriber_page(c, page[1].seq, 10).await.unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].subscriber, dave);
    assert_eq!(rest[0].sealed_ticket, None);
    assert!(s
        .list_subscriber_page(c, rest[0].seq, 10)
        .await
        .unwrap()
        .is_empty());

    // The subscriber leaves: no removal record.
    s.unsubscribe(c, dave).await.unwrap();
    s.unsubscribe(c, dave).await.unwrap();
    assert_eq!(s.subscriber_count(c).await.unwrap(), 2);
    assert!(!s.is_removed(c, dave).await.unwrap());

    // A publisher removes `bob`: the row goes, the key is recorded removed.
    assert!(s.remove_subscriber(c, bob, now_ms()).await.unwrap());
    assert!(!s.remove_subscriber(c, bob, now_ms()).await.unwrap());
    assert_eq!(s.subscriber_count(c).await.unwrap(), 1);
    assert!(s.is_removed(c, bob).await.unwrap());
    assert!(!s.is_subscribed(c, bob).await);
    assert!(matches!(
        s.fetch_envelope(c, bob).await,
        Err(StorageError::UnknownSubscriber)
    ));
    // Subscribing again (with a new invitation) clears it.
    assert!(s
        .subscribe_with_ticket(c, bob, now_ms(), Some(ticket_b))
        .await
        .unwrap());
    assert!(!s.is_removed(c, bob).await.unwrap());
    assert_eq!(s.subscriber_count(c).await.unwrap(), 2);

    // The stale-subscriber prune moves the counter and drops old removal
    // records.
    s.touch_subscriber_activity(c, alice, OLD_MS).await.unwrap();
    s.remove_subscriber(c, dave, OLD_MS).await.unwrap();
    assert!(s.is_removed(c, dave).await.unwrap());
    assert!(
        s.prune_subscribers_older_than(PRUNE_CUTOFF_MS)
            .await
            .unwrap()
            >= 1
    );
    assert!(!s.is_subscribed(c, alice).await);
    assert!(!s.is_removed(c, dave).await.unwrap());
    assert_eq!(s.subscriber_count(c).await.unwrap(), 1);

    // A public channel's subscribers cannot be removed.
    let public = channel();
    s.create(public, OWNER).await.unwrap();
    s.subscribe(public, alice, now_ms()).await.unwrap();
    assert!(matches!(
        s.remove_subscriber(public, alice, now_ms()).await,
        Err(StorageError::ChannelPublic)
    ));
    assert_eq!(s.subscriber_count(public).await.unwrap(), 1);
    assert!(matches!(
        s.subscriber_count(channel()).await,
        Err(StorageError::UnknownChannel)
    ));
}

#[tokio::test]
async fn the_in_memory_store_meets_the_adr_0087_contract() {
    let s = lakko_svc_channels::InMemoryChannelStore::new();
    flag_and_public_seeds(&s).await;
    invite_keys(&s).await;
    subscriber_rows_count_and_removal(&s).await;
}

#[tokio::test]
async fn the_sql_store_meets_the_adr_0087_contract() {
    let Some(s) = store("the_sql_store_meets_the_adr_0087_contract").await else {
        return;
    };
    flag_and_public_seeds(&s).await;
    invite_keys(&s).await;
    subscriber_rows_count_and_removal(&s).await;
}

/// The ticket column is exactly the codec's ticket length, so a ticket the
/// codec seals always fits and a change of the codec fails here.
#[test]
fn the_sealed_ticket_column_fits_the_ticket() {
    let migration = include_str!("../migrations/0010_private_channels.sql");
    let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalised.contains(&format!(
            "ADD COLUMN sealed_ticket BINARY({SEALED_TICKET_LEN}) NULL"
        )),
        "SEALED_TICKET_LEN ({SEALED_TICKET_LEN}) must match 0010_private_channels.sql"
    );
    assert_eq!(PUBLIC_SEED_LEN, 64);
    let normalised_seed = format!("envelope BINARY({PUBLIC_SEED_LEN}) NOT NULL");
    assert!(normalised.contains(&normalised_seed));
}
