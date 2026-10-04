// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Session issuance and refresh under the per-account key generation
//! ([ADR-0065] section 3).
//!
//! Every route that lands a session issues the same pair: a short-lived
//! **access token** (`v2`, carried as the bearer on every service) and a
//! **refresh credential** (`r1`, accepted by `POST /auth/session/refresh`
//! only), both minted at the account's current key generation
//! (`accounts.identity_version`). A reset bumps the generation; from then on
//! auth refuses every credential of a lower generation, which is what signs
//! the other devices out: `require_session` on every bearer request, and
//! [`refresh`] when a device tries to renew.
//!
//! The other verifiers (discovery, storage, gif-proxy) keep their stateless
//! verify, so their revocation latency is the access TTL
//! (`LAKKO_SESSION_TOKEN_TTL_SECS`, 15 minutes by default, chosen not
//! measured). A refresh credential lives `LAKKO_REFRESH_TOKEN_TTL_DAYS`
//! (default 30) and slides on every refresh.
//!
//! [ADR-0065]: ../../../docs/adr/0065-account-key-reset.md

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use sqlx::MySql;

use crate::webauthn::ApiError;
use crate::AppState;

/// The wire shape every session-landing route carries (flattened into its
/// own response), and the whole of the refresh route's answer.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionPair {
    /// The `v2` access token: the bearer for every service.
    pub(crate) session_token: String,
    /// Absolute unix-millisecond expiry of `session_token`, so the client
    /// refreshes ahead of it without parsing the token.
    pub(crate) session_expires_at_ms: i64,
    /// The `r1` refresh credential, accepted by `POST /auth/session/refresh`
    /// alone.
    pub(crate) refresh_token: String,
    /// Absolute unix-millisecond expiry of `refresh_token`.
    pub(crate) refresh_expires_at_ms: i64,
    /// The key generation both were issued under: the account's current
    /// one at issue time.
    pub(crate) key_generation: u64,
}

/// Current wall-clock time in unix milliseconds, saturating.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Read the account's current key generation, `None` when the account does
/// not exist.
///
/// # Errors
///
/// Any `sqlx::Error` from the read.
pub(crate) async fn key_generation_of<'e, E>(
    executor: E,
    account_id: &[u8; 16],
) -> Result<Option<u64>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    sqlx::query_scalar::<_, u64>("SELECT identity_version FROM accounts WHERE id = ?")
        .bind(&account_id[..])
        .fetch_optional(executor)
        .await
}

/// Issue the access + refresh pair for `account_id` at `key_generation`,
/// with the configured TTLs from now. The caller supplies the generation it
/// has already read (or just assigned) so this stays synchronous and usable
/// inside a transaction.
///
/// # Errors
///
/// [`ApiError::Internal`] when the token crate refuses (an account id of the
/// wrong length, which the `[u8; 16]` type rules out).
pub(crate) fn issue_pair_at(
    state: &AppState,
    account_id: &[u8; 16],
    key_generation: u64,
) -> Result<SessionPair, ApiError> {
    let now = now_ms();
    let session_expires_at_ms = now.saturating_add(state.session_token_ttl_ms);
    let refresh_expires_at_ms = now.saturating_add(state.refresh_token_ttl_ms);
    let session_token = state
        .session_token_secret
        .issue(account_id, key_generation, session_expires_at_ms)
        .map_err(|e| {
            tracing::error!(error = %e, "session-token issuance failed");
            ApiError::Internal
        })?;
    let refresh_token = state
        .session_token_secret
        .issue_refresh(account_id, key_generation, refresh_expires_at_ms)
        .map_err(|e| {
            tracing::error!(error = %e, "refresh-credential issuance failed");
            ApiError::Internal
        })?;
    Ok(SessionPair {
        session_token,
        session_expires_at_ms,
        refresh_token,
        refresh_expires_at_ms,
        key_generation,
    })
}

/// Issue the pair at the account's CURRENT generation (one indexed read).
/// The entry point for every login route.
///
/// # Errors
///
/// [`ApiError::Db`] on the read; [`ApiError::InvalidCredential`] (the
/// enumeration-safe `401`) when the account no longer exists, which a login
/// that raced an account deletion can see.
pub(crate) async fn issue_pair(
    state: &AppState,
    account_id: &[u8; 16],
) -> Result<SessionPair, ApiError> {
    let generation = key_generation_of(&state.db, account_id)
        .await
        .map_err(ApiError::Db)?
        .ok_or(ApiError::InvalidCredential)?;
    issue_pair_at(state, account_id, generation)
}

/// A key-reset grant as it rides on the wire, flattened into the answer of
/// `POST /login/finish` and of no other route (ADR-0065, amended 2026-10-04).
///
/// Deliberately NOT part of [`SessionPair`]: every session-landing route
/// shares that type and [`issue_pair`], and a grant must be reachable from
/// the password login alone. Recovery login, passkey login, TOTP recovery,
/// the device-authorize redeem, password reset, signup, refresh and the
/// key reset's own answer never construct this type.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ResetGrant {
    /// The `k1` key-reset grant: opaque to the client, sent back as
    /// `reset_grant` in the body of `POST /account/keys/reset/{start,finish}`.
    pub(crate) reset_grant: String,
    /// Absolute unix-millisecond expiry of `reset_grant`, so the client can
    /// ask for the password again ahead of a `403`.
    pub(crate) reset_grant_expires_at_ms: i64,
}

/// Mint a key-reset grant for `account_id` at `key_generation` with the
/// configured TTL from now. The ONLY caller is the password login's finish
/// handler, after `ServerLogin::finish` accepted the password proof.
///
/// # Errors
///
/// [`ApiError::Internal`] when the token crate refuses.
pub(crate) fn issue_reset_grant(
    state: &AppState,
    account_id: &[u8; 16],
    key_generation: u64,
) -> Result<ResetGrant, ApiError> {
    let reset_grant_expires_at_ms = now_ms().saturating_add(state.key_reset_grant_ttl_ms);
    let reset_grant = state
        .session_token_secret
        .issue_reset_grant(account_id, key_generation, reset_grant_expires_at_ms)
        .map_err(|e| {
            tracing::error!(error = %e, "key-reset grant issuance failed");
            ApiError::Internal
        })?;
    Ok(ResetGrant {
        reset_grant,
        reset_grant_expires_at_ms,
    })
}

/// Check the key-reset grant a reset request carries: present, well-formed,
/// tagged by us, unexpired, issued for `account_id` (the bearer's), and
/// issued at the account's CURRENT key generation (equal: a grant from
/// before a reset, or one somehow above the row, is refused). Returns that
/// generation, which the reset's transaction compares again under the row
/// lock.
///
/// Stateless but for one indexed read; charges no limiter and changes
/// nothing, so a stolen session without the password can call the reset
/// routes as often as it likes and spends none of the owner's budget.
///
/// # Errors
///
/// [`ApiError::PasswordRequired`] for every refusal (one code, so the caller
/// learns nothing about which check failed); [`ApiError::Db`] on the read.
pub(crate) async fn require_reset_grant(
    state: &AppState,
    account_id: &[u8; 16],
    grant: Option<&str>,
) -> Result<u64, ApiError> {
    let grant = grant.ok_or(ApiError::PasswordRequired)?;
    let verified = state
        .session_token_secret
        .verify_reset_grant(grant, now_ms())
        .map_err(|e| {
            tracing::debug!(error = %e, "key reset: grant refused");
            ApiError::PasswordRequired
        })?;
    if &verified.account_id != account_id {
        tracing::warn!("key reset: grant of another account presented");
        return Err(ApiError::PasswordRequired);
    }
    let current = key_generation_of(&state.db, account_id)
        .await
        .map_err(ApiError::Db)?
        .ok_or(ApiError::PasswordRequired)?;
    if verified.key_generation != current {
        return Err(ApiError::PasswordRequired);
    }
    Ok(current)
}

/// Body of `POST /auth/session/refresh`.
#[derive(Deserialize)]
pub(crate) struct RefreshReq {
    /// The `r1` refresh credential a login or an earlier refresh issued.
    pub(crate) refresh_token: String,
}

/// `POST /auth/session/refresh` — anonymous; the refresh credential is the
/// authorisation. Per-address limited (`Category::Attempt`).
///
/// Verifies the credential (version, tag, expiry), then reads the account's
/// current key generation: equal -> a fresh pair (the refresh credential
/// slides to a fresh 30 days); lower -> `401 key-generation-superseded`,
/// the signal that tells the device its keys are not the account's any more;
/// no such account -> `401 refresh-invalid`, the same body a bad tag or an
/// expired credential gets, so an attacker cannot tell a deleted account
/// from a wrong credential.
///
/// # Errors
///
/// [`ApiError::RefreshInvalid`], [`ApiError::KeyGenerationSuperseded`],
/// [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshReq>,
) -> Result<Json<SessionPair>, ApiError> {
    let verified = state
        .session_token_secret
        .verify_refresh(&req.refresh_token, now_ms())
        .map_err(|e| {
            tracing::debug!(error = %e, "session refresh: credential refused");
            ApiError::RefreshInvalid
        })?;
    let current = key_generation_of(&state.db, &verified.account_id)
        .await
        .map_err(ApiError::Db)?
        .ok_or(ApiError::RefreshInvalid)?;
    if verified.key_generation < current {
        return Err(ApiError::KeyGenerationSuperseded);
    }
    if verified.key_generation > current {
        // Unreachable for an honestly issued credential (the generation only
        // grows); refuse rather than issue a pair above the row.
        tracing::error!(
            token_generation = verified.key_generation,
            current,
            "session refresh: credential generation above the account's"
        );
        return Err(ApiError::RefreshInvalid);
    }
    Ok(Json(issue_pair_at(&state, &verified.account_id, current)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session pair every landing route shares never carries a key-reset
    /// grant: the grant is a separate type only the password login builds.
    /// This pins the wire keys, so a grant field added to the shared type
    /// (and so to passkey login, TOTP recovery, the device redeem, refresh)
    /// fails here.
    #[test]
    fn the_shared_session_pair_has_no_reset_grant_field() {
        let pair = SessionPair {
            session_token: "a".into(),
            session_expires_at_ms: 1,
            refresh_token: "b".into(),
            refresh_expires_at_ms: 2,
            key_generation: 3,
        };
        let value = serde_json::to_value(&pair).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "key_generation",
                "refresh_expires_at_ms",
                "refresh_token",
                "session_expires_at_ms",
                "session_token",
            ]
        );
    }
}
