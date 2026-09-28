// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Word vault — the server side of ADR-0049's three-word recovery.
//!
//! The client seals its recovery-phrase entropy under a key stretched from
//! three of its 24 words (`lakko_mnemonic::vault`); this module stores the
//! sealed bytes behind a guess counter and hands them back after a correct
//! guess. The server never sees the words, the key or the entropy.
//!
//! - **`POST /vault/enroll`** — bearer-auth. Stores `positions`, `salt`, the
//!   Argon2id cost, and `seal_with_key(vault_key, verifier || sealed_entropy)`.
//!   Replaces an existing vault (an availability-only residual, ADR-0049).
//! - **`GET /vault`** — bearer-auth. Whether this account has a vault, and
//!   its positions (for Settings and for the signup screen's highlight).
//! - **`POST /vault/recover/start`** — anonymous, per-address limited
//!   (ADR-0054 S4); names the account by login handle (ADR-0054 S5). Serves
//!   `positions`, `salt` and the cost. A handle no account holds, and an
//!   account without a vault, get pepper-derived decoys, stable per handle,
//!   so the response shape is identical.
//! - **`POST /vault/recover/finish`** — anonymous. Every guess is charged to
//!   a counter keyed on the login handle it names (ADR-0054 S5; default 5
//!   per 15 minutes, `429` `too-many-attempts`, unknown and known alike) and to the
//!   per-address bucket; then a constant-time compare of the guess against
//!   the stored verifier; a match returns the sealed entropy; a miss counts
//!   toward the 10-strike destruction of the row (answered with the same
//!   `401` as any miss).
//!
//! **Trust class — the stated invariant-#2 exception.** An operator who
//! ignores the counter and holds `LAKKO_VAULT_ENC_KEY` can search the 2^33
//! answers offline at one Argon2id evaluation each. ADR-0049 section 6
//! records this and proposes the invariant wording.

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_crypto::hash::{blake3_derive_key_from_material, blake3_keyed_xof};
use lakko_mnemonic::vault::{
    positions_are_valid, Positions, DEFAULT_PARAMS, SALT_LEN, VERIFIER_LEN,
};
use serde::{Deserialize, Serialize};

use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Length of the `account_id` (16-byte UUIDv7 raw form).
const ACCOUNT_ID_LEN: usize = 16;

/// Consecutive failed guesses after which the vault row is deleted.
const DESTRUCTION_CAP: u32 = 10;

/// Upper bound on the sealed-entropy envelope a client may enroll. The real
/// size is 12 (nonce) + 32 (entropy) + 16 (tag) = 60 bytes.
const MAX_SEALED_LEN: usize = 128;

/// Bounds on the Argon2id cost a client may enroll with, so a hostile
/// client cannot store a cost that no honest client could ever evaluate.
const MAX_M_KIB: u32 = 1 << 20;
const MAX_T: u32 = 16;
const MAX_P: u32 = 8;

/// The `/vault/recover/finish` row shape: `(body_enc, failed_attempts)`.
type FinishRow = (Vec<u8>, u8);

/// The `/vault/recover/start` row shape:
/// `(positions, salt, argon2_m_kib, argon2_t, argon2_p)`.
type StartRow = (Vec<u8>, Vec<u8>, u32, u32, u32);

// ---------------------------------------------------------------------------
// Env-loaded encryption key
// ---------------------------------------------------------------------------

/// Load the vault body-encryption key from `LAKKO_VAULT_ENC_KEY`.
///
/// `Ok(Some(_))` when the env var is present and decodes to exactly 32 bytes;
/// `Ok(None)` when it is unset (the `/vault/*` routes then return 503);
/// `Err` (loud-fail at startup) when it is present but malformed.
///
/// # Errors
///
/// Returns an error when the env var is set but is not valid base64 or does
/// not decode to exactly 32 bytes.
pub(crate) fn load_vault_enc_key() -> anyhow::Result<Option<[u8; 32]>> {
    let raw = match std::env::var("LAKKO_VAULT_ENC_KEY") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return Ok(None),
    };
    let bytes = STANDARD
        .decode(raw.trim())
        .map_err(|e| anyhow::anyhow!("LAKKO_VAULT_ENC_KEY is not valid base64: {e}"))?;
    let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "LAKKO_VAULT_ENC_KEY must decode to exactly 32 bytes (got {})",
            bytes.len()
        )
    })?;
    Ok(Some(key))
}

fn require_key(state: &AppState) -> Result<[u8; 32], ApiError> {
    state
        .vault_enc_key
        .as_ref()
        .map(|k| **k)
        .ok_or(ApiError::Disabled)
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// `POST /vault/enroll` request.
#[derive(Deserialize)]
pub(crate) struct EnrollReq {
    pub positions: Vec<u8>,
    pub salt_b64: String,
    pub argon2_m_kib: u32,
    pub argon2_t: u32,
    pub argon2_p: u32,
    pub verifier_b64: String,
    pub sealed_entropy_b64: String,
}

/// `GET /vault` response.
#[derive(Serialize)]
pub(crate) struct StatusRes {
    pub enrolled: bool,
    pub positions: Option<Vec<u8>>,
}

/// `POST /vault/recover/start` request.
#[derive(Deserialize)]
pub(crate) struct RecoverStartReq {
    /// The login handle of the account (ADR-0054 S5): 32 bytes, base64url
    /// without padding.
    pub handle: String,
}

/// `POST /vault/recover/start` response — what the client needs to stretch
/// its answer. Identical in shape for a real and an unknown account.
#[derive(Serialize)]
pub(crate) struct RecoverStartRes {
    pub positions: Vec<u8>,
    pub salt_b64: String,
    pub argon2_m_kib: u32,
    pub argon2_t: u32,
    pub argon2_p: u32,
}

/// `POST /vault/recover/finish` request.
#[derive(Deserialize)]
pub(crate) struct RecoverFinishReq {
    /// The login handle of the account (ADR-0054 S5).
    pub handle: String,
    pub verifier_b64: String,
}

/// `POST /vault/recover/finish` response on a match.
#[derive(Serialize)]
pub(crate) struct RecoverFinishRes {
    pub sealed_entropy_b64: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /vault/enroll` — store (or replace) this account's vault.
#[axum::debug_handler]
pub(crate) async fn enroll(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<EnrollReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let key = require_key(&state)?;
    let account_id = auth.0;

    let positions: Positions = req
        .positions
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::BadInput("positions must be three values"))?;
    if !positions_are_valid(&positions) {
        return Err(ApiError::BadInput(
            "positions must be three distinct ascending values in 1..=24",
        ));
    }
    let salt = decode_fixed::<SALT_LEN>(&req.salt_b64, "salt_b64 must decode to 16 bytes")?;
    let verifier =
        decode_fixed::<VERIFIER_LEN>(&req.verifier_b64, "verifier_b64 must decode to 32 bytes")?;
    let sealed = STANDARD
        .decode(&req.sealed_entropy_b64)
        .map_err(|_| ApiError::BadInput("sealed_entropy_b64 not base64"))?;
    if sealed.is_empty() || sealed.len() > MAX_SEALED_LEN {
        return Err(ApiError::BadInput(
            "sealed_entropy has an impossible length",
        ));
    }
    if !cost_in_bounds(req.argon2_m_kib, req.argon2_t, req.argon2_p) {
        return Err(ApiError::BadInput("argon2 cost out of bounds"));
    }

    let mut body = Vec::with_capacity(VERIFIER_LEN + sealed.len());
    body.extend_from_slice(&verifier);
    body.extend_from_slice(&sealed);
    let body_enc = lakko_crypto::aead::seal_with_key(&key, &body).map_err(|e| {
        tracing::error!(error = %e, "vault: body seal failed");
        ApiError::Internal
    })?;

    sqlx::query(
        "INSERT INTO word_vaults (account_id, positions, salt, argon2_m_kib, argon2_t, argon2_p, \
         body_enc, failed_attempts) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 0) \
         ON DUPLICATE KEY UPDATE positions = VALUES(positions), salt = VALUES(salt), \
         argon2_m_kib = VALUES(argon2_m_kib), argon2_t = VALUES(argon2_t), \
         argon2_p = VALUES(argon2_p), body_enc = VALUES(body_enc), \
         failed_attempts = 0",
    )
    .bind(account_id.as_slice())
    .bind(positions.as_slice())
    .bind(salt.as_slice())
    .bind(req.argon2_m_kib)
    .bind(req.argon2_t)
    .bind(req.argon2_p)
    .bind(body_enc.as_slice())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;

    tracing::info!(account_id = %URL_SAFE_NO_PAD.encode(account_id), "vault: enrolled");
    Ok(Json(serde_json::json!({ "enrolled": true })))
}

/// `GET /vault` — does this account have a vault, and at which positions.
#[axum::debug_handler]
pub(crate) async fn status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<StatusRes>, ApiError> {
    require_key(&state)?;
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT positions FROM word_vaults WHERE account_id = ?")
            .bind(auth.0.as_slice())
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::Db)?;
    Ok(Json(StatusRes {
        enrolled: row.is_some(),
        positions: row.map(|(p,)| p),
    }))
}

/// `POST /vault/recover/start` — the positions, salt and cost to answer with.
///
/// Anonymous, per-address limited. An unknown account gets decoys derived
/// from a pepper
/// over the presented id, so the response is indistinguishable from a real
/// one and stable across calls.
#[axum::debug_handler]
pub(crate) async fn recover_start(
    State(state): State<AppState>,
    Json(req): Json<RecoverStartReq>,
) -> Result<Json<RecoverStartRes>, ApiError> {
    let key = require_key(&state)?;
    let pepper = crate::login_handle::pepper(&state)?;
    let handle = crate::login_handle::decode_handle(&req.handle)?;
    // An unknown handle resolves to its stand-in id, which has no vault, so
    // it gets decoys stable per handle.
    let account_id = crate::login_handle::resolve(&state.db, &pepper, &handle)
        .await?
        .account_id;

    let row: Option<StartRow> = sqlx::query_as(
        "SELECT positions, salt, argon2_m_kib, argon2_t, argon2_p FROM word_vaults \
         WHERE account_id = ?",
    )
    .bind(account_id.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;

    let res = if let Some((positions, salt, m, t, p)) = row {
        RecoverStartRes {
            positions,
            salt_b64: STANDARD.encode(salt),
            argon2_m_kib: m,
            argon2_t: t,
            argon2_p: p,
        }
    } else {
        let (positions, salt) = decoy_for(&key, &account_id);
        RecoverStartRes {
            positions: positions.to_vec(),
            salt_b64: STANDARD.encode(salt),
            argon2_m_kib: DEFAULT_PARAMS.m_kib,
            argon2_t: DEFAULT_PARAMS.t,
            argon2_p: DEFAULT_PARAMS.p,
        }
    };
    Ok(Json(res))
}

/// `POST /vault/recover/finish` — compare the guess, release the sealed
/// entropy on a match.
///
/// Anonymous; names the account by login handle (ADR-0054 S5). The named
/// handle's attempt counter and the per-address bucket
/// → `429` `too-many-attempts` (charged before the lookup, so unknown and
/// known ids trip alike); a miss bumps the row's counter and deletes the row
/// on the 10th consecutive miss; an unknown account runs a dummy compare and
/// returns the same generic `401`.
#[axum::debug_handler]
pub(crate) async fn recover_finish(
    State(state): State<AppState>,
    Json(req): Json<RecoverFinishReq>,
) -> Result<Json<RecoverFinishRes>, ApiError> {
    let key = require_key(&state)?;
    let pepper = crate::login_handle::pepper(&state)?;
    let handle = crate::login_handle::decode_handle(&req.handle)?;
    let guess =
        decode_fixed::<VERIFIER_LEN>(&req.verifier_b64, "verifier_b64 must decode to 32 bytes")?;
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        crate::limits::Scope::VaultGuess,
        &handle,
    )
    .await?;
    let account_id = crate::login_handle::resolve(&state.db, &pepper, &handle)
        .await?
        .account_id;

    let row: Option<FinishRow> =
        sqlx::query_as("SELECT body_enc, failed_attempts FROM word_vaults WHERE account_id = ?")
            .bind(account_id.as_slice())
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::Db)?;

    // Unknown account (or a destroyed vault): a dummy compare so the shape
    // and timing match a miss, then the same generic 401. No DB write.
    let Some((body_enc, failed)) = row else {
        let _ = constant_time_eq(&guess, &[0u8; VERIFIER_LEN]);
        return Err(ApiError::InvalidCredential);
    };

    let failed = u32::from(failed);

    let body = lakko_crypto::aead::open_with_key(&key, &body_enc).map_err(|e| {
        tracing::error!(error = %e, "vault: body open failed");
        ApiError::Internal
    })?;
    if body.len() <= VERIFIER_LEN {
        tracing::error!("vault: stored body too short");
        return Err(ApiError::Internal);
    }
    let (stored_verifier, sealed_entropy) = body.split_at(VERIFIER_LEN);

    if !constant_time_eq(&guess, stored_verifier) {
        let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);
        if failed + 1 >= DESTRUCTION_CAP {
            sqlx::query("DELETE FROM word_vaults WHERE account_id = ?")
                .bind(account_id.as_slice())
                .execute(&state.db)
                .await
                .map_err(ApiError::Db)?;
            tracing::warn!(account_id = %account_id_b64, "vault: destroyed after repeated wrong guesses");
        } else {
            sqlx::query(
                "UPDATE word_vaults SET failed_attempts = failed_attempts + 1 WHERE account_id = ?",
            )
            .bind(account_id.as_slice())
            .execute(&state.db)
            .await
            .map_err(ApiError::Db)?;
        }
        return Err(ApiError::InvalidCredential);
    }

    sqlx::query("UPDATE word_vaults SET failed_attempts = 0 WHERE account_id = ?")
        .bind(account_id.as_slice())
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;
    sqlx::query("INSERT INTO vault_opens (account_id) VALUES (?)")
        .bind(account_id.as_slice())
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;

    tracing::info!(account_id = %URL_SAFE_NO_PAD.encode(account_id), "vault: opened");
    Ok(Json(RecoverFinishRes {
        sealed_entropy_b64: STANDARD.encode(sealed_entropy),
    }))
}

// ---------------------------------------------------------------------------
// Passkey vault (ADR-0049 section 3)
// ---------------------------------------------------------------------------

/// Upper bound on a WebAuthn credential id.
const MAX_CREDENTIAL_ID_LEN: usize = 1024;

/// `PUT /vault/passkey` request.
#[derive(Deserialize)]
pub(crate) struct PasskeyPutReq {
    pub credential_id_b64: String,
    pub sealed_entropy_b64: String,
}

/// `POST /vault/passkey/get` request.
#[derive(Deserialize)]
pub(crate) struct PasskeyGetReq {
    pub credential_id_b64: String,
}

/// `POST /vault/passkey/get` response.
#[derive(Serialize)]
pub(crate) struct PasskeyGetRes {
    pub sealed_entropy_b64: Option<String>,
}

/// `PUT /vault/passkey` — store (or replace) the entropy this account
/// sealed under one of its passkeys' PRF output. Bearer-auth; the blob is
/// opaque and only the credential's PRF output opens it.
#[axum::debug_handler]
pub(crate) async fn passkey_put(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<PasskeyPutReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let key = require_key(&state)?;
    let credential_id = URL_SAFE_NO_PAD
        .decode(&req.credential_id_b64)
        .map_err(|_| ApiError::BadInput("credential_id_b64 not URL-safe base64"))?;
    if credential_id.is_empty() || credential_id.len() > MAX_CREDENTIAL_ID_LEN {
        return Err(ApiError::BadInput("credential_id has an impossible length"));
    }
    let sealed = STANDARD
        .decode(&req.sealed_entropy_b64)
        .map_err(|_| ApiError::BadInput("sealed_entropy_b64 not base64"))?;
    if sealed.is_empty() || sealed.len() > MAX_SEALED_LEN {
        return Err(ApiError::BadInput(
            "sealed_entropy has an impossible length",
        ));
    }
    let body_enc = lakko_crypto::aead::seal_with_key(&key, &sealed).map_err(|e| {
        tracing::error!(error = %e, "passkey-vault: body seal failed");
        ApiError::Internal
    })?;
    sqlx::query(
        "INSERT INTO passkey_vaults (account_id, credential_id, body_enc) VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE body_enc = VALUES(body_enc)",
    )
    .bind(auth.0.as_slice())
    .bind(credential_id.as_slice())
    .bind(body_enc.as_slice())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;
    tracing::info!(account_id = %URL_SAFE_NO_PAD.encode(auth.0), "passkey-vault: stored");
    Ok(Json(serde_json::json!({ "stored": true })))
}

/// `POST /vault/passkey/get` — the blob for one of this account's
/// credentials, or `null`. Bearer-auth (the passkey login already issued the
/// token; only the account itself can read its own blobs).
#[axum::debug_handler]
pub(crate) async fn passkey_get(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<PasskeyGetReq>,
) -> Result<Json<PasskeyGetRes>, ApiError> {
    let key = require_key(&state)?;
    let credential_id = URL_SAFE_NO_PAD
        .decode(&req.credential_id_b64)
        .map_err(|_| ApiError::BadInput("credential_id_b64 not URL-safe base64"))?;
    let row: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT body_enc FROM passkey_vaults WHERE account_id = ? AND credential_id = ?",
    )
    .bind(auth.0.as_slice())
    .bind(credential_id.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    let Some((body_enc,)) = row else {
        return Ok(Json(PasskeyGetRes {
            sealed_entropy_b64: None,
        }));
    };
    let sealed = lakko_crypto::aead::open_with_key(&key, &body_enc).map_err(|e| {
        tracing::error!(error = %e, "passkey-vault: body open failed");
        ApiError::Internal
    })?;
    Ok(Json(PasskeyGetRes {
        sealed_entropy_b64: Some(STANDARD.encode(sealed)),
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const fn cost_in_bounds(m_kib: u32, t: u32, p: u32) -> bool {
    m_kib >= 8
        && m_kib <= MAX_M_KIB
        && t >= 1
        && t <= MAX_T
        && p >= 1
        && p <= MAX_P
        && m_kib >= 8 * p
}

/// Decoy positions + salt for an account that has no vault, derived from a
/// pepper over the presented id: stable across calls, unpredictable without
/// the server key, and indistinguishable in shape from a real row.
fn decoy_for(
    vault_key: &[u8; 32],
    account_id: &[u8; ACCOUNT_ID_LEN],
) -> (Positions, [u8; SALT_LEN]) {
    let pepper = blake3_derive_key_from_material("lakko/vault/decoy/v1", vault_key);
    let stream = blake3_keyed_xof(&pepper, account_id, 96);
    let mut positions: Positions = [0; 3];
    let mut filled = 0;
    for &b in &stream[..80] {
        if filled == 3 {
            break;
        }
        if b >= 240 {
            continue;
        }
        let candidate = b % 24 + 1;
        if positions[..filled].contains(&candidate) {
            continue;
        }
        positions[filled] = candidate;
        filled += 1;
    }
    if filled < 3 {
        // 80 bytes without three distinct accepted values has probability far
        // below 2^-100; keep the output well-formed regardless.
        positions = [1, 2, 3];
    }
    positions.sort_unstable();
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&stream[80..96]);
    (positions, salt)
}

fn decode_fixed<const N: usize>(b64: &str, msg: &'static str) -> Result<[u8; N], ApiError> {
    let bytes = STANDARD.decode(b64).map_err(|_| ApiError::BadInput(msg))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::BadInput(msg))
}

/// Constant-time byte equality; the length is folded in rather than
/// early-returned so timing does not branch on it.
fn constant_time_eq(candidate: &[u8; VERIFIER_LEN], stored: &[u8]) -> bool {
    if stored.len() != candidate.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in candidate.iter().zip(stored.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoys_are_stable_valid_and_account_specific() {
        let key = [0x42u8; 32];
        let a = [1u8; 16];
        let b = [2u8; 16];
        let (pa, sa) = decoy_for(&key, &a);
        let (pa2, sa2) = decoy_for(&key, &a);
        let (pb, sb) = decoy_for(&key, &b);
        assert_eq!((pa, sa), (pa2, sa2));
        assert!(positions_are_valid(&pa));
        assert!(positions_are_valid(&pb));
        assert!(sa != sb || pa != pb);
        let (pk, _) = decoy_for(&[0x43u8; 32], &a);
        // A different server key gives (almost surely) different decoys.
        assert!(pk != pa || decoy_for(&[0x43u8; 32], &a).1 != sa);
    }

    #[test]
    fn decoys_cover_every_position() {
        let key = [7u8; 32];
        let mut seen = [false; 25];
        for i in 0..400u32 {
            let mut id = [0u8; 16];
            id[..4].copy_from_slice(&i.to_be_bytes());
            let (p, _) = decoy_for(&key, &id);
            for x in p {
                seen[usize::from(x)] = true;
            }
        }
        assert!(seen[1..].iter().all(|&s| s));
    }

    #[test]
    fn cost_bounds() {
        assert!(cost_in_bounds(
            DEFAULT_PARAMS.m_kib,
            DEFAULT_PARAMS.t,
            DEFAULT_PARAMS.p
        ));
        assert!(cost_in_bounds(8, 1, 1));
        assert!(!cost_in_bounds(0, 1, 1));
        assert!(!cost_in_bounds(8, 0, 1));
        assert!(!cost_in_bounds(8, 1, 2)); // m must be >= 8 * p
        assert!(!cost_in_bounds(MAX_M_KIB + 1, 1, 1));
        assert!(!cost_in_bounds(1024, MAX_T + 1, 1));
        assert!(!cost_in_bounds(1024, 1, MAX_P + 1));
    }

    #[test]
    fn constant_time_eq_semantics() {
        let a = [9u8; 32];
        assert!(constant_time_eq(&a, &a));
        let mut b = a;
        b[31] ^= 1;
        assert!(!constant_time_eq(&a, &b));
        assert!(!constant_time_eq(&a, &a[..31]));
    }

    #[test]
    fn load_key_rejects_wrong_length() {
        let decoded = STANDARD.decode(STANDARD.encode([0u8; 31])).unwrap();
        let res: Result<[u8; 32], _> = decoded.as_slice().try_into();
        assert!(res.is_err());
    }
}

/// DB-backed state machine: enroll, status, start (real + decoy), a right
/// guess, wrong guesses through the lockout, and destruction on the 10th
/// consecutive miss. Runs only with `LAKKO_AUTH_TEST_DATABASE_URL` set.
#[cfg(test)]
mod vault_e2e_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, StatusCode};
    use http_body_util::BodyExt as _;
    use lakko_mnemonic::vault;
    use lakko_mnemonic::Mnemonic;
    use sqlx::mysql::MySqlPoolOptions;
    use sqlx::MySqlPool;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt as _;
    use uuid::Uuid;

    const VAULT_KEY: [u8; 32] = [0x6Bu8; 32];
    const FAST: lakko_crypto::kdf::Argon2Params = lakko_crypto::kdf::Argon2Params {
        m_kib: 8,
        t: 1,
        p: 1,
    };

    fn build_state(
        pool: MySqlPool,
        with_key: bool,
    ) -> (crate::AppState, lakko_auth_token::TokenSecret) {
        use opaque_ke::ServerSetup;
        use rand::rngs::OsRng;
        let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
        let opaque_setup = ServerSetup::<crate::opaque::LakkoCipherSuite>::new(&mut OsRng);
        let state = crate::AppState {
            db: pool,
            opaque_setup: Arc::new(opaque_setup),
            session_token_secret: Arc::new(token_secret.clone()),
            session_token_ttl_ms: 86_400_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
            turn: None,
            totp_enc_key: None,
            recovery_contact_pepper: None,
            vault_enc_key: with_key.then(|| Arc::new(VAULT_KEY)),
            otp_sender: Arc::new(crate::otp_sender::LogOnlyOtpSender),
            throttle: crate::throttle::ThrottleConfig::disabled(),
            backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
            anon_tokens: None,
            handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
            limits: Arc::new(crate::limits::Limits::defaults()),
        };
        (state, token_secret)
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

    fn bearer_for(token_secret: &lakko_auth_token::TokenSecret, account_id: &[u8; 16]) -> String {
        format!(
            "Bearer {}",
            token_secret
                .issue(account_id, now_ms() + 3_600_000)
                .expect("mint token")
        )
    }

    async fn json_req(
        state: &crate::AppState,
        method: Method,
        uri: &str,
        bearer: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, Bytes) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(b) = bearer {
            req = req.header(header::AUTHORIZATION, b);
        }
        let req = match body {
            Some(v) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&v).unwrap()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        send(state, req).await
    }

    fn picked(m: &Mnemonic, positions: &[u8]) -> Vec<String> {
        let words = m.words();
        positions
            .iter()
            .map(|&p| words[usize::from(p) - 1].clone())
            .collect()
    }

    async fn guess(
        state: &crate::AppState,
        handle_b64: &str,
        verifier: &[u8; 32],
    ) -> (StatusCode, Bytes) {
        json_req(
            state,
            Method::POST,
            "/vault/recover/finish",
            None,
            Some(serde_json::json!({
                "handle": handle_b64,
                "verifier_b64": STANDARD.encode(verifier),
            })),
        )
        .await
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn vault_state_machine() {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping vault_state_machine: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let _ = tracing_subscriber::fmt()
            .with_env_filter("lakko_svc_auth=debug")
            .with_test_writer()
            .try_init();
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect");
        crate::MIGRATIONS.run(&pool).await.expect("migrate");

        let account_id: [u8; 16] = *Uuid::now_v7().as_bytes();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .expect("insert account");
        // ADR-0054 S5: recovery names the account by its login handle.
        let (handle, handle_b64) = crate::login_handle::random_handle();
        crate::login_handle::bind_for_test(&pool, &account_id, &handle).await;

        // 503 when the key is not configured.
        let (state_off, secret) = build_state(pool.clone(), false);
        let bearer = bearer_for(&secret, &account_id);
        let (st, body) = json_req(&state_off, Method::GET, "/vault", Some(&bearer), None).await;
        assert_eq!(
            st,
            StatusCode::SERVICE_UNAVAILABLE,
            "body: {}",
            String::from_utf8_lossy(&body)
        );

        let (state, _) = build_state(pool.clone(), true);

        // Not enrolled yet.
        let (st, body) = json_req(&state, Method::GET, "/vault", Some(&bearer), None).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["enrolled"], false);

        // /recover/start for the not-yet-enrolled account answers with decoys,
        // stable across calls and shaped like a real row.
        let start = |state: crate::AppState, id: String| async move {
            let (st, body) = json_req(
                &state,
                Method::POST,
                "/vault/recover/start",
                None,
                Some(serde_json::json!({ "handle": id })),
            )
            .await;
            assert_eq!(
                st,
                StatusCode::OK,
                "body: {}",
                String::from_utf8_lossy(&body)
            );
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        };
        let d1 = start(state.clone(), handle_b64.clone()).await;
        let d2 = start(state.clone(), handle_b64.clone()).await;
        assert_eq!(d1, d2);
        assert_eq!(d1["positions"].as_array().unwrap().len(), 3);
        assert_eq!(d1["argon2_m_kib"], DEFAULT_PARAMS.m_kib);
        // A handle nobody holds gets decoys of the same shape, stable per
        // handle and different from another handle's.
        let (_, unknown_b64) = crate::login_handle::random_handle();
        let u1 = start(state.clone(), unknown_b64.clone()).await;
        assert_eq!(u1, start(state.clone(), unknown_b64.clone()).await);
        assert_ne!(u1["salt_b64"], d1["salt_b64"]);
        assert_eq!(
            u1.as_object().unwrap().keys().collect::<Vec<_>>(),
            d1.as_object().unwrap().keys().collect::<Vec<_>>()
        );

        // A guess against a decoy is a plain 401.
        let (st, body) = guess(&state, &handle_b64, &[0u8; 32]).await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "body: {}",
            String::from_utf8_lossy(&body)
        );

        // Enroll.
        let m = Mnemonic::generate();
        let e = vault::enroll(&m, [3, 11, 20], vault::random_salt(), FAST).unwrap();
        let enroll_body = serde_json::json!({
            "positions": e.positions.to_vec(),
            "salt_b64": STANDARD.encode(e.salt),
            "argon2_m_kib": e.params.m_kib,
            "argon2_t": e.params.t,
            "argon2_p": e.params.p,
            "verifier_b64": STANDARD.encode(e.verifier),
            "sealed_entropy_b64": STANDARD.encode(&e.sealed_entropy),
        });
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/vault/enroll",
            Some(&bearer),
            Some(enroll_body.clone()),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );

        // Bad positions are refused.
        let mut bad = enroll_body.clone();
        bad["positions"] = serde_json::json!([3, 3, 20]);
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/vault/enroll",
            Some(&bearer),
            Some(bad),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::BAD_REQUEST,
            "body: {}",
            String::from_utf8_lossy(&body)
        );

        // Status shows the positions.
        let (st, body) = json_req(&state, Method::GET, "/vault", Some(&bearer), None).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["enrolled"], true);
        assert_eq!(v["positions"], serde_json::json!([3, 11, 20]));

        // /recover/start now serves the real row.
        let s = start(state.clone(), handle_b64.clone()).await;
        assert_eq!(s["positions"], serde_json::json!([3, 11, 20]));
        assert_eq!(s["argon2_m_kib"], FAST.m_kib);
        let salt = STANDARD.decode(s["salt_b64"].as_str().unwrap()).unwrap();
        assert_eq!(salt, e.salt);

        // The right words open it.
        let words = picked(&m, &e.positions);
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        let keys = vault::answer(&refs, &salt, FAST).unwrap();
        let (st, body) = guess(&state, &handle_b64, keys.verifier()).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let sealed = STANDARD
            .decode(v["sealed_entropy_b64"].as_str().unwrap())
            .unwrap();
        let back = keys.open(&sealed).unwrap();
        assert_eq!(back.phrase(), m.phrase());
        let (opens,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM vault_opens WHERE account_id = ?")
                .bind(account_id.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(opens, 1);

        // Five wrong guesses: 401 x5, then 429 (the attempt limit), even for
        // the right one. The counter already holds this test's two earlier
        // guesses; start it from zero.
        let reset = || {
            crate::limits::reset_name(&pool, crate::limits::Scope::VaultGuess, handle.as_slice())
        };
        reset().await;
        let limit = state
            .limits
            .name_window(crate::limits::Scope::VaultGuess)
            .limit;
        let wrong = [0xEEu8; 32];
        for _ in 0..limit {
            let (st, body) = guess(&state, &handle_b64, &wrong).await;
            assert_eq!(
                st,
                StatusCode::UNAUTHORIZED,
                "body: {}",
                String::from_utf8_lossy(&body)
            );
        }
        let (st, body) = guess(&state, &handle_b64, keys.verifier()).await;
        assert_eq!(
            st,
            StatusCode::TOO_MANY_REQUESTS,
            "body: {}",
            String::from_utf8_lossy(&body)
        );

        // Let the window pass (the test cannot wait 15 minutes).
        reset().await;
        // Misses 6..=9 are 401s; the 10th destroys the row.
        for _ in limit..DESTRUCTION_CAP - 1 {
            let (st, body) = guess(&state, &handle_b64, &wrong).await;
            assert_eq!(
                st,
                StatusCode::UNAUTHORIZED,
                "body: {}",
                String::from_utf8_lossy(&body)
            );
            reset().await;
        }
        let (st, body) = guess(&state, &handle_b64, &wrong).await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let (st, body) = json_req(&state, Method::GET, "/vault", Some(&bearer), None).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["enrolled"], false,
            "the vault is destroyed after the 10th miss"
        );
        // The right words no longer open anything: same 401 as an unknown account.
        let (st, body) = guess(&state, &handle_b64, keys.verifier()).await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "body: {}",
            String::from_utf8_lossy(&body)
        );

        // Re-enrolling brings it back with a clean counter.
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/vault/enroll",
            Some(&bearer),
            Some(enroll_body),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let (st, body) = guess(&state, &handle_b64, keys.verifier()).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );

        // Passkey vault: absent, then stored, then served back verbatim.
        let cred = URL_SAFE_NO_PAD.encode([0xC1u8; 40]);
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/vault/passkey/get",
            Some(&bearer),
            Some(serde_json::json!({ "credential_id_b64": cred })),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["sealed_entropy_b64"].is_null());
        let blob = vault::seal_entropy_under_secret(&m, &[0x33u8; 32]).unwrap();
        let (st, body) = json_req(
            &state,
            Method::PUT,
            "/vault/passkey",
            Some(&bearer),
            Some(serde_json::json!({
                "credential_id_b64": cred,
                "sealed_entropy_b64": STANDARD.encode(&blob),
            })),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/vault/passkey/get",
            Some(&bearer),
            Some(serde_json::json!({ "credential_id_b64": cred })),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let served = STANDARD
            .decode(v["sealed_entropy_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(served, blob);
        assert_eq!(
            vault::open_entropy_under_secret(&[0x33u8; 32], &served)
                .unwrap()
                .phrase(),
            m.phrase()
        );

        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .unwrap();
    }
}
