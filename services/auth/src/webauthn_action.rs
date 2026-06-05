// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! WebAuthn action binding — challenge derivation (G-13, ADR-0014).
//!
//! A normal passkey assertion proves "the user has the device" but does
//! not bind to *which* action it authorises. To upgrade that to a
//! cryptographic per-action attestation, the action-bound auth flow issues
//! a WebAuthn challenge that is a deterministic function of the action:
//!
//! ```text
//! challenge = BLAKE3_keyed( derive_key(DOMAIN), action_hash || server_nonce )
//! ```
//!
//! The authenticator signs over `clientDataJSON` (which echoes the
//! challenge), so its signature attests to `action_hash`. The server-side
//! `AuthenticationState` (built by the forked
//! `webauthn-rs-core::generate_challenge_authenticate_with_challenge`)
//! carries this same challenge, so verification fails unless the
//! authenticator signed *this* action's challenge — an assertion produced
//! for action A cannot be replayed onto action B.
//!
//! - `action_hash` is a 32-byte opaque action identifier supplied by the
//!   caller (e.g. `BLAKE3(opaque_message_bytes)` for an OPAQUE-authorised
//!   request). This module treats it as opaque bytes.
//! - `server_nonce` is a fresh 32-byte CSPRNG value per challenge — it
//!   keeps the challenge unpredictable (a bare `H(action_hash)` would be
//!   guessable for a known action) and single-use, mirroring the random
//!   challenge the stock flow uses.
//!
//! The challenge-derivation here is the Lakko-owned crypto; the surrounding
//! WebAuthn assertion *verification* stays in upstream `webauthn-rs-core`
//! (ADR-0014). This module owns the whole action-bound flow: the challenge
//! derivation primitive, the `start_action_auth` / `finish_action_auth`
//! wiring over the vendored fork, and the bearer-authenticated
//! `POST /auth/action/{start,finish}` HTTP endpoints (backed by the
//! `webauthn_states.action_hash` column from migration 0008).

use rand::rngs::OsRng;
use rand::RngCore;

/// Domain separator bound into the action-challenge derivation. Bumping
/// this string invalidates every previously-issued action challenge.
pub(crate) const ACTION_CHALLENGE_DOMAIN: &str = "lakko/webauthn-action/v1";

/// Length of the opaque caller-supplied action hash.
pub(crate) const ACTION_HASH_LEN: usize = 32;

/// Length of the per-challenge server nonce.
pub(crate) const ACTION_NONCE_LEN: usize = 32;

/// Lifetime of a single-use action grant written on `/auth/action/finish`.
/// The grant is meant to be consumed by the sensitive endpoint immediately
/// after the passkey gesture, so the window is short; it is also strictly
/// single-use (consumption deletes the row).
pub(crate) const ACTION_GRANT_TTL_SECS: i64 = 120;

/// Domain separator for the *delete-passkey-credential* action. MUST match
/// the clients' `ACTION_DOMAIN_DELETE_CREDENTIAL` byte-for-byte so the
/// server recomputes the same `action_hash` the client bound the assertion
/// to. Kept here (the action module) because the server-side enforcement
/// lives next to the grant machinery.
pub(crate) const ACTION_DOMAIN_DELETE_CREDENTIAL: &str = "lakko/action/delete-credential/v1";

/// Generate a fresh 32-byte server nonce for an action-bound challenge.
pub(crate) fn random_action_nonce() -> [u8; ACTION_NONCE_LEN] {
    let mut nonce = [0u8; ACTION_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

/// Derive the action-bound WebAuthn challenge from a 32-byte action hash
/// and a 32-byte server nonce (ADR-0014).
///
/// `BLAKE3_keyed(derive_key(DOMAIN), action_hash || nonce)` — the
/// domain-derived key gives RFC-style domain separation, and keying the
/// hash (rather than a plain hash) keeps the construction in line with the
/// rest of Lakko's BLAKE3 usage.
pub(crate) fn derive_action_challenge(
    action_hash: &[u8; ACTION_HASH_LEN],
    nonce: &[u8; ACTION_NONCE_LEN],
) -> [u8; 32] {
    let key = lakko_crypto::hash::blake3_derive_key(ACTION_CHALLENGE_DOMAIN);
    let mut input = [0u8; ACTION_HASH_LEN + ACTION_NONCE_LEN];
    input[..ACTION_HASH_LEN].copy_from_slice(action_hash);
    input[ACTION_HASH_LEN..].copy_from_slice(nonce);
    lakko_crypto::hash::blake3_keyed(&key, &input)
}

// ----------------------------------------------------------------
// Action-bound start / finish over the forked webauthn-rs-core
// (ADR-0014). The high-level `Webauthn` does not expose its inner
// `WebauthnCore`, so the action-bound flow constructs its own core via
// `new_unsafe_experts_only` configured to MATCH the high-level RP config
// (same rp_id / origin / timeout) — any mismatch would change the
// rpIdHash or origin check and break verification. The username +
// discoverable LOGIN flows stay on the safe high-level `Webauthn`.
// ----------------------------------------------------------------

use std::time::Duration;
use url::Url;
use webauthn_rs::prelude::{Credential, Passkey};
use webauthn_rs_core::error::WebauthnError;
use webauthn_rs_core::proto::{
    AuthenticationResult, AuthenticationState, PublicKeyCredential, RequestChallengeResponse,
};
use webauthn_rs_core::WebauthnCore;

/// The authenticator-interaction timeout the high-level `WebauthnBuilder`
/// uses by default (`DEFAULT_AUTHENTICATOR_TIMEOUT`). The action-bound
/// core must match it so both flows issue equivalent challenges.
const ACTION_AUTH_TIMEOUT: Duration = Duration::from_secs(300);

/// Build a `WebauthnCore` for the action-bound flow, matching the
/// high-level `Webauthn`'s RP configuration.
///
/// `allow_subdomains` / `allow_any_port` are `false` to mirror the
/// `WebauthnBuilder` defaults the LOGIN flow uses (it never opts into
/// either) — so the origin check is byte-identical across both flows.
pub(crate) fn build_action_core(rp_name: &str, rp_id: &str, rp_origin: &Url) -> WebauthnCore {
    WebauthnCore::new_unsafe_experts_only(
        rp_name,
        rp_id,
        vec![rp_origin.clone()],
        ACTION_AUTH_TIMEOUT,
        Some(false), // allow_subdomains_origin — match high-level default
        Some(false), // allow_any_port — match high-level default
    )
}

/// Begin an action-bound authentication: issue a WebAuthn challenge that is
/// `derive_action_challenge(action_hash, nonce)` so the authenticator's
/// signature attests to `action_hash`.
///
/// Returns the `RequestChallengeResponse` to send to the client, the
/// `AuthenticationState` to persist (carries the same challenge that
/// `finish_action_auth` verifies against), and the fresh server `nonce`
/// (persist it alongside `action_hash` so the binding is auditable).
///
/// # Errors
///
/// Propagates [`WebauthnError`] from the core builder / challenge
/// generation (e.g. an empty / policy-inconsistent credential set).
pub(crate) fn start_action_auth(
    core: &WebauthnCore,
    passkeys: &[Passkey],
    action_hash: &[u8; ACTION_HASH_LEN],
) -> Result<(RequestChallengeResponse, AuthenticationState, [u8; ACTION_NONCE_LEN]), WebauthnError> {
    let nonce = random_action_nonce();
    let challenge = derive_action_challenge(action_hash, &nonce);

    let creds: Vec<Credential> = passkeys.iter().cloned().map(Credential::from).collect();
    // `None` lets the core infer the user-verification policy from the
    // credentials (the LOGIN flow's behaviour); the action-bound flow
    // always has a non-empty allow-list (the account's passkeys).
    let builder = core.new_challenge_authenticate_builder(creds, None)?;
    let (rcr, state) =
        core.generate_challenge_authenticate_with_challenge(builder, challenge.to_vec())?;
    Ok((rcr, state, nonce))
}

/// Finish an action-bound authentication: run upstream's
/// `authenticate_credential` against the stored `AuthenticationState`. It
/// fails unless the authenticator signed over the action-bound challenge,
/// so an assertion produced for a different action cannot be accepted.
///
/// # Errors
///
/// Propagates [`WebauthnError`] on signature / challenge / counter / policy
/// failure (upstream verification, unchanged).
pub(crate) fn finish_action_auth(
    core: &WebauthnCore,
    credential: &PublicKeyCredential,
    state: &AuthenticationState,
) -> Result<AuthenticationResult, WebauthnError> {
    core.authenticate_credential(credential, state)
}

// ----------------------------------------------------------------
// HTTP endpoints — POST /auth/action/{start,finish} (G-13, ADR-0014)
//
// Both are bearer-authenticated: the caller is an already-logged-in
// account proving possession of one of its registered passkeys over a
// specific action. The action core is configured from the SAME RP env
// vars as the high-level login `Webauthn`, so a passkey registered there
// verifies here (matching rpIdHash + origin). The username / discoverable
// LOGIN flows are untouched.
// ----------------------------------------------------------------

use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::MySqlPool;

use crate::webauthn::{
    hex_lower, load_passkeys_for, parse_state_id, random_state_id, state_kind, ApiError,
    AuthenticatedAccount, STATE_TTL_SECS,
};
use crate::AppState;

/// Build the action-binding [`WebauthnCore`] from the same relying-party
/// env vars the high-level login flow reads (`LAKKO_WEBAUTHN_RP_ID` /
/// `_RP_NAME` / `_RP_ORIGIN`), so the action core's rpIdHash + origin
/// check are byte-identical to the config the account's passkeys were
/// registered under. Returns `None` when `LAKKO_WEBAUTHN_RP_ID` is unset —
/// mirrors `webauthn::load_webauthn`: the service still runs and the
/// `/auth/action/*` routes return 503.
///
/// # Errors
///
/// Propagates a configuration error when `LAKKO_WEBAUTHN_RP_ID` is set but
/// `LAKKO_WEBAUTHN_RP_ORIGIN` is missing or unparseable.
pub(crate) fn load_action_core() -> anyhow::Result<Option<Arc<WebauthnCore>>> {
    let Ok(rp_id) = std::env::var("LAKKO_WEBAUTHN_RP_ID") else {
        return Ok(None);
    };
    let rp_name = std::env::var("LAKKO_WEBAUTHN_RP_NAME").unwrap_or_else(|_| "Lakko".into());
    let rp_origin = std::env::var("LAKKO_WEBAUTHN_RP_ORIGIN").map_err(|_| {
        anyhow::anyhow!(
            "LAKKO_WEBAUTHN_RP_ID is set but LAKKO_WEBAUTHN_RP_ORIGIN is not — pass the full origin (e.g. https://lakko.me)"
        )
    })?;
    let origin =
        Url::parse(&rp_origin).map_err(|e| anyhow::anyhow!("LAKKO_WEBAUTHN_RP_ORIGIN parse: {e}"))?;
    Ok(Some(Arc::new(build_action_core(&rp_name, &rp_id, &origin))))
}

/// Decode the URL-safe base64 32-byte action hash from the wire.
fn decode_action_hash(b64: &str) -> Result<[u8; ACTION_HASH_LEN], ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("action_hash_b64 not URL-safe base64"))?;
    if bytes.len() != ACTION_HASH_LEN {
        return Err(ApiError::BadInput("action_hash must decode to 32 bytes"));
    }
    let mut out = [0u8; ACTION_HASH_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// `POST /auth/action/start` — issue an action-bound passkey challenge.
///
/// Bearer-authenticated. Loads the caller's registered passkeys, derives
/// `challenge = BLAKE3_keyed(DOMAIN, action_hash || nonce)`, builds the
/// allow-list challenge over the forked core, and persists the
/// `AuthenticationState` plus the `action_hash` in `webauthn_states`
/// (kind = action). The authenticator will sign over this challenge, so
/// its signature attests to `action_hash`.
#[axum::debug_handler]
pub(crate) async fn action_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<ActionStartReq>,
) -> Result<Json<ActionStartRes>, ApiError> {
    let core = state
        .webauthn_action_core
        .as_ref()
        .ok_or(ApiError::Disabled)?
        .clone();
    let action_hash = decode_action_hash(&req.action_hash_b64)?;

    let passkeys = load_passkeys_for(&state.db, &auth.0).await?;
    if passkeys.is_empty() {
        return Err(ApiError::NoCredentials);
    }

    let (rcr, auth_state, _nonce) =
        start_action_auth(&core, &passkeys, &action_hash).map_err(ApiError::Webauthn)?;
    let state_bytes = serde_json::to_vec(&auth_state).map_err(ApiError::SerdeJson)?;
    let state_id = random_state_id();
    persist_action_state(&state.db, &state_id, &auth.0, &action_hash, &state_bytes).await?;

    Ok(Json(ActionStartRes {
        state_id: hex_lower(&state_id),
        options: rcr,
    }))
}

/// `POST /auth/action/finish` — verify an action-bound assertion.
///
/// Bearer-authenticated. Loads the issued state, refuses if the state is
/// not bound to the calling account, re-checks the presented `action_hash`
/// against the stored one (defence in depth), runs upstream-verbatim
/// `authenticate_credential` (which fails unless the authenticator signed
/// over our action-bound challenge), deletes the single-use state row, and
/// advances the credential's signature counter.
#[axum::debug_handler]
pub(crate) async fn action_finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<ActionFinishReq>,
) -> Result<Json<ActionFinishRes>, ApiError> {
    let core = state
        .webauthn_action_core
        .as_ref()
        .ok_or(ApiError::Disabled)?
        .clone();
    let presented = decode_action_hash(&req.action_hash_b64)?;
    let state_id = parse_state_id(&req.state_id)?;

    let row = load_action_state(&state.db, &state_id)
        .await?
        .ok_or(ApiError::StateMissing)?;

    // Bind the state to the calling account. Don't leak which side
    // mismatched — same posture as `webauthn::register_finish`.
    if row.account_id != auth.0 {
        return Err(ApiError::StateMissing);
    }
    // Defence in depth: the caller must re-present the `action_hash` the
    // challenge was minted for. The signature already guarantees this (the
    // challenge is a function of `action_hash`), so this compare is
    // belt-and-braces — refuse before touching the crypto verifier.
    if row.action_hash != presented {
        return Err(ApiError::BadInput(
            "action_hash does not match the issued challenge",
        ));
    }

    let auth_state: AuthenticationState =
        serde_json::from_slice(&row.state_bytes).map_err(ApiError::SerdeJson)?;
    let result = finish_action_auth(&core, &req.credential, &auth_state)
        .map_err(ApiError::Webauthn)?;

    // Single-use: drop the in-flight row so a verified action-bound
    // assertion cannot be replayed (mirrors `webauthn::auth_finish`).
    let _ = sqlx::query("DELETE FROM webauthn_states WHERE state_id = ?")
        .bind(state_id.as_slice())
        .execute(&state.db)
        .await;

    // Advance the credential's signature counter / last_used_at and catch
    // a counter regression (possible cloned authenticator). Same shape as
    // `webauthn::auth_finish`'s counter block.
    bump_action_credential(&state.db, &auth.0, &result).await?;

    // Server-side enforcement (G-13): write a short-lived single-use grant
    // bound to (account, action_hash). The sensitive endpoint recomputes
    // the same action_hash and consumes this grant, so a stolen token
    // cannot perform the action without a fresh passkey gesture.
    write_action_grant(&state.db, &auth.0, &presented).await?;

    Ok(Json(ActionFinishRes {
        account_id_b64: URL_SAFE_NO_PAD.encode(auth.0),
        action_hash_b64: URL_SAFE_NO_PAD.encode(presented),
        credential_id_b64: URL_SAFE_NO_PAD.encode(result.cred_id().as_ref()),
    }))
}

/// Decoded `webauthn_states` row for an action-bound `/finish`. Both
/// `account_id` and `action_hash` are non-NULL for kind = action rows.
struct ActionStateRow {
    account_id: [u8; 16],
    action_hash: [u8; ACTION_HASH_LEN],
    state_bytes: Vec<u8>,
}

/// Raw query projection — the DB columns are nullable in general (the
/// other `webauthn_states` kinds leave `action_hash` NULL), so we read
/// `Option` and validate before decoding into [`ActionStateRow`].
#[derive(sqlx::FromRow)]
struct ActionStateQueryRow {
    account_id: Option<Vec<u8>>,
    action_hash: Option<Vec<u8>>,
    state_bytes: Vec<u8>,
}

async fn persist_action_state(
    pool: &MySqlPool,
    state_id: &[u8; 16],
    account_id: &[u8; 16],
    action_hash: &[u8; ACTION_HASH_LEN],
    state_bytes: &[u8],
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO webauthn_states \
            (state_id, account_id, kind, action_hash, state_bytes, expires_at) \
         VALUES (?, ?, ?, ?, ?, NOW(3) + INTERVAL ? SECOND)",
    )
    .bind(state_id.as_slice())
    .bind(account_id.as_slice())
    .bind(state_kind::ACTION)
    .bind(action_hash.as_slice())
    .bind(state_bytes)
    .bind(STATE_TTL_SECS)
    .execute(pool)
    .await
    .map_err(ApiError::Db)?;
    Ok(())
}

async fn load_action_state(
    pool: &MySqlPool,
    state_id: &[u8; 16],
) -> Result<Option<ActionStateRow>, ApiError> {
    let row: Option<ActionStateQueryRow> = sqlx::query_as(
        "SELECT account_id, action_hash, state_bytes \
         FROM webauthn_states \
         WHERE state_id = ? AND kind = ? AND expires_at > NOW(3)",
    )
    .bind(state_id.as_slice())
    .bind(state_kind::ACTION)
    .fetch_optional(pool)
    .await
    .map_err(ApiError::Db)?;

    let Some(row) = row else {
        return Ok(None);
    };
    // Both columns are non-NULL for kind = action rows; a NULL or
    // wrong-length value means a corrupt row — refuse as StateMissing.
    let account_id = row
        .account_id
        .filter(|v| v.len() == 16)
        .map(|v| {
            let mut a = [0u8; 16];
            a.copy_from_slice(&v);
            a
        })
        .ok_or(ApiError::StateMissing)?;
    let action_hash = row
        .action_hash
        .filter(|v| v.len() == ACTION_HASH_LEN)
        .map(|v| {
            let mut a = [0u8; ACTION_HASH_LEN];
            a.copy_from_slice(&v);
            a
        })
        .ok_or(ApiError::StateMissing)?;
    Ok(Some(ActionStateRow {
        account_id,
        action_hash,
        state_bytes: row.state_bytes,
    }))
}

/// Advance the credential's signature counter + `last_used_at` after a
/// successful action-bound assertion. Mirrors `webauthn::auth_finish`'s
/// counter block; kept local so the audited login handler is untouched.
async fn bump_action_credential(
    pool: &MySqlPool,
    account_id: &[u8; 16],
    result: &AuthenticationResult,
) -> Result<(), ApiError> {
    let cred_id_bytes: Vec<u8> = result.cred_id().as_ref().to_vec();
    let row: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT passkey_bytes, account_id FROM webauthn_credentials WHERE credential_id = ?",
    )
    .bind(&cred_id_bytes)
    .fetch_optional(pool)
    .await
    .map_err(ApiError::Db)?;
    let (passkey_bytes, row_account) = row.ok_or(ApiError::StateMissing)?;
    // Defence in depth: the credential must belong to the calling account.
    if row_account.len() != 16 || row_account.as_slice() != account_id.as_slice() {
        tracing::warn!("webauthn action: credential account mismatch");
        return Err(ApiError::StateMissing);
    }
    let mut passkey: Passkey =
        serde_json::from_slice(&passkey_bytes).map_err(ApiError::SerdeJson)?;
    let changed = passkey.update_credential(result).unwrap_or(false);
    if changed {
        let serialised = serde_json::to_vec(&passkey).map_err(ApiError::SerdeJson)?;
        sqlx::query(
            "UPDATE webauthn_credentials \
             SET passkey_bytes = ?, last_used_at = NOW(3) \
             WHERE credential_id = ?",
        )
        .bind(&serialised)
        .bind(&cred_id_bytes)
        .execute(pool)
        .await
        .map_err(ApiError::Db)?;
    } else {
        sqlx::query("UPDATE webauthn_credentials SET last_used_at = NOW(3) WHERE credential_id = ?")
            .bind(&cred_id_bytes)
            .execute(pool)
            .await
            .map_err(ApiError::Db)?;
    }
    Ok(())
}

// ----------------------------------------------------------------
// Server-side action grants (G-13, ADR-0014)
//
// `/auth/action/finish` writes a single-use grant; the sensitive endpoint
// recomputes the expected action_hash from its own parameters and consumes
// a matching grant before acting. This is what makes the gate real rather
// than client-side-only: a stolen session token cannot mint a grant (that
// needs a passkey gesture at /finish).
// ----------------------------------------------------------------

/// Recompute the action hash a *delete-passkey-credential* ceremony binds
/// to: `SHA-256(domain ‖ credential_id)`. The clients use the same domain
/// and identifier (the raw credential id bytes), so a grant written from a
/// client ceremony matches what `webauthn::delete_credential` computes.
pub(crate) fn delete_credential_action_hash(credential_id: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ACTION_DOMAIN_DELETE_CREDENTIAL.as_bytes());
    hasher.update(credential_id);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Write (or refresh) a single-use action grant for `(account_id, action_hash)`.
async fn write_action_grant(
    pool: &MySqlPool,
    account_id: &[u8; 16],
    action_hash: &[u8; ACTION_HASH_LEN],
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO webauthn_action_grants (account_id, action_hash, expires_at) \
         VALUES (?, ?, NOW(3) + INTERVAL ? SECOND) \
         ON DUPLICATE KEY UPDATE expires_at = VALUES(expires_at)",
    )
    .bind(account_id.as_slice())
    .bind(action_hash.as_slice())
    .bind(ACTION_GRANT_TTL_SECS)
    .execute(pool)
    .await
    .map_err(ApiError::Db)?;
    Ok(())
}

/// Atomically consume a non-expired action grant for `(account_id,
/// action_hash)`. Returns `true` iff a matching live grant existed and was
/// consumed — a single `DELETE` whose `rows_affected == 1`, so the consume
/// is race-free and strictly single-use.
pub(crate) async fn consume_action_grant(
    pool: &MySqlPool,
    account_id: &[u8; 16],
    action_hash: &[u8; ACTION_HASH_LEN],
) -> Result<bool, ApiError> {
    let result = sqlx::query(
        "DELETE FROM webauthn_action_grants \
         WHERE account_id = ? AND action_hash = ? AND expires_at > NOW(3)",
    )
    .bind(account_id.as_slice())
    .bind(action_hash.as_slice())
    .execute(pool)
    .await
    .map_err(ApiError::Db)?;
    Ok(result.rows_affected() == 1)
}

// ----------------------------------------------------------------
// Wire shapes — /auth/action/{start,finish}
// ----------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct ActionStartReq {
    /// URL-safe base64 of the 32-byte opaque action hash to bind the
    /// assertion to (e.g. `BLAKE3` of the canonical action description).
    /// The server treats it as opaque bytes.
    pub action_hash_b64: String,
}

#[derive(Serialize)]
pub(crate) struct ActionStartRes {
    /// Hex-lowercase 16-byte state id. Echo back on `/auth/action/finish`.
    pub state_id: String,
    /// `RequestChallengeResponse` whose `challenge` is the action-bound
    /// value — feed into `navigator.credentials.get({publicKey})` on web
    /// or the platform authenticator API, exactly like the login flow.
    pub options: RequestChallengeResponse,
}

#[derive(Deserialize)]
pub(crate) struct ActionFinishReq {
    /// The `state_id` issued by `/auth/action/start`.
    pub state_id: String,
    /// The same `action_hash_b64` presented at `/start` (re-checked
    /// against the stored value as defence in depth).
    pub action_hash_b64: String,
    /// `PublicKeyCredential` produced by the authenticator (the asserted
    /// signature + clientDataJSON + authenticatorData).
    pub credential: PublicKeyCredential,
}

#[derive(Serialize)]
// The `_b64` suffix is the wire convention across this service (it tells the
// client the field is URL-safe base64); keeping it is clearer than dropping
// it just because every field in this particular response happens to be one.
#[allow(clippy::struct_field_names)]
pub(crate) struct ActionFinishRes {
    /// URL-safe base64 of the 16-byte account id (the authenticated caller).
    pub account_id_b64: String,
    /// Echo of the verified action hash the passkey attested to.
    pub action_hash_b64: String,
    /// URL-safe base64 of the credential id that signed.
    pub credential_id_b64: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic() {
        let action = [0x11u8; ACTION_HASH_LEN];
        let nonce = [0x22u8; ACTION_NONCE_LEN];
        assert_eq!(
            derive_action_challenge(&action, &nonce),
            derive_action_challenge(&action, &nonce),
            "same (action, nonce) must yield the same challenge"
        );
    }

    #[test]
    fn distinct_action_yields_distinct_challenge() {
        let nonce = [0x22u8; ACTION_NONCE_LEN];
        let a = derive_action_challenge(&[0x11u8; ACTION_HASH_LEN], &nonce);
        let b = derive_action_challenge(&[0x12u8; ACTION_HASH_LEN], &nonce);
        assert_ne!(a, b, "different action hashes must bind to different challenges");
    }

    #[test]
    fn distinct_nonce_yields_distinct_challenge() {
        let action = [0x11u8; ACTION_HASH_LEN];
        let a = derive_action_challenge(&action, &[0x22u8; ACTION_NONCE_LEN]);
        let b = derive_action_challenge(&action, &[0x23u8; ACTION_NONCE_LEN]);
        assert_ne!(a, b, "different nonces must yield different challenges");
    }

    #[test]
    fn challenge_is_32_bytes() {
        let c = derive_action_challenge(&[0u8; ACTION_HASH_LEN], &[0u8; ACTION_NONCE_LEN]);
        assert_eq!(c.len(), 32);
    }

    #[test]
    fn nonce_is_not_constant() {
        // Two fresh nonces must differ (CSPRNG, not a fixed value). A
        // collision here is astronomically unlikely; a constant nonce
        // would make the challenge guessable for a known action.
        assert_ne!(random_action_nonce(), random_action_nonce());
    }

    #[test]
    fn fork_delta_puts_derived_challenge_on_the_wire() {
        use webauthn_rs_core::proto::UserVerificationPolicy;

        let rp_origin = Url::parse("https://localhost").expect("valid url");
        let core = build_action_core("Lakko", "localhost", &rp_origin);

        let action_hash = [0x42u8; ACTION_HASH_LEN];
        let nonce = [0x7eu8; ACTION_NONCE_LEN];
        let challenge = derive_action_challenge(&action_hash, &nonce);

        // Build a request through the FORK method with our derived
        // challenge. An empty allow-list with an explicit policy is the
        // discoverable shape; it is enough to observe which challenge lands
        // on the wire (the credential set does not affect the challenge).
        let builder = core
            .new_challenge_authenticate_builder(Vec::new(), Some(UserVerificationPolicy::Preferred))
            .expect("builder");
        let (rcr, _state) = core
            .generate_challenge_authenticate_with_challenge(builder, challenge.to_vec())
            .expect("challenge response");

        // The fork delta's whole job: the wire challenge is OUR action-bound
        // value, not a fresh random one. `authenticate_credential` (upstream,
        // unchanged) later checks the assertion against exactly this
        // challenge, so an assertion produced for a different action — whose
        // clientDataJSON echoes a different challenge — cannot verify.
        assert_eq!(
            rcr.public_key.challenge.as_ref(),
            &challenge[..],
            "fork method must place the action-bound challenge on the wire"
        );
    }

    #[test]
    fn stock_flow_does_not_emit_the_action_challenge() {
        use webauthn_rs_core::proto::UserVerificationPolicy;

        let rp_origin = Url::parse("https://localhost").expect("valid url");
        let core = build_action_core("Lakko", "localhost", &rp_origin);
        let challenge =
            derive_action_challenge(&[0x42u8; ACTION_HASH_LEN], &[0x7eu8; ACTION_NONCE_LEN]);

        let builder = core
            .new_challenge_authenticate_builder(Vec::new(), Some(UserVerificationPolicy::Preferred))
            .expect("builder");
        let (rcr, _state) = core
            .generate_challenge_authenticate(builder)
            .expect("challenge response");

        // Sanity: the stock random-challenge path must NOT coincidentally
        // emit our derived value — otherwise the previous test would pass
        // even if the fork delta were a no-op copy of the stock method.
        assert_ne!(
            rcr.public_key.challenge.as_ref(),
            &challenge[..],
            "stock flow must emit a random challenge, distinct from the action-bound one"
        );
    }

    #[test]
    fn domain_separation_holds() {
        // Re-derive with the real domain vs a different one; the keyed
        // construction must produce different challenges so a challenge
        // minted for the action domain can't collide with another
        // BLAKE3-keyed use of the same (action || nonce) bytes.
        let action = [0x11u8; ACTION_HASH_LEN];
        let nonce = [0x22u8; ACTION_NONCE_LEN];
        let real = derive_action_challenge(&action, &nonce);

        let other_key = lakko_crypto::hash::blake3_derive_key("lakko/other-domain/v1");
        let mut input = [0u8; ACTION_HASH_LEN + ACTION_NONCE_LEN];
        input[..ACTION_HASH_LEN].copy_from_slice(&action);
        input[ACTION_HASH_LEN..].copy_from_slice(&nonce);
        let other = lakko_crypto::hash::blake3_keyed(&other_key, &input);

        assert_ne!(real, other, "domain separation must change the challenge");
    }

    #[test]
    fn delete_credential_action_hash_matches_client_convention() {
        // The server recomputes the same action_hash the clients send
        // (`SHA-256("lakko/action/delete-credential/v1" || credential_id)`)
        // so a grant minted from a client ceremony matches the delete. Pin
        // both the domain string and the hash construction here — a drift
        // would silently make every delete fail the grant check (or, worse,
        // accept a grant for the wrong action).
        use sha2::{Digest, Sha256};
        assert_eq!(
            ACTION_DOMAIN_DELETE_CREDENTIAL, "lakko/action/delete-credential/v1",
            "server domain must match the clients' ACTION_DOMAIN_DELETE_CREDENTIAL"
        );
        let credential_id = b"a-credential-id-of-some-bytes";
        let expected = {
            let mut h = Sha256::new();
            h.update(b"lakko/action/delete-credential/v1");
            h.update(credential_id);
            let d = h.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&d);
            out
        };
        assert_eq!(delete_credential_action_hash(credential_id), expected);
        // Distinct credential ids must bind to distinct grants.
        assert_ne!(
            delete_credential_action_hash(b"id-one"),
            delete_credential_action_hash(b"id-two"),
        );
    }
}

/// Full action-binding round-trip property tests (ADR-0014). These drive a
/// real software WebAuthn authenticator (`SoftPasskey`) end-to-end: register
/// a passkey, then have it sign an assertion over the action-bound challenge.
/// The security property under test is that an assertion produced for action
/// A cannot be accepted on action B's session — the whole point of G-13.
///
/// The assertion *verification* (`finish_action_auth` →
/// `authenticate_credential`) is upstream-verbatim; these tests prove the
/// Lakko wiring feeds it the action-bound challenge so the binding holds
/// through a genuine signature, not just at the derivation layer.
#[cfg(test)]
mod action_roundtrip_tests {
    use super::{build_action_core, finish_action_auth, start_action_auth, ACTION_HASH_LEN};
    use url::Url;
    use uuid::Uuid;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_authenticator_rs::WebauthnAuthenticator;
    use webauthn_rs::prelude::{Passkey, Webauthn, WebauthnBuilder};

    const RP_ID: &str = "lakko.app";
    const RP_ORIGIN: &str = "https://lakko.app";
    const RP_NAME: &str = "Lakko";

    fn rp_origin() -> Url {
        Url::parse(RP_ORIGIN).expect("static RP origin parses")
    }

    /// The high-level `Webauthn` used only to register a passkey for the test
    /// (its RP config matches `build_action_core`, so the resulting passkey
    /// verifies under the action-bound core).
    fn high_level() -> Webauthn {
        WebauthnBuilder::new(RP_ID, &rp_origin())
            .expect("WebauthnBuilder::new")
            .rp_name(RP_NAME)
            .build()
            .expect("WebauthnBuilder::build")
    }

    /// Register a fresh passkey with the software authenticator and return the
    /// server-side `Passkey` record.
    fn register_passkey(wa: &mut WebauthnAuthenticator<SoftPasskey>, hl: &Webauthn) -> Passkey {
        let (ccr, reg_state) = hl
            .start_passkey_registration(Uuid::new_v4(), "tester", "Tester", None)
            .expect("start_passkey_registration");
        let rpkc = wa
            .do_registration(rp_origin(), ccr)
            .expect("software authenticator registration");
        hl.finish_passkey_registration(&rpkc, &reg_state)
            .expect("finish_passkey_registration")
    }

    #[test]
    fn action_bound_assertion_round_trips() {
        let hl = high_level();
        // falsify_uv = true → set the user-verified flag (the high-level
        // default policy is Preferred), so verification accepts the UV bit.
        let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let passkey = register_passkey(&mut wa, &hl);

        let core = build_action_core(RP_NAME, RP_ID, &rp_origin());
        let action = [0xAAu8; ACTION_HASH_LEN];

        let (rcr, state, _nonce) = start_action_auth(&core, std::slice::from_ref(&passkey), &action)
            .expect("start_action_auth");
        // The authenticator signs the assertion over the action-bound
        // challenge carried in `rcr`.
        let pkc = wa
            .do_authentication(rp_origin(), rcr)
            .expect("software authenticator assertion");

        let result = finish_action_auth(&core, &pkc, &state)
            .expect("correctly-bound assertion must verify");
        assert_eq!(
            result.cred_id(),
            passkey.cred_id(),
            "verification must resolve to the registered credential"
        );
    }

    #[test]
    fn assertion_is_rejected_on_a_different_action() {
        let hl = high_level();
        let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let passkey = register_passkey(&mut wa, &hl);

        let core = build_action_core(RP_NAME, RP_ID, &rp_origin());
        let action_a = [0xAAu8; ACTION_HASH_LEN];
        let action_b = [0xBBu8; ACTION_HASH_LEN];

        // Two independent action-bound sessions: A and B carry different
        // challenges (different action hash AND different fresh nonce).
        let (rcr_a, state_a, _) =
            start_action_auth(&core, std::slice::from_ref(&passkey), &action_a)
                .expect("start_action_auth A");
        let (_rcr_b, state_b, _) =
            start_action_auth(&core, std::slice::from_ref(&passkey), &action_b)
                .expect("start_action_auth B");

        // The authenticator signs ONLY action A's challenge.
        let pkc_a = wa
            .do_authentication(rp_origin(), rcr_a)
            .expect("assertion over action A");

        // Sanity: it verifies on its own session A.
        assert!(
            finish_action_auth(&core, &pkc_a, &state_a).is_ok(),
            "the action-A assertion must verify on the action-A session"
        );

        // Security property: the SAME assertion must be REJECTED on action
        // B's session — the authenticator never attested to action B.
        let swapped = finish_action_auth(&core, &pkc_a, &state_b);
        assert!(
            swapped.is_err(),
            "an assertion bound to action A must not verify on action B (got {swapped:?})"
        );
    }
}
