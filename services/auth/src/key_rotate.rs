// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Rotate the account keys and keep the other devices:
//! `POST /account/keys/rotate/{start,finish}` ([ADR-0074] section 3.4,
//! Stage A; built in sprint T5, see the ADR's "T5 as built").
//!
//! The interface calls it "Remove this device and change your account keys".
//! A device that still holds the account's keys mints a new recovery phrase,
//! derives a new identity from it, signs a **supersession proof** with the
//! OLD identity key over the new one, and asks the server to make the new
//! identity the account's. The account's other devices are not signed out
//! for good: they are handed the new seed inside their own DeviceGroup (the
//! `lakko-contacts` key-rotation envelope) and exchange their superseded
//! session for a current one with a statement signed by the new key
//! (`POST /auth/session/refresh`, [`crate::session::refresh`]).
//!
//! # A rotation is not a reset, and what tells them apart
//!
//! A reset ([`crate::key_reset`]) needs a session and the password or a
//! passkey. So does a rotation, **and it also needs the old identity key**:
//! the supersession proof is verified here against the key auth recorded for
//! the account (`accounts.identity_pubkey`, ADR-0074 section 0.1). Someone
//! who learned the password can reset (which the owner can take back, see
//! [`crate::recall`]); they cannot rotate, because they cannot sign with a
//! key they do not hold. No recorded key, a proof that does not verify, a
//! proof by another key, an expired proof, or a proof for another new key
//! than the request names: `403 {"error":"account-key-required"}`, one code
//! for all of them.
//!
//! # The two calls
//!
//! 1. `start {fresh_grant, recovery_request}` -> `{recovery_response}`: the
//!    reset's start, unchanged. Stateless.
//! 2. `finish {fresh_grant, recovery_upload, new_signature_pubkey_b64,
//!    supersession_proof_b64, revoke_device_ids_b64}` -> the reset's answer
//!    (`key_reset_attest_b64`, `key_generation` and the session pair), after
//!    ONE transaction under the account row's lock, in this order:
//!    1. the grant's generation equals the row's (what makes a grant
//!       single-use), and the proof verifies against the identity key the
//!       row holds NOW (read under the lock: a reset or a recall that
//!       committed since the pre-check has moved it);
//!    2. the recall anchor is captured by the ordinary rule
//!       ([`crate::recall::capture_anchor`]);
//!    3. the generation is bumped and the new identity key recorded;
//!    4. the recovery credential is replaced (the in-flight recovery logins
//!       and password resets of the account are deleted first);
//!    5. the passkey vaults are deleted, with every in-flight WebAuthn
//!       ceremony and action grant, device grant and device request;
//!    6. each id in `revoke_device_ids_b64` gets a session floor, reason
//!       "removed";
//!    7. the key-reset attestation is minted and the caller gets a pair at
//!       the new generation, naming the device its bearer names.
//!
//! # What is kept and what goes (decision D9)
//!
//! - **Kept: the password, TOTP, and the passkeys.** None of them is derived
//!   from the account seed, and the person just proved one of them.
//! - **Deleted: every passkey VAULT of the account.** A vault is the old
//!   recovery phrase's entropy sealed under a passkey's PRF output. After
//!   the rotation that phrase is not the account's: opening the vault would
//!   hand a device a seed that derives the superseded identity and a
//!   recovery credential the server no longer accepts. Deleting is the whole
//!   of "turned off": the passkey still signs in, `GET /vault/passkeys`
//!   stops listing it as carrying keys, and the person turns the feature on
//!   again from a device that holds the NEW phrase, which seals the new
//!   entropy. The server cannot re-seal a vault itself (it never sees the
//!   entropy or the PRF output, invariant #2), so there is nothing to keep.
//! - **Deleted: the email vault** (ADR-0075 section 3), for the same reason:
//!   it seals the old phrase. With `phrase_saved: true` in `finish` the
//!   rotation is "Save a recovery phrase": the account is marked
//!   phrase-protected in the same transaction (it never gets an email vault
//!   again, and a mailed code changes its password only) and its devices get
//!   the "recovery phrase saved" notice. The `recovery_emails` row is kept.
//! - **Deleted: in-flight WebAuthn states and action grants, device grants,
//!   device requests.** A device grant minted before the rotation would land
//!   a new device with the old phrase.
//! - **Kept: the device session floors.** A device removed before the
//!   rotation stays refused; a recall drops the floors, a rotation must not.
//!
//! # Every other device after the commit
//!
//! Its credentials are of the previous generation: `require_session` and the
//! refresh route answer `401 key-generation-superseded`. A kept device that
//! has read the key-rotation envelope holds the new seed and upgrades; the
//! stolen device was removed from the DeviceGroup before the envelope was
//! sent, never learns the new seed, cannot sign the upgrade statement, and
//! its session is dead. So is its recovery credential, which is what a seed
//! holder would otherwise use to sign in or set a password.
//!
//! # Limits and order
//!
//! Both calls charge [`Scope::RecoveryReplace`]; `finish` also charges
//! [`Scope::KeyRotate`] (3 per day by default). Order in `finish`: the grant
//! (`403 fresh-auth-required`), the body (`400`), the proof against the
//! recorded key (`403 account-key-required`), the counters (`429`), the
//! transaction. A caller without the grant or without the account key spends
//! none of the owner's budget.
//!
//! [ADR-0074]: ../../../docs/adr/0074-account-takeover-resistance.md

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_attest::KeyResetAttestation;
use lakko_auth_token::UNBOUND_DEVICE_ID;
use opaque_ke::ServerRegistration;
use serde::Deserialize;

use crate::key_reset::{ResetFinishRes, ResetStartReq, ResetStartRes};
use crate::limits::Scope;
use crate::opaque::{
    decode_registration_request, decode_registration_upload, opaque_registration_start,
    LakkoCipherSuite, OpaqueError, KIND_PASSWORD_RESET, KIND_RECOVERY,
};
use crate::session::{DeviceId, FLOOR_REASON_REMOVED};
use crate::webauthn::{ApiError, AuthenticatedAccount, AuthenticatedDevice};
use crate::AppState;

/// At most this many device ids ride in one `finish` (ADR-0074 section 3.4;
/// chosen). A rotation removes the one stolen device; the list exists for a
/// removal that raced the commit.
pub(crate) const MAX_REVOKE_DEVICE_IDS: usize = 8;

/// Body of `POST /account/keys/rotate/finish`.
#[derive(Deserialize)]
pub(crate) struct RotateFinishReq {
    /// The `k1` fresh-auth grant a password or passkey sign-in returned.
    /// Optional on the wire so that its absence is the `403
    /// fresh-auth-required` every other refusal gets.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
    /// Standard base64 of the `RegistrationUpload` for the NEW recovery
    /// seed.
    pub(crate) recovery_upload: String,
    /// URL-safe base64 of the NEW identity's 32-byte Ed25519 signature
    /// public key.
    pub(crate) new_signature_pubkey_b64: String,
    /// URL-safe base64 of the 64-byte identity proof by the NEW identity key
    /// over the account id and that key (`signIdentityProof`, ADR-0099 F3).
    /// Absent or invalid: `400 identity-proof-invalid`.
    #[serde(default)]
    pub(crate) identity_proof_b64: Option<String>,
    /// URL-safe base64 of the 145-byte supersession proof: the OLD identity
    /// key (the one auth has recorded) signing the new one.
    pub(crate) supersession_proof_b64: String,
    /// URL-safe base64 device ids (32 bytes each, at most
    /// [`MAX_REVOKE_DEVICE_IDS`]) that get a session floor in the same
    /// transaction. Absent or empty: none.
    #[serde(default)]
    pub(crate) revoke_device_ids_b64: Vec<String>,
    /// ADR-0075 section 3: this rotation is "Save a recovery phrase". The
    /// person is shown the NEW phrase's words once it commits; the account
    /// is marked phrase-protected and its devices are told. Absent: false.
    #[serde(default)]
    pub(crate) phrase_saved: bool,
}

/// Decode the URL-safe base64 new identity key and refuse an unusable one.
fn decode_new_pubkey(b64: &str) -> Result<[u8; 32], ApiError> {
    let key: [u8; 32] = URL_SAFE_NO_PAD
        .decode(b64)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        .ok_or(ApiError::BadInput(
            "new_signature_pubkey_b64 must be 32 bytes of URL-safe base64",
        ))?;
    crate::genesis_attest::require_usable_identity_key(&key)?;
    Ok(key)
}

/// Decode the supersession proof's wire bytes (shape only).
fn decode_proof(b64: &str) -> Result<Vec<u8>, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("supersession_proof_b64 not URL-safe base64"))?;
    if bytes.len() != lakko_transparency::SUPERSESSION_PROOF_LEN {
        return Err(ApiError::BadInput(
            "supersession_proof_b64 must decode to 145 bytes",
        ));
    }
    Ok(bytes)
}

/// Decode the device ids to floor: at most [`MAX_REVOKE_DEVICE_IDS`], each 32
/// bytes, none all zero, none the caller's own device.
fn decode_revoke_ids(list: &[String], caller: &DeviceId) -> Result<Vec<DeviceId>, ApiError> {
    if list.len() > MAX_REVOKE_DEVICE_IDS {
        return Err(ApiError::BadInput(
            "revoke_device_ids_b64 carries too many device ids",
        ));
    }
    let mut out = Vec::with_capacity(list.len());
    for b64 in list {
        let id = crate::session::decode_device_id(Some(b64))?;
        if id == UNBOUND_DEVICE_ID {
            return Err(ApiError::BadInput(
                "revoke_device_ids_b64 must not name the all-zero id",
            ));
        }
        if id == *caller {
            return Err(ApiError::BadInput(
                "a device cannot revoke itself in its own rotation",
            ));
        }
        out.push(id);
    }
    Ok(out)
}

/// `true` when `proof` is a supersession proof by `recorded` (the identity
/// key auth holds for the account) that authorises exactly `new_key`, has a
/// sane validity window and has not expired at `now_ms`.
fn proof_authorises(
    proof: &[u8],
    recorded: Option<&[u8]>,
    new_key: &[u8; 32],
    now_ms: i64,
) -> bool {
    let Some(recorded) = recorded.and_then(|bytes| <[u8; 32]>::try_from(bytes).ok()) else {
        tracing::debug!("key rotation: the account has no recorded identity key");
        return false;
    };
    match lakko_transparency::verify_supersession(proof, &recorded, now_ms / 1000) {
        Ok(authorised) => authorised == *new_key && recorded != *new_key,
        Err(e) => {
            tracing::debug!(error = %e, "key rotation: supersession proof refused");
            false
        },
    }
}

/// `POST /account/keys/rotate/start` — bearer. The first OPAQUE message of
/// the recovery-credential replace; the reset's start under the rotation's
/// name. Without a live fresh-auth grant it is a `403 fresh-auth-required`;
/// a malformed request is a `400`; neither spends anything. A well-formed
/// one is charged to the account's [`Scope::RecoveryReplace`] counter.
/// Stores nothing, and does not ask for the proof: it changes nothing a
/// proof could protect.
///
/// # Errors
///
/// [`ApiError::FreshAuthRequired`], [`ApiError::Limit`], [`ApiError::Db`],
/// a `400`, inside [`OpaqueError`].
#[axum::debug_handler]
pub(crate) async fn start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(payload): Json<ResetStartReq>,
) -> Result<Json<ResetStartRes>, OpaqueError> {
    let account_id = auth.0;
    crate::session::require_fresh_grant(&state, &account_id, payload.fresh_grant.as_deref())
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

/// `POST /account/keys/rotate/finish` — bearer. The transaction of the
/// module doc, then the attestation and the new pair.
///
/// A failure anywhere in the transaction rolls the whole rotation back: the
/// generation, the identity key, the credential, the floors and every delete
/// move together or not at all.
///
/// # Errors
///
/// [`ApiError::FreshAuthRequired`], [`ApiError::BadInput`],
/// [`ApiError::AccountKeyRequired`], [`ApiError::Limit`],
/// [`ApiError::InvalidCredential`] (the account vanished), [`ApiError::Db`],
/// [`ApiError::Internal`], inside [`OpaqueError`].
#[axum::debug_handler]
#[allow(clippy::too_many_lines)] // one transaction, its steps numbered as in the module doc
pub(crate) async fn finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Extension(caller): Extension<AuthenticatedDevice>,
    Json(payload): Json<RotateFinishReq>,
) -> Result<Json<ResetFinishRes>, OpaqueError> {
    let account_id = auth.0;
    let grant_generation =
        crate::session::require_fresh_grant(&state, &account_id, payload.fresh_grant.as_deref())
            .await?;
    let upload = decode_registration_upload(&payload.recovery_upload)?;
    let new_signature_pubkey = decode_new_pubkey(&payload.new_signature_pubkey_b64)?;
    crate::identity_target::require_identity_proof(
        payload.identity_proof_b64.as_deref(),
        &account_id,
        &new_signature_pubkey,
    )?;
    let proof = decode_proof(&payload.supersession_proof_b64)?;
    let revoke_ids = decode_revoke_ids(&payload.revoke_device_ids_b64, &caller.device_id)?;
    let registration_bytes = ServerRegistration::<LakkoCipherSuite>::finish(upload)
        .serialize()
        .to_vec();

    // The proof against the recorded key, before any counter: a caller who
    // holds the session and the password but not the account key spends
    // nothing. Checked again under the row lock below.
    let recorded = crate::devices::identity_pubkey_of(&state.db, &account_id)
        .await
        .map_err(ApiError::Db)?;
    if !proof_authorises(
        &proof,
        recorded.as_ref().map(<[u8; 32]>::as_slice),
        &new_signature_pubkey,
        crate::session::now_ms(),
    ) {
        return Err(ApiError::AccountKeyRequired.into());
    }

    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::RecoveryReplace,
        &account_id,
    )
    .await?;
    crate::limits::charge_name(&state.db, &state.limits, Scope::KeyRotate, &account_id).await?;

    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;

    // 1. Lock the row. The grant is held to the generation the row has now
    //    (single use), and the proof to the identity key it has now: a reset
    //    or a recall that committed after the pre-check moved the key, and a
    //    proof by the key before it must not rotate the account they made.
    let locked: Option<(u64, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT identity_version, identity_pubkey FROM accounts WHERE id = ? FOR UPDATE",
    )
    .bind(account_id.as_slice())
    .fetch_optional(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    let Some((current, recorded_now)) = locked else {
        return Err(ApiError::InvalidCredential.into());
    };
    if current != grant_generation {
        return Err(ApiError::FreshAuthRequired.into());
    }
    let now = crate::session::now_ms();
    if !proof_authorises(&proof, recorded_now.as_deref(), &new_signature_pubkey, now) {
        return Err(ApiError::AccountKeyRequired.into());
    }

    // 2. The recall anchor (ADR-0074 section 0.2), before anything changes.
    crate::recall::capture_anchor(
        &mut tx,
        &account_id,
        grant_generation,
        crate::recall::Change::IdentityKey,
        state.limits.recall_window_ms(),
    )
    .await
    .map_err(ApiError::Db)?;

    // 3. The new key's target, claimed for this account (ADR-0099 F3), then
    //    the generation and the identity key, in one statement.
    crate::identity_target::claim(&mut tx, &account_id, &new_signature_pubkey, now).await?;
    let bumped = sqlx::query(
        "UPDATE accounts SET identity_version = LAST_INSERT_ID(identity_version + 1), phrase_in_vault = 0, \
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

    // 4. The recovery credential. From here the old seed neither signs in
    //    nor sets a password.
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

    // 5. What sealed or would carry the OLD phrase. The passkeys themselves
    //    (`webauthn_credentials`) are kept.
    for statement in [
        "DELETE FROM passkey_vaults WHERE account_id = ?",
        "DELETE FROM email_vaults WHERE account_id = ?",
        "DELETE FROM email_tickets WHERE account_id = ?",
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

    //    ADR-0075 section 3: "Save a recovery phrase". The vault of the old
    //    phrase is gone (above); from here the account is phrase-protected.
    if payload.phrase_saved {
        crate::account_notices::mark_phrase_saved(&mut tx, &account_id, now)
            .await
            .map_err(ApiError::Db)?;
    }

    // 6. The devices removed with the rotation.
    for device_id in &revoke_ids {
        crate::session::raise_floor(&mut *tx, &account_id, device_id, now, FLOOR_REASON_REMOVED)
            .await
            .map_err(ApiError::Db)?;
    }

    // 7. The attestation and the caller's pair, at the new generation. The
    //    kind is the reset's: what tells a rotation from a reset to
    //    discovery and to a peer is the proof beside it.
    let attestation =
        KeyResetAttestation::for_account(&account_id, key_generation, new_signature_pubkey, now)
            .sign(&state.attest_key.signing);
    let session =
        crate::session::issue_pair_at(&state, &account_id, key_generation, &caller.device_id)?;

    tx.commit().await.map_err(ApiError::Db)?;

    tracing::info!(
        key_generation,
        revoked = revoke_ids.len(),
        "account keys rotated",
    );
    Ok(Json(ResetFinishRes {
        key_reset_attest_b64: URL_SAFE_NO_PAD.encode(attestation),
        session,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;
    use lakko_transparency::SupersessionProof;

    const NOW_MS: i64 = 1_800_000_000_000;

    fn proof(old: &SigningKey, new: &SigningKey, issued: i64, expires: i64) -> Vec<u8> {
        SupersessionProof::new(
            old.verifying_key().to_bytes(),
            new.verifying_key().to_bytes(),
            issued,
            expires,
        )
        .sign(old)
        .to_vec()
    }

    #[test]
    fn a_proof_authorises_only_the_key_it_names_under_the_recorded_key() {
        let old = SigningKey::from_seed(&[1; 32]);
        let new = SigningKey::from_seed(&[2; 32]);
        let other = SigningKey::from_seed(&[3; 32]);
        let now_s = NOW_MS / 1000;
        let good = proof(&old, &new, now_s, now_s + 14 * 86_400);
        let recorded = old.verifying_key().to_bytes();
        let new_key = new.verifying_key().to_bytes();

        assert!(proof_authorises(&good, Some(&recorded), &new_key, NOW_MS));
        // No recorded key; another recorded key; another new key.
        assert!(!proof_authorises(&good, None, &new_key, NOW_MS));
        assert!(!proof_authorises(
            &good,
            Some(&other.verifying_key().to_bytes()),
            &new_key,
            NOW_MS
        ));
        assert!(!proof_authorises(
            &good,
            Some(&recorded),
            &other.verifying_key().to_bytes(),
            NOW_MS
        ));
        // A recorded key of the wrong length (a corrupt row) refuses.
        assert!(!proof_authorises(
            &good,
            Some(&recorded[..31]),
            &new_key,
            NOW_MS
        ));
        // Signed by someone who is not the recorded key but claims it.
        let forged = SupersessionProof::new(recorded, new_key, now_s, now_s + 60)
            .sign(&other)
            .to_vec();
        assert!(!proof_authorises(
            &forged,
            Some(&recorded),
            &new_key,
            NOW_MS
        ));
        // Expired, and a window longer than the proof format allows.
        let expired = proof(&old, &new, now_s - 100, now_s - 1);
        assert!(!proof_authorises(
            &expired,
            Some(&recorded),
            &new_key,
            NOW_MS
        ));
        let too_long = proof(&old, &new, now_s, now_s + 31 * 86_400);
        assert!(!proof_authorises(
            &too_long,
            Some(&recorded),
            &new_key,
            NOW_MS
        ));
        // A proof of a key over itself rotates nothing.
        let same = proof(&old, &old, now_s, now_s + 60);
        assert!(!proof_authorises(&same, Some(&recorded), &recorded, NOW_MS));
        // Garbage.
        assert!(!proof_authorises(
            &[0u8; 145],
            Some(&recorded),
            &new_key,
            NOW_MS
        ));
    }

    #[test]
    fn the_revoke_list_is_bounded_and_never_names_the_caller() {
        let caller = [0xC1u8; 32];
        let id = |b: u8| URL_SAFE_NO_PAD.encode([b; 32]);
        assert!(decode_revoke_ids(&[], &caller).unwrap().is_empty());
        assert_eq!(
            decode_revoke_ids(&[id(1), id(2)], &caller).unwrap(),
            vec![[1u8; 32], [2u8; 32]]
        );
        let eight: Vec<String> = (1..=8).map(id).collect();
        assert_eq!(decode_revoke_ids(&eight, &caller).unwrap().len(), 8);
        let nine: Vec<String> = (1..=9).map(id).collect();
        for bad in [
            nine,
            vec![id(0)],
            vec![id(0xC1)],
            vec!["!!".to_string()],
            vec![URL_SAFE_NO_PAD.encode([1u8; 31])],
        ] {
            assert!(
                matches!(decode_revoke_ids(&bad, &caller), Err(ApiError::BadInput(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_body_decoders_refuse_wrong_shapes() {
        assert!(decode_proof(&URL_SAFE_NO_PAD.encode([0u8; 145])).is_ok());
        for bad in [
            URL_SAFE_NO_PAD.encode([0u8; 144]),
            URL_SAFE_NO_PAD.encode([0u8; 146]),
            "!!".to_string(),
        ] {
            assert!(matches!(decode_proof(&bad), Err(ApiError::BadInput(_))));
        }
        let good = SigningKey::from_seed(&[5; 32]).verifying_key().to_bytes();
        assert_eq!(
            decode_new_pubkey(&URL_SAFE_NO_PAD.encode(good)).unwrap(),
            good
        );
        // The identity point: small order, refused.
        let mut weak = [0u8; 32];
        weak[0] = 1;
        assert!(decode_new_pubkey(&URL_SAFE_NO_PAD.encode(weak)).is_err());
        assert!(decode_new_pubkey("!!").is_err());
        assert!(decode_new_pubkey(&URL_SAFE_NO_PAD.encode([1u8; 31])).is_err());
    }
}
