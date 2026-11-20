// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Accounts nobody kept are deleted after a day.
//!
//! A person who joins a call from a link without an account is given one
//! silently ([ADR-0032](../../../docs/adr/0032-multi-joiner-call-links.md),
//! guest join): the server will not admit a caller it cannot name, and the
//! call's encryption needs an identity. Whether they keep it is decided
//! after the call. Keeping it means choosing a name, which binds a login
//! handle (`login_handle.rs`); declining deletes the account at once through
//! `DELETE /account`.
//!
//! This module is for the third case: the person closed the tab and decided
//! nothing. An account that still has **no login handle and no passkey** a
//! day after it was created cannot be signed into by anyone, so it is
//! removed. The foreign keys cascade over every account-keyed auth table,
//! and the purge of what the account left in discovery and storage is
//! queued in the same transaction (`crate::purge`).
//!
//! Two guards keep the rule from deleting accounts it does not describe:
//!
//! - it runs only where sign-in by name is configured (the handle pepper is
//!   set). Without the pepper no account has a handle, and "no handle" would
//!   mean every account;
//! - a passkey counts as kept, whatever else is missing.
//!
//! The age is read from `accounts.created_at`, which the server sets; a
//! client cannot make an account look younger. The limit (a day) is chosen,
//! not measured.

use std::time::Duration;

use sqlx::MySqlPool;

/// The environment knob: seconds an unclaimed account lives. `0` turns the
/// pruner off.
pub(crate) const TTL_VAR: &str = "LAKKO_AUTH_UNCLAIMED_ACCOUNT_TTL_SECS";

/// One day.
pub(crate) const DEFAULT_TTL_SECS: u64 = 86_400;

/// How often the pruner looks.
const PRUNE_INTERVAL: Duration = Duration::from_secs(600);

/// The most accounts one pass deletes, so a backlog never holds a long
/// transaction; the next pass takes the rest.
const PRUNE_BATCH: u32 = 500;

/// Read the lifetime from the environment.
///
/// # Errors
///
/// When the variable is set but is not a whole number of seconds.
pub(crate) fn ttl_from_env() -> anyhow::Result<u64> {
    parse_ttl(std::env::var(TTL_VAR).ok().as_deref())
        .map_err(|raw| anyhow::anyhow!("{TTL_VAR} must be a whole number of seconds, got {raw:?}"))
}

/// Unset or blank is the default; a number is itself; anything else is
/// returned as the error.
fn parse_ttl(raw: Option<&str>) -> Result<u64, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(DEFAULT_TTL_SECS),
        Some(text) => text.parse::<u64>().map_err(|_| text.to_owned()),
    }
}

/// Whether `account_id` is an account nobody named: no login handle and no
/// passkey. Such an account may be deleted on its session alone.
///
/// # Errors
///
/// A database error.
pub(crate) async fn is_unclaimed(
    pool: &MySqlPool,
    account_id: &[u8; 16],
) -> Result<bool, sqlx::Error> {
    let named: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM accounts a WHERE a.id = ? AND ( \
           EXISTS (SELECT 1 FROM login_handles h WHERE h.account_id = a.id) \
           OR EXISTS (SELECT 1 FROM webauthn_credentials w WHERE w.account_id = a.id)) \
         LIMIT 1",
    )
    .bind(account_id.as_slice())
    .fetch_optional(pool)
    .await?;
    Ok(named.is_none())
}

/// Whether `account_id` is older than the unclaimed-account lifetime. Past
/// it, releasing the login handle needs a fresh-auth grant: without the
/// handle the account would be unclaimed, deletable on a session alone and
/// taken by the sweep, which would let a stolen session destroy a named
/// account around the fresh-auth rule and around an open recall window.
/// A younger account (a sign-up or a guest claim being undone) releases
/// freely. The lifetime is the configured one, or the default when the
/// sweep is off or the variable is malformed.
///
/// # Errors
///
/// A database error.
pub(crate) async fn is_past_unclaimed_lifetime(
    pool: &MySqlPool,
    account_id: &[u8; 16],
) -> Result<bool, sqlx::Error> {
    let ttl = match ttl_from_env() {
        Ok(0) | Err(_) => DEFAULT_TTL_SECS,
        Ok(secs) => secs,
    };
    let old: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM accounts WHERE id = ? AND created_at < NOW() - INTERVAL ? SECOND",
    )
    .bind(account_id.as_slice())
    .bind(ttl)
    .fetch_optional(pool)
    .await?;
    Ok(old.is_some())
}

/// Delete up to [`PRUNE_BATCH`] accounts older than `ttl_secs` that have no
/// login handle and no passkey, and queue the purge of each one's discovery
/// rows and backup blob (`crate::purge`) in the same transaction. Returns how
/// many were deleted.
///
/// The candidates are locked first; each delete re-checks the rule, so an
/// account that gained a handle or a passkey before its lock was taken is
/// left alone (both tables reference `accounts`, so a bind after the lock
/// waits for this transaction).
///
/// # Errors
///
/// A database error; nothing is deleted or queued then.
pub(crate) async fn prune_unclaimed(pool: &MySqlPool, ttl_secs: u64) -> Result<u64, sqlx::Error> {
    // One transaction: the batch is chosen and locked, each account's
    // identity-key history is deleted or kept (ADR-0099 F3: kept while the
    // account is suspended), then the accounts go and their purge is queued.
    let mut conn = crate::session::read_committed_conn(pool).await?;
    let mut tx = sqlx::Acquire::begin(&mut conn).await?;
    let candidates: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT id FROM accounts \
         WHERE created_at < NOW() - INTERVAL ? SECOND \
           AND NOT EXISTS (SELECT 1 FROM login_handles h WHERE h.account_id = accounts.id) \
           AND NOT EXISTS (SELECT 1 FROM webauthn_credentials w WHERE w.account_id = accounts.id) \
         LIMIT ? FOR UPDATE",
    )
    .bind(ttl_secs)
    .bind(PRUNE_BATCH)
    .fetch_all(&mut *tx)
    .await?;

    let now = crate::session::now_ms();
    let mut deleted = 0u64;
    for raw in candidates {
        let Ok(id) = <[u8; 16]>::try_from(raw.as_slice()) else {
            continue;
        };
        // Re-check the rule before the history is touched, so an account
        // that is not deleted keeps its history as it was.
        let still: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM accounts WHERE id = ? \
               AND NOT EXISTS (SELECT 1 FROM login_handles h WHERE h.account_id = accounts.id) \
               AND NOT EXISTS (SELECT 1 FROM webauthn_credentials w WHERE w.account_id = accounts.id)",
        )
        .bind(id.as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        if still.is_none() {
            continue;
        }
        crate::identity_target::on_account_deleted(&mut tx, &id, now).await?;
        let gone = sqlx::query(
            "DELETE FROM accounts WHERE id = ? \
               AND NOT EXISTS (SELECT 1 FROM login_handles h WHERE h.account_id = accounts.id) \
               AND NOT EXISTS (SELECT 1 FROM webauthn_credentials w WHERE w.account_id = accounts.id)",
        )
        .bind(id.as_slice())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if gone == 0 {
            continue;
        }
        crate::purge::enqueue(&mut tx, &id).await?;
        deleted += 1;
    }
    tx.commit().await?;
    Ok(deleted)
}

/// Start the pruner, or not: `None` when sign-in by name is not configured or
/// the lifetime is `0`.
pub(crate) fn spawn_pruner(
    pool: MySqlPool,
    names_enabled: bool,
    ttl_secs: u64,
) -> Option<tokio::task::JoinHandle<()>> {
    if !names_enabled || ttl_secs == 0 {
        tracing::info!(names_enabled, ttl_secs, "unclaimed-account pruner is off");
        return None;
    }
    tracing::info!(ttl_secs, "unclaimed-account pruner is on");
    let job = crate::metrics::jobs::UNCLAIMED_ACCOUNT_PRUNE;
    lakko_metrics::job_registered(job);
    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        // The first tick fires at once; skip it so a restart is not a prune.
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = prune_unclaimed(&pool, ttl_secs).await;
            crate::metrics::job_ran(job, result.is_ok());
            match result {
                Ok(0) => {},
                Ok(rows) => tracing::info!(rows, "pruned unclaimed accounts"),
                Err(e) => tracing::warn!(error = %e, "unclaimed-account prune failed"),
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lifetime_defaults_to_a_day() {
        assert_eq!(parse_ttl(None), Ok(DEFAULT_TTL_SECS));
        assert_eq!(parse_ttl(Some("")), Ok(DEFAULT_TTL_SECS));
        assert_eq!(parse_ttl(Some("  ")), Ok(DEFAULT_TTL_SECS));
    }

    #[test]
    fn a_number_is_taken_as_seconds_and_zero_means_off() {
        assert_eq!(parse_ttl(Some("3600")), Ok(3600));
        assert_eq!(parse_ttl(Some(" 0 ")), Ok(0));
    }

    #[test]
    fn anything_else_is_an_error_not_a_default() {
        assert!(parse_ttl(Some("1d")).is_err());
        assert!(parse_ttl(Some("-5")).is_err());
    }

    /// `LAKKO_AUTH_TEST_DATABASE_URL`-gated; skips without it.
    async fn test_pool() -> Option<MySqlPool> {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping unclaimed-account test: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return None;
        };
        let pool = MySqlPool::connect(&url).await.expect("connect");
        crate::MIGRATIONS.run(&pool).await.expect("migrate");
        Some(pool)
    }

    async fn account(pool: &MySqlPool, age_secs: u64) -> [u8; 16] {
        let id = *uuid::Uuid::now_v7().as_bytes();
        sqlx::query("INSERT INTO accounts (id, created_at) VALUES (?, NOW() - INTERVAL ? SECOND)")
            .bind(id.as_slice())
            .bind(age_secs)
            .execute(pool)
            .await
            .expect("insert account");
        id
    }

    async fn exists(pool: &MySqlPool, id: &[u8; 16]) -> bool {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM accounts WHERE id = ?")
            .bind(id.as_slice())
            .fetch_one(pool)
            .await
            .expect("count")
            == 1
    }

    #[tokio::test]
    async fn only_an_old_account_with_no_handle_and_no_passkey_is_deleted() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let day = DEFAULT_TTL_SECS;

        let abandoned = account(&pool, day + 60).await;
        let fresh_guest = account(&pool, 60).await;
        let named = account(&pool, day + 60).await;
        let with_passkey = account(&pool, day + 60).await;

        sqlx::query("INSERT INTO login_handles (handle_key, account_id) VALUES (?, ?)")
            .bind(uuid::Uuid::now_v7().as_bytes().repeat(2))
            .bind(named.as_slice())
            .execute(&pool)
            .await
            .expect("bind handle");
        sqlx::query(
            "INSERT INTO webauthn_credentials (credential_id, account_id, passkey_bytes) \
             VALUES (?, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().as_bytes().as_slice())
        .bind(with_passkey.as_slice())
        .bind(&[0u8; 8][..])
        .execute(&pool)
        .await
        .expect("insert passkey");

        prune_unclaimed(&pool, day).await.expect("prune");

        assert!(
            !exists(&pool, &abandoned).await,
            "an abandoned account stays"
        );
        assert!(
            crate::purge::is_queued(&pool, &abandoned).await,
            "the deleted account's purge is not queued"
        );
        for kept in [&fresh_guest, &named, &with_passkey] {
            assert!(
                !crate::purge::is_queued(&pool, kept).await,
                "a kept account was queued for purge"
            );
        }
        sqlx::query("DELETE FROM account_purges WHERE account_id = ?")
            .bind(abandoned.as_slice())
            .execute(&pool)
            .await
            .expect("cleanup purge row");
        assert!(
            exists(&pool, &fresh_guest).await,
            "a guest of a minute ago was deleted"
        );
        assert!(
            exists(&pool, &named).await,
            "an account with a name was deleted"
        );
        assert!(
            exists(&pool, &with_passkey).await,
            "an account with a passkey was deleted"
        );

        for id in [fresh_guest, named, with_passkey] {
            sqlx::query("DELETE FROM accounts WHERE id = ?")
                .bind(id.as_slice())
                .execute(&pool)
                .await
                .expect("cleanup");
        }
    }
}
