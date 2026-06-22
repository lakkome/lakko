// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A3 (GDPR right-to-erasure) — purge a discovery account's account-keyed rows.
//!
//! Part of the cross-service account deletion: `services/auth` purges the auth
//! tier (FK cascade over OPAQUE / passkeys / recovery factors), THIS purges the
//! discovery tier (username binding, identity bundle, KeyPackage, owned invite
//! capabilities), and `services/storage` purges the backup blob. The DS, push,
//! channels, transparency, and witness tiers are account-opaque (invariant #5 —
//! they key on group / device / channel / username-hash pseudonyms, never the
//! account_id) and hold nothing to delete.

use axum::extract::State;
use axum::http::StatusCode;

use crate::auth::AuthenticatedAccount;
use crate::AppState;

/// Delete every account-keyed discovery row for the authenticated account.
/// Bearer-auth via `require_session`: the `account_id` is taken from the
/// verified session token (not the request body), so a caller can only erase
/// their OWN discovery footprint. Idempotent — each `DELETE` is a no-op when the
/// row is already absent, so a retry after a partial failure completes cleanly.
pub(crate) async fn delete_account(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<AuthenticatedAccount>,
) -> Result<StatusCode, (StatusCode, String)> {
    let account_id = authed.0;
    let db_err = |e: sqlx::Error| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}"));

    // The username binding, the published identity bundle, and the MLS
    // KeyPackage are keyed on `account_id`.
    for sql in [
        "DELETE FROM usernames WHERE account_id = ?",
        "DELETE FROM identity_bundles WHERE account_id = ?",
        "DELETE FROM keypackages WHERE account_id = ?",
    ] {
        sqlx::query(sql)
            .bind(account_id.as_slice())
            .execute(&state.db)
            .await
            .map_err(db_err)?;
    }

    // Owned contact- and call-invite capabilities are keyed on
    // `owner_account_id`. (The per-joiner consumer rows are not account-keyed
    // and prune on their own TTL; they carry no link back to this account.)
    for sql in [
        "DELETE FROM invites WHERE owner_account_id = ?",
        "DELETE FROM call_invites WHERE owner_account_id = ?",
    ] {
        sqlx::query(sql)
            .bind(account_id.as_slice())
            .execute(&state.db)
            .await
            .map_err(db_err)?;
    }

    Ok(StatusCode::NO_CONTENT)
}
