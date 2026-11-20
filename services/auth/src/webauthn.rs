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
//! - `LAKKO_WEBAUTHN_EXTRA_ORIGINS` — optional, comma-separated further
//!   origins accepted by exact match. A native Android app signs as
//!   `android:apk-key-hash:<base64url SHA-256 of its signing certificate>`,
//!   not as the web origin, so the Android client's passkeys need it.
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
use std::time::Duration;

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
///
/// Gated on a fresh-auth grant (`fresh_grant` in the body, `403
/// fresh-auth-required` otherwise, checked before anything else): a passkey
/// sign-in mints a grant, so a bare session that could add its own passkey
/// could mint itself the proof for the key reset, account deletion and
/// add-device (ADR-0073, amended 2026-10-06).
#[axum::debug_handler]
pub(crate) async fn register_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<RegisterStartReq>,
) -> Result<Json<RegisterStartRes>, ApiError> {
    // The fresh-auth grant first (ADR-0073, amended 2026-10-06): nothing is
    // read, written or charged for a request without a usable one.
    crate::session::require_fresh_grant(&state, &auth.0, req.fresh_grant.as_deref()).await?;
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
    let (mut ccr, reg_state): (CreationChallengeResponse, PasskeyRegistration) = webauthn
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
    require_discoverable(&mut ccr);

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

/// Ask the authenticator for a discoverable credential (a "resident key").
///
/// `start_passkey_registration` asks for the opposite (`require_resident_key
/// (false)`, sent as `residentKey: "discouraged"`), and an authenticator that
/// honours it (a hardware key, some password managers) stores a passkey only
/// the server can name. A device that holds no account yet signs in through
/// the discoverable start (`POST /webauthn/auth/start {}`), which offers only
/// discoverable credentials, so such a passkey could neither sign in nor
/// restore keys on a new device (base scenario 4, 2026-11-11). Verification
/// is unchanged: the registration state ignores this flag.
pub(crate) fn require_discoverable(ccr: &mut CreationChallengeResponse) {
    use webauthn_rs_proto::{
        AuthenticatorSelectionCriteria, ResidentKeyRequirement, UserVerificationPolicy,
    };
    let selection =
        ccr.public_key
            .authenticator_selection
            .get_or_insert(AuthenticatorSelectionCriteria {
                authenticator_attachment: None,
                resident_key: None,
                require_resident_key: false,
                user_verification: UserVerificationPolicy::Required,
            });
    selection.resident_key = Some(ResidentKeyRequirement::Required);
    selection.require_resident_key = true;
}

/// Finalise the registration: verify the attestation, persist the
/// public-key half. Caller passes back the `state_id` we issued plus
/// the authenticator's `PublicKeyCredential` JSON.
/// Gated on a fresh-auth grant like the start (`403 fresh-auth-required`,
/// checked first, nothing stored).
#[axum::debug_handler]
pub(crate) async fn register_finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Extension(caller): Extension<AuthenticatedDevice>,
    Json(req): Json<RegisterFinishReq>,
) -> Result<Json<RegisterFinishRes>, ApiError> {
    // The fresh-auth grant first, again: the start's check alone would let a
    // registration state outlive the grant that opened it, and a reset in
    // between must stop the finish.
    crate::session::require_fresh_grant(&state, &auth.0, req.fresh_grant.as_deref()).await?;
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
    //
    // Inside the generation fence (ADR-0074 "T3 as built"): a passkey is a
    // way to sign in and to get a fresh-auth grant, so one registered by a
    // session that a key reset or a recall has just displaced must not
    // survive it.
    let mut tx =
        crate::session::begin_at_generation(&state.db, &auth.0, caller.key_generation).await?;
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
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;

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

    // The passkey's key-restoring blob (ADR-0072 decision 6) goes with the
    // passkey, in one transaction: a credential that no longer signs in must
    // not leave a blob behind. `passkey_vaults` cannot reference
    // `webauthn_credentials` by foreign key (the id columns differ in
    // length), so the delete is explicit. Blob first, then the credential:
    // the same order as `passkey_vault::passkey_put` checks them.
    let mut tx = state.db.begin().await.map_err(ApiError::Db)?;
    sqlx::query("DELETE FROM passkey_vaults WHERE account_id = ? AND credential_id = ?")
        .bind(auth.0.as_slice())
        .bind(&credential_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    sqlx::query("DELETE FROM webauthn_credentials WHERE credential_id = ? AND account_id = ?")
        .bind(&credential_id)
        .bind(auth.0.as_slice())
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;
    Ok(StatusCode::NO_CONTENT)
}

/// A3 (GDPR right-to-erasure) — delete the authenticated account and ALL of its
/// server-side **auth** state. `DELETE FROM accounts` cascades to every
/// account-keyed table (OPAQUE registrations, passkeys, recovery factors,
/// in-flight states + grants); the one table without the foreign key,
/// `device_requests` (ADR-0056), is cleared beside it in one transaction.
///
/// Authorization (ADR-0073, amended 2026-10-06):
///
/// 1. The session token proves the account (bearer middleware).
/// 2. Then ONE fresh proof that the person is present, and which one depends
///    on whether the account has a passkey:
///
///    - **The account has a passkey:** a single-use action grant bound to
///      `SHA-256("lakko/action/delete-account/v1" ‖ account_id)` (ADR-0014).
///      It alone authorises the delete. Without one: `403
///      action-authorization-required` (plain text), nothing deleted. The
///      `x-lakko-fresh-grant` header is NOT read for such an account: absent,
///      valid, expired or garbage, it changes nothing, so a stale header can
///      neither refuse a delete the action grant authorises nor stand in for
///      a missing action grant.
///    - **The account has no passkey:** a live **fresh-auth grant** in the
///      `x-lakko-fresh-grant` request header: the person signed in with the
///      password within the grant's lifetime, at the account's current key
///      generation. Without one: `403 {"error":"fresh-auth-required"}`,
///      nothing deleted. A header and not a body, because a `DELETE` with a
///      body is not portable across client stacks.
///
/// Why the action grant is enough on its own. The only writer of
/// `webauthn_action_grants` is `POST /auth/action/finish`
/// (`webauthn_action::action_finish`), and it writes the row only after
/// `finish_action_auth` verified a WebAuthn assertion by one of the account's
/// registered passkeys over a challenge derived from this very action hash
/// and a server nonce (`/auth/action/start` refuses an account with no
/// passkey, and the state row is bound to the calling account). A bare
/// session therefore cannot obtain one: it takes a passkey gesture. The grant
/// lives [`crate::webauthn_action::ACTION_GRANT_TTL_SECS`] (120 s, against
/// the fresh-auth grant's ten minutes), is single-use (the consume below is
/// one `DELETE`), and names the account and the delete-account action, so it
/// is a stronger and narrower proof than the fresh-auth grant a passkey
/// sign-in would mint. Requiring both made a person with a passkey perform
/// two passkey gestures for one delete and added nothing. Either way a stolen
/// session, or a device left unlocked, cannot erase an account.
///
/// The passkey lookup and the consume are two statements, not one
/// transaction. A passkey added between them cannot help an attacker
/// (registering one needs a fresh-auth grant), and one removed between them
/// only means the stricter branch was taken.
///
/// The discovery + storage tiers (username binding, identity bundle, KeyPackage,
/// search terms, public record, backup blob) are account-keyed in their own
/// DBs. The transaction that deletes the account also queues its purge there
/// (`account_purges`, worked by `crate::purge`), so it is owed exactly when
/// the account is gone and never for a refused delete. The DS / push /
/// channels / transparency tiers are account-opaque (invariant #5) and hold
/// nothing to delete.
pub(crate) async fn delete_account(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    headers: axum::http::HeaderMap,
) -> Result<StatusCode, ApiError> {
    // Which proof applies depends on whether the account has a passkey.
    let has_passkey: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM webauthn_credentials WHERE account_id = ? LIMIT 1")
            .bind(auth.0.as_slice())
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::Db)?;

    if has_passkey.is_some() {
        // A passkey gesture bound to this action, seconds ago (ADR-0014). It
        // is the whole authorisation: the fresh-auth header is deliberately
        // not read here.
        let action_hash = crate::webauthn_action::delete_account_action_hash(&auth.0);
        let consumed =
            crate::webauthn_action::consume_action_grant(&state.db, &auth.0, &action_hash).await?;
        if !consumed {
            return Err(ApiError::ActionRequired);
        }
    } else if crate::unclaimed::is_unclaimed(&state.db, &auth.0)
        .await
        .map_err(ApiError::Db)?
    {
        // An account nobody named (a call-link guest, ADR-0032): nobody knows
        // its password, so no fresh proof can exist, and there is nothing to
        // protect that the unclaimed-account sweep would not remove anyway.
        // A named account cannot reach this state on a bare session: an
        // account older than the sweep's lifetime needs the fresh-auth grant
        // to release its login handle (`login_handle::delete`).
    } else {
        // No passkey: the password, proven within the grant's lifetime.
        crate::session::require_fresh_grant(
            &state,
            &auth.0,
            crate::session::fresh_grant_header(&headers),
        )
        .await?;
    }

    // FK ON DELETE CASCADE purges every account-keyed auth table except
    // `device_requests`, whose foreign key ADR-0056 dropped (an unknown
    // handle's request is stored against a stand-in id that is not an
    // account); its row is removed in the same transaction.
    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;

    // ADR-0074 section 1.3: while a recall window is open the account cannot
    // be deleted. Without this the party who cannot keep the account could
    // destroy it, and a deleted account cannot be recalled. Checked under
    // the account row's lock, which every capture also takes, so a deletion
    // racing the reset that opens the window either runs first (and there
    // was no window yet) or sees the anchor. The proof above was already
    // spent for a passkey account; the client asks for a new one after the
    // window, which is at least days away.
    let locked: Option<u64> =
        sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ? FOR UPDATE")
            .bind(auth.0.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    if locked.is_some() {
        if let Some(until_ms) =
            crate::recall::open_window_until(&mut tx, &auth.0, crate::session::now_ms())
                .await
                .map_err(ApiError::Db)?
        {
            return Err(ApiError::RecallWindowOpen { until_ms });
        }
    }

    sqlx::query("DELETE FROM device_requests WHERE account_id = ?")
        .bind(auth.0.as_slice())
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(auth.0.as_slice())
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    // In the transaction that deletes the account: committed together, or
    // not at all. A second delete racing this one finds no row and queues
    // the same purge again, which changes nothing.
    crate::purge::enqueue(&mut tx, &auth.0)
        .await
        .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;
    Ok(StatusCode::NO_CONTENT)
}

/// A3 (GDPR data-portability) — what `services/auth` holds about the
/// authenticated account. Deliberately thin: a zero-trust auth tier stores only
/// account metadata + auth-factor EXISTENCE, never the factor secrets/hashes
/// (invariant #1 — exporting the TOTP secret would be
/// both useless to the user and a security risk). The user's actual content
/// (messages, contacts) lives client-side and is exported there from the backup.
#[derive(serde::Serialize)]
pub(crate) struct AccountExport {
    /// URL-safe base64 of the raw 16-byte account_id.
    account_id_b64: String,
    /// Account creation time, unix-milliseconds.
    created_at_ms: i64,
    /// KT-log identity version (bumped on each rotation).
    identity_version: i64,
    /// Number of registered passkeys.
    passkey_count: i64,
    /// Whether a confirmed TOTP authenticator is enrolled.
    has_totp: bool,
}

/// `GET /account/export` (A3) — bearer-auth; returns [`AccountExport`] for the
/// authenticated account.
pub(crate) async fn export_account(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<AccountExport>, ApiError> {
    let account: Option<(i64, i64)> = sqlx::query_as(
        "SELECT CAST(UNIX_TIMESTAMP(created_at) * 1000 AS SIGNED) AS created_at_ms, \
                CAST(identity_version AS SIGNED) AS identity_version \
         FROM accounts WHERE id = ?",
    )
    .bind(auth.0.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    // A valid session token implies the account exists; a missing row is a
    // benign deleted-account-vs-stale-token race — report zeros, not a 500.
    let (created_at_ms, identity_version) = account.unwrap_or((0, 0));

    let passkey_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_credentials WHERE account_id = ?")
            .bind(auth.0.as_slice())
            .fetch_one(&state.db)
            .await
            .map_err(ApiError::Db)?;

    let has_totp: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM totp_secrets WHERE account_id = ? AND confirmed_at IS NOT NULL LIMIT 1",
    )
    .bind(auth.0.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;

    Ok(Json(AccountExport {
        account_id_b64: URL_SAFE_NO_PAD.encode(auth.0),
        created_at_ms,
        identity_version,
        passkey_count,
        has_totp: has_totp.is_some(),
    }))
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

/// `POST /webauthn/auth/finish`, counted as a passkey login (ADR-0096); the
/// flow is [`auth_finish_answer`].
pub(crate) async fn auth_finish(
    state: State<AppState>,
    req: Json<AuthFinishReq>,
) -> axum::response::Response {
    crate::metrics::login(
        crate::metrics::LoginKind::Passkey,
        auth_finish_answer(state, req).await,
    )
}

/// Verify the authenticator's signature, identify the account,
/// bump the credential's signature counter, mint a session token.
///
/// Wire-equivalent to OPAQUE's `login_finish` — returns the same
/// `(account_id, session_token, session_expires_at_ms)` triple so
/// downstream clients hold a single shape regardless of which auth
/// path produced the token.
#[axum::debug_handler]
async fn auth_finish_answer(
    State(state): State<AppState>,
    Json(req): Json<AuthFinishReq>,
) -> Result<Json<AuthFinishRes>, ApiError> {
    let webauthn = state.webauthn.as_ref().ok_or(ApiError::Disabled)?.clone();

    let state_id = parse_state_id(&req.state_id)?;
    let device_id = crate::session::decode_device_id(req.device_id_b64.as_deref())?;
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

    // The generation the pair is issued at, read BEFORE the credential row
    // below is consulted (ADR-0074 "T3 as built"). A key reset and a recall
    // delete the account's passkeys in the transaction that bumps the
    // generation, so a sign-in that straddles one either finds no credential
    // row further down, or is issued at the generation from before the
    // change and refused on first use. Reading it after, as before sprint
    // T3, let a passkey sign-in in flight at the moment of a recall come out
    // with a session (and a fresh-auth grant) of the new generation.
    let key_generation = crate::session::key_generation_of(&state.db, &account_id)
        .await
        .map_err(ApiError::Db)?
        .ok_or(ApiError::InvalidCredential)?;

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

    // Issue the session pair at the account's current key generation
    // (ADR-0073) — the same shape every login route returns — and, because
    // the passkey assertion just verified, a fresh-auth grant at that same
    // generation (amended 2026-10-06).
    let session = crate::session::issue_pair_at(&state, &account_id, key_generation, &device_id)?;
    let grant = crate::session::issue_fresh_grant(&state, &account_id, session.key_generation)?;

    Ok(Json(AuthFinishRes {
        account_id_b64: URL_SAFE_NO_PAD.encode(account_id),
        session,
        grant,
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
    let extra =
        parse_extra_origins(&std::env::var("LAKKO_WEBAUTHN_EXTRA_ORIGINS").unwrap_or_default())?;
    let mut builder = WebauthnBuilder::new(&rp_id, &origin_url)
        .map_err(|e| anyhow::anyhow!("WebauthnBuilder::new: {e}"))?
        .rp_name(&rp_name);
    for origin in &extra {
        builder = builder.append_allowed_origin(origin);
    }
    let webauthn = builder
        .build()
        .map_err(|e| anyhow::anyhow!("WebauthnBuilder::build: {e}"))?;
    tracing::info!(extra_origins = extra.len(), "webauthn configured");
    Ok(Some(Arc::new(webauthn)))
}

/// The further origins of `LAKKO_WEBAUTHN_EXTRA_ORIGINS`: comma-separated,
/// blanks ignored, each a URL (an `android:apk-key-hash:...` origin parses
/// as one with the `android` scheme). An entry that does not parse refuses
/// the start, rather than leaving a client unable to sign in unexplained.
pub(crate) fn parse_extra_origins(raw: &str) -> anyhow::Result<Vec<Url>> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            Url::parse(s)
                .map_err(|e| anyhow::anyhow!("LAKKO_WEBAUTHN_EXTRA_ORIGINS entry parse: {e}"))
        })
        .collect()
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
    const JOB: &str = crate::metrics::jobs::WEBAUTHN_STATE_PRUNE;
    lakko_metrics::job_registered(JOB);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = sqlx::query("DELETE FROM webauthn_states WHERE expires_at < NOW(3)")
                .execute(&pool)
                .await;
            let mut all_ok = result.is_ok();
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
            all_ok &= grants.is_ok();
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
            all_ok &= device_grants.is_ok();
            match device_grants {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned expired device_grants");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "device_grants prune failed"),
            }
            crate::metrics::job_ran(JOB, all_ok);
        }
    })
}

// ----------------------------------------------------------------
// Middleware
// ----------------------------------------------------------------

/// JSON `error` code of the `401` auth answers to a credential of a key
/// generation below the account's current one (ADR-0073 section 3).
pub(crate) const CODE_KEY_GENERATION_SUPERSEDED: &str = "key-generation-superseded";

/// JSON `error` code of the `401` the refresh route answers to a credential
/// that does not verify, expired, or names no account (ADR-0073 section 3.2).
pub(crate) const CODE_REFRESH_INVALID: &str = "refresh-invalid";

/// Body code of the `403` a fresh-auth-gated route (reset account keys,
/// delete account, add a device) answers when the request carries no live
/// fresh-auth grant for the bearer's account at its current key generation
/// (ADR-0073, amended 2026-10-04 and 2026-10-06): the person signs in again
/// with the password (`POST /login/{start,finish}`) or a passkey
/// (`POST /webauthn/auth/{start,finish}`).
pub(crate) const CODE_FRESH_AUTH_REQUIRED: &str = "fresh-auth-required";

/// JSON `error` code of the `401` auth answers to a credential that names a
/// device removed from the account after the credential was issued (ADR-0074
/// section 0.4). Discovery answers a `403` with the same code to a KeyPackage
/// publish under a tombstoned device id.
pub(crate) const CODE_DEVICE_REVOKED: &str = "device-revoked";

/// Body code of the `403` a route answers when it needs a statement signed
/// by the account's recorded identity key and none verifies (ADR-0074).
pub(crate) const CODE_ACCOUNT_KEY_REQUIRED: &str = "account-key-required";

/// Body code of the `409` the genesis mint answers to a key other than the
/// account's recorded one (ADR-0074 section 0.1).
pub(crate) const CODE_IDENTITY_KEY_MISMATCH: &str = "identity-key-mismatch";

/// Body code of the `409` `POST /auth/session/bind-device` answers to a
/// session that already names another device.
pub(crate) const CODE_DEVICE_ALREADY_BOUND: &str = "device-already-bound";

/// Body code of the one `401` every recall route answers to anything that is
/// not a wrong password (ADR-0074 section 1.2).
pub(crate) const CODE_RECALL_REFUSED: &str = "recall-refused";

/// Body code of the `401` `POST /account/recall/finish` answers when the
/// anchored password did not verify.
pub(crate) const CODE_WRONG_PASSWORD: &str = "wrong-password";

/// `401` body code of an account statement that verifies but was signed with
/// a clock outside the two-minute window (ADR-0074 "T5 as built"). The body
/// also carries `server_time_ms`.
pub(crate) const CODE_STATEMENT_STALE: &str = "statement-stale";

/// Body code of the `409` `DELETE /account` answers while a recall window is
/// open (ADR-0074 section 1.3).
pub(crate) const CODE_RECALL_WINDOW_OPEN: &str = "recall-window-open";

/// The device a verified bearer names and when the token was issued, stamped
/// into the request extensions beside [`AuthenticatedAccount`] (ADR-0074
/// section 0.4). `device_id` is all zero for a session that names no device
/// yet.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AuthenticatedDevice {
    /// The token's device id.
    pub(crate) device_id: [u8; 32],
    /// The token's key generation (the account's current one: the
    /// middleware refused anything else).
    pub(crate) key_generation: u64,
}

/// Bearer-auth middleware. The same shape as
/// `services/storage::auth::require_session` plus two stateful checks, both
/// primary-key reads per bearer request:
///
/// - the ADR-0073 generation check: a token whose generation is below
///   `accounts.identity_version` is refused with `401
///   {"error":"key-generation-superseded"}`;
/// - the ADR-0074 device floor: a token of the CURRENT generation issued at
///   or before its device's floor is refused, with `401
///   {"error":"device-revoked"}` when the device was removed and the plain
///   `401` when it logged out.
///
/// **The generation is checked first** (sprint T3; T1 read the floor first).
/// A device answers `device-revoked` by wiping everything and keeping no
/// recall record, and a floor is written by whoever holds the account's
/// CURRENT identity key. After a hostile key reset that is the attacker: with
/// the floor first they could revoke the owner's device ids and make those
/// devices destroy the very key a recall needs. A credential of an older
/// generation is therefore always told `key-generation-superseded`, whatever
/// floors exist. The cost: a device the owner removed and whose account was
/// reset afterwards keeps a recall record for the window instead of wiping
/// at once; it still needs the password from before to use it. Every other
/// failure —
/// no header, no `Bearer` prefix, a token that does not verify or has
/// expired, an account that no longer exists — is the plain bodiless `401`
/// it always was. Stamps an [`AuthenticatedAccount`] and an
/// [`AuthenticatedDevice`] into the request extensions on success.
pub(crate) async fn require_session(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, axum::response::Response> {
    use axum::http::header;
    let plain = || StatusCode::UNAUTHORIZED.into_response();
    let header_value = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            tracing::debug!("webauthn auth middleware: missing Authorization header");
            plain()
        })?;
    let token = header_value.strip_prefix("Bearer ").ok_or_else(|| {
        tracing::warn!("webauthn auth middleware: Authorization missing Bearer prefix");
        plain()
    })?;
    let verified = state
        .session_token_secret
        .verify(token, crate::session::now_ms())
        .map_err(|e| {
            tracing::warn!(error = %e, "webauthn auth: session-token verify failed");
            plain()
        })?;
    let current = crate::session::key_generation_of(&state.db, &verified.account_id)
        .await
        .map_err(|e| ApiError::Db(e).into_response())?
        .ok_or_else(|| {
            tracing::debug!("webauthn auth: token names an account that does not exist");
            plain()
        })?;
    if verified.key_generation < current {
        tracing::info!(
            token_generation = verified.key_generation,
            current,
            "webauthn auth: token of a superseded key generation refused"
        );
        return Err(ApiError::KeyGenerationSuperseded.into_response());
    }
    if verified.key_generation > current {
        // Unreachable for an honestly issued token (the generation only
        // grows and the secret is auth's); refuse rather than admit a token
        // the row cannot account for.
        tracing::error!(
            token_generation = verified.key_generation,
            current,
            "webauthn auth: token generation above the account's"
        );
        return Err(plain());
    }
    match crate::session::floor_verdict(&state.db, &verified)
        .await
        .map_err(|e| ApiError::Db(e).into_response())?
    {
        crate::session::FloorVerdict::Above => {},
        crate::session::FloorVerdict::LoggedOut => {
            tracing::debug!("webauthn auth: token of a logged-out device session refused");
            return Err(plain());
        },
        crate::session::FloorVerdict::Removed => {
            tracing::info!("webauthn auth: token of a removed device refused");
            return Err(ApiError::DeviceRevoked.into_response());
        },
    }
    request
        .extensions_mut()
        .insert(AuthenticatedAccount(verified.account_id));
    request.extensions_mut().insert(AuthenticatedDevice {
        device_id: verified.device_id,
        key_generation: verified.key_generation,
    });
    Ok(next.run(request).await)
}

// ----------------------------------------------------------------
// Wire shapes
// ----------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct RegisterStartReq {
    /// The fresh-auth grant a password login, a passkey login or a signup
    /// returned. Optional in the type so a body without it is answered the
    /// `403 fresh-auth-required` every other refusal gets, not a
    /// deserialisation error.
    #[serde(default)]
    pub fresh_grant: Option<String>,
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
    /// The fresh-auth grant, as on the start. Optional in the type for the
    /// same reason.
    #[serde(default)]
    pub fresh_grant: Option<String>,
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
    /// URL-safe base64 (no padding) of the 32-byte id of the device this
    /// session lands on: its conversation leaf signature key (ADR-0074
    /// section 0.4). Absent when the device has no keys yet; the session then
    /// names no device and the client calls `POST /auth/session/bind-device`
    /// once it has them.
    #[serde(default)]
    pub device_id_b64: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct AuthFinishRes {
    /// URL-safe base64 of the 16-byte UUIDv7 account id.
    pub account_id_b64: String,
    /// The session pair (`session_token`, `session_expires_at_ms`,
    /// `refresh_token`, `refresh_expires_at_ms`, `key_generation`), the
    /// same shape every login route returns (ADR-0073).
    #[serde(flatten)]
    pub session: crate::session::SessionPair,
    /// `fresh_grant` + `fresh_grant_expires_at_ms`: a passkey sign-in is one
    /// of the two that prove who the person is (ADR-0073, amended
    /// 2026-10-06), so its answer carries a fresh-auth grant like the
    /// password login's.
    #[serde(flatten)]
    pub grant: crate::session::FreshGrant,
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
    /// ADR-0037 F5b (`/auth/backup/head`): the per-account mint rate-limit
    /// is spent. Surfaced as a generic `429`.
    RateLimited,
    /// ADR-0018 factor (c) (`/totp/enroll/start`): the account already has a
    /// CONFIRMED authenticator. Surfaced as `409` so a stolen session cannot
    /// silently swap the authenticator (the user must disable it first).
    TotpAlreadyEnabled,
    /// ADR-0054 S4: an attempt limit refused the request — `429` with a
    /// stable body code and `Retry-After`, or `503` when the counter store
    /// failed (fail closed). Never `401`: a limit is not a bad credential.
    Limit(crate::limits::LimitError),
    /// ADR-0054 S5: `LAKKO_AUTH_HANDLE_PEPPER` is unset, so no route that
    /// names an account by login handle can resolve it. `503` with the JSON
    /// body `{"error":"login-by-name-disabled"}`.
    LoginByNameDisabled,
    /// ADR-0054 S5 (`PUT /auth/login-handle`): another account holds the
    /// handle. `409` with the JSON body `{"error":"handle-taken"}`.
    HandleTaken,
    /// ADR-0073 section 3: the credential was issued under a key generation
    /// below the account's current one — the account's keys were reset on
    /// another device and this device is signed out. `401` with the JSON
    /// body `{"error":"key-generation-superseded"}`, the one `401` a client
    /// acts on by wiping itself.
    KeyGenerationSuperseded,
    /// ADR-0073 section 3.2 (`POST /auth/session/refresh`): the refresh
    /// credential did not verify, expired, or names no account. One body for
    /// all three, `401 {"error":"refresh-invalid"}`.
    RefreshInvalid,
    /// ADR-0073 (amended 2026-10-04 and 2026-10-06), the fresh-auth-gated
    /// routes: no live fresh-auth grant for the bearer's account at its
    /// current generation (missing, malformed, expired, another account's,
    /// or of a generation a reset has since replaced). One body for all of
    /// them, `403 {"error":"fresh-auth-required"}`.
    FreshAuthRequired,
    /// ADR-0074 section 0.4: the credential names a device that was removed
    /// from the account after the credential was issued. `401` with the JSON
    /// body `{"error":"device-revoked"}`: the device stops, shows the removed
    /// screen and wipes itself.
    DeviceRevoked,
    /// ADR-0074 sections 0.1 and 2.1: the route needs a statement signed by
    /// the account's recorded identity key and got none that verifies (no
    /// key recorded yet, a malformed statement, another key, another device,
    /// outside the time window). One body for all of them, `403
    /// {"error":"account-key-required"}`.
    AccountKeyRequired,
    /// ADR-0074 section 0.1 (`POST /auth/genesis/attest`): the account's
    /// identity key is already recorded and the request names another one.
    /// `409 {"error":"identity-key-mismatch"}`.
    IdentityKeyMismatch,
    /// ADR-0074 section 0.4 (`POST /auth/session/bind-device`): the session
    /// already names another device. `409 {"error":"device-already-bound"}`.
    DeviceAlreadyBound,
    /// ADR-0074 section 1.2: a recall request without a statement that
    /// verifies under a live anchor's key, or naming a state, an anchor or an
    /// account that is not there. One body for every such refusal, `401
    /// {"error":"recall-refused"}`, so a caller without the anchor key learns
    /// nothing, not even whether the account exists.
    RecallRefused,
    /// ADR-0074 section 1.2 (`POST /account/recall/finish`): the statement
    /// verified and the password from before the change did not. `401
    /// {"error":"wrong-password"}`.
    WrongPassword,
    /// ADR-0074 "T5 as built": an account statement that verifies under the
    /// key the route checks it against, but whose signing time lies outside
    /// the two-minute window: the device's clock is wrong. `401
    /// {"error":"statement-stale","server_time_ms":...}`; the device signs
    /// again with the server's time. Answered ONLY when the signature
    /// verified, so a caller without the key never sees it.
    StatementStale {
        /// The server's clock, unix milliseconds.
        server_time_ms: i64,
    },
    /// ADR-0074 section 1.3 (`DELETE /account`): an unexpired recall anchor
    /// exists, so the account cannot be deleted yet. `409
    /// {"error":"recall-window-open","until_ms":...}`.
    RecallWindowOpen {
        /// Unix milliseconds at which the last open anchor expires.
        until_ms: i64,
    },
    /// A refusal that is a status and one stable JSON body code,
    /// `{"error": <code>}`, with nothing else to carry. Used by the
    /// ADR-0075 recovery-email routes (`recovery-email-disabled`,
    /// `code-refused`, `ticket-refused` and the rest, listed in
    /// `crate::recovery_email`).
    Coded(StatusCode, &'static str),
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
        if matches!(self, Self::FreshAuthRequired) {
            crate::metrics::fresh_auth_refused();
        }
        let code = self.body_code();
        let mut response = self.render();
        if let Some(code) = code {
            response
                .extensions_mut()
                .insert(lakko_metrics::ErrorCode(code));
        }
        response
    }
}

impl ApiError {
    /// The stable `{"error": <code>}` body code of the answer, for the
    /// metrics `code` label (ADR-0096); `None` for the plain-text answers.
    const fn body_code(&self) -> Option<&'static str> {
        match self {
            Self::Limit(e) => Some(e.code()),
            Self::LoginByNameDisabled => Some("login-by-name-disabled"),
            Self::HandleTaken => Some("handle-taken"),
            Self::KeyGenerationSuperseded => Some(CODE_KEY_GENERATION_SUPERSEDED),
            Self::RefreshInvalid => Some(CODE_REFRESH_INVALID),
            Self::FreshAuthRequired => Some(CODE_FRESH_AUTH_REQUIRED),
            Self::DeviceRevoked => Some(CODE_DEVICE_REVOKED),
            Self::AccountKeyRequired => Some(CODE_ACCOUNT_KEY_REQUIRED),
            Self::IdentityKeyMismatch => Some(CODE_IDENTITY_KEY_MISMATCH),
            Self::DeviceAlreadyBound => Some(CODE_DEVICE_ALREADY_BOUND),
            Self::RecallRefused => Some(CODE_RECALL_REFUSED),
            Self::WrongPassword => Some(CODE_WRONG_PASSWORD),
            Self::StatementStale { .. } => Some(CODE_STATEMENT_STALE),
            Self::RecallWindowOpen { .. } => Some(CODE_RECALL_WINDOW_OPEN),
            Self::Coded(_, code) => Some(*code),
            Self::Disabled
            | Self::BadInput(_)
            | Self::StateMissing
            | Self::NoCredentials
            | Self::ActionRequired
            | Self::InvalidCredential
            | Self::RateLimited
            | Self::TotpAlreadyEnabled
            | Self::Internal
            | Self::Db(_)
            | Self::Webauthn(_)
            | Self::SerdeJson(_) => None,
        }
    }

    #[allow(clippy::too_many_lines)] // a flat variant-to-answer table
    fn render(self) -> axum::response::Response {
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
            Self::Limit(e) => return e.into_response(),
            Self::LoginByNameDisabled => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    axum::Json(serde_json::json!({ "error": "login-by-name-disabled" })),
                )
                    .into_response();
            },
            Self::HandleTaken => {
                return (
                    StatusCode::CONFLICT,
                    axum::Json(serde_json::json!({ "error": "handle-taken" })),
                )
                    .into_response();
            },
            Self::KeyGenerationSuperseded => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": CODE_KEY_GENERATION_SUPERSEDED })),
                )
                    .into_response();
            },
            Self::RefreshInvalid => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": CODE_REFRESH_INVALID })),
                )
                    .into_response();
            },
            Self::FreshAuthRequired => {
                return (
                    StatusCode::FORBIDDEN,
                    axum::Json(serde_json::json!({ "error": CODE_FRESH_AUTH_REQUIRED })),
                )
                    .into_response();
            },
            Self::DeviceRevoked => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": CODE_DEVICE_REVOKED })),
                )
                    .into_response();
            },
            Self::AccountKeyRequired => {
                return (
                    StatusCode::FORBIDDEN,
                    axum::Json(serde_json::json!({ "error": CODE_ACCOUNT_KEY_REQUIRED })),
                )
                    .into_response();
            },
            Self::IdentityKeyMismatch => {
                return (
                    StatusCode::CONFLICT,
                    axum::Json(serde_json::json!({ "error": CODE_IDENTITY_KEY_MISMATCH })),
                )
                    .into_response();
            },
            Self::DeviceAlreadyBound => {
                return (
                    StatusCode::CONFLICT,
                    axum::Json(serde_json::json!({ "error": CODE_DEVICE_ALREADY_BOUND })),
                )
                    .into_response();
            },
            Self::RecallRefused => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": CODE_RECALL_REFUSED })),
                )
                    .into_response();
            },
            Self::WrongPassword => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": CODE_WRONG_PASSWORD })),
                )
                    .into_response();
            },
            Self::StatementStale { server_time_ms } => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({
                        "error": CODE_STATEMENT_STALE,
                        "server_time_ms": server_time_ms,
                    })),
                )
                    .into_response();
            },
            Self::RecallWindowOpen { until_ms } => {
                return (
                    StatusCode::CONFLICT,
                    axum::Json(serde_json::json!({
                        "error": CODE_RECALL_WINDOW_OPEN,
                        "until_ms": until_ms,
                    })),
                )
                    .into_response();
            },
            Self::Coded(status, code) => {
                return (status, axum::Json(serde_json::json!({ "error": code }))).into_response();
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

impl From<crate::limits::LimitError> for ApiError {
    fn from(e: crate::limits::LimitError) -> Self {
        Self::Limit(e)
    }
}

#[cfg(test)]
mod extra_origin_tests {
    use super::parse_extra_origins;
    use url::Url;
    use webauthn_rs::prelude::WebauthnBuilder;

    const ANDROID: &str = "android:apk-key-hash:LzZYH_1lt2CydanB0V530qXBoc8jdpYFJyS0YaibshI";

    #[test]
    fn empty_and_blank_entries_give_no_origin() {
        assert!(parse_extra_origins("").unwrap().is_empty());
        assert!(parse_extra_origins(" , ,").unwrap().is_empty());
    }

    #[test]
    fn an_android_app_origin_parses_and_matches_only_itself() {
        let extra = parse_extra_origins(&format!(" {ANDROID} ")).unwrap();
        assert_eq!(extra.len(), 1);
        let own = Url::parse(ANDROID).unwrap();
        let other = Url::parse("android:apk-key-hash:AAAA").unwrap();
        assert!(webauthn_rs_core::WebauthnCore::origins_match(
            false, false, &own, &extra[0]
        ));
        assert!(!webauthn_rs_core::WebauthnCore::origins_match(
            false, false, &other, &extra[0]
        ));
        let web = Url::parse("https://test.lakko.example").unwrap();
        let webauthn = WebauthnBuilder::new("test.lakko.example", &web)
            .unwrap()
            .append_allowed_origin(&extra[0])
            .build();
        assert!(webauthn.is_ok());
    }

    #[test]
    fn an_entry_that_is_not_a_url_refuses_the_start() {
        assert!(parse_extra_origins("not a url").is_err());
    }
}

#[cfg(test)]
mod discoverable_tests {
    use super::*;

    fn origin() -> Url {
        Url::parse("https://test.lakko.example").expect("origin")
    }

    fn webauthn() -> Webauthn {
        WebauthnBuilder::new("test.lakko.example", &origin())
            .expect("builder")
            .rp_name("Lakko test")
            .build()
            .expect("build")
    }

    #[test]
    fn passkey_registration_asks_for_a_discoverable_credential() {
        let wa = webauthn();
        let (mut ccr, _state) = wa
            .start_passkey_registration(Uuid::new_v4(), "tester", "Tester", None)
            .expect("start");
        require_discoverable(&mut ccr);
        let json = serde_json::to_value(&ccr).expect("json");
        let sel = &json["publicKey"]["authenticatorSelection"];
        assert_eq!(sel["residentKey"], "required");
        assert_eq!(sel["requireResidentKey"], true);
        assert_eq!(sel["userVerification"], "required");
    }

    #[test]
    fn only_the_authenticator_request_changes() {
        // The registration state is not touched (its verification ignores
        // the resident-key request); in the options sent to the client only
        // the authenticator selection moves: challenge, user, relying party,
        // algorithms and extensions stay as webauthn-rs built them.
        let wa = webauthn();
        let (ccr, _state) = wa
            .start_passkey_registration(Uuid::new_v4(), "tester", "Tester", None)
            .expect("start");
        let mut changed = ccr.clone();
        require_discoverable(&mut changed);
        let mut before = serde_json::to_value(&ccr).expect("json");
        let mut after = serde_json::to_value(&changed).expect("json");
        before["publicKey"]
            .as_object_mut()
            .expect("object")
            .remove("authenticatorSelection");
        after["publicKey"]
            .as_object_mut()
            .expect("object")
            .remove("authenticatorSelection");
        assert_eq!(before, after);
    }
}
