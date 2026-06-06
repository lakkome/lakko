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

/// Lifetime of the action attestation signed on `/auth/action/finish`
/// (G-13, ADR-0015). It must comfortably cover the removing device posting
/// its `Remove` commit and online peers applying it. The apply-path
/// freshness semantics for late-polling (offline) peers — possibly relative
/// to the commit's epoch rather than wall-clock now — are a sprint-3
/// decision; this is the issuance-time window.
pub(crate) const ATTESTATION_TTL_SECS: i64 = 3600;

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
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_attest::ActionAttestation;
use lakko_call_jwt::{CallSigningKey, Jwk, Jwks};
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

/// Load (or generate + persist on first run) the dedicated Ed25519 key the
/// auth service uses to sign device-removal attestations (G-13, ADR-0015).
/// Mirrors `call_jwt::load_or_init_call_signing_key`, but a SEPARATE key
/// (key separation from the call-JWT key). Returns a [`CallSigningKey`],
/// reused purely as an Ed25519 keypair + kid holder; the kid lets clients
/// identify the published verifying key.
///
/// # Errors
///
/// Propagates sqlx / DB-corruption errors (a stored seed of the wrong
/// length); fatal at bootstrap.
pub(crate) async fn load_or_init_attest_key(pool: &MySqlPool) -> anyhow::Result<CallSigningKey> {
    use anyhow::Context as _;
    use rand::RngCore as _;

    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT seed FROM action_attest_signing_key WHERE id = 1")
            .fetch_optional(pool)
            .await
            .context("read action_attest_signing_key")?;
    if let Some((seed_bytes,)) = row {
        return Ok(CallSigningKey::from_seed(&attest_seed_from_bytes(&seed_bytes)?));
    }

    // No row — generate + insert. `INSERT IGNORE` so two racing startups
    // don't both insert; the re-read below lets a racing insert win.
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    sqlx::query("INSERT IGNORE INTO action_attest_signing_key (id, seed) VALUES (1, ?)")
        .bind(&seed[..])
        .execute(pool)
        .await
        .context("insert action_attest_signing_key")?;
    let (seed_bytes,): (Vec<u8>,) =
        sqlx::query_as("SELECT seed FROM action_attest_signing_key WHERE id = 1")
            .fetch_one(pool)
            .await
            .context("re-read action_attest_signing_key after insert")?;
    Ok(CallSigningKey::from_seed(&attest_seed_from_bytes(&seed_bytes)?))
}

fn attest_seed_from_bytes(bytes: &[u8]) -> anyhow::Result<[u8; 32]> {
    if bytes.len() != 32 {
        anyhow::bail!(
            "action_attest_signing_key.seed length {} != 32 — DB corruption?",
            bytes.len()
        );
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(bytes);
    Ok(seed)
}

/// `GET /.well-known/device-attest-jwks.json` — publish the attestation
/// verifying key (as an Ed25519 / OKP JWK) so operators can pin it and
/// clients can distribute it. Unauthenticated, like the call-JWT JWKS.
///
/// The zero-trust verification path does NOT fetch the key from here — peer
/// devices hold it from pinned / bootstrap trust config; this endpoint is
/// for discovery.
#[axum::debug_handler]
pub(crate) async fn device_attest_jwks_handler(State(state): State<AppState>) -> Json<Jwks> {
    Json(Jwks {
        keys: vec![Jwk::from_signing_key(&state.attest_key)],
    })
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

    // Server-side enforcement (G-13, ADR-0014): write a short-lived
    // single-use grant bound to (account, action_hash). The auth-service
    // endpoint (delete-passkey) recomputes the same action_hash and consumes
    // this grant, so a stolen token cannot perform the action without a
    // fresh passkey gesture.
    write_action_grant(&state.db, &auth.0, &presented).await?;

    // Peer-verified enforcement (G-13, ADR-0015): sign an attestation over
    // (account, action_hash, expiry). The remove-device client carries it
    // alongside its MLS Remove commit; peer devices verify it in the
    // lakko-mls apply path (delete-passkey ignores it — it uses the grant).
    let now_secs = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
    .unwrap_or(i64::MAX);
    let attestation =
        ActionAttestation::new(auth.0, presented, now_secs.saturating_add(ATTESTATION_TTL_SECS))
            .sign(&state.attest_key.signing);

    Ok(Json(ActionFinishRes {
        account_id_b64: URL_SAFE_NO_PAD.encode(auth.0),
        action_hash_b64: URL_SAFE_NO_PAD.encode(presented),
        credential_id_b64: URL_SAFE_NO_PAD.encode(result.cred_id().as_ref()),
        attestation_b64: URL_SAFE_NO_PAD.encode(attestation),
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
    /// URL-safe base64 of the signed `ActionAttestation` (G-13, ADR-0015).
    /// The remove-device client carries this alongside its MLS Remove commit
    /// for peer verification; the delete-passkey client ignores it (it relies
    /// on the server-side grant instead).
    pub attestation_b64: String,
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

/// End-to-end HTTP test of the server-side action-grant enforcement
/// (G-13, ADR-0014). Drives a real software authenticator through the
/// production router + a live MariaDB: register passkeys, run the
/// `/auth/action/{start,finish}` ceremony to mint a grant, and prove that
/// `DELETE /webauthn/credentials/:id` succeeds only with the grant and is
/// refused (403) without it.
///
/// Gated on `LAKKO_AUTH_TEST_DATABASE_URL` — skips (does not fail) when no
/// test database is provided, so `cargo test` stays green in DB-less CI.
/// Run it with, e.g.:
/// `LAKKO_AUTH_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:13306/lakko_auth_test \`
/// `  cargo test -p lakko-svc-auth action_grant_enforces`.
#[cfg(test)]
mod action_grant_e2e_tests {
    use super::{build_action_core, delete_credential_action_hash};
    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, StatusCode};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use http_body_util::BodyExt as _;
    use sqlx::mysql::MySqlPoolOptions;
    use sqlx::MySqlPool;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt as _;
    use url::Url;
    use uuid::Uuid;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_authenticator_rs::WebauthnAuthenticator;
    use webauthn_rs::prelude::{
        Passkey, PublicKeyCredential, RequestChallengeResponse, Webauthn, WebauthnBuilder,
    };

    const RP_ID: &str = "lakko.app";
    const RP_ORIGIN: &str = "https://lakko.app";
    const RP_NAME: &str = "Lakko";
    /// Fixed seed for the test attestation key so the test can reconstruct
    /// the verifying key and check the attestation `/finish` returns.
    const ATTEST_SEED: [u8; 32] = [0xCD; 32];

    fn rp_origin() -> Url {
        Url::parse(RP_ORIGIN).expect("static origin")
    }

    fn high_level() -> Webauthn {
        WebauthnBuilder::new(RP_ID, &rp_origin())
            .expect("builder")
            .rp_name(RP_NAME)
            .build()
            .expect("build")
    }

    fn build_state(pool: MySqlPool) -> (crate::AppState, lakko_auth_token::TokenSecret) {
        use opaque_ke::ServerSetup;
        use rand::rngs::OsRng;
        let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
        let opaque_setup = ServerSetup::<crate::opaque::LakkoCipherSuite>::new(&mut OsRng);
        let action_core = build_action_core(RP_NAME, RP_ID, &rp_origin());
        let state = crate::AppState {
            db: pool,
            opaque_setup: Arc::new(opaque_setup),
            session_token_secret: Arc::new(token_secret.clone()),
            session_token_ttl_ms: 86_400_000,
            webauthn: Some(Arc::new(high_level())),
            webauthn_action_core: Some(Arc::new(action_core)),
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&ATTEST_SEED)),
            turn: None,
        };
        (state, token_secret)
    }

    /// Register a fresh passkey with the software authenticator and persist
    /// it exactly as `webauthn::register_finish` does.
    async fn register_and_store(
        pool: &MySqlPool,
        account_id: &[u8; 16],
        hl: &Webauthn,
        wa: &mut WebauthnAuthenticator<SoftPasskey>,
    ) -> Passkey {
        let (ccr, reg_state) = hl
            .start_passkey_registration(Uuid::from_bytes(*account_id), "tester", "Tester", None)
            .expect("reg start");
        let rpkc = wa.do_registration(rp_origin(), ccr).expect("soft register");
        let passkey = hl
            .finish_passkey_registration(&rpkc, &reg_state)
            .expect("reg finish");
        let cred_id = passkey.cred_id().as_ref().to_vec();
        let passkey_bytes = serde_json::to_vec(&passkey).expect("serialize passkey");
        sqlx::query(
            "INSERT INTO webauthn_credentials (credential_id, account_id, passkey_bytes, nickname) \
             VALUES (?, ?, ?, '')",
        )
        .bind(&cred_id)
        .bind(account_id.as_slice())
        .bind(&passkey_bytes)
        .execute(pool)
        .await
        .expect("insert credential");
        passkey
    }

    async fn send(state: &crate::AppState, req: Request<Body>) -> (StatusCode, Bytes) {
        let resp = crate::build_app(state.clone())
            .oneshot(req)
            .await
            .expect("router oneshot");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, bytes)
    }

    fn now_ms() -> i64 {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(i64::MAX)
    }

    #[tokio::test]
    // a/b-suffixed bindings (pk_a/pk_b, cred_a/cred_b) are intentional and
    // clear in a positive-vs-negative test; the single linear flow is long
    // by design (it drives the full ceremony + three delete cases).
    #[allow(clippy::too_many_lines, clippy::similar_names)]
    async fn action_grant_enforces_passkey_delete() {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping action_grant_enforces_passkey_delete: LAKKO_AUTH_TEST_DATABASE_URL unset"
            );
            return;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");

        let (state, token_secret) = build_state(pool.clone());
        let hl = high_level();
        let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));

        // Fresh isolated account + two registered passkeys.
        let account_id: [u8; 16] = Uuid::new_v4().into_bytes();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .expect("insert account");
        let pk_a = register_and_store(&pool, &account_id, &hl, &mut wa).await;
        let pk_b = register_and_store(&pool, &account_id, &hl, &mut wa).await;

        let token = token_secret
            .issue(&account_id, now_ms() + 3_600_000)
            .expect("mint session token");
        let bearer = format!("Bearer {token}");

        // ---- Positive: ceremony writes a grant, delete consumes it. ----
        let cred_a = pk_a.cred_id().as_ref().to_vec();
        let action_hash = delete_credential_action_hash(&cred_a);
        let action_hash_b64 = URL_SAFE_NO_PAD.encode(action_hash);

        let (st, body) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/auth/action/start")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "action_hash_b64": action_hash_b64 }))
                        .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "action/start: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let state_id = v["state_id"].as_str().unwrap().to_string();
        let rcr: RequestChallengeResponse =
            serde_json::from_value(v["options"].clone()).expect("parse options");

        // The software authenticator signs the action-bound challenge.
        let pkc: PublicKeyCredential = wa
            .do_authentication(rp_origin(), rcr)
            .expect("soft assertion");

        let (st, body) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/auth/action/finish")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "state_id": state_id,
                        "action_hash_b64": action_hash_b64,
                        "credential": pkc,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "action/finish: {}",
            String::from_utf8_lossy(&body)
        );

        // G-13 (ADR-0015): the finish response carries a signed attestation
        // that verifies under the published attestation key and is bound to
        // this (account, action_hash).
        let fin: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let att_b64 = fin["attestation_b64"]
            .as_str()
            .expect("attestation_b64 present");
        let att_bytes = URL_SAFE_NO_PAD.decode(att_b64).expect("attestation decodes");
        let attest_vk = lakko_call_jwt::CallSigningKey::from_seed(&ATTEST_SEED).verifying;
        let verified = lakko_attest::verify_bound(
            &att_bytes,
            &attest_vk,
            &account_id,
            &action_hash,
            now_ms() / 1000,
        )
        .expect("attestation verifies + is bound to this account/action");
        assert_eq!(verified.account_id, account_id);
        assert_eq!(verified.action_hash, action_hash);
        // A different action must not verify against this attestation.
        assert!(
            lakko_attest::verify_bound(
                &att_bytes,
                &attest_vk,
                &account_id,
                &[0x00; 32],
                now_ms() / 1000,
            )
            .is_err(),
            "attestation must not verify for a different action_hash"
        );

        // The published JWKS exposes exactly the key that signed it.
        let (st, jwks_body) = send(
            &state,
            Request::builder()
                .method(Method::GET)
                .uri("/.well-known/device-attest-jwks.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "device-attest jwks must serve");
        let jwks: serde_json::Value = serde_json::from_slice(&jwks_body).unwrap();
        let published_x = jwks["keys"][0]["x"].as_str().expect("jwks x present");
        let published_pubkey = URL_SAFE_NO_PAD.decode(published_x).expect("jwks x decodes");
        assert_eq!(
            published_pubkey,
            attest_vk.to_bytes().to_vec(),
            "the published attestation key must be the one that signed"
        );

        let grants: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_action_grants WHERE account_id = ?")
                .bind(account_id.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(grants, 1, "finish must write exactly one grant");

        let cred_a_b64 = URL_SAFE_NO_PAD.encode(&cred_a);
        let (st, _b) = send(
            &state,
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/webauthn/credentials/{cred_a_b64}"))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::NO_CONTENT, "delete WITH a grant must 204");

        let grants: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_action_grants WHERE account_id = ?")
                .bind(account_id.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(grants, 0, "the delete must consume the grant");
        let left_a: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_credentials WHERE credential_id = ?")
                .bind(&cred_a)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(left_a, 0, "pk_a must be gone");

        // ---- Negative: deleting an existing credential WITHOUT a grant 403s. ----
        let cred_b = pk_b.cred_id().as_ref().to_vec();
        let cred_b_b64 = URL_SAFE_NO_PAD.encode(&cred_b);
        let (st, _b) = send(
            &state,
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/webauthn/credentials/{cred_b_b64}"))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "delete WITHOUT a grant must 403 (action-authorization-required)"
        );
        let left_b: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_credentials WHERE credential_id = ?")
                .bind(&cred_b)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(left_b, 1, "pk_b must survive a grant-less delete");

        // ---- Idempotent no-op: deleting a non-existent credential needs no grant. ----
        let ghost = URL_SAFE_NO_PAD.encode([0x77u8; 20]);
        let (st, _b) = send(
            &state,
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/webauthn/credentials/{ghost}"))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::NO_CONTENT,
            "deleting a non-existent credential is an idempotent no-op"
        );

        // Cleanup — cascade drops credentials + grants.
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .ok();
    }
}
