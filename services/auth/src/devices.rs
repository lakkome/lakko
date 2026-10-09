// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Removing a device: `POST /account/devices/revoke` ([ADR-0066] section
//! 2.1).
//!
//! Removing a device in Settings takes it out of the DeviceGroup and the
//! conversations; this route is what makes its **server session** stop. It
//! raises the removed device's session floor
//! (`device_session_floors`, reason "removed"), so from that moment every
//! access token and refresh credential the device holds is refused by auth
//! with `401 {"error":"device-revoked"}`, which the device answers by wiping
//! itself.
//!
//! ## Why two proofs
//!
//! The caller presents a bearer, **and** a fresh-auth grant (the person
//! proved who they are with the password or a passkey within the last ten
//! minutes), **and** an account statement signed by the account's identity
//! key (`lakko-attest`, kind `device-revoke`, over the target device id).
//!
//! The grant alone is not enough, and that is the point: a revoked device
//! wipes itself, so if the password sufficed, someone who knows only the
//! password could sign in on a device without keys and wipe every device the
//! owner has. Requiring a signature by the account key means only a device
//! that already holds the account's keys can remove another. A signed-in
//! device that has not restored its keys cannot remove devices; it can reset
//! the account keys instead.
//!
//! The key the statement is verified under is `accounts.identity_pubkey`,
//! recorded at the account's first genesis mint and moved only by a key
//! reset. An account with no recorded key yet cannot revoke by this route.
//!
//! [ADR-0066]: ../../../docs/adr/0066-account-takeover-resistance.md

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_attest::{device_revoke_body, verify_account_statement, AccountStatementKind};
use lakko_auth_token::UNBOUND_DEVICE_ID;
use lakko_crypto::signing::VerifyingKey;
use serde::Deserialize;
use sqlx::MySql;

use crate::limits::Scope;
use crate::session::{DeviceId, FLOOR_REASON_REMOVED};
use crate::webauthn::{ApiError, AuthenticatedAccount, AuthenticatedDevice};
use crate::AppState;

/// Body of `POST /account/devices/revoke`.
#[derive(Deserialize)]
pub(crate) struct RevokeReq {
    /// The `k1` fresh-auth grant a password or passkey sign-in returned.
    /// Optional on the wire so that its absence is the `403
    /// fresh-auth-required` every other refusal of the grant gets, not a
    /// deserialisation error.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
    /// URL-safe base64 of the 32-byte id of the device to remove.
    pub(crate) device_id_b64: String,
    /// URL-safe base64 of the 138-byte account statement: kind
    /// `device-revoke`, this account, body the device id, signed by the
    /// account's identity key.
    pub(crate) statement_b64: String,
}

/// Read the account's recorded identity public key: `None` when the account
/// does not exist or has recorded none yet.
///
/// # Errors
///
/// Any `sqlx::Error` from the read.
pub(crate) async fn identity_pubkey_of<'e, E>(
    executor: E,
    account_id: &[u8; 16],
) -> Result<Option<[u8; 32]>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let row: Option<Option<Vec<u8>>> =
        sqlx::query_scalar("SELECT identity_pubkey FROM accounts WHERE id = ?")
            .bind(&account_id[..])
            .fetch_optional(executor)
            .await?;
    Ok(row
        .flatten()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok()))
}

/// Verify a `device-revoke` account statement for `device_id` under the
/// account's recorded identity key. Every failure (no recorded key, bad
/// base64, a malformed statement, another kind, another account, outside the
/// time window, a bad signature, another device) is the one
/// [`ApiError::AccountKeyRequired`].
async fn require_revoke_statement(
    state: &AppState,
    account_id: &[u8; 16],
    device_id: &DeviceId,
    statement_b64: &str,
) -> Result<(), ApiError> {
    let refused = |why: &str| {
        tracing::debug!(why, "device revoke: account statement refused");
        ApiError::AccountKeyRequired
    };
    let statement = URL_SAFE_NO_PAD
        .decode(statement_b64)
        .map_err(|_| refused("statement is not URL-safe base64"))?;
    let recorded = identity_pubkey_of(&state.db, account_id)
        .await
        .map_err(ApiError::Db)?
        .ok_or_else(|| refused("the account has no recorded identity key"))?;
    let verifying = VerifyingKey::from_bytes(&recorded)
        .map_err(|_| refused("the recorded identity key does not decode"))?;
    verify_account_statement(
        &statement,
        &verifying,
        AccountStatementKind::DeviceRevoke,
        account_id,
        &device_revoke_body(device_id),
        crate::session::now_ms(),
    )
    .map_err(|e| {
        tracing::debug!(error = %e, "device revoke: account statement refused");
        ApiError::AccountKeyRequired
    })?;
    Ok(())
}

/// `POST /account/devices/revoke` — bearer. Answers `204`.
///
/// Checks, in this order:
///
/// 1. the fresh-auth grant: `403 fresh-auth-required`, before any limiter, so
///    a stolen session without the password or a passkey spends none of the
///    owner's budget;
/// 2. the body: the device id is 32 bytes (`400`), is not the all-zero
///    "no device" id (`400`), and is not the caller's own device (`400`: a
///    device signs itself out with `POST /auth/session/logout`);
/// 3. the account statement under the account's recorded identity key:
///    `403 account-key-required`;
/// 4. [`Scope::DeviceRevoke`] (`429`).
///
/// Then raises the target's session floor to now, reason "removed". The call
/// is idempotent: revoking a device twice leaves the later floor.
///
/// The server does not check that the id names a device the account has: it
/// keeps no registry of devices, and a floor for an id no credential names
/// refuses nothing.
///
/// # Errors
///
/// [`ApiError::FreshAuthRequired`], [`ApiError::BadInput`],
/// [`ApiError::AccountKeyRequired`], [`ApiError::Limit`], [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn revoke(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Extension(caller): Extension<AuthenticatedDevice>,
    Json(req): Json<RevokeReq>,
) -> Result<StatusCode, ApiError> {
    let account_id = auth.0;
    crate::session::require_fresh_grant(&state, &account_id, req.fresh_grant.as_deref()).await?;

    let device_id = crate::session::decode_device_id(Some(&req.device_id_b64))?;
    if device_id == UNBOUND_DEVICE_ID {
        return Err(ApiError::BadInput("device_id_b64 must not be all zero"));
    }
    if device_id == caller.device_id {
        return Err(ApiError::BadInput(
            "a device cannot revoke itself; log out instead",
        ));
    }

    require_revoke_statement(&state, &account_id, &device_id, &req.statement_b64).await?;

    crate::limits::charge_name(&state.db, &state.limits, Scope::DeviceRevoke, &account_id).await?;

    // Inside the generation fence (ADR-0066 "T3 as built"): the statement
    // above was verified under the key that was the account's a moment ago.
    // A floor written after a recall has moved that key would be the
    // displaced party removing the owner's device.
    let mut tx =
        crate::session::begin_at_generation(&state.db, &account_id, caller.key_generation).await?;
    crate::session::raise_floor(
        &mut *tx,
        &account_id,
        &device_id,
        crate::session::now_ms(),
        FLOOR_REASON_REMOVED,
    )
    .await
    .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;

    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        "device revoked",
    );
    Ok(StatusCode::NO_CONTENT)
}
