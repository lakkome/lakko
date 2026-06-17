// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! WebAuthn / passkey opt-in (Phase 2a2).
//!
//! Passkey is **additional** to OPAQUE-PAKE, not a replacement.
//! Every account always has a recovery phrase as the last-resort
//! path; a passkey is an auth-convenience binding the user attaches
//! to an existing account after they have signed in via the
//! password flow at least once.
//!
//! ## Flow split
//!
//! - **Registration** (2a2a) — the user is already logged in
//!   (the start/finish pair is gated by the session-token
//!   middleware). The browser / iOS / Android authenticator mints
//!   a fresh (private-key, credential-id, public-key) tuple bound
//!   to the lakko relying-party id; the server persists the
//!   public-key half so a future authentication round can verify
//!   a signed challenge.
//!
//! - **Authentication** (2a2b, this sub-sprint) — anonymous; the
//!   user presents the passkey to obtain a session token without
//!   going through the OPAQUE password round-trip. Two sub-paths:
//!   *allow-list* (caller supplies `account_id_b64`, server
//!   returns a constrained challenge) and *discoverable* (caller
//!   omits the id, the authenticator picks any of its stored
//!   passkeys for the lakko RP id and the user handle lands on
//!   /finish). The discoverable path powers the "Sign in with
//!   passkey" zero-friction UX.
//!
//! ## Relying-party identity
//!
//! Three env vars drive the [`Webauthn`] instance:
//!
//! - `LAKKO_WEBAUTHN_RP_ID`     — eTLD+1 (`lakko.me` in prod,
//!   `localhost` in dev).
//! - `LAKKO_WEBAUTHN_RP_NAME`   — human label shown by the
//!   authenticator UI (`Lakko`).
//! - `LAKKO_WEBAUTHN_RP_ORIGIN` — full origin (`https://lakko.me`,
//!   `http://localhost:5173` in dev).
//!
//! ## State persistence
//!
//! WebAuthn is two-step (start/finish) with an opaque server state
//! the client doesn't see. `webauthn-rs`'s default in-memory pattern
//! doesn't survive across axum requests; we enable the
//! `danger-allow-state-serialisation` feature and park the
//! [`PasskeyRegistration`] / [`PasskeyAuthentication`] blob in
//! `webauthn_states` keyed by a server-generated `state_id`. Five-
//! minute TTL; the same pruner shape as `opaque_login_states`.
//!
//! ## Privacy posture
//!
//! - WebAuthn's `user.id` field is the raw 16-byte account_id —
//!   indistinguishable from a random UUIDv7. No PII leaks via that
//!   handle.
//! - `user.name` and `user.displayName` flow through to the
//!   authenticator's UI when the user picks which passkey to use;
//!   the client supplies these as the username they already have on
//!   screen, the server does not store them anywhere.
//! - Credential blobs (`Passkey`) contain only public-key material,
//!   the signature counter, and transport hints. No private key
//!   ever crosses the wire.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::{
    CreationChallengeResponse, CredentialID, DiscoverableAuthentication, DiscoverableKey, Passkey,
    PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, Webauthn, WebauthnBuilder,
};

use crate::AppState;

/// Five-minute TTL on in-flight `PasskeyRegistration` /
/// `PasskeyAuthentication` rows. Matches the WebAuthn level-2 spec
/// recommendation for challenge validity.
pub(crate) const STATE_TTL_SECS: i64 = 300;

/// Pruner tick interval — sweep expired rows every 60 seconds.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

/// `webauthn_states.kind` discriminator. Stored as
/// `TINYINT UNSIGNED` in the DB.
pub(crate) mod state_kind {
    pub(crate) const REGISTRATION: u8 = 0;
    pub(crate) const AUTHENTICATION: u8 = 1;
    pub(crate) const DISCOVERABLE: u8 = 2;
    /// Action-bound `AuthenticationState` (G-13, ADR-0014). Rows of
    /// this kind also carry a non-NULL `action_hash`.
    pub(crate) const ACTION: u8 = 3;
}

/// Bearer-authenticated endpoint that prepares a fresh passkey
/// registration challenge. The caller has already gone through
/// OPAQUE login (or signup); the auth middleware stamps the
/// account_id into request extensions.
#[axum::debug_handler]
pub(crate) async fn register_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<RegisterStartReq>,
) -> Result<Json<RegisterStartRes>, ApiError> {
    let webauthn = state.webauthn.as_ref().ok_or(ApiError::Disabled)?.clone();

    // Refuse empty user_name — webauthn-rs accepts it but the
    // authenticator UI degrades to "<no name>" which is no better
    // than enforcing a value here.
    let trimmed = req.user_name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadInput("user_name must be non-empty"));
    }
    if trimmed.len() > 128 {
        return Err(ApiError::BadInput("user_name too long (>128 chars)"));
    }

    // Load existing credential ids so the authenticator's
    // `excludeCredentials` list prevents re-enrolling a passkey that
    // is already bound to this account. Optional; webauthn-rs
    // tolerates a single duplicate registration if the
    // authenticator allows it, but we surface a cleaner UX by
    // hinting it not to.
    let existing: Vec<CredentialID> = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT credential_id FROM webauthn_credentials WHERE account_id = ?",
    )
    .bind(auth.0.as_slice())
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::Db)?
    .into_iter()
    .map(CredentialID::from)
    .collect();

    let account_uuid = Uuid::from_bytes(auth.0);
    let (ccr, reg_state): (CreationChallengeResponse, PasskeyRegistration) = webauthn
        .start_passkey_registration(
            account_uuid,
            trimmed,
            trimmed,
            if existing.is_empty() {
                None
            } else {
                Some(existing)
            },
        )
        .map_err(ApiError::Webauthn)?;

    let state_bytes = serde_json::to_vec(&reg_state).map_err(ApiError::SerdeJson)?;
    let state_id = random_state_id();
    persist_state(
        &state.db,
        &state_id,
        Some(&auth.0),
        state_kind::REGISTRATION,
        &state_bytes,
    )
    .await?;

    // Bake the user-supplied nickname into the response so the
    // /finish handler can pull it back out alongside the state_id
    // and persist it on the row. (The nickname is server-bookkeeping
    // only; the authenticator doesn't see it.)
    Ok(Json(RegisterStartRes {
        state_id: hex_lower(&state_id),
        nickname: req.nickname.clone(),
        options: ccr,
    }))
}

/// Finalise the registration: verify the attestation, persist the
/// public-key half. Caller passes back the `state_id` we issued plus
/// the authenticator's `PublicKeyCredential` JSON.
#[axum::debug_handler]
pub(crate) async fn register_finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<RegisterFinishReq>,
) -> Result<Json<RegisterFinishRes>, ApiError> {
    let webauthn = state.webauthn.as_ref().ok_or(ApiError::Disabled)?.clone();

    let state_id = parse_state_id(&req.state_id)?;
    let row = load_state(&state.db, &state_id, state_kind::REGISTRATION).await?;
    let row = row.ok_or(ApiError::StateMissing)?;

    // Bind: the row must belong to the calling account. A token
    // with a different account_id presenting our state_id is
    // rejected — guards against state-id replay across accounts.
    let row_account = row.account_id.ok_or(ApiError::StateMissing)?;
    if row_account != auth.0 {
        // Don't leak which side mismatched.
        return Err(ApiError::StateMissing);
    }

    let reg_state: PasskeyRegistration =
        serde_json::from_slice(&row.state_bytes).map_err(ApiError::SerdeJson)?;

    let passkey: Passkey = webauthn
        .finish_passkey_registration(&req.credential, &reg_state)
        .map_err(ApiError::Webauthn)?;

    let credential_id_bytes: Vec<u8> = passkey.cred_id().as_ref().to_vec();
    let passkey_bytes = serde_json::to_vec(&passkey).map_err(ApiError::SerdeJson)?;

    // Idempotent insert: the same `credential_id` should never
    // appear twice — webauthn-rs's excludeCredentials hint at /start
    // should have prevented it, but the authenticator may ignore
    // the hint. `ON DUPLICATE KEY` swallows the retry cleanly.
    sqlx::query(
        "INSERT INTO webauthn_credentials \
            (credential_id, account_id, passkey_bytes, nickname) \
         VALUES (?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE \
            passkey_bytes = VALUES(passkey_bytes), \
            nickname = VALUES(nickname)",
    )
    .bind(&credential_id_bytes)
    .bind(auth.0.as_slice())
    .bind(&passkey_bytes)
    .bind(&req.nickname)
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;

    // Drop the in-flight state row; the registration is finalised.
    let _ = sqlx::query("DELETE FROM webauthn_states WHERE state_id = ?")
        .bind(state_id.as_slice())
        .execute(&state.db)
        .await;

    Ok(Json(RegisterFinishRes {
        credential_id_b64: URL_SAFE_NO_PAD.encode(&credential_id_bytes),
    }))
}

/// List credentials registered to the calling account. The
/// response carries only management-surface fields (id, nickname,
/// timestamps); the raw public-key material stays server-side.
#[axum::debug_handler]
pub(crate) async fn list_credentials(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<ListCredentialsRes>, ApiError> {
    let rows = sqlx::query_as::<_, CredentialRow>(
        "SELECT credential_id, nickname, \
                UNIX_TIMESTAMP(created_at) * 1000 AS created_at_ms, \
                UNIX_TIMESTAMP(last_used_at) * 1000 AS last_used_at_ms \
         FROM webauthn_credentials \
         WHERE account_id = ? \
         ORDER BY created_at",
    )
    .bind(auth.0.as_slice())
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::Db)?;

    Ok(Json(ListCredentialsRes {
        credentials: rows
            .into_iter()
            .map(|r| CredentialSummary {
                credential_id_b64: URL_SAFE_NO_PAD.encode(&r.credential_id),
                nickname: r.nickname,
                created_at_ms: r.created_at_ms,
                last_used_at_ms: r.last_used_at_ms,
            })
            .collect(),
    }))
}

/// Delete a single credential. Idempotent — deleting a nonexistent
/// or already-deleted row returns 200. The account_id binding
/// guards cross-account deletion (a session token issued for one
/// account cannot wipe another account's credential).
#[axum::debug_handler]
pub(crate) async fn delete_credential(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Path(credential_id_b64): Path<String>,
) -> Result<StatusCode, ApiError> {
    let credential_id = URL_SAFE_NO_PAD
        .decode(credential_id_b64)
        .map_err(|_| ApiError::BadInput("credential_id_b64 not URL-safe base64"))?;

    // Idempotent no-op when the credential doesn't exist for this account —
    // there is nothing to protect, so no action authorization is required.
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM webauthn_credentials WHERE credential_id = ? AND account_id = ?",
    )
    .bind(&credential_id)
    .bind(auth.0.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    if exists.is_none() {
        return Ok(StatusCode::NO_CONTENT);
    }

    // G-13 (ADR-0014) server-side enforcement: the credential exists, so
    // deleting it requires a fresh single-use action grant bound to THIS
    // credential. The client obtains the grant by completing the
    // `/auth/action/{start,finish}` passkey ceremony immediately before
    // calling delete. Recompute the action hash the ceremony bound to and
    // consume the grant atomically; refuse if none is present — a stolen
    // session token alone (no passkey gesture) cannot pass this gate.
    let action_hash = crate::webauthn_action::delete_credential_action_hash(&credential_id);
    let consumed =
        crate::webauthn_action::consume_action_grant(&state.db, &auth.0, &action_hash).await?;
    if !consumed {
        return Err(ApiError::ActionRequired);
    }

    sqlx::query("DELETE FROM webauthn_credentials WHERE credential_id = ? AND account_id = ?")
        .bind(&credential_id)
        .bind(auth.0.as_slice())
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;
    Ok(StatusCode::NO_CONTENT)
}

// ----------------------------------------------------------------
// 2a2b — Authentication (anonymous)
// ----------------------------------------------------------------

/// Anonymous endpoint that prepares a fresh passkey authentication
/// challenge.
///
/// Two paths:
///
/// 1. **Allow-list.** Caller supplies `account_id_b64`; we load the
///    user's stored passkeys and build an `allowCredentials`
///    challenge so the authenticator restricts itself to that list.
///    Used when the user has already typed their username on the
///    Login screen (the username → account_id lookup happens in
///    `services/discovery`, then the result is passed here).
///
/// 2. **Discoverable.** Caller omits `account_id_b64`. The server
///    issues a challenge with an empty `allowCredentials`; the
///    authenticator picks any of its stored passkeys for the lakko
///    RP id and signs. The /finish handler identifies the user from
///    the response's `userHandle`. This is the "Sign in with
///    passkey" zero-friction flow.
#[axum::debug_handler]
pub(crate) async fn auth_start(
    State(state): State<AppState>,
    Json(req): Json<AuthStartReq>,
) -> Result<Json<AuthStartRes>, ApiError> {
    let webauthn = state.webauthn.as_ref().ok_or(ApiError::Disabled)?.clone();

    if let Some(b64) = req.account_id_b64.as_deref() {
        // Allow-list path. Resolve the account, load its passkeys.
        let account_id = decode_account_id(b64)?;
        let passkeys = load_passkeys_for(&state.db, &account_id).await?;
        if passkeys.is_empty() {
            // The user has no passkeys; surface a clean error so
            // the UI can fall back to password.
            return Err(ApiError::NoCredentials);
        }
        let (rcr, auth_state): (RequestChallengeResponse, PasskeyAuthentication) = webauthn
            .start_passkey_authentication(&passkeys)
            .map_err(ApiError::Webauthn)?;
        let state_bytes = serde_json::to_vec(&auth_state).map_err(ApiError::SerdeJson)?;
        let state_id = random_state_id();
        persist_state(
            &state.db,
            &state_id,
            Some(&account_id),
            state_kind::AUTHENTICATION,
            &state_bytes,
        )
        .await?;
        Ok(Json(AuthStartRes {
            state_id: hex_lower(&state_id),
            discoverable: false,
            options: rcr,
        }))
    } else {
        // Discoverable path. No account binding yet; the user
        // handle lands on /finish.
        let (rcr, auth_state): (RequestChallengeResponse, DiscoverableAuthentication) = webauthn
            .start_discoverable_authentication()
            .map_err(ApiError::Webauthn)?;
        let state_bytes = serde_json::to_vec(&auth_state).map_err(ApiError::SerdeJson)?;
        let state_id = random_state_id();
        persist_state(
            &state.db,
            &state_id,
            None,
            state_kind::DISCOVERABLE,
            &state_bytes,
        )
        .await?;
        Ok(Json(AuthStartRes {
            state_id: hex_lower(&state_id),
            discoverable: true,
            options: rcr,
        }))
    }
}

/// Verify the authenticator's signature, identify the account,
/// bump the credential's signature counter, mint a session token.
///
/// Wire-equivalent to OPAQUE's `login_finish` — returns the same
/// `(account_id, session_token, session_expires_at_ms)` triple so
/// downstream clients hold a single shape regardless of which auth
/// path produced the token.
#[axum::debug_handler]
pub(crate) async fn auth_finish(
    State(state): State<AppState>,
    Json(req): Json<AuthFinishReq>,
) -> Result<Json<AuthFinishRes>, ApiError> {
    let webauthn = state.webauthn.as_ref().ok_or(ApiError::Disabled)?.clone();

    let state_id = parse_state_id(&req.state_id)?;
    // Try the allow-list row first, then the discoverable row. We
    // look up by `state_id` only — the `kind` discriminator tells
    // us which path to take. A `state_id` from one kind cannot be
    // replayed against the other-kind /finish because we dispatch
    // on the loaded row's kind, not on caller intent.
    let (auth_result, account_id) = if let Some(row) =
        load_state(&state.db, &state_id, state_kind::AUTHENTICATION).await?
    {
        let auth_state: PasskeyAuthentication =
            serde_json::from_slice(&row.state_bytes).map_err(ApiError::SerdeJson)?;
        let result = webauthn
            .finish_passkey_authentication(&req.credential, &auth_state)
            .map_err(ApiError::Webauthn)?;
        let account_id = row.account_id.ok_or(ApiError::StateMissing)?;
        (result, account_id)
    } else if let Some(row) = load_state(&state.db, &state_id, state_kind::DISCOVERABLE).await? {
        let auth_state: DiscoverableAuthentication =
            serde_json::from_slice(&row.state_bytes).map_err(ApiError::SerdeJson)?;
        // Identify the user from the authenticator's userHandle.
        let (user_uuid, _cred_id) = webauthn
            .identify_discoverable_authentication(&req.credential)
            .map_err(ApiError::Webauthn)?;
        let account_id = *user_uuid.as_bytes();
        let passkeys = load_passkeys_for(&state.db, &account_id).await?;
        if passkeys.is_empty() {
            return Err(ApiError::NoCredentials);
        }
        let discoverable_keys: Vec<DiscoverableKey> =
            passkeys.iter().map(DiscoverableKey::from).collect();
        let result = webauthn
            .finish_discoverable_authentication(&req.credential, auth_state, &discoverable_keys)
            .map_err(ApiError::Webauthn)?;
        (result, account_id)
    } else {
        return Err(ApiError::StateMissing);
    };

    // Drop the in-flight state row immediately — a state_id is
    // single-use; clearing protects against accidental client
    // retries replaying a verified challenge.
    let _ = sqlx::query("DELETE FROM webauthn_states WHERE state_id = ?")
        .bind(state_id.as_slice())
        .execute(&state.db)
        .await;

    // Bump the credential's counter + last_used_at if anything
    // changed. webauthn-rs's `Passkey::update_credential` flags
    // whether the row needs persisting; we only re-serialise on
    // change to avoid VARBINARY churn.
    let cred_id_bytes: Vec<u8> = auth_result.cred_id().as_ref().to_vec();
    let row: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT passkey_bytes, account_id FROM webauthn_credentials WHERE credential_id = ?",
    )
    .bind(&cred_id_bytes)
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    let (passkey_bytes, row_account_id_vec) = row.ok_or(ApiError::StateMissing)?;
    // Defense in depth: the credential's account binding must
    // match the path we authenticated. If a previously-deleted-
    // and-re-issued credential id collides, refuse rather than
    // hand out a token for the wrong account.
    if row_account_id_vec.len() != 16 || row_account_id_vec != account_id {
        tracing::warn!("webauthn auth: credential account mismatch");
        return Err(ApiError::StateMissing);
    }
    let mut passkey: Passkey =
        serde_json::from_slice(&passkey_bytes).map_err(ApiError::SerdeJson)?;
    let changed = passkey.update_credential(&auth_result).unwrap_or(false);
    if changed {
        let serialised = serde_json::to_vec(&passkey).map_err(ApiError::SerdeJson)?;
        sqlx::query(
            "UPDATE webauthn_credentials \
             SET passkey_bytes = ?, last_used_at = NOW(3) \
             WHERE credential_id = ?",
        )
        .bind(&serialised)
        .bind(&cred_id_bytes)
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;
    } else {
        // Even with no counter advance we want last_used_at to
        // reflect this successful auth.
        sqlx::query(
            "UPDATE webauthn_credentials SET last_used_at = NOW(3) WHERE credential_id = ?",
        )
        .bind(&cred_id_bytes)
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;
    }

    // Issue the session token. Same TTL math as OPAQUE login —
    // see `opaque::issue_token_for`.
    let now_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let session_expires_at_ms = now_ms.saturating_add(state.session_token_ttl_ms);
    let session_token = state
        .session_token_secret
        .issue(&account_id, session_expires_at_ms)
        .map_err(|e| {
            tracing::error!(error = %e, "webauthn auth: session-token issuance failed");
            ApiError::Internal
        })?;

    Ok(Json(AuthFinishRes {
        account_id_b64: URL_SAFE_NO_PAD.encode(account_id),
        session_token,
        session_expires_at_ms,
    }))
}

// ----------------------------------------------------------------
// Setup / state plumbing
// ----------------------------------------------------------------

/// Build a [`Webauthn`] instance from environment variables. Returns
/// `None` when `LAKKO_WEBAUTHN_RP_ID` is unset — the service still
/// runs (OPAQUE-only mode) and the four passkey routes return 503.
pub(crate) fn load_webauthn() -> anyhow::Result<Option<Arc<Webauthn>>> {
    let Ok(rp_id) = std::env::var("LAKKO_WEBAUTHN_RP_ID") else {
        return Ok(None);
    };
    let rp_name = std::env::var("LAKKO_WEBAUTHN_RP_NAME").unwrap_or_else(|_| "Lakko".into());
    let rp_origin = std::env::var("LAKKO_WEBAUTHN_RP_ORIGIN")
        .map_err(|_| anyhow::anyhow!(
            "LAKKO_WEBAUTHN_RP_ID is set but LAKKO_WEBAUTHN_RP_ORIGIN is not — pass the full origin (e.g. https://lakko.me)"
        ))?;

    let origin_url = Url::parse(&rp_origin)
        .map_err(|e| anyhow::anyhow!("LAKKO_WEBAUTHN_RP_ORIGIN parse: {e}"))?;
    let webauthn = WebauthnBuilder::new(&rp_id, &origin_url)
        .map_err(|e| anyhow::anyhow!("WebauthnBuilder::new: {e}"))?
        .rp_name(&rp_name)
        .build()
        .map_err(|e| anyhow::anyhow!("WebauthnBuilder::build: {e}"))?;
    Ok(Some(Arc::new(webauthn)))
}

/// Authenticated principal injected by the session-token
/// middleware. Mirror of `services/storage::auth::AuthenticatedAccount`.
#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedAccount(pub(crate) [u8; 16]);

/// State row pulled out of `webauthn_states` for a /finish leg.
struct StateRow {
    account_id: Option<[u8; 16]>,
    state_bytes: Vec<u8>,
}

#[derive(sqlx::FromRow)]
struct CredentialRow {
    credential_id: Vec<u8>,
    nickname: String,
    created_at_ms: Option<i64>,
    last_used_at_ms: Option<i64>,
}

/// Decode the URL-safe base64 account id the auth-start payload
/// carries on the allow-list path. Validates length so a 401 from
/// downstream isn't a length-mismatch surprise.
fn decode_account_id(b64: &str) -> Result<[u8; 16], ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("account_id_b64 not URL-safe base64"))?;
    if bytes.len() != 16 {
        return Err(ApiError::BadInput("account_id must decode to 16 bytes"));
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Load every `Passkey` registered to this account. Used by both
/// the allow-list /start handler (to populate `allowCredentials`)
/// and the discoverable /finish handler (to look up the user's
/// keys after `identify_discoverable_authentication`).
pub(crate) async fn load_passkeys_for(
    pool: &MySqlPool,
    account_id: &[u8; 16],
) -> Result<Vec<Passkey>, ApiError> {
    let rows: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT passkey_bytes FROM webauthn_credentials WHERE account_id = ?")
            .bind(account_id.as_slice())
            .fetch_all(pool)
            .await
            .map_err(ApiError::Db)?;
    let mut keys: Vec<Passkey> = Vec::with_capacity(rows.len());
    for row in rows {
        let pk: Passkey = serde_json::from_slice(&row).map_err(ApiError::SerdeJson)?;
        keys.push(pk);
    }
    Ok(keys)
}

async fn load_state(
    pool: &MySqlPool,
    state_id: &[u8; 16],
    kind: u8,
) -> Result<Option<StateRow>, ApiError> {
    let row: Option<(Option<Vec<u8>>, Vec<u8>)> = sqlx::query_as(
        "SELECT account_id, state_bytes \
         FROM webauthn_states \
         WHERE state_id = ? AND kind = ? AND expires_at > NOW(3)",
    )
    .bind(state_id.as_slice())
    .bind(kind)
    .fetch_optional(pool)
    .await
    .map_err(ApiError::Db)?;

    Ok(row.map(|(account_id_vec, state_bytes)| {
        let account_id = account_id_vec.as_ref().and_then(|v| {
            if v.len() == 16 {
                let mut a = [0u8; 16];
                a.copy_from_slice(v);
                Some(a)
            } else {
                None
            }
        });
        StateRow {
            account_id,
            state_bytes,
        }
    }))
}

async fn persist_state(
    pool: &MySqlPool,
    state_id: &[u8; 16],
    account_id: Option<&[u8; 16]>,
    kind: u8,
    state_bytes: &[u8],
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO webauthn_states \
            (state_id, account_id, kind, state_bytes, expires_at) \
         VALUES (?, ?, ?, ?, NOW(3) + INTERVAL ? SECOND)",
    )
    .bind(state_id.as_slice())
    .bind(account_id.map(<[u8; 16]>::as_slice))
    .bind(kind)
    .bind(state_bytes)
    .bind(STATE_TTL_SECS)
    .execute(pool)
    .await
    .map_err(ApiError::Db)?;
    Ok(())
}

pub(crate) fn random_state_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    id
}

pub(crate) fn parse_state_id(s: &str) -> Result<[u8; 16], ApiError> {
    if s.len() != 32 {
        return Err(ApiError::BadInput(
            "state_id must be 32 lowercase hex chars",
        ));
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        let lo = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)
            .map_err(|_| ApiError::BadInput("state_id not hex"))?;
        *b = lo;
    }
    Ok(out)
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Background pruner. Sweep expired rows every minute. Aborted via
/// the `JoinHandle` returned to `main`.
pub(crate) fn spawn_state_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = sqlx::query("DELETE FROM webauthn_states WHERE expires_at < NOW(3)")
                .execute(&pool)
                .await;
            match result {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned expired webauthn_states");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "webauthn_states prune failed"),
            }
            // G-13 (ADR-0014): sweep expired single-use action grants on the
            // same timer. Consumed grants are deleted on use; this catches
            // the ones a ceremony minted but no delete ever consumed.
            let grants =
                sqlx::query("DELETE FROM webauthn_action_grants WHERE expires_at < NOW(3)")
                    .execute(&pool)
                    .await;
            match grants {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(
                        rows = r.rows_affected(),
                        "pruned expired webauthn_action_grants"
                    );
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "webauthn_action_grants prune failed"),
            }
            // ADR-0018 (factor (a)): sweep expired single-use device grants
            // on the same timer. Redeemed grants are deleted on use; this
            // catches the ones a /start minted but no /redeem ever consumed.
            let device_grants = sqlx::query("DELETE FROM device_grants WHERE expires_at < NOW(3)")
                .execute(&pool)
                .await;
            match device_grants {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned expired device_grants");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "device_grants prune failed"),
            }

            // ADR-0018 factor (d) — sweep expired phone/email OTP challenges
            // on the same timer (migration 0013). A consumed challenge is
            // deleted on a successful recover; this catches the ones a
            // recover/start sent but no recover/finish ever consumed. The
            // verify path already filters `expires_at > NOW(3)`, so this is
            // hygiene (the PK is `(account_id, kind)`, so growth is bounded
            // anyway), not a correctness requirement.
            let otp_challenges =
                sqlx::query("DELETE FROM otp_challenges WHERE expires_at < NOW(3)")
                    .execute(&pool)
                    .await;
            match otp_challenges {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned expired otp_challenges");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "otp_challenges prune failed"),
            }
        }
    })
}

// ----------------------------------------------------------------
// Middleware
// ----------------------------------------------------------------

/// Bearer-auth middleware. Same shape as
/// `services/storage::auth::require_session`. Stamps an
/// [`AuthenticatedAccount`] into request extensions on success;
/// returns 401 on any failure.
pub(crate) async fn require_session(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    use axum::http::header;
    let header_value = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            tracing::debug!("webauthn auth middleware: missing Authorization header");
            StatusCode::UNAUTHORIZED
        })?;
    let token = header_value.strip_prefix("Bearer ").ok_or_else(|| {
        tracing::warn!("webauthn auth middleware: Authorization missing Bearer prefix");
        StatusCode::UNAUTHORIZED
    })?;
    let now_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let account_id_bytes = state
        .session_token_secret
        .verify(token, now_ms)
        .map_err(|e| {
            tracing::warn!(error = %e, "webauthn auth: session-token verify failed");
            StatusCode::UNAUTHORIZED
        })?;
    let account_id: [u8; 16] = account_id_bytes.as_slice().try_into().map_err(|_| {
        tracing::error!(
            "webauthn auth: account_id length {} after verify (expected 16)",
            account_id_bytes.len()
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    request
        .extensions_mut()
        .insert(AuthenticatedAccount(account_id));
    Ok(next.run(request).await)
}

// ----------------------------------------------------------------
// Wire shapes
// ----------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct RegisterStartReq {
    /// Username surface for the authenticator UI. The server does
    /// NOT store this; it round-trips into webauthn-rs's
    /// `start_passkey_registration` and is preserved inside the
    /// credential by the authenticator for its own display logic.
    pub user_name: String,
    /// Friendly nickname for the credential row ("MacBook Touch
    /// ID"). Bookkeeping; the authenticator never sees it.
    pub nickname: String,
}

#[derive(Serialize)]
pub(crate) struct RegisterStartRes {
    /// Hex-lowercase 16-byte state id. Echo back on /register/finish.
    pub state_id: String,
    /// Reflected `nickname` so the client doesn't have to thread
    /// it through the WebAuthn round-trip.
    pub nickname: String,
    /// `CreationChallengeResponse` ready to feed into
    /// `navigator.credentials.create({publicKey: ...})` on web /
    /// `ASAuthorizationPlatformPublicKeyCredentialProvider` on iOS /
    /// Jetpack `CredentialManager` on Android.
    pub options: CreationChallengeResponse,
}

#[derive(Deserialize)]
pub(crate) struct RegisterFinishReq {
    pub state_id: String,
    /// User-friendly label. Surfaces in `list_credentials`.
    pub nickname: String,
    /// `PublicKeyCredential` produced by the authenticator.
    pub credential: RegisterPublicKeyCredential,
}

#[derive(Serialize)]
pub(crate) struct RegisterFinishRes {
    pub credential_id_b64: String,
}

#[derive(Serialize)]
pub(crate) struct ListCredentialsRes {
    pub credentials: Vec<CredentialSummary>,
}

#[derive(Serialize)]
pub(crate) struct CredentialSummary {
    pub credential_id_b64: String,
    pub nickname: String,
    pub created_at_ms: Option<i64>,
    pub last_used_at_ms: Option<i64>,
}

// 2a2b — authentication wire ------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct AuthStartReq {
    /// Optional. When set, drives the allow-list flow (server
    /// loads the user's passkeys and returns a constrained
    /// challenge). When omitted, drives the discoverable flow —
    /// the authenticator picks any passkey for the lakko RP id
    /// and the user handle lands on /finish.
    #[serde(default)]
    pub account_id_b64: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct AuthStartRes {
    /// Hex-lowercase 16-byte state id. Echo back on /auth/finish.
    pub state_id: String,
    /// `true` when the server picked the discoverable path. The
    /// client uses this to decide whether to render a
    /// `mediation: "conditional"` autofill prompt on web.
    pub discoverable: bool,
    /// `RequestChallengeResponse` ready to feed into
    /// `navigator.credentials.get({publicKey: ...})` on web /
    /// `ASAuthorizationController` on iOS / Jetpack
    /// `CredentialManager.getCredential` on Android.
    pub options: RequestChallengeResponse,
}

#[derive(Deserialize)]
pub(crate) struct AuthFinishReq {
    pub state_id: String,
    /// `PublicKeyCredential` produced by the authenticator (the
    /// asserted signature + clientDataJSON + authenticatorData).
    pub credential: PublicKeyCredential,
}

#[derive(Serialize)]
pub(crate) struct AuthFinishRes {
    /// URL-safe base64 of the 16-byte UUIDv7 account id.
    pub account_id_b64: String,
    /// Bearer token. Same shape as OPAQUE login's `session_token`
    /// (HMAC-SHA256-signed, dot-segmented, v1) — issued by the
    /// shared `lakko-auth-token::TokenSecret::issue`.
    pub session_token: String,
    /// Absolute unix-ms expiry, mirrors OPAQUE login.
    pub session_expires_at_ms: i64,
}

// ----------------------------------------------------------------
// Error → HTTP mapping
// ----------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum ApiError {
    Disabled,
    BadInput(&'static str),
    StateMissing,
    /// 2a2b: caller asked to authenticate via passkey but no
    /// credentials are registered for the resolved account.
    /// Surfaced as 404 so the UI can fall back to password.
    NoCredentials,
    /// G-13 (ADR-0014): a sensitive operation requires a completed
    /// action-binding ceremony (a fresh passkey gesture) and no valid
    /// single-use grant was present. Surfaced as 403 so the client knows
    /// it must run `/auth/action/{start,finish}` first.
    ActionRequired,
    /// ADR-0018 (device-authorization redeem): the presented credential
    /// (a device grant token) did not match a live grant. Surfaced as a
    /// GENERIC `401` so it is indistinguishable from "account does not
    /// exist" / "grant expired" / "grant never existed" — the same
    /// enumeration-safe shape an OPAQUE bad-credential login returns.
    InvalidCredential,
    /// ADR-0018 factor (c) (`/totp/recover`): too many consecutive failed
    /// code attempts — the account is in its lockout window. Surfaced as a
    /// generic `429` carrying no account-existence information.
    RateLimited,
    /// ADR-0018 factor (c) (`/totp/enroll/start`): the account already has a
    /// CONFIRMED authenticator. Surfaced as `409` so a stolen session cannot
    /// silently swap the authenticator (the user must disable it first).
    TotpAlreadyEnabled,
    /// ADR-0018: a PoW-gated endpoint in this module (`/device/authorize/*`)
    /// rejected the proof. Delegates to [`crate::pow::PowError`]'s existing
    /// `(StatusCode, String)` mapping so the HTTP response is byte-identical
    /// to every other PoW-gated endpoint (400 bad encoding / 401 unknown,
    /// expired, insufficient, invalid / 500 db).
    Pow(crate::pow::PowError),
    /// 2a2b: catch-all for unrecoverable server-side errors
    /// (session-token issuance failures). The OPAQUE module
    /// folds the same case into a tuple-StatusCode; we keep an
    /// `ApiError` variant for symmetry with the rest of the
    /// `?`-using handlers in this module.
    Internal,
    Db(sqlx::Error),
    Webauthn(webauthn_rs::prelude::WebauthnError),
    SerdeJson(serde_json::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let (code, msg) = match self {
            Self::Disabled => (StatusCode::SERVICE_UNAVAILABLE, "webauthn-disabled"),
            Self::BadInput(m) => (StatusCode::BAD_REQUEST, m),
            Self::StateMissing => (StatusCode::GONE, "state-missing-or-expired"),
            Self::NoCredentials => (StatusCode::NOT_FOUND, "no-passkeys-for-account"),
            Self::ActionRequired => (StatusCode::FORBIDDEN, "action-authorization-required"),
            Self::InvalidCredential => (StatusCode::UNAUTHORIZED, "invalid credentials"),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many attempts; try again later",
            ),
            Self::TotpAlreadyEnabled => (
                StatusCode::CONFLICT,
                "authenticator already enabled; disable it first",
            ),
            Self::Pow(e) => {
                // Reuse PoW's own status+message mapping verbatim.
                let (code, msg): (StatusCode, String) = e.into();
                return (code, msg).into_response();
            },
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
            Self::Db(e) => {
                tracing::error!(error = %e, "webauthn db error");
                (StatusCode::INTERNAL_SERVER_ERROR, "db-error")
            },
            Self::Webauthn(e) => {
                tracing::warn!(error = %e, "webauthn verify error");
                (StatusCode::BAD_REQUEST, "webauthn-error")
            },
            Self::SerdeJson(e) => {
                tracing::error!(error = %e, "webauthn serde error");
                (StatusCode::INTERNAL_SERVER_ERROR, "serde-error")
            },
        };
        (code, msg).into_response()
    }
}

impl From<crate::pow::PowError> for ApiError {
    fn from(e: crate::pow::PowError) -> Self {
        Self::Pow(e)
    }
}
