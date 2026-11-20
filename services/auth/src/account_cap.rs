// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The beta account cap (ADR-0046 amendment of 2026-11-18).
//!
//! The public beta runs with at most `LAKKO_AUTH_MAX_ACCOUNTS` accounts that
//! have a username (`0` or unset: no cap). What counts is a row in
//! `login_handles`, so:
//!
//! - an account takes a place when it binds its **first** login handle; a
//!   change of handle or a re-bind of the held one is never refused;
//! - a guest from a call link has no handle, takes no place, and can still
//!   join calls when the cap is reached;
//! - an account refused at its first bind keeps no name and is removed by the
//!   unclaimed-account sweep (`src/unclaimed.rs`) a day after it was made;
//! - deleting an account deletes its handle (foreign key) and frees a place.
//!
//! A refused bind is `403 {"error":"registration-closed"}`. Before a person
//! solves the sign-up proof of work, the client asks
//! `GET /signup/open` -> `{"open": bool}`; that answer can be stale by the
//! time the bind runs, and the bind decides. The count adds nothing about any
//! person (invariant #1): it is a number of rows.

use axum::extract::State;
use axum::response::Json;
use sqlx::{MySql, Transaction};

use crate::webauthn::ApiError;
use crate::AppState;

/// Environment variable holding the cap.
pub(crate) const MAX_ACCOUNTS_VAR: &str = "LAKKO_AUTH_MAX_ACCOUNTS";

/// Error code of a first bind refused at the cap.
pub(crate) const CODE_REGISTRATION_CLOSED: &str = "registration-closed";

/// Read the cap. Unset or empty: `0` (no cap).
///
/// # Errors
///
/// When the variable is set and is not an unsigned integer.
pub(crate) fn max_from_env() -> anyhow::Result<u32> {
    match std::env::var(MAX_ACCOUNTS_VAR) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("{MAX_ACCOUNTS_VAR} must be an unsigned integer")),
        _ => Ok(0),
    }
}

/// Inside a first bind's transaction: take the cap lock and say whether one
/// more account with a handle fits. `max == 0` admits without locking.
///
/// The lock is taken after the account row's (see
/// `login_handle::bind_once`), and only first binds take it, so it adds no
/// lock-order cycle.
pub(crate) async fn admits_one_more(
    tx: &mut Transaction<'_, MySql>,
    max: u32,
) -> Result<bool, sqlx::Error> {
    if max == 0 {
        return Ok(true);
    }
    sqlx::query("SELECT id FROM account_cap_lock WHERE id = 1 FOR UPDATE")
        .fetch_optional(&mut **tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM login_handles")
        .fetch_one(&mut **tx)
        .await?;
    Ok(count < i64::from(max))
}

/// Whether a new account with a username fits now, outside any transaction.
async fn is_open(pool: &sqlx::MySqlPool, max: u32) -> Result<bool, sqlx::Error> {
    if max == 0 {
        return Ok(true);
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM login_handles")
        .fetch_one(pool)
        .await?;
    Ok(count < i64::from(max))
}

/// [`is_open`] for tests in other modules.
#[cfg(test)]
pub(crate) async fn is_open_for_test(pool: &sqlx::MySqlPool, max: u32) -> bool {
    is_open(pool, max).await.expect("count login handles")
}

/// `GET /signup/open` — `{"open": bool}`: whether sign-up is open now.
/// Anonymous, behind the per-address challenge bucket. Asked before the
/// sign-up proof of work, so nobody solves it only to be refused.
///
/// # Errors
///
/// [`ApiError::Db`].
pub(crate) async fn status(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let open = is_open(&state.db, state.limits.max_accounts())
        .await
        .map_err(ApiError::Db)?;
    Ok(Json(serde_json::json!({ "open": open })))
}

/// Log the cap once at startup.
pub(crate) fn log_startup(max: u32) {
    if max == 0 {
        tracing::info!(knob = MAX_ACCOUNTS_VAR, "auth account cap off");
    } else {
        tracing::info!(knob = MAX_ACCOUNTS_VAR, max, "auth account cap");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_cap_is_off() {
        // The variable is not set in the test environment of this crate.
        if std::env::var(MAX_ACCOUNTS_VAR).is_err() {
            assert_eq!(max_from_env().expect("cap"), 0);
        }
    }
}
