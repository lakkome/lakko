// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's second factors and the rule that the last one stays
//! (ADR-0073 and ADR-0075, amended 2026-11-26).
//!
//! A key reset needs the password and one second factor: a confirmed
//! authenticator, a recovery email, or a passkey. An account left with none
//! could never reset, so the routes that remove one refuse to remove the
//! last: `DELETE /recovery-email`, `DELETE /webauthn/credentials/{id}` and a
//! `PUT /auth/login-handle` that would drop the recovery email without
//! moving it answer `409 {"error":"last-factor"}`. The key reset itself
//! (which deletes the passkeys) and the recall (which clears the
//! authenticator and the passkeys) are exempt: their clients ask for a
//! factor before the app (UX `identity-recovery.md` 19.1c).
//!
//! The check runs inside the caller's transaction, after it took the account
//! row's lock, so two concurrent removals cannot both see the other factor.

use axum::http::StatusCode;
use sqlx::{MySql, Transaction};

use crate::webauthn::ApiError;

/// The `409` body of a removal that would leave the account no factor.
pub(crate) const CODE_LAST_FACTOR: &str = "last-factor";

/// What a route is about to remove.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Leaving<'a> {
    /// The recovery email (removed, or dropped by a change of login handle).
    RecoveryEmail,
    /// The passkey with this raw credential id.
    Passkey(&'a [u8]),
}

/// `(confirmed authenticator, recovery email, passkeys, this passkey)` of an
/// account.
type FactorRow = (i64, i64, i64, i64);

/// Refuse with `409 last-factor` when the factor `leaving` exists and is the
/// account's only one. A factor that does not exist removes nothing, so it
/// is never refused here. The caller holds the account row's lock.
///
/// # Errors
///
/// `409 {"error":"last-factor"}`; [`ApiError::Db`].
pub(crate) async fn refuse_if_last(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
    leaving: Leaving<'_>,
) -> Result<(), ApiError> {
    if is_last(tx, account_id, leaving)
        .await
        .map_err(ApiError::Db)?
    {
        tracing::info!("factor removal refused: it is the account's last");
        return Err(ApiError::Coded(StatusCode::CONFLICT, CODE_LAST_FACTOR));
    }
    Ok(())
}

/// Whether removing `leaving` would leave the account no second factor:
/// `true` only when that factor exists and nothing else does.
///
/// # Errors
///
/// Any `sqlx::Error` from the read.
pub(crate) async fn is_last(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
    leaving: Leaving<'_>,
) -> Result<bool, sqlx::Error> {
    let credential: &[u8] = match leaving {
        Leaving::Passkey(id) => id,
        Leaving::RecoveryEmail => &[],
    };
    let (totp, email, passkeys, this_passkey): FactorRow = sqlx::query_as(
        "SELECT \
            (SELECT COUNT(*) FROM totp_secrets \
              WHERE account_id = ? AND confirmed_at IS NOT NULL), \
            (SELECT COUNT(*) FROM recovery_emails WHERE account_id = ?), \
            (SELECT COUNT(*) FROM webauthn_credentials WHERE account_id = ?), \
            (SELECT COUNT(*) FROM webauthn_credentials \
              WHERE account_id = ? AND credential_id = ?)",
    )
    .bind(&account_id[..])
    .bind(&account_id[..])
    .bind(&account_id[..])
    .bind(&account_id[..])
    .bind(credential)
    .fetch_one(&mut **tx)
    .await?;
    let removed = match leaving {
        Leaving::RecoveryEmail => email,
        Leaving::Passkey(_) => this_passkey,
    };
    Ok(removed > 0 && totp + email + passkeys - removed <= 0)
}
