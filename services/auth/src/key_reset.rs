// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Reset the account keys: `POST /account/keys/reset/{start,finish}`
//! ([ADR-0073] section 2).
//!
//! The interface calls it "Reset account keys"; it is the server half of the
//! client's `mintFreshIdentity`. The person holds a session and no route to
//! the old keys, mints a new recovery phrase, derives a new identity, and
//! asks the server to make that identity the account's. Two round trips,
//! because replacing the recovery credential is an OPAQUE registration and
//! that is two messages:
//!
//! 1. `start {reset_grant, recovery_request}` -> `{recovery_response}`:
//!    stateless, `ServerRegistration::start` under `account_id ||
//!    KIND_RECOVERY`.
//! 2. `finish {reset_grant, recovery_upload, new_signature_pubkey_b64,
//!    identity_proof_b64, device_id_b64?}` -> the key generation, the
//!    key-reset attestation and a fresh session pair, after ONE transaction,
//!    in this order:
//!    1. lock the account row and compare its key generation with the
//!       grant's (equal, or the whole reset is refused), capture the recall
//!       anchor (ADR-0074 section 0.2: the identity key and the password
//!       envelope as they are before this change, unless an anchor for the
//!       current key already exists; see `crate::recall`), then bump
//!       `accounts.identity_version`, record the new key in
//!       `accounts.identity_pubkey` (ADR-0074 section 0.1) and read the new
//!       value `g` in one statement (the `LAST_INSERT_ID` idiom);
//!    2. replace the recovery credential: delete the account's in-flight
//!       recovery-login and password-reset states, upsert `KIND_RECOVERY`;
//!    3. delete the passkey vaults, then the passkeys, then the account's
//!       WebAuthn states and action grants, so no half-finished ceremony of
//!       the old generation can complete;
//!    4. delete the account's device grants and device requests: a grant an
//!       old device minted must not land a fresh device in a session that
//!       then holds the old keys;
//!    5. mint the [`KeyResetAttestation`] over `(account_ref, g,
//!       new_signature_pubkey, now)` with the shared attest key (since
//!       ADR-0078 phase 6 it names no DeviceGroup journal: auth cannot derive
//!       that id and must not learn it);
//!    6. issue the caller an access token and a refresh credential at `g`.
//!
//! After the commit every other device's credentials are of generation
//! `g - 1` and refused by `require_session` and the refresh route; the
//! caller's own old token is superseded too, which is why the response
//! carries its replacement.
//!
//! # The password and a second factor (ADR-0073, amended 2026-11-26)
//!
//! Since ADR-0101 a reset brings the account's conversations, channels and
//! history back, so whoever resets becomes the account toward everyone. Both
//! calls require a **reset factor grant** (`reset_grant` in the body): the
//! `f1` credential of `lakko-auth-token` that only the four reset factor
//! routes of [`crate::reset_factor`] mint, each after a fresh-auth grant
//! minted by the PASSWORD (a passkey sign-in does not stand in for it) AND
//! one second factor the account has (its authenticator, a code mailed to
//! its recovery email, or an assertion by one of its passkeys). A fresh-auth
//! grant alone is refused.
//!
//! A grant that is missing, malformed, expired, issued for another account
//! than the bearer's, or at another key generation than the account's
//! current one is answered `403 {"error":"reset-factor-required"}`: one code
//! for all of them, nothing done, nothing charged. It is stateless, and the
//! reset spends it by construction: `finish` accepts it only while the row
//! is at exactly the generation it names, compared under the row lock that
//! bumps it. The same grant opens `POST /account/owned-channels-secret`
//! (ADR-0101 section 7), which does not spend it.
//!
//! Reverses the 2026-10-04 reasoning that a reset must not need TOTP or a
//! passkey: the cost the earlier text named is now accepted, a person whose
//! only factor was on the phone they lost cannot reset (UX 19.10).
//!
//! **Kept, deliberately:** the password envelope and the TOTP secret. The
//! password is the credential the person just used to get here; TOTP lives
//! in an authenticator app.
//!
//! Limits: both calls charge [`Scope::RecoveryReplace`] (10 per hour by
//! default) and `finish` also charges [`Scope::KeyReset`] (3 per day by
//! default); a `429 too-many-attempts` carries `retry_after_secs`. Order in
//! both handlers: the grant (`403`), the body (`400`), the counters (`429`).
//!
//! The `503 key-reset-disabled` the ADR names for a missing attest key cannot
//! occur: `load_or_init_attest_key` runs at boot and the key is not optional.
//!
//! [ADR-0073]: ../../../docs/adr/0073-account-key-reset.md

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_attest::KeyResetAttestation;
use opaque_ke::ServerRegistration;
use serde::{Deserialize, Serialize};

use crate::limits::Scope;
use crate::opaque::{
    decode_registration_request, decode_registration_upload, opaque_registration_start,
    LakkoCipherSuite, OpaqueError, KIND_PASSWORD_RESET, KIND_RECOVERY,
};
use crate::session::SessionPair;
use crate::webauthn::{ApiError, AuthenticatedAccount, AuthenticatedDevice};
use crate::AppState;

/// Body of `POST /account/keys/reset/start`.
#[derive(Deserialize)]
pub(crate) struct ResetStartReq {
    /// The `f1` reset factor grant a reset factor route returned. Optional
    /// on the wire so that its absence is the `403 reset-factor-required`
    /// every other refusal gets, not a deserialisation error.
    #[serde(default)]
    pub(crate) reset_grant: Option<String>,
    /// Base64-encoded `RegistrationRequest` made from the NEW recovery
    /// phrase's 64-byte BIP39 seed.
    pub(crate) recovery_request: String,
}

/// Response of `POST /account/keys/reset/start`.
#[derive(Serialize)]
pub(crate) struct ResetStartRes {
    /// Base64-encoded `RegistrationResponse`; the client passes it to the
    /// matching `ClientRegistration::finish`.
    pub(crate) recovery_response: String,
}

/// Body of `POST /account/keys/reset/finish`.
#[derive(Deserialize)]
pub(crate) struct ResetFinishReq {
    /// The `f1` reset factor grant, as in [`ResetStartReq`]. The same grant
    /// serves both calls of one reset.
    #[serde(default)]
    pub(crate) reset_grant: Option<String>,
    /// Base64-encoded `RegistrationUpload` for the new recovery seed.
    pub(crate) recovery_upload: String,
    /// URL-safe base64 of the new identity's 32-byte Ed25519 signature
    /// pubkey: the key the reset installs, bound into the attestation and
    /// recorded as the account's identity key (ADR-0074 section 0.1).
    pub(crate) new_signature_pubkey_b64: String,
    /// URL-safe base64 of the 64-byte identity proof by the NEW identity key
    /// over the account id and that key (`signIdentityProof`, ADR-0099 F3).
    /// Absent or invalid: `400 identity-proof-invalid`.
    #[serde(default)]
    pub(crate) identity_proof_b64: Option<String>,
    /// URL-safe base64 of the 32-byte device id the new pair names
    /// (ADR-0074 section 0.4). Absent: the pair names the device the
    /// caller's bearer names. A reset that gives the device a new
    /// conversation leaf key sends the new id here.
    #[serde(default)]
    pub(crate) device_id_b64: Option<String>,
}

/// Response of `POST /account/keys/reset/finish`.
#[derive(Serialize)]
pub(crate) struct ResetFinishRes {
    /// URL-safe base64 of the 145-byte key-reset attestation the client
    /// carries to discovery (the identity upload).
    pub(crate) key_reset_attest_b64: String,
    /// `key_generation` (the generation the account is now at),
    /// `session_token`, `session_expires_at_ms`, `refresh_token`,
    /// `refresh_expires_at_ms`, all at the new generation.
    #[serde(flatten)]
    pub(crate) session: SessionPair,
}

/// Decode the URL-safe base64 32-byte signature pubkey from the wire.
fn decode_pubkey(b64: &str) -> Result<[u8; 32], ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("new_signature_pubkey_b64 not URL-safe base64"))?;
    let key: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::BadInput("new_signature_pubkey must decode to 32 bytes"))?;
    // The key is recorded as the account's identity key and will verify
    // account statements: an undecodable or small-order one is refused.
    crate::genesis_attest::require_usable_identity_key(&key)?;
    Ok(key)
}

/// Steps 3 and 4 of the reset's transaction: delete the passkey vaults, the
/// passkeys, every in-flight WebAuthn ceremony and action grant, and the
/// account's device grants and device requests. Shared with the recall
/// (`crate::recall::finish`), which is this reset plus a password change.
///
/// ADR-0075: recovery-email tickets in flight go too. The `recovery_emails`
/// row (the password route, and since 2026-11-26 a reset factor) is kept by
/// a reset and deleted by a recall.
///
/// ADR-0092 section 4.2: the anonymous-credential commitment and the
/// issuance rows go too; the person's new seed registers a new commitment.
pub(crate) async fn delete_superseded_state(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    account_id: &[u8; 16],
) -> Result<(), ApiError> {
    for statement in [
        "DELETE FROM passkey_vaults WHERE account_id = ?",
        "DELETE FROM email_tickets WHERE account_id = ?",
        "DELETE FROM webauthn_credentials WHERE account_id = ?",
        "DELETE FROM webauthn_states WHERE account_id = ?",
        "DELETE FROM webauthn_action_grants WHERE account_id = ?",
        "DELETE FROM device_grants WHERE account_id = ?",
        "DELETE FROM device_requests WHERE account_id = ?",
        // ADR-0092 section 4.2: the new seed gives a new credential secret,
        // which registers a new commitment.
        "DELETE FROM anon_credential_commitments WHERE account_id = ?",
        "DELETE FROM anon_credential_issued WHERE account_id = ?",
    ] {
        sqlx::query(statement)
            .bind(account_id.as_slice())
            .execute(&mut **tx)
            .await
            .map_err(ApiError::Db)?;
    }
    Ok(())
}

/// The device the reset's new pair names: the one the body asserts, else the
/// one the caller's bearer names.
fn pair_device(
    device_id_b64: Option<&str>,
    caller: &AuthenticatedDevice,
) -> Result<crate::session::DeviceId, ApiError> {
    device_id_b64.map_or(Ok(caller.device_id), |b64| {
        crate::session::decode_device_id(Some(b64))
    })
}

/// `POST /account/keys/reset/start` — bearer-auth. The first OPAQUE message
/// of the recovery-credential replace. Without a live reset factor grant it
/// is a `403 reset-factor-required`; a malformed request is a `400`; neither spends
/// anything. A well-formed one is charged to the account's
/// [`Scope::RecoveryReplace`] counter. Stores nothing.
#[axum::debug_handler]
pub(crate) async fn start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(payload): Json<ResetStartReq>,
) -> Result<Json<ResetStartRes>, OpaqueError> {
    let account_id = auth.0;
    crate::session::require_reset_grant(&state, &account_id, payload.reset_grant.as_deref())
        .await?;
    let request = decode_registration_request(&payload.recovery_request)?;
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::RecoveryReplace,
        &account_id,
    )
    .await?;
    let recovery_response =
        opaque_registration_start(&state.opaque_setup, &account_id, KIND_RECOVERY, request)?;
    Ok(Json(ResetStartRes { recovery_response }))
}

/// `POST /account/keys/reset/finish`, counted as a key reset (ADR-0096); the
/// flow is [`finish_answer`].
pub(crate) async fn finish(
    state: State<AppState>,
    auth: Extension<AuthenticatedAccount>,
    caller: Extension<AuthenticatedDevice>,
    payload: Json<ResetFinishReq>,
) -> axum::response::Response {
    crate::metrics::key_reset(finish_answer(state, auth, caller, payload).await)
}

/// `POST /account/keys/reset/finish` — bearer-auth. The transaction of the
/// module doc, then the attestation and the new pair.
///
/// Order: check the reset factor grant (`403`), decode the body (`400`), charge
/// [`Scope::RecoveryReplace`] then [`Scope::KeyReset`] (`429`), run the
/// transaction, whose first step compares the grant's generation with the
/// row's again under the row lock (`403`). A failure anywhere in it rolls the
/// whole reset back: the generation, the credential and every delete move
/// together or not at all.
///
/// A live token for an account deleted meanwhile reaches the generation bump
/// with no row to update and is answered with the enumeration-safe `401`.
#[axum::debug_handler]
#[allow(clippy::too_many_lines)] // one transaction, its steps numbered as in the module doc
async fn finish_answer(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Extension(caller): Extension<AuthenticatedDevice>,
    Json(payload): Json<ResetFinishReq>,
) -> Result<Json<ResetFinishRes>, OpaqueError> {
    let account_id = auth.0;
    let device_id = pair_device(payload.device_id_b64.as_deref(), &caller)?;
    let grant_generation =
        crate::session::require_reset_grant(&state, &account_id, payload.reset_grant.as_deref())
            .await?;
    let upload = decode_registration_upload(&payload.recovery_upload)?;
    let new_signature_pubkey = decode_pubkey(&payload.new_signature_pubkey_b64)?;
    crate::identity_target::require_identity_proof(
        payload.identity_proof_b64.as_deref(),
        &account_id,
        &new_signature_pubkey,
    )?;
    let registration_bytes = ServerRegistration::<LakkoCipherSuite>::finish(upload)
        .serialize()
        .to_vec();

    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::RecoveryReplace,
        &account_id,
    )
    .await?;
    crate::limits::charge_name(&state.db, &state.limits, Scope::KeyReset, &account_id).await?;

    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;

    // 1. Lock the row and hold the grant to the generation it now has: of
    //    two requests carrying grants of one generation, the second waits
    //    here, then reads the bumped value and is refused. This is what makes
    //    a grant single-use.
    let locked: Option<u64> =
        sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ? FOR UPDATE")
            .bind(account_id.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    match locked {
        None => return Err(ApiError::InvalidCredential.into()),
        Some(current) if current != grant_generation => {
            return Err(ApiError::Coded(
                axum::http::StatusCode::FORBIDDEN,
                crate::session::CODE_RESET_FACTOR_REQUIRED,
            )
            .into());
        },
        Some(_) => {},
    }

    //    The recall anchor (ADR-0074 section 0.2), before anything changes:
    //    the identity key and the password envelope as they are now, unless
    //    an anchor for this identity key already holds an earlier state.
    //    Under the row lock just taken, so two changes capture one anchor.
    crate::recall::capture_anchor(
        &mut tx,
        &account_id,
        grant_generation,
        crate::recall::Change::IdentityKey,
        state.limits.recall_window_ms(),
    )
    .await
    .map_err(ApiError::Db)?;

    //    The new key's target, claimed for this account (ADR-0099 F3): a key
    //    another account holds is refused and the reset rolls back.
    crate::identity_target::claim(
        &mut tx,
        &account_id,
        &new_signature_pubkey,
        crate::session::now_ms(),
    )
    .await?;

    //    Bump the key generation and read the new value in one statement.
    //    The same statement records the new identity key (ADR-0074 section
    //    0.1).
    let bumped = sqlx::query(
        "UPDATE accounts SET identity_version = LAST_INSERT_ID(identity_version + 1), \
         identity_pubkey = ?, identity_target = ? WHERE id = ?",
    )
    .bind(&new_signature_pubkey[..])
    .bind(&crate::identity_target::target_of(&new_signature_pubkey)[..])
    .bind(account_id.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    if bumped.rows_affected() == 0 {
        return Err(ApiError::InvalidCredential.into());
    }
    let key_generation: u64 = sqlx::query_scalar("SELECT LAST_INSERT_ID()")
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::Db)?;

    // 2. Replace the recovery credential.
    sqlx::query("DELETE FROM opaque_login_states WHERE account_id = ? AND kind IN (?, ?)")
        .bind(account_id.as_slice())
        .bind(KIND_RECOVERY)
        .bind(KIND_PASSWORD_RESET)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    sqlx::query(
        "INSERT INTO opaque_registrations (account_id, kind, registration_bytes) \
         VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE registration_bytes = VALUES(registration_bytes)",
    )
    .bind(account_id.as_slice())
    .bind(KIND_RECOVERY)
    .bind(&registration_bytes)
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;

    // 3. Passkeys: the vault blobs first, then the credentials (the order
    //    `delete_credential` uses), then every in-flight ceremony and grant.
    // 4. Device grants and requests.
    delete_superseded_state(&mut tx, &account_id).await?;

    // 5. The attestation, over the new generation and the asserted key.
    let signed_at = crate::session::now_ms();
    let attestation = KeyResetAttestation::for_account(
        &account_id,
        key_generation,
        new_signature_pubkey,
        signed_at,
    )
    .sign(&state.attest_key.signing);

    // 6. The caller's new pair at the new generation.
    let session = crate::session::issue_pair_at(&state, &account_id, key_generation, &device_id)?;

    tx.commit().await.map_err(ApiError::Db)?;

    tracing::info!(key_generation, "account keys reset",);
    Ok(Json(ResetFinishRes {
        key_reset_attest_b64: URL_SAFE_NO_PAD.encode(attestation),
        session,
    }))
}
