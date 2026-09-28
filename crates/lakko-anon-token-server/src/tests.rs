// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tests. The configuration tests need nothing; the store tests are gated on
//! `LAKKO_ANON_TOKEN_TEST_DATABASE_URL` and skip (do not fail) when it is
//! unset, the same harness shape as the auth service's DB tests. Run with,
//! e.g.:
//!
//! ```text
//! LAKKO_ANON_TOKEN_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_anon_token_test \
//!   cargo test -p lakko-anon-token-server
//! ```

use super::*;

const HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

#[test]
fn config_is_dormant_without_a_master_secret() {
    assert!(AnonTokenServerConfig::from_values(None, None)
        .unwrap()
        .is_none());
    assert!(AnonTokenServerConfig::from_values(Some("  "), Some("60"))
        .unwrap()
        .is_none());
}

#[test]
fn config_defaults_the_window_and_refuses_malformed_values() {
    let cfg = AnonTokenServerConfig::from_values(Some(HEX), None)
        .unwrap()
        .unwrap();
    assert_eq!(cfg.window_secs, DEFAULT_WINDOW_SECS);
    let cfg = AnonTokenServerConfig::from_values(Some(HEX), Some(" 3600 "))
        .unwrap()
        .unwrap();
    assert_eq!(cfg.window_secs, 3_600);
    assert_eq!(
        AnonTokenServerConfig::from_values(Some("abcd"), None).unwrap_err(),
        ConfigError::InvalidMasterSecret
    );
    assert_eq!(
        AnonTokenServerConfig::from_values(Some(HEX), Some("0")).unwrap_err(),
        ConfigError::InvalidWindow
    );
    assert_eq!(
        AnonTokenServerConfig::from_values(Some(HEX), Some("1d")).unwrap_err(),
        ConfigError::InvalidWindow
    );
}

#[test]
fn current_epoch_ends_after_it_begins() {
    let cfg = AnonTokenServerConfig::from_values(Some(HEX), Some("60"))
        .unwrap()
        .unwrap();
    let now = now_unix_secs();
    let end = cfg.current_epoch_ends_at();
    assert!(end > now && end - now <= 60);
}

#[test]
fn migration_sql_names_the_tables_the_queries_use() {
    assert!(ISSUANCE_MIGRATION_SQL.contains("CREATE TABLE IF NOT EXISTS anon_token_issuance"));
    assert!(SPENT_SET_MIGRATION_SQL.contains("CREATE TABLE IF NOT EXISTS anon_token_spent"));
}

async fn test_pool(name: &str) -> Option<MySqlPool> {
    let Ok(url) = std::env::var("LAKKO_ANON_TOKEN_TEST_DATABASE_URL") else {
        eprintln!("skipping {name}: LAKKO_ANON_TOKEN_TEST_DATABASE_URL unset");
        return None;
    };
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .expect("connect test DB");
    sqlx::raw_sql(ISSUANCE_MIGRATION_SQL)
        .execute(&pool)
        .await
        .expect("issuance table");
    sqlx::raw_sql(SPENT_SET_MIGRATION_SQL)
        .execute(&pool)
        .await
        .expect("spent table");
    Some(pool)
}

/// Epoch offset for every store test except the pruning one, which prunes
/// everything below its own (lower) epochs; keeping the other tests above it
/// stops a concurrently running prune deleting their rows mid-test.
const HIGH: u64 = 1 << 62;

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    lakko_crypto::random::fill(&mut b);
    b
}

#[tokio::test]
async fn quota_grants_until_the_limit_then_refuses_the_whole_batch() {
    let Some(pool) = test_pool("quota_grants_until_the_limit").await else {
        return;
    };
    let acct = random::<16>();
    let ctx = TokenContext::DsKnock;
    let epoch = HIGH + 1_000;
    assert_eq!(
        reserve_issuance(&pool, &acct, ctx, epoch, 4, 10)
            .await
            .unwrap(),
        IssuanceDecision::Granted { remaining: 6 }
    );
    assert_eq!(
        reserve_issuance(&pool, &acct, ctx, epoch, 6, 10)
            .await
            .unwrap(),
        IssuanceDecision::Granted { remaining: 0 }
    );
    assert_eq!(
        reserve_issuance(&pool, &acct, ctx, epoch, 1, 10)
            .await
            .unwrap(),
        IssuanceDecision::Exhausted { remaining: 0 }
    );
    // Another epoch, another context and another account each start fresh.
    assert_eq!(
        reserve_issuance(&pool, &acct, ctx, epoch + 1, 10, 10)
            .await
            .unwrap(),
        IssuanceDecision::Granted { remaining: 0 }
    );
    assert_eq!(
        reserve_issuance(&pool, &acct, TokenContext::PushRegister, epoch, 3, 10)
            .await
            .unwrap(),
        IssuanceDecision::Granted { remaining: 7 }
    );
    assert_eq!(
        reserve_issuance(&pool, &random::<16>(), ctx, epoch, 10, 10)
            .await
            .unwrap(),
        IssuanceDecision::Granted { remaining: 0 }
    );
}

#[tokio::test]
async fn an_over_quota_batch_counts_nothing() {
    let Some(pool) = test_pool("an_over_quota_batch_counts_nothing").await else {
        return;
    };
    let acct = random::<16>();
    let ctx = TokenContext::InviteConsume;
    assert_eq!(
        reserve_issuance(&pool, &acct, ctx, HIGH + 5, 8, 10)
            .await
            .unwrap(),
        IssuanceDecision::Granted { remaining: 2 }
    );
    assert_eq!(
        reserve_issuance(&pool, &acct, ctx, HIGH + 5, 3, 10)
            .await
            .unwrap(),
        IssuanceDecision::Exhausted { remaining: 2 }
    );
    // The refused batch did not consume the two that were left.
    assert_eq!(
        reserve_issuance(&pool, &acct, ctx, HIGH + 5, 2, 10)
            .await
            .unwrap(),
        IssuanceDecision::Granted { remaining: 0 }
    );
    // A zero limit issues nothing.
    assert_eq!(
        reserve_issuance(&pool, &acct, ctx, HIGH + 6, 1, 0)
            .await
            .unwrap(),
        IssuanceDecision::Exhausted { remaining: 0 }
    );
}

#[tokio::test]
async fn concurrent_reservations_never_exceed_the_limit() {
    let Some(pool) = test_pool("concurrent_reservations_never_exceed_the_limit").await else {
        return;
    };
    let acct = random::<16>();
    let mut handles = Vec::new();
    for _ in 0..16 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            reserve_issuance(
                &pool,
                &acct,
                TokenContext::DiscoveryLookup,
                HIGH + 77,
                3,
                20,
            )
            .await
        }));
    }
    let mut granted = 0u32;
    for h in handles {
        if let IssuanceDecision::Granted { .. } = h.await.unwrap().unwrap() {
            granted += 3;
        }
    }
    assert_eq!(granted, 18, "six batches of three fit in twenty, no more");
}

#[tokio::test]
async fn a_token_is_spent_once() {
    let Some(pool) = test_pool("a_token_is_spent_once").await else {
        return;
    };
    let id = random::<32>();
    spend(&pool, TokenContext::DsKnock, &id, HIGH + 9)
        .await
        .unwrap();
    assert!(matches!(
        spend(&pool, TokenContext::DsKnock, &id, HIGH + 9).await,
        Err(SpendError::AlreadySpent)
    ));
    // The spent-set is per context: the same identifier under another context
    // is a different row (a real token cannot collide: the id binds context).
    spend(&pool, TokenContext::PushRegister, &id, HIGH + 9)
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_spends_of_one_token_succeed_exactly_once() {
    let Some(pool) = test_pool("concurrent_spends_of_one_token").await else {
        return;
    };
    let id = random::<32>();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            spend(&pool, TokenContext::InviteConsume, &id, HIGH + 3).await
        }));
    }
    let mut ok = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(()) => ok += 1,
            Err(SpendError::AlreadySpent) => {},
            Err(e) => panic!("unexpected: {e}"),
        }
    }
    assert_eq!(ok, 1);
}

#[tokio::test]
async fn pruning_keeps_the_current_and_previous_epoch() {
    let Some(pool) = test_pool("pruning_keeps_the_current_and_previous_epoch").await else {
        return;
    };
    // Epochs far above any real clock so other rows are untouched.
    let base: u64 = 9_000_000_000;
    let ids: Vec<[u8; 32]> = (0..3).map(|_| random::<32>()).collect();
    for (i, id) in ids.iter().enumerate() {
        spend(&pool, TokenContext::DsKnock, id, base + i as u64)
            .await
            .unwrap();
    }
    let acct = random::<16>();
    for e in 0..3 {
        reserve_issuance(&pool, &acct, TokenContext::DsKnock, base + e, 1, 5)
            .await
            .unwrap();
    }
    // Current epoch base + 2: keep base + 1 and base + 2, drop base.
    prune_spent(&pool, base + 2).await.unwrap();
    prune_issuance(&pool, base + 2).await.unwrap();

    let mut spent = Vec::new();
    for id in &ids {
        let e: Option<u64> = sqlx::query_scalar(
            "SELECT epoch FROM anon_token_spent WHERE context = ? AND spend_id = ?",
        )
        .bind(TokenContext::DsKnock.as_u8())
        .bind(&id[..])
        .fetch_optional(&pool)
        .await
        .unwrap();
        spent.extend(e);
    }
    assert_eq!(spent, vec![base + 1, base + 2]);
    let issued: Vec<u64> = sqlx::query_scalar(
        "SELECT epoch FROM anon_token_issuance WHERE account_id = ? ORDER BY epoch",
    )
    .bind(&acct[..])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(issued, vec![base + 1, base + 2]);
    // The old token's identifier is free again, and it no longer verifies:
    // that is why dropping it is safe.
    spend(&pool, TokenContext::DsKnock, &ids[0], base)
        .await
        .unwrap();
    prune_spent(&pool, base + 2).await.unwrap();
}

// ---- The spend gate ----------------------------------------------------------

fn gate_config() -> AnonTokenServerConfig {
    // A one-second window lets the tests pick any epoch as "now" without the
    // clock: epoch == unix second. Epochs sit above HIGH, clear of the prune
    // test's range.
    AnonTokenServerConfig::from_values(Some(HEX), Some("1"))
        .unwrap()
        .unwrap()
}

fn header(value: &str) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    h.insert(TOKEN_HEADER, value.parse().unwrap());
    h
}

#[tokio::test]
async fn the_gate_refuses_before_the_store_when_disabled_or_malformed() {
    // A pool that is never reached: every refusal here happens before a spend.
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .connect_lazy("mysql://nobody@127.0.0.1:1/none")
        .unwrap();
    let cfg = gate_config();
    let master = cfg.master.clone();
    let now = HIGH + 500;
    let good = testing::mint_header(&master, TokenContext::DsKnock, now);

    let disabled = TokenVerifier::new(pool.clone(), None);
    assert_eq!(
        disabled
            .spend_header_at(&header(&good), TokenContext::DsKnock, now)
            .await
            .unwrap_err(),
        TokenRejection::Disabled
    );
    assert_eq!(
        disabled
            .spend_header_at(&axum::http::HeaderMap::new(), TokenContext::DsKnock, now)
            .await
            .unwrap_err(),
        TokenRejection::Disabled
    );

    let v = TokenVerifier::new(pool, Some(cfg));
    let refuse = |h: axum::http::HeaderMap, ctx: TokenContext, now: u64| {
        let v = v.clone();
        async move { v.spend_header_at(&h, ctx, now).await.unwrap_err() }
    };
    assert_eq!(
        refuse(axum::http::HeaderMap::new(), TokenContext::DsKnock, now).await,
        TokenRejection::Missing
    );
    assert_eq!(
        refuse(header("not-a-token"), TokenContext::DsKnock, now).await,
        TokenRejection::Malformed
    );
    assert_eq!(
        refuse(header(&good), TokenContext::PushRegister, now).await,
        TokenRejection::WrongContext
    );
    assert_eq!(
        refuse(header(&good), TokenContext::DsKnock, now + 2).await,
        TokenRejection::Expired
    );
    let other = testing::mint_header(
        &lakko_anon_token::MasterSecret::from_bytes([9; 32]),
        TokenContext::DsKnock,
        now,
    );
    assert_eq!(
        refuse(header(&other), TokenContext::DsKnock, now).await,
        TokenRejection::Invalid
    );
}

#[tokio::test]
async fn the_gate_spends_once_and_accepts_the_previous_epoch() {
    let Some(pool) = test_pool("the_gate_spends_once").await else {
        return;
    };
    let cfg = gate_config();
    let master = cfg.master.clone();
    let v = TokenVerifier::new(pool, Some(cfg));
    let now = HIGH + 1_000_000 + u64::from(random::<2>()[0]) * 10;

    let t = testing::mint_header(&master, TokenContext::InviteConsume, now);
    let ok = v
        .spend_header_at(&header(&t), TokenContext::InviteConsume, now)
        .await
        .unwrap();
    assert_eq!(ok.epoch, now);
    assert_eq!(
        v.spend_header_at(&header(&t), TokenContext::InviteConsume, now)
            .await
            .unwrap_err(),
        TokenRejection::Spent
    );

    let prev = testing::mint_header(&master, TokenContext::InviteConsume, now - 1);
    assert!(v
        .spend_header_at(&header(&prev), TokenContext::InviteConsume, now)
        .await
        .is_ok());
    let older = testing::mint_header(&master, TokenContext::InviteConsume, now - 2);
    assert_eq!(
        v.spend_header_at(&header(&older), TokenContext::InviteConsume, now)
            .await
            .unwrap_err(),
        TokenRejection::Expired
    );
}
