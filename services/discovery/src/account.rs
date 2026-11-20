// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A3 (GDPR right-to-erasure) — purge a discovery account's account-keyed rows.
//!
//! Part of the cross-service account deletion: `services/auth` purges the auth
//! tier (FK cascade over OPAQUE / passkeys / recovery factors), THIS purges the
//! discovery tier (username binding, identity bundle, KeyPackage, name search
//! terms), and `services/storage` purges the backup blob.
//!
//! Invite ledger rows are not deleted here: since ADR-0078 section 10 they name
//! no owner, so there is nothing to find them by. A deleting client revokes its
//! own live links first (it holds each link's revocation seed); a row it could
//! not revoke lives until its expiry or its last use. The DS, push, channels, transparency, and witness tiers are
//! account-opaque (invariant #5 — they key on group / device / channel /
//! username-hash pseudonyms, never the account_id) and hold nothing to delete.

use axum::extract::State;
use axum::http::StatusCode;

use crate::auth::AuthenticatedAccount;
use crate::AppState;

/// Delete every account-keyed discovery row for the authenticated account.
/// Bearer-auth via `require_session`: the `account_id` is taken from the
/// verified session token (not the request body), so a caller can only erase
/// their OWN discovery footprint. Idempotent — each `DELETE` is a no-op when the
/// row is already absent, so a retry after a partial failure completes cleanly.
///
/// `services/auth` also purges these rows itself after it deleted an account,
/// through the internal listener (`crate::internal`), which runs the same
/// [`purge_account`].
pub(crate) async fn delete_account(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<AuthenticatedAccount>,
) -> Result<StatusCode, (StatusCode, String)> {
    purge_account(&state.db, &authed.0)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Delete every account-keyed discovery row of `account_id`: its names (and
/// their one-namespace claims), its identity bundle, and everything
/// [`purge_owned_rows`] removes. Idempotent.
///
/// # Errors
///
/// A database error; the deletes already run stay done, and running it again
/// finishes the rest.
pub(crate) async fn purge_account(
    pool: &sqlx::MySqlPool,
    account_id: &[u8; 16],
) -> Result<(), sqlx::Error> {
    // The username binding and the published identity bundle are keyed on
    // `account_id`; so is everything `purge_owned_rows` removes.
    for sql in [
        // The one-namespace claims of its names (ADR-0087), while the rows
        // still say which names they are; then every row, current and
        // retired (ADR-0076). The handles of groups and channels the account
        // owns are not found here (a handle names no account): the client
        // retires them before deleting the account.
        "DELETE FROM name_claims WHERE lookup_key IN \
           (SELECT oprf_hash FROM usernames WHERE account_id = ?)",
        "DELETE FROM usernames WHERE account_id = ?",
        "DELETE FROM identity_bundles WHERE account_id = ?",
    ] {
        delete_keyed(pool, sql, account_id).await?;
    }
    purge_owned_rows(pool, account_id).await
}

/// Everything an account owns here besides its username binding and its
/// identity bundle: the MLS KeyPackages, the ADR-0060 search terms and public
/// record, a moderator's hide (ADR-0098). Invite ledger rows are not among them (ADR-0078 section 10: no row
/// names its owner).
async fn purge_owned_rows(
    pool: &sqlx::MySqlPool,
    account_id: &[u8; 16],
) -> Result<(), sqlx::Error> {
    for sql in [
        "DELETE FROM keypackages WHERE account_id = ?",
        // ADR-0074 - the tombstones of device ids deleted from the directory.
        "DELETE FROM keypackage_tombstones WHERE account_id = ?",
        "DELETE FROM search_terms WHERE kind = 1 AND target = ?",
        "DELETE FROM public_records WHERE kind = 1 AND target = ?",
        // ADR-0098 step 5 - a moderator's hide of this account.
        "DELETE FROM moderation_hidden WHERE account_id = ?",
    ] {
        delete_keyed(pool, sql, account_id).await?;
    }
    Ok(())
}

/// Run one account-keyed `DELETE` on its own READ COMMITTED connection, again
/// when it meets a deadlock or a lock-wait timeout (at most
/// [`crate::handles::LOCK_ATTEMPTS`] times in all).
///
/// Under the default REPEATABLE READ a `DELETE FROM search_terms WHERE kind
/// = 1 AND target = ?` takes next-key locks on the primary key, whose gaps a
/// concurrent publish of another target inserts into; the two deadlocked
/// (MariaDB 1213, seen in the tests as a `500` on `DELETE
/// /discovery/account`). READ COMMITTED takes no gap locks, as `PUT
/// /discovery/visibility` already does for the same table.
async fn delete_keyed(
    pool: &sqlx::MySqlPool,
    sql: &'static str,
    account_id: &[u8; 16],
) -> Result<(), sqlx::Error> {
    let mut tries = 1;
    loop {
        let attempt = async {
            let mut conn = pool.acquire().await?;
            sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
                .execute(&mut *conn)
                .await?;
            sqlx::query(sql)
                .bind(account_id.as_slice())
                .execute(&mut *conn)
                .await
        };
        match attempt.await {
            Err(e)
                if tries < crate::handles::LOCK_ATTEMPTS
                    && crate::identity_bundle::is_lock_conflict(&e) =>
            {
                tries += 1;
            },
            Err(e) => return Err(e),
            Ok(_) => return Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Accounts nobody kept (ADR-0032 guest lifecycle)
// ---------------------------------------------------------------------------

/// The environment knob: seconds an unclaimed account's rows live here. `0`
/// turns the pruner off.
pub(crate) const UNCLAIMED_TTL_VAR: &str = "LAKKO_DISCOVERY_UNCLAIMED_ACCOUNT_TTL_SECS";

/// One day, the same lifetime `services/auth` gives the account itself.
pub(crate) const DEFAULT_UNCLAIMED_TTL_SECS: u64 = 86_400;

const UNCLAIMED_PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);

/// The most accounts one pass removes; the next pass takes the rest.
const UNCLAIMED_PRUNE_BATCH: u32 = 500;

/// Read the lifetime from the environment.
///
/// # Errors
///
/// When the variable is set but is not a whole number of seconds.
pub(crate) fn unclaimed_ttl_from_env() -> anyhow::Result<u64> {
    parse_unclaimed_ttl(std::env::var(UNCLAIMED_TTL_VAR).ok().as_deref()).map_err(|raw| {
        anyhow::anyhow!("{UNCLAIMED_TTL_VAR} must be a whole number of seconds, got {raw:?}")
    })
}

fn parse_unclaimed_ttl(raw: Option<&str>) -> Result<u64, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(DEFAULT_UNCLAIMED_TTL_SECS),
        Some(text) => text.parse::<u64>().map_err(|_| text.to_owned()),
    }
}

/// Remove the rows of accounts that published an identity bundle more than
/// `ttl_secs` ago and never registered a username. Returns how many accounts
/// were removed.
///
/// A person who joins a call from a link without an account gets one
/// silently and decides after the call whether to keep it; keeping it means
/// choosing a username. `services/auth` deletes the account itself after the
/// same lifetime; this removes what it published here, which the auth service
/// cannot reach (each service has its own database).
///
/// The bundle is deleted first and only while the account still has no
/// username, so a person who chooses a name between the read and the delete
/// keeps everything: their rows are left alone.
///
/// # Errors
///
/// A database error.
pub(crate) async fn prune_unclaimed(
    pool: &sqlx::MySqlPool,
    ttl_secs: u64,
) -> Result<u64, sqlx::Error> {
    let candidates: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT b.account_id FROM identity_bundles b \
         WHERE b.created_at < NOW() - INTERVAL ? SECOND \
           AND NOT EXISTS (SELECT 1 FROM usernames u WHERE u.account_id = b.account_id) \
         LIMIT ?",
    )
    .bind(ttl_secs)
    .bind(UNCLAIMED_PRUNE_BATCH)
    .fetch_all(pool)
    .await?;

    let mut removed = 0u64;
    for raw in candidates {
        let Ok(account_id) = <[u8; 16]>::try_from(raw.as_slice()) else {
            continue;
        };
        let gone = sqlx::query(
            "DELETE FROM identity_bundles \
             WHERE account_id = ? \
               AND NOT EXISTS (SELECT 1 FROM usernames u WHERE u.account_id = identity_bundles.account_id)",
        )
        .bind(account_id.as_slice())
        .execute(pool)
        .await?
        .rows_affected();
        if gone == 0 {
            continue;
        }
        purge_owned_rows(pool, &account_id).await?;
        removed += 1;
    }
    Ok(removed)
}

/// Start the pruner; `None` when the lifetime is `0`.
pub(crate) fn spawn_unclaimed_pruner(
    pool: sqlx::MySqlPool,
    ttl_secs: u64,
) -> Option<tokio::task::JoinHandle<()>> {
    if ttl_secs == 0 {
        tracing::info!("unclaimed-account pruner is off");
        return None;
    }
    tracing::info!(ttl_secs, "unclaimed-account pruner is on");
    lakko_metrics::job_registered(crate::metrics::jobs::UNCLAIMED_ACCOUNT_PRUNE);
    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(UNCLAIMED_PRUNE_INTERVAL);
        // The first tick fires at once; skip it so a restart is not a prune.
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = prune_unclaimed(&pool, ttl_secs).await;
            crate::metrics::job_run(crate::metrics::jobs::UNCLAIMED_ACCOUNT_PRUNE, &result);
            match result {
                Ok(0) => {},
                Ok(removed) => tracing::info!(removed, "pruned unclaimed accounts"),
                Err(e) => tracing::warn!(error = %e, "unclaimed-account prune failed"),
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lifetime_defaults_to_a_day_and_zero_means_off() {
        assert_eq!(parse_unclaimed_ttl(None), Ok(DEFAULT_UNCLAIMED_TTL_SECS));
        assert_eq!(
            parse_unclaimed_ttl(Some(" ")),
            Ok(DEFAULT_UNCLAIMED_TTL_SECS)
        );
        assert_eq!(parse_unclaimed_ttl(Some("0")), Ok(0));
        assert_eq!(parse_unclaimed_ttl(Some("7200")), Ok(7200));
        assert!(parse_unclaimed_ttl(Some("soon")).is_err());
    }

    /// An id no other test run has used: the clock and a counter.
    fn unique_id() -> [u8; 16] {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(
            &u64::try_from(nanos & u128::from(u64::MAX))
                .unwrap_or(0)
                .to_be_bytes(),
        );
        id[8..].copy_from_slice(&NEXT.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        id
    }

    async fn bundle(pool: &sqlx::MySqlPool, age_secs: u64) -> [u8; 16] {
        let id = unique_id();
        sqlx::query(
            "INSERT INTO identity_bundles (account_id, signature_pubkey, x25519_pubkey, created_at) \
             VALUES (?, ?, ?, NOW() - INTERVAL ? SECOND)",
        )
        .bind(id.as_slice())
        .bind(&[7u8; 32][..])
        .bind(&[9u8; 32][..])
        .bind(age_secs)
        .execute(pool)
        .await
        .expect("insert bundle");
        sqlx::query(
            "INSERT INTO keypackages (account_id, device_id, keypackage_bytes) VALUES (?, ?, ?)",
        )
        .bind(id.as_slice())
        .bind(&[3u8; 32][..])
        .bind(&[1u8; 16][..])
        .execute(pool)
        .await
        .expect("insert keypackage");
        id
    }

    #[derive(Clone, Copy)]
    enum Table {
        Usernames,
        Bundles,
        KeyPackages,
    }

    async fn count(pool: &sqlx::MySqlPool, table: Table, id: &[u8; 16]) -> i64 {
        let sql = match table {
            Table::Usernames => "SELECT COUNT(*) FROM usernames WHERE account_id = ?",
            Table::Bundles => "SELECT COUNT(*) FROM identity_bundles WHERE account_id = ?",
            Table::KeyPackages => "SELECT COUNT(*) FROM keypackages WHERE account_id = ?",
        };
        sqlx::query_scalar(sql)
            .bind(id.as_slice())
            .fetch_one(pool)
            .await
            .expect("count")
    }

    async fn clear(pool: &sqlx::MySqlPool, table: Table, id: &[u8; 16]) {
        let sql = match table {
            Table::Usernames => "DELETE FROM usernames WHERE account_id = ?",
            Table::Bundles => "DELETE FROM identity_bundles WHERE account_id = ?",
            Table::KeyPackages => "DELETE FROM keypackages WHERE account_id = ?",
        };
        sqlx::query(sql)
            .bind(id.as_slice())
            .execute(pool)
            .await
            .expect("cleanup");
    }

    /// `LAKKO_DISCOVERY_TEST_DATABASE_URL`-gated; skips without it.
    #[tokio::test]
    async fn only_an_old_account_without_a_username_is_removed() {
        let Some(pool) = crate::anon_token_route_tests::test_pool("unclaimed_prune").await else {
            return;
        };
        let day = DEFAULT_UNCLAIMED_TTL_SECS;
        let abandoned = bundle(&pool, day + 60).await;
        let fresh_guest = bundle(&pool, 60).await;
        let named = bundle(&pool, day + 60).await;
        let mut hash = [0u8; 64];
        hash[..16].copy_from_slice(&unique_id());
        sqlx::query("INSERT INTO usernames (oprf_hash, account_id) VALUES (?, ?)")
            .bind(&hash[..])
            .bind(named.as_slice())
            .execute(&pool)
            .await
            .expect("insert username");

        prune_unclaimed(&pool, day).await.expect("prune");

        assert_eq!(count(&pool, Table::Bundles, &abandoned).await, 0);
        assert_eq!(count(&pool, Table::KeyPackages, &abandoned).await, 0);
        assert_eq!(count(&pool, Table::Bundles, &fresh_guest).await, 1);
        assert_eq!(count(&pool, Table::KeyPackages, &fresh_guest).await, 1);
        assert_eq!(count(&pool, Table::Bundles, &named).await, 1);
        assert_eq!(count(&pool, Table::KeyPackages, &named).await, 1);
        assert_eq!(count(&pool, Table::Usernames, &named).await, 1);

        for id in [fresh_guest, named] {
            for table in [Table::Usernames, Table::Bundles, Table::KeyPackages] {
                clear(&pool, table, &id).await;
            }
        }
    }
}
