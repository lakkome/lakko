// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The second factor of a key reset ([ADR-0073], amended 2026-11-26).
//!
//! `POST /account/keys/reset/{start,finish}` need the password AND one
//! second factor the account has. Each route here first requires a
//! fresh-auth grant minted by the PASSWORD (a password sign-in within the
//! grant's ten minutes; a passkey sign-in does not stand in for it: `403
//! {"error":"fresh-auth-required"}`, checked before anything is charged),
//! then checks its factor and answers `{reset_grant,
//! reset_grant_expires_at_ms}`: an `f1` reset factor grant of
//! `lakko-auth-token`, valid `LAKKO_RESET_FACTOR_GRANT_TTL_SECS` (600 by
//! default, chosen, not measured), naming the factor.
//!
//! | Route (bearer) | Body | Factor |
//! |---|---|---|
//! | `POST /account/keys/reset/factor/totp` | `{fresh_grant, code}` | a code of the account's confirmed authenticator (`t`); charged to [`Scope::ResetFactor`] before the check; the matched step is consumed |
//! | `POST /account/keys/reset/factor/email/start` | `{fresh_grant, handle, address, lang?}` | see `crate::recovery_email` |
//! | `POST /account/keys/reset/factor/email/finish` | `{fresh_grant, challenge_id, code}` | a mailed code (`e`) |
//! | `POST /account/keys/reset/factor/passkey` | `{fresh_grant}` | an ADR-0014 action grant for [`ACTION_DOMAIN_RESET_KEYS`] over the account id, obtained through `/auth/action/{start,finish}` (`w`) |
//!
//! A factor that does not verify (a wrong or replayed code, a decoy or spent
//! challenge, no such factor on the account, no action grant) is the one
//! `401 {"error":"factor-refused"}`: the client offers only the factors the
//! account has.
//!
//! [ADR-0073]: ../../../docs/adr/0073-account-key-reset.md

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use lakko_auth_token::ResetFactor;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::limits::Scope;
use crate::session::ResetFactorGrant;
use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// The `401` body of a factor that did not verify.
pub(crate) const CODE_FACTOR_REFUSED: &str = "factor-refused";

/// The ADR-0014 action domain of the passkey reset factor. The clients'
/// constant must match byte for byte.
pub(crate) const ACTION_DOMAIN_RESET_KEYS: &str = "lakko/action/reset-keys/v1";

/// `401 {"error":"factor-refused"}`.
pub(crate) const fn factor_refused() -> ApiError {
    ApiError::Coded(StatusCode::UNAUTHORIZED, CODE_FACTOR_REFUSED)
}

/// The action hash a passkey reset-factor ceremony binds to:
/// `SHA-256(ACTION_DOMAIN_RESET_KEYS ‖ account_id)`, the delete-account
/// shape.
pub(crate) fn reset_keys_action_hash(account_id: &[u8; 16]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ACTION_DOMAIN_RESET_KEYS.as_bytes());
    hasher.update(account_id);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Body of `POST /account/keys/reset/factor/totp`.
#[derive(Deserialize)]
pub(crate) struct TotpReq {
    /// The `k1` fresh-auth grant of a PASSWORD sign-in.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
    /// The six digits the authenticator shows.
    pub(crate) code: String,
}

/// `POST /account/keys/reset/factor/totp` — bearer + a fresh-auth grant of
/// the password. Order: the grant (`403`), [`Scope::ResetFactor`] for the
/// account (`429 too-many-attempts`), the code (`401 factor-refused`; `503`
/// when the server has no authenticator key, which `main` refuses to start
/// without), the grant.
///
/// # Errors
///
/// As listed; [`ApiError::Db`]; [`ApiError::Internal`].
#[axum::debug_handler]
pub(crate) async fn totp(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<TotpReq>,
) -> Result<Json<ResetFactorGrant>, ApiError> {
    let account_id = auth.0;
    let generation =
        crate::session::require_password_grant(&state, &account_id, req.fresh_grant.as_deref())
            .await?;
    crate::limits::charge_name(&state.db, &state.limits, Scope::ResetFactor, &account_id).await?;
    if !crate::totp::check_code(&state, &account_id, &req.code).await? {
        return Err(factor_refused());
    }
    tracing::info!("reset factor: authenticator code accepted");
    Ok(Json(crate::session::issue_reset_factor_grant(
        &state,
        &account_id,
        generation,
        ResetFactor::Authenticator,
    )?))
}

/// Body of `POST /account/keys/reset/factor/passkey`.
#[derive(Deserialize)]
pub(crate) struct PasskeyReq {
    /// The `k1` fresh-auth grant of a PASSWORD sign-in.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
}

/// `POST /account/keys/reset/factor/passkey` — bearer + a fresh-auth grant
/// of the password. Consumes the account's single-use ADR-0014 action grant
/// for [`reset_keys_action_hash`] (written by `/auth/action/finish` after an
/// assertion by one of the account's passkeys); none is `401
/// factor-refused`. A passkey that carries no keys counts; the reset deletes
/// it afterwards as before. The client asks the same gesture for the passkey
/// seal's `prf` output (ADR-0101 section 10), which this route never sees.
///
/// # Errors
///
/// As listed; [`ApiError::Db`]; [`ApiError::Internal`].
#[axum::debug_handler]
pub(crate) async fn passkey(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<PasskeyReq>,
) -> Result<Json<ResetFactorGrant>, ApiError> {
    let account_id = auth.0;
    let generation =
        crate::session::require_password_grant(&state, &account_id, req.fresh_grant.as_deref())
            .await?;
    let consumed = crate::webauthn_action::consume_action_grant(
        &state.db,
        &account_id,
        &reset_keys_action_hash(&account_id),
    )
    .await?;
    if !consumed {
        return Err(factor_refused());
    }
    tracing::info!("reset factor: passkey assertion accepted");
    Ok(Json(crate::session::issue_reset_factor_grant(
        &state,
        &account_id,
        generation,
        ResetFactor::Passkey,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reset_keys_action_hash_is_its_own_domain_over_the_account() {
        let a = reset_keys_action_hash(&[1; 16]);
        assert_ne!(a, reset_keys_action_hash(&[2; 16]));
        assert_ne!(
            a,
            crate::webauthn_action::delete_account_action_hash(&[1; 16])
        );
        let mut h = Sha256::new();
        h.update(b"lakko/action/reset-keys/v1");
        h.update([1u8; 16]);
        assert_eq!(a.as_slice(), h.finalize().as_slice());
    }
}
