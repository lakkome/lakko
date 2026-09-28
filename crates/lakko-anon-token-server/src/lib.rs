// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Server-side bookkeeping for anonymous tokens ([ADR-0054] decision 2).
//!
//! The pure primitive (issuance, finalize, verify) is `lakko-anon-token`; this
//! crate carries the state around it, once, for every service that needs it -
//! the same split as `lakko-pow` / `lakko-pow-server`:
//!
//! - [`AnonTokenServerConfig`] - the master secret and the epoch window every
//!   issuing and verifying service must share, read from the environment;
//! - the issuer's per-account quota counter ([`reserve_issuance`]), used by
//!   `services/auth` only;
//! - a verifier's spent-set ([`spend`]), one table per consuming service;
//! - pruners for both, and the migration SQL both tables are created from.
//!
//! # Including the tables in a service
//!
//! sqlx migrations live in each service's own `migrations/` directory, and a
//! service owns its database. A service that issues copies
//! `migrations/anon_token_issuance.sql` from this crate into its migrations
//! directory under its next free number; a service that verifies does the same
//! with `migrations/anon_token_spent.sql`. The copy must stay byte-identical,
//! and the service pins it with a test:
//!
//! ```ignore
//! assert_eq!(
//!     include_str!("../migrations/00NN_anon_token_spent.sql"),
//!     lakko_anon_token_server::SPENT_SET_MIGRATION_SQL,
//! );
//! ```
//!
//! # Failure posture
//!
//! Both counters fail CLOSED on a store error, unlike the ADR-0033 and
//! ADR-0037 rate limits: the issuance quota is the one rationing point the
//! scheme rests on (a quota that fails open mints without limit), and a
//! spent-set that fails open lets one token be spent without limit. Callers
//! map an `Err` to a 5xx and do not honour the request.
//!
//! [ADR-0054]: ../../../docs/adr/0054-anti-abuse-without-client-work.md

#![forbid(unsafe_code)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lakko_anon_token::{epoch_at, MasterSecret, SpendId, TokenContext, DEFAULT_WINDOW_SECS};
use sqlx::MySqlPool;

/// Environment variable holding the master secret: 64 hex characters.
pub const MASTER_SECRET_VAR: &str = "LAKKO_ANON_TOKEN_MASTER_SECRET";
/// Environment variable holding the epoch window in seconds (default one day,
/// [`DEFAULT_WINDOW_SECS`]). Must be identical on every issuing and verifying
/// service.
pub const WINDOW_VAR: &str = "LAKKO_ANON_TOKEN_WINDOW_SECS";

/// The issuance-quota table schema (`services/auth` embeds a byte-identical
/// copy).
pub const ISSUANCE_MIGRATION_SQL: &str = include_str!("../migrations/anon_token_issuance.sql");
/// The spent-set table schema (every verifying service embeds a byte-identical
/// copy).
pub const SPENT_SET_MIGRATION_SQL: &str = include_str!("../migrations/anon_token_spent.sql");

/// Period between pruner runs.
const PRUNE_INTERVAL: Duration = Duration::from_secs(3_600);

/// A configuration value that is set but unusable. Startup should refuse to
/// continue on this, rather than run with a secret or window nobody intended.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The master secret is set but is not 64 hex characters.
    #[error("{MASTER_SECRET_VAR} must be 64 hex characters (32 bytes)")]
    InvalidMasterSecret,
    /// The window is set but is not a positive integer number of seconds.
    #[error("{WINDOW_VAR} must be a positive integer number of seconds")]
    InvalidWindow,
}

/// The configuration every issuing and verifying service shares.
#[derive(Debug, Clone)]
pub struct AnonTokenServerConfig {
    /// The shared master secret.
    pub master: MasterSecret,
    /// The epoch window in seconds (never zero).
    pub window_secs: u64,
}

impl AnonTokenServerConfig {
    /// Read the configuration from the environment.
    ///
    /// `Ok(None)` when the master secret is unset or empty: anonymous tokens
    /// are then dormant and the caller answers the token routes with 503.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when the master secret or the window is set but
    /// malformed. A malformed window is refused rather than defaulted, since a
    /// window that differs between services makes every token fail.
    pub fn from_env() -> Result<Option<Self>, ConfigError> {
        Self::from_values(
            std::env::var(MASTER_SECRET_VAR).ok().as_deref(),
            std::env::var(WINDOW_VAR).ok().as_deref(),
        )
    }

    /// [`Self::from_env`] over explicit values (testable without touching the
    /// process environment).
    ///
    /// # Errors
    ///
    /// See [`Self::from_env`].
    pub fn from_values(
        master_hex: Option<&str>,
        window: Option<&str>,
    ) -> Result<Option<Self>, ConfigError> {
        let Some(master_hex) = master_hex.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let master =
            MasterSecret::from_hex(master_hex).map_err(|_| ConfigError::InvalidMasterSecret)?;
        let window_secs = match window.map(str::trim).filter(|s| !s.is_empty()) {
            None => DEFAULT_WINDOW_SECS,
            Some(w) => match w.parse::<u64>() {
                Ok(v) if v > 0 => v,
                _ => return Err(ConfigError::InvalidWindow),
            },
        };
        Ok(Some(Self {
            master,
            window_secs,
        }))
    }

    /// The current epoch by the system clock.
    #[must_use]
    pub fn current_epoch(&self) -> u64 {
        current_epoch(self.window_secs)
    }

    /// The unix second at which the current epoch ends.
    #[must_use]
    pub fn current_epoch_ends_at(&self) -> u64 {
        self.current_epoch()
            .saturating_add(1)
            .saturating_mul(self.window_secs)
    }
}

/// Seconds since the unix epoch by the system clock (0 if the clock is before
/// 1970).
#[must_use]
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The current epoch under `window_secs` by the system clock. A zero window
/// (refused by [`AnonTokenServerConfig`]) reads as epoch 0.
#[must_use]
pub fn current_epoch(window_secs: u64) -> u64 {
    epoch_at(now_unix_secs(), window_secs).unwrap_or(0)
}

// ============================================================================
// Issuance quota (the issuer)
// ============================================================================

/// The outcome of an issuance reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssuanceDecision {
    /// The whole batch was counted; `remaining` tokens are left this epoch.
    Granted {
        /// Tokens still available to the account in this (context, epoch).
        remaining: u32,
    },
    /// The batch would exceed the quota and nothing was counted.
    Exhausted {
        /// Tokens still available (fewer than were asked for).
        remaining: u32,
    },
}

/// Count `count` tokens against `account_id`'s quota of `limit` for
/// `(context, epoch)`, all or nothing.
///
/// One transaction: create the row if absent, lock it, and add `count` only if
/// the total stays within `limit`. Concurrent requests from one account (two
/// devices refilling at once) are serialised on the row lock, so the quota
/// cannot be exceeded by a race. A `limit` of 0 issues nothing.
///
/// # Errors
///
/// Any `sqlx::Error`. The caller must fail closed on it (see the crate docs).
pub async fn reserve_issuance(
    pool: &MySqlPool,
    account_id: &[u8; 16],
    context: TokenContext,
    epoch: u64,
    count: u32,
    limit: u32,
) -> Result<IssuanceDecision, sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO anon_token_issuance (account_id, context, epoch, issued) \
         VALUES (?, ?, ?, 0) \
         ON DUPLICATE KEY UPDATE issued = issued",
    )
    .bind(&account_id[..])
    .bind(context.as_u8())
    .bind(epoch)
    .execute(&mut *tx)
    .await?;

    let issued: u32 = sqlx::query_scalar(
        "SELECT issued FROM anon_token_issuance \
         WHERE account_id = ? AND context = ? AND epoch = ? FOR UPDATE",
    )
    .bind(&account_id[..])
    .bind(context.as_u8())
    .bind(epoch)
    .fetch_one(&mut *tx)
    .await?;

    let remaining = limit.saturating_sub(issued);
    if count > remaining {
        tx.rollback().await?;
        return Ok(IssuanceDecision::Exhausted { remaining });
    }

    sqlx::query(
        "UPDATE anon_token_issuance SET issued = issued + ? \
         WHERE account_id = ? AND context = ? AND epoch = ?",
    )
    .bind(count)
    .bind(&account_id[..])
    .bind(context.as_u8())
    .bind(epoch)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(IssuanceDecision::Granted {
        remaining: remaining - count,
    })
}

/// Delete issuance rows older than the previous epoch.
///
/// # Errors
///
/// Any `sqlx::Error` from the DELETE.
pub async fn prune_issuance(pool: &MySqlPool, current_epoch: u64) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM anon_token_issuance WHERE epoch < ?")
        .bind(current_epoch.saturating_sub(1))
        .execute(pool)
        .await
        .map(|r| r.rows_affected())
}

/// Spawn the hourly issuance-row pruner. Aborts with the returned handle.
pub fn spawn_issuance_pruner(pool: MySqlPool, window_secs: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_INTERVAL);
        loop {
            tick.tick().await;
            match prune_issuance(&pool, current_epoch(window_secs)).await {
                Ok(n) if n > 0 => tracing::debug!(rows = n, "pruned anon_token_issuance"),
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "prune anon_token_issuance"),
            }
        }
    })
}

// ============================================================================
// Spent-set (a verifier)
// ============================================================================

/// Why a spend was refused.
#[derive(Debug, thiserror::Error)]
pub enum SpendError {
    /// This token was already spent at this service.
    #[error("anonymous token already spent")]
    AlreadySpent,
    /// The store failed. The caller must fail closed.
    #[error("spent-set store error: {0}")]
    Db(#[source] sqlx::Error),
}

/// Record a verified token's spend identifier; refuse if it is already there.
///
/// One INSERT on the `(context, spend_id)` primary key, so of two concurrent
/// spends of one token exactly one succeeds. Call it after
/// `lakko_anon_token::verify_token` and before doing the work the token pays
/// for.
///
/// # Errors
///
/// [`SpendError::AlreadySpent`] on a duplicate, [`SpendError::Db`] otherwise.
pub async fn spend(
    pool: &MySqlPool,
    context: TokenContext,
    spend_id: &SpendId,
    epoch: u64,
) -> Result<(), SpendError> {
    let res =
        sqlx::query("INSERT INTO anon_token_spent (context, spend_id, epoch) VALUES (?, ?, ?)")
            .bind(context.as_u8())
            .bind(&spend_id[..])
            .bind(epoch)
            .execute(pool)
            .await;
    match res {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(SpendError::AlreadySpent),
        Err(e) => Err(SpendError::Db(e)),
    }
}

/// Delete spent rows older than the previous epoch: those tokens no longer
/// verify, so they cannot be replayed.
///
/// # Errors
///
/// Any `sqlx::Error` from the DELETE.
pub async fn prune_spent(pool: &MySqlPool, current_epoch: u64) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM anon_token_spent WHERE epoch < ?")
        .bind(current_epoch.saturating_sub(1))
        .execute(pool)
        .await
        .map(|r| r.rows_affected())
}

/// Spawn the hourly spent-set pruner. Aborts with the returned handle.
pub fn spawn_spent_pruner(pool: MySqlPool, window_secs: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_INTERVAL);
        loop {
            tick.tick().await;
            match prune_spent(&pool, current_epoch(window_secs)).await {
                Ok(n) if n > 0 => tracing::debug!(rows = n, "pruned anon_token_spent"),
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "prune anon_token_spent"),
            }
        }
    })
}

#[cfg(test)]
mod tests;
