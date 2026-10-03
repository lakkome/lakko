// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Reset the account keys: `POST /account/keys/reset/{start,finish}`
//! ([ADR-0065] section 2).
//!
//! The interface calls it "Reset account keys"; it is the server half of the
//! client's `mintFreshIdentity`. The person holds a session and no route to
//! the old keys, mints a new recovery phrase, derives a new identity, and
//! asks the server to make that identity the account's. Two round trips,
//! because replacing the recovery credential is an OPAQUE registration and
//! that is two messages:
//!
//! 1. `start {recovery_request}` -> `{recovery_response}`: stateless,
//!    `ServerRegistration::start` under `account_id || KIND_RECOVERY`.
//! 2. `finish {recovery_upload, new_signature_pubkey_b64}` -> the key
//!    generation, the key-reset attestation and a fresh session pair, after
//!    ONE transaction, in this order:
//!    1. bump `accounts.identity_version` (the key generation) and read the
//!       new value `g` in one statement (the `LAST_INSERT_ID` idiom);
//!    2. replace the recovery credential: delete the account's in-flight
//!       recovery-login and password-reset states, upsert `KIND_RECOVERY`;
//!    3. delete the passkey vaults, then the passkeys, then the account's
//!       WebAuthn states and action grants, so no half-finished ceremony of
//!       the old generation can complete;
//!    4. delete the account's device grants and device requests: a grant an
//!       old device minted must not land a fresh device in a session that
//!       then holds the old keys;
//!    5. mint the [`KeyResetAttestation`] over `(account_ref, journal id, g,
//!       new_signature_pubkey, now)` with the shared attest key;
//!    6. issue the caller an access token and a refresh credential at `g`.
//!
//! After the commit every other device's credentials are of generation
//! `g - 1` and refused by `require_session` and the refresh route; the
//! caller's own old token is superseded too, which is why the response
//! carries its replacement.
//!
//! **Kept, deliberately:** the password envelope and the TOTP secret. The
//! password is the credential the person just used to get here; TOTP lives
//! in an authenticator app, not on a Lakko device, and is an ACCESS factor
//! that cannot yield keys. **No passkey action grant**, even when the account
//! has a passkey: the route exists for the lost-everything path, where the
//! passkey may be gone too. The residual, stated in the ADR: a stolen session
//! can reset the keys, strip every passkey and sign every other device out,
//! bounded by the limits below; what changes for the owner is that the theft
//! is loud.
//!
//! Limits: both calls charge [`Scope::RecoveryReplace`] (the folded-in
//! credential replace's counter, 10 per hour by default) and `finish` also
//! charges [`Scope::KeyReset`] (3 per day by default); a `429
//! too-many-attempts` carries `retry_after_secs`.
//!
//! The `503 key-reset-disabled` the ADR names for a missing attest key cannot
//! occur: `load_or_init_attest_key` runs at boot and the key is not optional.
//!
//! [ADR-0065]: ../../../docs/adr/0065-account-key-reset.md

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
use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Body of `POST /account/keys/reset/start`.
#[derive(Deserialize)]
pub(crate) struct ResetStartReq {
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
    /// Base64-encoded `RegistrationUpload` for the new recovery seed.
    pub(crate) recovery_upload: String,
    /// URL-safe base64 of the new identity's 32-byte Ed25519 signature
    /// pubkey: the key the reset installs, bound into the attestation.
    pub(crate) new_signature_pubkey_b64: String,
}

/// Response of `POST /account/keys/reset/finish`.
#[derive(Serialize)]
pub(crate) struct ResetFinishRes {
    /// URL-safe base64 of the 177-byte key-reset attestation the client
    /// carries to discovery (the identity upload) and to the DS (the
    /// DeviceGroup journal reset).
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
    Ok(key)
}

/// `POST /account/keys/reset/start` — bearer-auth. The first OPAQUE message
/// of the recovery-credential replace. A malformed request is a `400` and
/// spends nothing; a well-formed one is charged to the account's
/// [`Scope::RecoveryReplace`] counter. Stores nothing.
#[axum::debug_handler]
pub(crate) async fn start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(payload): Json<ResetStartReq>,
) -> Result<Json<ResetStartRes>, OpaqueError> {
    let account_id = auth.0;
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

/// `POST /account/keys/reset/finish` — bearer-auth. The transaction of the
/// module doc, then the attestation and the new pair.
///
/// Order: decode the body (`400`), charge [`Scope::RecoveryReplace`] then
/// [`Scope::KeyReset`] (`429`), run the transaction. A failure anywhere in it
/// rolls the whole reset back: the generation, the credential and every
/// delete move together or not at all.
///
/// A live token for an account deleted meanwhile reaches the generation bump
/// with no row to update and is answered with the enumeration-safe `401`.
#[axum::debug_handler]
pub(crate) async fn finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(payload): Json<ResetFinishReq>,
) -> Result<Json<ResetFinishRes>, OpaqueError> {
    let account_id = auth.0;
    let upload = decode_registration_upload(&payload.recovery_upload)?;
    let new_signature_pubkey = decode_pubkey(&payload.new_signature_pubkey_b64)?;
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

    let mut tx = state.db.begin().await.map_err(ApiError::Db)?;

    // 1. Bump the key generation and read the new value in one statement.
    let bumped = sqlx::query(
        "UPDATE accounts SET identity_version = LAST_INSERT_ID(identity_version + 1) WHERE id = ?",
    )
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
    for statement in [
        "DELETE FROM passkey_vaults WHERE account_id = ?",
        "DELETE FROM webauthn_credentials WHERE account_id = ?",
        "DELETE FROM webauthn_states WHERE account_id = ?",
        "DELETE FROM webauthn_action_grants WHERE account_id = ?",
        "DELETE FROM device_grants WHERE account_id = ?",
        "DELETE FROM device_requests WHERE account_id = ?",
    ] {
        sqlx::query(statement)
            .bind(account_id.as_slice())
            .execute(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    }

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
    let session = crate::session::issue_pair_at(&state, &account_id, key_generation)?;

    tx.commit().await.map_err(ApiError::Db)?;

    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        key_generation,
        "account keys reset",
    );
    Ok(Json(ResetFinishRes {
        key_reset_attest_b64: URL_SAFE_NO_PAD.encode(attestation),
        session,
    }))
}
