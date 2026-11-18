// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! OPAQUE-PAKE (RFC draft `opaque`) auth flows for the auth service.
//!
//! The server never sees the plaintext password — client and server exchange
//! OPRF-blinded messages, and the server stores only an "envelope" from
//! which the password cannot be recovered without an MITM at registration
//! time (guaranteed by OPAQUE math).
//!
//! ## Two parallel registrations per account
//!
//! Phase 1.1d adds a second OPAQUE registration kind alongside the
//! password one:
//!
//!   * **`KIND_PASSWORD`** — derived from the user-typed password
//!     (Phase 1.1b/c).
//!   * **`KIND_RECOVERY`** — derived from a BIP39 12-word mnemonic seed
//!     (Phase 1.1d). Lets the user re-authenticate after losing every
//!     device on which the password is cached. The mnemonic itself is
//!     generated and held by the client; the server only ever sees the
//!     OPAQUE-blinded envelope, exactly as for the password.
//!
//! Both envelopes are stored in `opaque_registrations` keyed by
//! `(account_id, kind)`. To prevent cross-flow attacks (e.g. replaying a
//! password envelope at a recovery login challenge), the OPAQUE
//! `credential_id` is domain-separated as `account_id || kind_byte`.
//!
//! ## State machine
//!
//! Registration keeps one row between `/start` and `/finish`
//! (`signup_states`, keyed on `account_id`, TTL [`SIGNUP_STATE_TTL_SECS`]):
//! `/signup/start` is the one auth route that still demands a proof of work
//! (ADR-0054 S4), and the row is what binds `/signup/finish` to a start whose
//! proof was spent — finish deletes it in the account-creating transaction,
//! so one proof creates at most one account and a finish without a live start
//! is refused (`410`). The OPAQUE state itself is not stored: the math binds
//! each upload to its response.
//!
//! Login is **stateful**: `ServerLogin<CS>` is serialized and stored in
//! `opaque_login_states` keyed by `session_id` (UUIDv7), with TTL 2
//! minutes and a periodic pruner. The row also stores `kind`, and the
//! `/login/finish` (`KIND_PASSWORD`) and `/recovery/login/finish`
//! (`KIND_RECOVERY`) handlers refuse a state created for the wrong flow.
//!
//! The server-static state (OPRF + AKE keypairs) is generated once on
//! first start and persisted in `opaque_server_setup`.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use opaque_ke::key_exchange::tripledh::TripleDh;
use opaque_ke::{
    CipherSuite, CredentialFinalization, CredentialRequest, RegistrationRequest,
    RegistrationUpload, Ristretto255, ServerLogin, ServerLoginStartParameters, ServerRegistration,
    ServerSetup,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;
use uuid::Uuid;

use crate::webauthn::ApiError;

/// Error type of the OPAQUE handlers: the plain `(status, text)` shape they
/// always had, or a limiter refusal (`429` + body code + `Retry-After`, or
/// `503` when the counter store fails).
pub(crate) enum OpaqueError {
    /// A plain status + text body.
    Plain(StatusCode, String),
    /// A limiter refusal.
    Limit(crate::limits::LimitError),
    /// A shared-module refusal (ADR-0054 S5: login by name disabled, a
    /// malformed handle, a handle-lookup store error).
    Api(crate::webauthn::ApiError),
}

impl From<crate::webauthn::ApiError> for OpaqueError {
    fn from(e: crate::webauthn::ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<(StatusCode, String)> for OpaqueError {
    fn from((code, msg): (StatusCode, String)) -> Self {
        Self::Plain(code, msg)
    }
}

impl From<crate::limits::LimitError> for OpaqueError {
    fn from(e: crate::limits::LimitError) -> Self {
        Self::Limit(e)
    }
}

impl From<crate::pow::PowError> for OpaqueError {
    fn from(e: crate::pow::PowError) -> Self {
        let (code, msg): (StatusCode, String) = e.into();
        Self::Plain(code, msg)
    }
}

impl axum::response::IntoResponse for OpaqueError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::Plain(code, msg) => (code, msg).into_response(),
            Self::Limit(e) => e.into_response(),
            Self::Api(e) => e.into_response(),
        }
    }
}

/// Lakko OPAQUE-PAKE ciphersuite.
///
/// - **OPRF**: Ristretto255 (constant-time, software, no hardware deps)
/// - **KE group**: Ristretto255 (matches OPRF for TripleDH)
/// - **KE**: TripleDH (3-message authenticated key exchange)
/// - **KSF**: Argon2id (client-side key stretching; protects the password
///   against brute force even if the server-side state leaks)
pub(crate) struct LakkoCipherSuite;

impl CipherSuite for LakkoCipherSuite {
    type OprfCs = Ristretto255;
    type KeGroup = Ristretto255;
    type KeyExchange = TripleDh;
    type Ksf = argon2::Argon2<'static>;
}

/// `account_id` length in bytes (UUIDv7 raw form).
const ACCOUNT_ID_LEN: usize = 16;

/// Length of the domain-separated OPAQUE credential_id (account_id + kind).
const CREDENTIAL_ID_LEN: usize = ACCOUNT_ID_LEN + 1;

/// OPAQUE registration kind: derived from the user-typed password.
pub(crate) const KIND_PASSWORD: u8 = 1;

/// OPAQUE registration kind: derived from the BIP39 mnemonic seed.
pub(crate) const KIND_RECOVERY: u8 = 2;

/// Flow tag for `opaque_login_states` rows created during a password
/// reset. The OPAQUE math underneath uses `KIND_RECOVERY`'s
/// credential_id (the user is proving knowledge of the recovery
/// secret), but the row tag is distinct so neither `/login/finish`
/// nor `/recovery/login/finish` can consume a state created for a
/// reset (anti confused-deputy, mirrors the existing kind guard
/// between `/login/finish` and `/recovery/login/finish`).
pub(crate) const KIND_PASSWORD_RESET: u8 = 3;

/// Build the OPAQUE credential_id by appending the registration kind to
/// `account_id`. Domain-separates the password and recovery flows so an
/// envelope from one flow cannot answer a challenge in the other.
pub(crate) fn build_credential_id(
    account_id: &[u8; ACCOUNT_ID_LEN],
    kind: u8,
) -> [u8; CREDENTIAL_ID_LEN] {
    let mut out = [0u8; CREDENTIAL_ID_LEN];
    out[..ACCOUNT_ID_LEN].copy_from_slice(account_id);
    out[ACCOUNT_ID_LEN] = kind;
    out
}

/// Load the server-static OPAQUE setup from the DB; on first start,
/// generate a fresh one and persist it.
///
/// Singleton-row pattern — the `opaque_server_setup` table has a
/// `CHECK(id=1)` constraint, so it physically cannot hold more than one row.
///
/// # Errors
///
/// Any DB error or OPAQUE deserialization failure propagates via
/// `anyhow::Error`. This is a fail-fast critical path — the service must
/// not start with a broken setup.
pub(crate) async fn load_or_init_server_setup(
    pool: &MySqlPool,
) -> Result<ServerSetup<LakkoCipherSuite>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT setup_bytes FROM opaque_server_setup WHERE id = 1")
            .fetch_optional(pool)
            .await
            .context("query opaque_server_setup")?;

    if let Some((bytes,)) = row {
        let setup = ServerSetup::<LakkoCipherSuite>::deserialize(&bytes)
            .map_err(|e| anyhow!("deserialize ServerSetup from DB: {e:?}"))?;
        tracing::info!("loaded existing OPAQUE ServerSetup from opaque_server_setup");
        return Ok(setup);
    }

    let setup = ServerSetup::<LakkoCipherSuite>::new(&mut OsRng);
    let bytes = setup.serialize().to_vec();
    sqlx::query("INSERT INTO opaque_server_setup (id, setup_bytes) VALUES (1, ?)")
        .bind(&bytes)
        .execute(pool)
        .await
        .context("insert fresh ServerSetup")?;
    tracing::info!("generated and persisted fresh OPAQUE ServerSetup (first run)");
    Ok(setup)
}

/// A stored `opaque_registrations` row that does not deserialize: a `500`,
/// logged, never a panic.
fn corrupted_registration(e: opaque_ke::errors::ProtocolError) -> (StatusCode, String) {
    tracing::error!(error = ?e, "opaque_registrations row does not deserialize");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal db error".to_string(),
    )
}

/// Decode a base64 `account_id` and validate its length.
fn decode_account_id(b64: &str) -> Result<[u8; ACCOUNT_ID_LEN], (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid account_id base64: {e}"),
        )
    })?;
    if bytes.len() != ACCOUNT_ID_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("account_id must be {ACCOUNT_ID_LEN} bytes after base64 decode"),
        ));
    }
    let mut out = [0u8; ACCOUNT_ID_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Decode a base64-encoded OPAQUE `RegistrationRequest`.
pub(crate) fn decode_registration_request(
    b64: &str,
) -> Result<RegistrationRequest<LakkoCipherSuite>, (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid base64 in RegistrationRequest: {e}"),
        )
    })?;
    RegistrationRequest::<LakkoCipherSuite>::deserialize(&bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid RegistrationRequest: {e:?}"),
        )
    })
}

/// Decode a base64-encoded OPAQUE `RegistrationUpload`.
pub(crate) fn decode_registration_upload(
    b64: &str,
) -> Result<RegistrationUpload<LakkoCipherSuite>, (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid base64 in RegistrationUpload: {e}"),
        )
    })?;
    RegistrationUpload::<LakkoCipherSuite>::deserialize(&bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid RegistrationUpload: {e:?}"),
        )
    })
}

/// Run `ServerRegistration::start` for the given kind and return the
/// base64-encoded response. Pure function — no DB interaction.
pub(crate) fn opaque_registration_start(
    setup: &ServerSetup<LakkoCipherSuite>,
    account_id: &[u8; ACCOUNT_ID_LEN],
    kind: u8,
    request: RegistrationRequest<LakkoCipherSuite>,
) -> Result<String, (StatusCode, String)> {
    let credential_id = build_credential_id(account_id, kind);
    let server_start =
        ServerRegistration::<LakkoCipherSuite>::start(setup, request, &credential_id).map_err(
            |e| {
                tracing::error!(kind, error = ?e, "ServerRegistration::start failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal opaque error".to_string(),
                )
            },
        )?;
    Ok(B64.encode(server_start.message.serialize()))
}

// ============================================================================
// /signup/start
// ============================================================================

/// Registration-start request. The client has already generated an
/// `account_id` (typically UUIDv7) and a `RegistrationRequest` via
/// `ClientRegistration::start` for the password. If the user is also
/// enrolling a recovery mnemonic in the same signup flow, they include a
/// second `RegistrationRequest` derived from the BIP39 seed.
#[derive(Deserialize)]
pub(crate) struct SignupStartRequest {
    /// PoW proof obtained via `POST /pow/challenge` — the one proof of work
    /// left in the auth service (ADR-0054 S4), at the
    /// `LAKKO_SIGNUP_POW_EFFORT_BITS` effort. Verified and consumed before
    /// any other work.
    pub(crate) pow: crate::pow::PowProof,
    /// Base64-encoded raw bytes of `account_id` (16 bytes, UUIDv7).
    pub(crate) account_id: String,
    /// Base64-encoded `RegistrationRequest` from the password flow.
    pub(crate) password_request: String,
    /// Optional base64-encoded `RegistrationRequest` from the BIP39
    /// mnemonic seed flow. Recommended — clients SHOULD always enrol a
    /// recovery mnemonic at signup. Made optional so test / fixture
    /// callers can opt out.
    #[serde(default)]
    pub(crate) recovery_request: Option<String>,
}

/// Response to `/signup/start`. The client passes each response back into
/// the matching `ClientRegistration::finish` call.
#[derive(Serialize)]
pub(crate) struct SignupStartResponse {
    /// Base64-encoded `RegistrationResponse` for the password flow.
    pub(crate) password_response: String,
    /// Base64-encoded `RegistrationResponse` for the recovery flow, if
    /// the request included `recovery_request`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recovery_response: Option<String>,
}

/// `POST /signup/start` — first step of registration.
///
/// Runs `ServerRegistration::start` for the password flow and, if the
/// caller included `recovery_request`, also for the recovery flow. Both
/// run with the same server-static setup but distinct `credential_id`
/// values (`account_id || 0x01` vs `account_id || 0x02`).
///
/// Behind the per-address `Signup` bucket (a `429` `signup-rate-limited`).
/// After the proof verifies and both OPAQUE starts succeed, records a
/// `signup_states` row for `account_id` (replacing an earlier pending one) —
/// the only thing that lets `/signup/finish` create the account.
pub(crate) async fn signup_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<SignupStartRequest>,
) -> Result<Json<SignupStartResponse>, OpaqueError> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let account_id = decode_account_id(&payload.account_id)?;

    let password_request = decode_registration_request(&payload.password_request)?;
    let password_response = opaque_registration_start(
        &state.opaque_setup,
        &account_id,
        KIND_PASSWORD,
        password_request,
    )?;

    let recovery_response = match payload.recovery_request {
        Some(req_b64) => {
            let req = decode_registration_request(&req_b64)?;
            Some(opaque_registration_start(
                &state.opaque_setup,
                &account_id,
                KIND_RECOVERY,
                req,
            )?)
        },
        None => None,
    };

    sqlx::query(
        "INSERT INTO signup_states (account_id, with_recovery, created_at) \
         VALUES (?, ?, NOW(3)) \
         ON DUPLICATE KEY UPDATE with_recovery = VALUES(with_recovery), created_at = NOW(3)",
    )
    .bind(&account_id[..])
    .bind(recovery_response.is_some())
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert signup_states");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    Ok(Json(SignupStartResponse {
        password_response,
        recovery_response,
    }))
}

// ============================================================================
// /signup/finish
// ============================================================================

/// Take the live `signup_states` row for `account_id` inside the
/// account-creating transaction: `410` when there is none (no start with a
/// spent proof, expired, or already used), `400` when the recovery upload
/// does not match what the start carried.
async fn take_signup_state(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    account_id: &[u8; ACCOUNT_ID_LEN],
    with_recovery: bool,
) -> Result<(), OpaqueError> {
    let db_err = |what: &'static str| {
        move |e: sqlx::Error| {
            tracing::error!(error = %e, what, "signup_states");
            OpaqueError::Plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        }
    };
    let pending: Option<(bool,)> = sqlx::query_as(
        "SELECT with_recovery FROM signup_states \
         WHERE account_id = ? AND created_at >= NOW(3) - INTERVAL ? SECOND \
         FOR UPDATE",
    )
    .bind(&account_id[..])
    .bind(SIGNUP_STATE_TTL_SECS)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err("select"))?;
    let Some((started_with_recovery,)) = pending else {
        return Err(OpaqueError::Plain(
            StatusCode::GONE,
            "signup not started or expired".to_string(),
        ));
    };
    if started_with_recovery != with_recovery {
        return Err(OpaqueError::Plain(
            StatusCode::BAD_REQUEST,
            "recovery_upload must be present iff signup/start carried recovery_request".to_string(),
        ));
    }
    sqlx::query("DELETE FROM signup_states WHERE account_id = ?")
        .bind(&account_id[..])
        .execute(&mut **tx)
        .await
        .map_err(db_err("delete"))?;
    Ok(())
}

/// Registration-finish request. Includes both uploads if the caller
/// started both flows in `/signup/start`.
#[derive(Deserialize)]
pub(crate) struct SignupFinishRequest {
    /// Must name an `account_id` whose `/signup/start` (and its proof of
    /// work) completed within [`SIGNUP_STATE_TTL_SECS`]. No proof here.
    pub(crate) account_id: String,
    /// Base64-encoded `RegistrationUpload` for the password flow.
    pub(crate) password_upload: String,
    /// Base64-encoded `RegistrationUpload` for the recovery flow. Must
    /// be present iff `signup_start` was called with `recovery_request`.
    #[serde(default)]
    pub(crate) recovery_upload: Option<String>,
    /// URL-safe base64 (no padding) of the 32-byte id of the device this
    /// session lands on: its conversation leaf signature key (ADR-0074
    /// section 0.4). Absent when the device has no keys yet; the session then
    /// names no device and the client calls `POST /auth/session/bind-device`
    /// once it has them.
    #[serde(default)]
    pub(crate) device_id_b64: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct SignupFinishResponse {
    pub(crate) ok: bool,
    /// The session pair (`session_token`, `session_expires_at_ms`,
    /// `refresh_token`, `refresh_expires_at_ms`, `key_generation`), the
    /// same shape `/login/finish` returns (ADR-0073): the client treats
    /// signup + auto-login as one ceremony.
    #[serde(flatten)]
    pub(crate) session: crate::session::SessionPair,
    /// `fresh_grant` + `fresh_grant_expires_at_ms` (ADR-0073, amended
    /// 2026-10-06): the person typed the new password seconds ago, in this
    /// very exchange, and onboarding offers "add a passkey" right after
    /// signup, which requires a grant. A separate field, never part of the
    /// shared [`crate::session::SessionPair`].
    #[serde(flatten)]
    pub(crate) grant: crate::session::FreshGrant,
}

/// `POST /signup/finish` — finalize registration.
///
/// In a single DB transaction:
///
///   0. DELETE the live `signup_states` row for `account_id` — none means
///      no start with a spent proof (or it expired, or it was already
///      used): `410 Gone`. The recovery upload must match what the start
///      carried, else `400`.
///   1. INSERT `accounts(id = account_id)` — uniqueness here is the
///      anti-replay check.
///   2. INSERT the password envelope into `opaque_registrations` with
///      `kind = KIND_PASSWORD`.
///   3. If `recovery_upload` is present, INSERT the recovery envelope
///      with `kind = KIND_RECOVERY`.
///
/// All-or-nothing: if step 3 fails, step 1 and 2 are rolled back so the
/// account is not half-registered.
///
/// If `account_id` is already registered, returns `409 Conflict`.
pub(crate) async fn signup_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<SignupFinishRequest>,
) -> Result<Json<SignupFinishResponse>, OpaqueError> {
    let account_id = decode_account_id(&payload.account_id)?;
    let device_id = crate::session::decode_device_id(payload.device_id_b64.as_deref())?;

    let password_upload = decode_registration_upload(&payload.password_upload)?;
    let password_registration = ServerRegistration::<LakkoCipherSuite>::finish(password_upload);
    let password_bytes = password_registration.serialize().to_vec();

    let recovery_bytes = match payload.recovery_upload {
        Some(b64) => {
            let upload = decode_registration_upload(&b64)?;
            let registration = ServerRegistration::<LakkoCipherSuite>::finish(upload);
            Some(registration.serialize().to_vec())
        },
        None => None,
    };

    let mut tx = state.db.begin().await.map_err(|e| {
        tracing::error!(error = %e, "begin tx");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    // Bind to the start: take the live pending row (one shot).
    take_signup_state(&mut tx, &account_id, recovery_bytes.is_some()).await?;

    // Insert account row. PK collision means account_id is already taken.
    let account_insert = sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&account_id[..])
        .execute(&mut *tx)
        .await;

    match account_insert {
        Ok(_) => {},
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
            return Err(OpaqueError::Plain(
                StatusCode::CONFLICT,
                "account_id already registered".to_string(),
            ));
        },
        Err(e) => {
            tracing::error!(error = %e, "insert account");
            return Err(OpaqueError::Plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            ));
        },
    }

    sqlx::query(
        "INSERT INTO opaque_registrations (account_id, kind, registration_bytes) \
         VALUES (?, ?, ?)",
    )
    .bind(&account_id[..])
    .bind(KIND_PASSWORD)
    .bind(&password_bytes)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert opaque_registrations (password)");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    let with_recovery = recovery_bytes.is_some();
    if let Some(bytes) = recovery_bytes {
        sqlx::query(
            "INSERT INTO opaque_registrations (account_id, kind, registration_bytes) \
             VALUES (?, ?, ?)",
        )
        .bind(&account_id[..])
        .bind(KIND_RECOVERY)
        .bind(&bytes)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "insert opaque_registrations (recovery)");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;
    }

    tx.commit().await.map_err(|e| {
        tracing::error!(error = %e, "commit tx");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    tracing::info!(with_recovery, "signup completed",);
    // A fresh account is at the genesis generation; the read is one more
    // round trip but keeps a single issuance path.
    let session = crate::session::issue_pair(&state, &account_id, &device_id).await?;
    // The password was set in this exchange: mint the fresh-auth grant at
    // the generation the pair was issued under (a guest signup is this same
    // route, so it gets one too).
    let grant = crate::session::issue_fresh_grant(&state, &account_id, session.key_generation)?;
    Ok(Json(SignupFinishResponse {
        ok: true,
        session,
        grant,
    }))
}

// ============================================================================
// Login state — DB-backed
// ============================================================================

/// TTL for in-flight login state. The client must complete `/login/finish`
/// within this window. Mitigates memory growth and replay attacks.
/// Typed as `i64` so it can be passed directly to SQL `INTERVAL ? SECOND`
/// without a cast.
const LOGIN_STATE_TTL_SECS: i64 = 120;

/// TTL of a pending signup (`signup_states`): how long after a
/// `/signup/start` its `/signup/finish` is accepted. Covers the client-side
/// Argon2id stretch between the two on a slow phone.
pub(crate) const SIGNUP_STATE_TTL_SECS: i64 = 600;

/// Period between pruner runs that delete expired rows.
const LOGIN_STATE_PRUNE_INTERVAL: Duration = Duration::from_secs(30);

/// Insert a `ServerLogin` state into `opaque_login_states`, returning a
/// fresh session_id (UUIDv7).
async fn insert_login_state(
    pool: &MySqlPool,
    state: &ServerLogin<LakkoCipherSuite>,
    account_id: &[u8; ACCOUNT_ID_LEN],
    kind: u8,
) -> Result<Uuid, sqlx::Error> {
    let session_id = Uuid::now_v7();
    let state_bytes = state.serialize().to_vec();
    sqlx::query(
        "INSERT INTO opaque_login_states (session_id, account_id, kind, state_bytes) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(session_id.as_bytes().as_slice())
    .bind(&account_id[..])
    .bind(kind)
    .bind(&state_bytes)
    .execute(pool)
    .await?;
    Ok(session_id)
}

/// A login state taken out of `opaque_login_states`.
struct TakenLoginState {
    /// The OPAQUE server state `/…/start` stored.
    server_login: ServerLogin<LakkoCipherSuite>,
    /// The account the start resolved (a stand-in id for an unknown handle).
    account_id: [u8; ACCOUNT_ID_LEN],
    /// The flow the state was created for.
    kind: u8,
    /// The account's key generation, read in the SAME transaction that
    /// removed the state row; `None` when the account does not exist. The
    /// caller issues its session pair at this generation, never at one read
    /// later: see [`take_login_state`].
    key_generation: Option<u64>,
}

/// Fetch a `ServerLogin` state by session_id and **delete the row in the
/// same transaction** (one-shot — replaying the same session_id on a second
/// `/login/finish` returns 401).
///
/// The transaction also reads the account's key generation, while it holds
/// the state row's lock. A key reset and a recall delete an account's
/// in-flight login states inside the transaction that bumps the generation
/// (ADR-0073 section 2, ADR-0074 section 1.2), so one of two things is true
/// for a login that straddles one: the state row was already gone (`None`
/// here), or the generation read here is the one from BEFORE the change, and
/// the pair the caller issues at it is refused on first use. Reading the
/// generation after the state was consumed, as the handlers did until sprint
/// T3, let a login in flight at the moment of a recall come out with a
/// session of the new generation.
///
/// Returns `None` if session_id is unknown or expired (TTL elapsed).
async fn take_login_state(
    pool: &MySqlPool,
    session_id: Uuid,
) -> Result<Option<TakenLoginState>, sqlx::Error> {
    // Atomically: fetch the row (with TTL check) and delete it.
    // MySQL/MariaDB has no `DELETE ... RETURNING`, so we use a transaction:
    //   1. SELECT ... FOR UPDATE
    //   2. DELETE
    let mut tx = pool.begin().await?;

    let row: Option<(Vec<u8>, Vec<u8>, u8)> = sqlx::query_as(
        "SELECT state_bytes, account_id, kind FROM opaque_login_states \
         WHERE session_id = ? \
           AND created_at >= NOW(3) - INTERVAL ? SECOND \
         FOR UPDATE",
    )
    .bind(session_id.as_bytes().as_slice())
    .bind(LOGIN_STATE_TTL_SECS)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((state_bytes, account_id_vec, kind)) = row else {
        // No row, or expired. DELETE anyway in case the row exists and is
        // simply past its TTL (the pruner will eventually clean it up too,
        // but doing it here keeps the table tight).
        sqlx::query("DELETE FROM opaque_login_states WHERE session_id = ?")
            .bind(session_id.as_bytes().as_slice())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    };

    // Still holding the state row's lock: the generation as committed now.
    let key_generation: Option<u64> =
        sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ?")
            .bind(&account_id_vec)
            .fetch_optional(&mut *tx)
            .await?;

    sqlx::query("DELETE FROM opaque_login_states WHERE session_id = ?")
        .bind(session_id.as_bytes().as_slice())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let server_login = ServerLogin::<LakkoCipherSuite>::deserialize(&state_bytes).map_err(|e| {
        tracing::error!(error = ?e, "opaque_login_states row does not deserialize");
        sqlx::Error::Protocol("corrupted opaque_login_states row".to_string())
    })?;
    let Ok(account_id) = <[u8; ACCOUNT_ID_LEN]>::try_from(account_id_vec.as_slice()) else {
        return Ok(None);
    };
    Ok(Some(TakenLoginState {
        server_login,
        account_id,
        kind,
        key_generation,
    }))
}

/// Spawn a background task that runs every `LOGIN_STATE_PRUNE_INTERVAL` and
/// deletes expired login states and pending signups from the DB.
pub(crate) fn spawn_login_state_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(LOGIN_STATE_PRUNE_INTERVAL);
        // The first tick fires immediately — skip it (the table is usually
        // empty or near-empty on startup).
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = sqlx::query(
                "DELETE FROM opaque_login_states \
                 WHERE created_at < NOW(3) - INTERVAL ? SECOND",
            )
            .bind(LOGIN_STATE_TTL_SECS)
            .execute(&pool)
            .await;
            match result {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(
                        rows = r.rows_affected(),
                        "pruned expired opaque_login_states"
                    );
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "opaque_login_states prune failed"),
            }
            if let Err(e) = sqlx::query(
                "DELETE FROM signup_states WHERE created_at < NOW(3) - INTERVAL ? SECOND",
            )
            .bind(SIGNUP_STATE_TTL_SECS)
            .execute(&pool)
            .await
            {
                tracing::warn!(error = %e, "signup_states prune failed");
            }
        }
    })
}

// ============================================================================
// Login: shared inner flow
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct LoginStartRequest {
    /// The login handle of the account to log into (ADR-0054 S5): 32 bytes,
    /// base64url without padding. No proof of work (ADR-0054 S4): the start
    /// is limited per named handle and per source address instead.
    pub(crate) handle: String,
    /// Base64-encoded `CredentialRequest` from `ClientLogin::start`.
    pub(crate) request: String,
}

#[derive(Serialize)]
pub(crate) struct LoginStartResponse {
    /// Session ID used in `/login/finish` (UUIDv7 — embeds timestamp + random).
    pub(crate) session_id: String,
    /// Base64-encoded `CredentialResponse` from `ServerLogin::start`.
    pub(crate) response: String,
}

#[derive(Deserialize)]
pub(crate) struct LoginFinishRequest {
    pub(crate) session_id: String,
    /// Base64-encoded `CredentialFinalization` from `ClientLogin::finish`.
    pub(crate) finalize: String,
    /// URL-safe base64 (no padding) of the 32-byte id of the device this
    /// session lands on: its conversation leaf signature key (ADR-0074
    /// section 0.4). Absent when the device has no keys yet; the session then
    /// names no device and the client calls `POST /auth/session/bind-device`
    /// once it has them.
    #[serde(default)]
    pub(crate) device_id_b64: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct LoginFinishResponse {
    pub(crate) ok: bool,
    /// Base64-encoded account_id (16 raw UUIDv7 bytes) — the client pins
    /// this for follow-up requests.
    pub(crate) account_id: String,
    /// The session pair (`session_token`, `session_expires_at_ms`,
    /// `refresh_token`, `refresh_expires_at_ms`, `key_generation`),
    /// ADR-0073: the access token is the bearer on every service, the
    /// refresh credential renews it at `POST /auth/session/refresh`.
    #[serde(flatten)]
    pub(crate) session: crate::session::SessionPair,
}

/// Answer of `POST /login/finish` (the PASSWORD flow) alone: the common
/// login answer plus a fresh-auth grant (`fresh_grant`,
/// `fresh_grant_expires_at_ms`), the proof that the password was proven just
/// now which reset account keys, delete account and add a device require
/// (ADR-0073, amended 2026-10-04 and 2026-10-06; the passkey login's answer
/// carries one too). `/recovery/login/finish` answers the plain
/// [`LoginFinishResponse`].
#[derive(Serialize)]
pub(crate) struct PasswordLoginFinishResponse {
    #[serde(flatten)]
    pub(crate) login: LoginFinishResponse,
    #[serde(flatten)]
    pub(crate) grant: crate::session::FreshGrant,
}

/// Shared `/login/start` handler parameterised by `kind`.
///
/// Resolves the login handle to its account (ADR-0054 S5); a handle nobody
/// holds resolves to a stand-in account id derived from it, stable per
/// handle. Then looks up `opaque_registrations(account_id, kind)`. If no row
/// is found (unknown handle, or no recovery enrolled), passes `None` to
/// `ServerLogin::start` — opaque-ke 3.0 takes the fake-credentials path and
/// produces a plausible response that cannot be distinguished from a real
/// login with the wrong secret. The credential identifier on that path is
/// `stand_in || kind`, so the same unknown name meets the same OPRF key on
/// every attempt, as a real account's name does. This blocks both account
/// enumeration AND recovery-enrolment enumeration.
///
/// Every start is charged to the named handle's counter (per kind) before
/// anything is looked up, so the limit trips for an unknown name exactly as
/// for a known one.
async fn login_start_inner(
    state: &crate::AppState,
    payload: LoginStartRequest,
    kind: u8,
) -> Result<Json<LoginStartResponse>, OpaqueError> {
    let pepper = crate::login_handle::pepper(state)?;
    let handle = crate::login_handle::decode_handle(&payload.handle)?;
    // Decode the whole body before charging the counter, so a malformed
    // request (a 400 that does no work) does not spend the named handle's
    // budget. Still before any lookup: a 400 says nothing about the handle.
    let request_bytes = B64.decode(&payload.request).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid request base64: {e}"),
        )
    })?;
    let request_message = CredentialRequest::<LakkoCipherSuite>::deserialize(&request_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid CredentialRequest: {e:?}"),
            )
        })?;
    let scope = if kind == KIND_RECOVERY {
        crate::limits::Scope::LoginRecovery
    } else {
        crate::limits::Scope::LoginPassword
    };
    crate::limits::charge_name(&state.db, &state.limits, scope, &handle).await?;
    let account_id = crate::login_handle::resolve(&state.db, &pepper, &handle)
        .await?
        .account_id;
    let credential_id = build_credential_id(&account_id, kind);

    let stored: Option<ServerRegistration<LakkoCipherSuite>> = sqlx::query_as::<_, (Vec<u8>,)>(
        "SELECT registration_bytes FROM opaque_registrations \
         WHERE account_id = ? AND kind = ?",
    )
    .bind(&account_id[..])
    .bind(kind)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "query opaque_registrations");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?
    .map(|(bytes,)| ServerRegistration::<LakkoCipherSuite>::deserialize(&bytes))
    .transpose()
    .map_err(corrupted_registration)?;

    let mut rng = OsRng;
    let server_start = ServerLogin::<LakkoCipherSuite>::start(
        &mut rng,
        &state.opaque_setup,
        stored,
        request_message,
        &credential_id,
        ServerLoginStartParameters::default(),
    )
    .map_err(|e| {
        tracing::error!(kind, error = ?e, "ServerLogin::start failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal opaque error".to_string(),
        )
    })?;

    let session_id = insert_login_state(&state.db, &server_start.state, &account_id, kind)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "insert opaque_login_states");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    Ok(Json(LoginStartResponse {
        session_id: session_id.to_string(),
        response: B64.encode(server_start.message.serialize()),
    }))
}

/// Shared `/login/finish` handler parameterised by `expected_kind`.
///
/// The login state row carries the kind it was created for; we refuse to
/// finalize if it doesn't match the endpoint that's serving the request
/// (anti confused-deputy: a session started at `/login/start` cannot be
/// finalized at `/recovery/login/finish`, and vice versa).
///
/// Returns the verified account id beside the answer so the password
/// handler can mint its fresh-auth grant; this function mints none, so the
/// recovery flow that shares it cannot return one.
async fn login_finish_inner(
    state: &crate::AppState,
    payload: LoginFinishRequest,
    expected_kind: u8,
) -> Result<([u8; ACCOUNT_ID_LEN], LoginFinishResponse), OpaqueError> {
    let session_id = Uuid::parse_str(&payload.session_id).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid session_id (not a UUID): {e}"),
        )
    })?;
    // Decoded before the single-use login state is taken, so a malformed id
    // costs the caller nothing.
    let device_id = crate::session::decode_device_id(payload.device_id_b64.as_deref())?;

    let cached = take_login_state(&state.db, session_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "take opaque_login_states");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "session_id unknown or expired".to_string(),
        ))?;

    let TakenLoginState {
        server_login,
        account_id,
        kind,
        key_generation,
    } = cached;

    if kind != expected_kind {
        // The session was started for a different flow. Treat as 401 —
        // we already deleted the row in `take_login_state`, so an attacker
        // gets one shot at most.
        tracing::warn!(
            session_kind = kind,
            expected_kind,
            "login_finish: session kind mismatch (cross-flow attempt)",
        );
        return Err((
            StatusCode::UNAUTHORIZED,
            "session_id was started for a different flow".to_string(),
        )
            .into());
    }

    let finalize_bytes = B64.decode(&payload.finalize).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid finalize base64: {e}"),
        )
    })?;
    let finalize_message = CredentialFinalization::<LakkoCipherSuite>::deserialize(&finalize_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid CredentialFinalization: {e:?}"),
            )
        })?;

    let _result = server_login.finish(finalize_message).map_err(|e| {
        // Internal-only log — we never leak "user exists but wrong secret"
        // vs "user doesn't exist" to the client.
        tracing::debug!(
            kind,
            error = ?e,
            account_id = %B64.encode(account_id),
            "login finalization failed",
        );
        (StatusCode::UNAUTHORIZED, "invalid credentials".to_string())
    })?;

    tracing::info!(kind, "login completed");

    // At the generation read with the state row (see `take_login_state`),
    // never at one read now. No row: the account was deleted meanwhile.
    let key_generation = key_generation.ok_or(crate::webauthn::ApiError::InvalidCredential)?;
    let session = crate::session::issue_pair_at(state, &account_id, key_generation, &device_id)?;
    Ok((
        account_id,
        LoginFinishResponse {
            ok: true,
            account_id: B64.encode(account_id),
            session,
        },
    ))
}

// ============================================================================
// /login/{start,finish} — password flow
// ============================================================================

/// `POST /login/start` — first login step (password flow).
pub(crate) async fn login_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginStartRequest>,
) -> Result<Json<LoginStartResponse>, OpaqueError> {
    login_start_inner(&state, payload, KIND_PASSWORD).await
}

/// `POST /login/finish` — finalize login (password flow).
///
/// One of the two routes whose answer carries a fresh-auth grant (the other
/// is the passkey login, `POST /webauthn/auth/finish`): the password was
/// proven in this very exchange, and the grant is minted at the generation
/// the session pair was issued under. A handle nobody holds never gets here
/// with a grant: its login runs OPAQUE's fake-credentials path, which no
/// client can finish, and its stand-in account id has no row to issue at.
pub(crate) async fn login_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginFinishRequest>,
) -> Result<Json<PasswordLoginFinishResponse>, OpaqueError> {
    let (account_id, login) = login_finish_inner(&state, payload, KIND_PASSWORD).await?;
    let grant =
        crate::session::issue_fresh_grant(&state, &account_id, login.session.key_generation)?;
    Ok(Json(PasswordLoginFinishResponse { login, grant }))
}

// ============================================================================
// /recovery/login/{start,finish} — BIP39 mnemonic flow
// ============================================================================

/// `POST /recovery/login/start` — first step of recovery login.
///
/// The "password" input on the client side is a 64-byte seed derived
/// from a 12-word BIP39 mnemonic via PBKDF2-HMAC-SHA512 per the BIP39
/// spec. The server treats the seed as opaque bytes and runs the same
/// OPAQUE math as the password flow, only with a different stored
/// envelope (kind = `KIND_RECOVERY`).
pub(crate) async fn recovery_login_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginStartRequest>,
) -> Result<Json<LoginStartResponse>, OpaqueError> {
    login_start_inner(&state, payload, KIND_RECOVERY).await
}

/// `POST /recovery/login/finish` — finalize recovery login.
pub(crate) async fn recovery_login_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginFinishRequest>,
) -> Result<Json<LoginFinishResponse>, OpaqueError> {
    login_finish_inner(&state, payload, KIND_RECOVERY)
        .await
        .map(|(_, answer)| Json(answer))
}

// ============================================================================
// /password/reset/{start,finish} — replace the password envelope after the
// user proves ownership via the BIP39 recovery mnemonic.
// ============================================================================
//
// Flow (one user-facing operation, two HTTP round-trips):
//
//   1. Client computes:
//        - recovery_request   = ClientLogin::start(seed)            (KIND_RECOVERY)
//        - new_password_request = ClientRegistration::start(newpw)  (KIND_PASSWORD)
//      Sends both to /password/reset/start.
//
//   2. Server:
//        - charges the named handle's reset counter (ADR-0054 S4) and
//          resolves the handle to its account (ADR-0054 S5),
//        - looks up the stored KIND_RECOVERY envelope (or None),
//        - runs ServerLogin::start with KIND_RECOVERY credential_id (proves the
//          user knows the recovery secret, no real password yet),
//        - runs ServerRegistration::start with KIND_PASSWORD credential_id
//          (begins enrolling the new password envelope),
//        - persists the ServerLogin state in `opaque_login_states` with
//          kind = KIND_PASSWORD_RESET (3) so the existing /login/finish and
//          /recovery/login/finish handlers refuse to consume it.
//
//   3. Client computes:
//        - recovery_finalize = ClientLogin::finish(...)
//        - new_password_upload = ClientRegistration::finish(...)
//      Sends both with the same session_id to /password/reset/finish.
//
//   4. Server:
//        - takes the KIND_PASSWORD_RESET row (one-shot, deletes it),
//        - verifies the recovery finalization (this is the moment the user
//          actually proves ownership of the mnemonic),
//        - builds the new ServerRegistration from the upload,
//        - REPLACES `opaque_registrations(account_id, kind=KIND_PASSWORD)` —
//          the existing recovery envelope stays untouched, so the same
//          mnemonic continues to work after this operation.
//
// The limit lives only on /password/reset/start (finish runs against an
// already-issued, one-shot session_id).

#[derive(Deserialize)]
pub(crate) struct PasswordResetStartRequest {
    /// The login handle of the account (ADR-0054 S5): 32 bytes, base64url
    /// without padding.
    pub(crate) handle: String,
    /// Base64-encoded `CredentialRequest` from
    /// `ClientLogin::start(recovery_seed)`. The server feeds this to
    /// `ServerLogin::start` against the KIND_RECOVERY envelope.
    pub(crate) recovery_request: String,
    /// Base64-encoded `RegistrationRequest` from
    /// `ClientRegistration::start(new_password)`. The server begins a
    /// fresh password registration with this.
    pub(crate) new_password_request: String,
}

#[derive(Serialize)]
pub(crate) struct PasswordResetStartResponse {
    /// UUIDv7 the server assigned. Echoed back in `/password/reset/finish`.
    pub(crate) session_id: String,
    /// Base64-encoded `CredentialResponse` from `ServerLogin::start`.
    pub(crate) recovery_response: String,
    /// Base64-encoded `RegistrationResponse` from `ServerRegistration::start`.
    pub(crate) new_password_response: String,
}

#[derive(Deserialize)]
pub(crate) struct PasswordResetFinishRequest {
    pub(crate) session_id: String,
    /// Base64-encoded `CredentialFinalization` proving recovery ownership.
    pub(crate) recovery_finalize: String,
    /// Base64-encoded `RegistrationUpload` for the new password envelope.
    pub(crate) new_password_upload: String,
    /// URL-safe base64 (no padding) of the 32-byte id of the device this
    /// session lands on: its conversation leaf signature key (ADR-0074
    /// section 0.4). Absent when the device has no keys yet; the session then
    /// names no device and the client calls `POST /auth/session/bind-device`
    /// once it has them.
    #[serde(default)]
    pub(crate) device_id_b64: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PasswordResetFinishResponse {
    pub(crate) ok: bool,
    /// Base64-encoded `account_id` echo, same shape as
    /// [`LoginFinishResponse::account_id`].
    pub(crate) account_id: String,
    /// The session pair for the rotated password, the same shape as
    /// [`LoginFinishResponse::session`] (ADR-0073). A password reset does
    /// not bump the key generation: the sessions other devices hold stay
    /// valid (a key reset is what signs them out).
    #[serde(flatten)]
    pub(crate) session: crate::session::SessionPair,
}

/// `POST /password/reset/start`.
///
/// Mirrors the math in `login_start_inner` (KIND_RECOVERY) plus the
/// math in `signup_start` (KIND_PASSWORD), folded into one round trip.
/// The two operations are independent — the `ServerLogin` state is
/// what we persist; the `ServerRegistration` step is stateless.
pub(crate) async fn password_reset_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<PasswordResetStartRequest>,
) -> Result<Json<PasswordResetStartResponse>, OpaqueError> {
    let pepper = crate::login_handle::pepper(&state)?;
    let handle = crate::login_handle::decode_handle(&payload.handle)?;

    // Decode both client messages up-front; surfaces wire errors as 400
    // before the counter is charged (a malformed request does not spend the
    // named handle's budget) and before we touch the DB.
    let recovery_request_bytes = B64.decode(&payload.recovery_request).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid recovery_request base64: {e}"),
        )
    })?;
    let recovery_request_message = CredentialRequest::<LakkoCipherSuite>::deserialize(
        &recovery_request_bytes,
    )
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid recovery_request: {e:?}"),
        )
    })?;
    let new_password_request = decode_registration_request(&payload.new_password_request)?;

    crate::limits::charge_name(
        &state.db,
        &state.limits,
        crate::limits::Scope::PasswordReset,
        &handle,
    )
    .await?;
    // An unknown handle resolves to its stand-in id: every lookup below
    // misses and OPAQUE takes its fake-credentials path, as for an account
    // without a recovery envelope.
    let account_id = crate::login_handle::resolve(&state.db, &pepper, &handle)
        .await?
        .account_id;

    // Look up the stored KIND_RECOVERY envelope. None → unknown account
    // (or no recovery enrolled); ServerLogin::start takes the
    // fake-credentials path and produces a plausible response that
    // can't be distinguished from a real one without the seed.
    let recovery_credential_id = build_credential_id(&account_id, KIND_RECOVERY);
    let stored: Option<ServerRegistration<LakkoCipherSuite>> = sqlx::query_as::<_, (Vec<u8>,)>(
        "SELECT registration_bytes FROM opaque_registrations \
         WHERE account_id = ? AND kind = ?",
    )
    .bind(&account_id[..])
    .bind(KIND_RECOVERY)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "query opaque_registrations (recovery)");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?
    .map(|(bytes,)| ServerRegistration::<LakkoCipherSuite>::deserialize(&bytes))
    .transpose()
    .map_err(corrupted_registration)?;

    let mut rng = OsRng;
    let recovery_start = ServerLogin::<LakkoCipherSuite>::start(
        &mut rng,
        &state.opaque_setup,
        stored,
        recovery_request_message,
        &recovery_credential_id,
        ServerLoginStartParameters::default(),
    )
    .map_err(|e| {
        tracing::error!(error = ?e, "ServerLogin::start (password_reset) failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal opaque error".to_string(),
        )
    })?;

    // Begin the new password registration. Stateless — we keep the
    // response and forget; the `RegistrationUpload` arriving on the
    // finish endpoint embeds enough state for ServerRegistration::finish.
    let password_credential_id = build_credential_id(&account_id, KIND_PASSWORD);
    let password_start = ServerRegistration::<LakkoCipherSuite>::start(
        &state.opaque_setup,
        new_password_request,
        &password_credential_id,
    )
    .map_err(|e| {
        tracing::error!(error = ?e, "ServerRegistration::start (password_reset) failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal opaque error".to_string(),
        )
    })?;

    // Persist the recovery ServerLogin state under the password-reset
    // flow tag — confused-deputy guard.
    let session_id = insert_login_state(
        &state.db,
        &recovery_start.state,
        &account_id,
        KIND_PASSWORD_RESET,
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert opaque_login_states (password_reset)");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    Ok(Json(PasswordResetStartResponse {
        session_id: session_id.to_string(),
        recovery_response: B64.encode(recovery_start.message.serialize()),
        new_password_response: B64.encode(password_start.message.serialize()),
    }))
}

/// `POST /password/reset/finish`.
///
/// Verifies the recovery finalization (this is when ownership of the
/// mnemonic is actually proven), builds the new password envelope, and
/// REPLACES `opaque_registrations(account_id, kind=KIND_PASSWORD)`.
/// The recovery envelope stays untouched — the same mnemonic keeps
/// working for future resets.
///
/// The replacement is one transaction under the account row's lock (ADR-0074
/// section 0.2): before the envelope changes, the recall anchor for the
/// account's current identity key is captured if it has none, so the
/// password in force before the FIRST change is what a recall is checked
/// against, and a later change cannot overwrite it. The same transaction
/// refuses a proof made under an older key generation.
#[allow(clippy::too_many_lines)] // one flow, read top to bottom: proof, envelope, transaction
pub(crate) async fn password_reset_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<PasswordResetFinishRequest>,
) -> Result<Json<PasswordResetFinishResponse>, OpaqueError> {
    let session_id = Uuid::parse_str(&payload.session_id).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid session_id (not a UUID): {e}"),
        )
    })?;
    let device_id = crate::session::decode_device_id(payload.device_id_b64.as_deref())?;

    let cached = take_login_state(&state.db, session_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "take opaque_login_states (password_reset)");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "session_id unknown or expired".to_string(),
        ))?;

    let TakenLoginState {
        server_login,
        account_id,
        kind,
        key_generation,
    } = cached;

    if kind != KIND_PASSWORD_RESET {
        // Caller tried to finalize a regular login session via the
        // password-reset endpoint. The row is already deleted, so an
        // attacker gets one shot at most; surface 401.
        tracing::warn!(
            session_kind = kind,
            "password_reset_finish: session kind mismatch (cross-flow attempt)",
        );
        return Err((
            StatusCode::UNAUTHORIZED,
            "session_id was started for a different flow".to_string(),
        )
            .into());
    }

    // 1. Verify the recovery proof (mnemonic ownership).
    let finalize_bytes = B64.decode(&payload.recovery_finalize).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid recovery_finalize base64: {e}"),
        )
    })?;
    let finalize_message = CredentialFinalization::<LakkoCipherSuite>::deserialize(&finalize_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid recovery CredentialFinalization: {e:?}"),
            )
        })?;
    let _ = server_login.finish(finalize_message).map_err(|e| {
        // Same indistinguishable-failure policy as login_finish_inner.
        tracing::debug!(
            error = ?e,
            account_id = %B64.encode(account_id),
            "password_reset finalization failed",
        );
        (StatusCode::UNAUTHORIZED, "invalid credentials".to_string())
    })?;

    // 2. Build the new password envelope.
    let new_password_upload = decode_registration_upload(&payload.new_password_upload)?;
    let new_registration = ServerRegistration::<LakkoCipherSuite>::finish(new_password_upload);
    let new_bytes = new_registration.serialize().to_vec();

    // 3. One transaction (ADR-0074 section 0.2; it was a bare upsert before):
    //    lock the account row, hold the proof to the generation it was made
    //    under, capture the recall anchor, then replace the envelope.
    let invalid = || (StatusCode::UNAUTHORIZED, "invalid credentials".to_string());
    let proven_generation = key_generation.ok_or_else(invalid)?;
    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;
    let current: Option<u64> =
        sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ? FOR UPDATE")
            .bind(&account_id[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    // The recovery credential this proof was checked against is replaced only
    // by a key reset or a recall, and both bump the generation: a different
    // generation here means the proof is of a credential that is no longer
    // the account's, and the password it would set must not land.
    if current != Some(proven_generation) {
        tracing::info!(
            "password_reset_finish: the account's keys changed since the proof; refused",
        );
        return Err(invalid().into());
    }

    //    The anchor: the account as it was before the first change made under
    //    its current identity key. A password change does not move the key,
    //    so an existing anchor is left exactly as it is.
    crate::recall::capture_anchor(
        &mut tx,
        &account_id,
        proven_generation,
        crate::recall::Change::Password,
        state.limits.recall_window_ms(),
    )
    .await
    .map_err(ApiError::Db)?;

    //    Replace the password envelope. ON DUPLICATE KEY UPDATE relies on the
    //    (account_id, kind) composite primary key; the `updated_at` column
    //    auto-bumps via the table's `ON UPDATE CURRENT_TIMESTAMP` clause.
    sqlx::query(
        "INSERT INTO opaque_registrations (account_id, kind, registration_bytes) \
         VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE registration_bytes = VALUES(registration_bytes)",
    )
    .bind(&account_id[..])
    .bind(KIND_PASSWORD)
    .bind(&new_bytes)
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;

    tracing::info!("password reset completed (mnemonic-driven)",);

    let session =
        crate::session::issue_pair_at(&state, &account_id, proven_generation, &device_id)?;
    Ok(Json(PasswordResetFinishResponse {
        ok: true,
        account_id: B64.encode(account_id),
        session,
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use opaque_ke::{
        ClientLogin, ClientLoginFinishParameters, ClientRegistration,
        ClientRegistrationFinishParameters,
    };

    /// End-to-end OPAQUE registration math, no HTTP / DB. Confirms our
    /// CipherSuite is consistent between client and server.
    #[test]
    fn registration_roundtrip_in_memory() {
        let mut rng = OsRng;
        let server_setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let credential_id = b"0123456789abcdef\x01"; // 17 bytes — account_id || KIND_PASSWORD
        let password = b"correct horse battery staple";

        // Client: start.
        let client_start = ClientRegistration::<LakkoCipherSuite>::start(&mut rng, password)
            .expect("client registration start");

        // Server: start.
        let server_start = ServerRegistration::<LakkoCipherSuite>::start(
            &server_setup,
            client_start.message,
            credential_id,
        )
        .expect("server registration start");

        // Client: finish.
        let client_finish = client_start
            .state
            .finish(
                &mut rng,
                password,
                server_start.message,
                ClientRegistrationFinishParameters::default(),
            )
            .expect("client registration finish");

        // Server: finish — store the resulting registration.
        let server_registration =
            ServerRegistration::<LakkoCipherSuite>::finish(client_finish.message);

        // Sanity: serialize/deserialize round-trip the stored registration.
        let bytes = server_registration.serialize();
        let restored = ServerRegistration::<LakkoCipherSuite>::deserialize(&bytes)
            .expect("deserialize stored registration");
        assert_eq!(bytes.as_slice(), restored.serialize().as_slice());
    }

    #[test]
    fn decode_account_id_validates_length() {
        // Too short (8 bytes encoded).
        let short = B64.encode([0u8; 8]);
        let err = decode_account_id(&short).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Right length (16 bytes encoded).
        let ok = B64.encode([0u8; 16]);
        let bytes = decode_account_id(&ok).unwrap();
        assert_eq!(bytes.len(), 16);

        // Invalid base64.
        let err = decode_account_id("not-valid-base64!!!").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn build_credential_id_includes_kind_byte() {
        let acct = [0xABu8; ACCOUNT_ID_LEN];
        let pw = build_credential_id(&acct, KIND_PASSWORD);
        let rc = build_credential_id(&acct, KIND_RECOVERY);
        assert_eq!(pw.len(), CREDENTIAL_ID_LEN);
        assert_eq!(rc.len(), CREDENTIAL_ID_LEN);
        assert_eq!(&pw[..ACCOUNT_ID_LEN], &acct[..]);
        assert_eq!(&rc[..ACCOUNT_ID_LEN], &acct[..]);
        assert_eq!(pw[ACCOUNT_ID_LEN], KIND_PASSWORD);
        assert_eq!(rc[ACCOUNT_ID_LEN], KIND_RECOVERY);
        assert_ne!(pw, rc, "password and recovery must domain-separate");
    }

    /// Helper: full OPAQUE registration round-trip, returns the stored
    /// `ServerRegistration` for use in a subsequent login.
    fn register(
        setup: &ServerSetup<LakkoCipherSuite>,
        rng: &mut OsRng,
        credential_id: &[u8],
        password: &[u8],
    ) -> ServerRegistration<LakkoCipherSuite> {
        let cs = ClientRegistration::<LakkoCipherSuite>::start(rng, password).unwrap();
        let ss = ServerRegistration::<LakkoCipherSuite>::start(setup, cs.message, credential_id)
            .unwrap();
        let cf = cs
            .state
            .finish(
                rng,
                password,
                ss.message,
                ClientRegistrationFinishParameters::default(),
            )
            .unwrap();
        ServerRegistration::<LakkoCipherSuite>::finish(cf.message)
    }

    /// Helper: full OPAQUE login round-trip; returns whether the session
    /// keys agree (i.e. authentication succeeded).
    fn try_login(
        setup: &ServerSetup<LakkoCipherSuite>,
        rng: &mut OsRng,
        credential_id: &[u8],
        password: &[u8],
        stored: Option<ServerRegistration<LakkoCipherSuite>>,
    ) -> bool {
        let cl_start = ClientLogin::<LakkoCipherSuite>::start(rng, password).unwrap();
        let sl_start = ServerLogin::<LakkoCipherSuite>::start(
            rng,
            setup,
            stored,
            cl_start.message,
            credential_id,
            ServerLoginStartParameters::default(),
        )
        .unwrap();
        let cl_finish = cl_start.state.finish(
            password,
            sl_start.message,
            ClientLoginFinishParameters::default(),
        );
        let Ok(cl_finish) = cl_finish else {
            return false;
        };
        let Ok(sl_finish) = sl_start.state.finish(cl_finish.message) else {
            return false;
        };
        cl_finish.session_key.as_slice() == sl_finish.session_key.as_slice()
    }

    /// Full flow: register → login with the correct password.
    #[test]
    fn login_with_correct_password_succeeds() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let acct = [0u8; ACCOUNT_ID_LEN];
        let cred = build_credential_id(&acct, KIND_PASSWORD);
        let password = b"correct horse battery staple";

        let stored = register(&setup, &mut rng, &cred, password);
        assert!(try_login(&setup, &mut rng, &cred, password, Some(stored)));
    }

    /// Login with the wrong password must fail at one of the AKE finish
    /// steps (envelope decryption produces the wrong session_key).
    #[test]
    fn login_with_wrong_password_fails() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let acct = [0u8; ACCOUNT_ID_LEN];
        let cred = build_credential_id(&acct, KIND_PASSWORD);
        let real = b"correct horse battery staple";
        let wrong = b"hunter2";

        let stored = register(&setup, &mut rng, &cred, real);
        assert!(!try_login(&setup, &mut rng, &cred, wrong, Some(stored)));
    }

    /// `ServerLogin` serializes and deserializes byte-identically — proxy
    /// for the DB roundtrip without needing a real MariaDB.
    #[test]
    fn server_login_state_serializes_roundtrip() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let acct = [0u8; ACCOUNT_ID_LEN];
        let cred = build_credential_id(&acct, KIND_PASSWORD);
        let pw = b"x";

        let stored = register(&setup, &mut rng, &cred, pw);
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut rng, pw).unwrap();
        let sl = ServerLogin::<LakkoCipherSuite>::start(
            &mut rng,
            &setup,
            Some(stored),
            cl.message,
            &cred,
            ServerLoginStartParameters::default(),
        )
        .unwrap();

        let bytes = sl.state.serialize().to_vec();
        let restored =
            ServerLogin::<LakkoCipherSuite>::deserialize(&bytes).expect("ServerLogin deserialize");
        assert_eq!(bytes.as_slice(), restored.serialize().as_slice());
    }

    /// Phase 1.1d: an account can carry parallel password and recovery
    /// envelopes. Each unlocks only with its own secret AND only via its
    /// own credential_id.
    #[test]
    fn recovery_envelope_unlocks_with_seed() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let acct = [0xAB; ACCOUNT_ID_LEN];

        let pw_cred = build_credential_id(&acct, KIND_PASSWORD);
        let rc_cred = build_credential_id(&acct, KIND_RECOVERY);

        let password = b"swordfish";
        // 64-byte BIP39 seed (real clients derive this from a 12-word
        // mnemonic via PBKDF2-HMAC-SHA512; the server only sees opaque
        // bytes here, so the test uses a constant byte string).
        let seed: &[u8; 64] = b"\
            0123456789abcdef0123456789abcdef\
            0123456789abcdef0123456789abcdef";

        let pw_stored = register(&setup, &mut rng, &pw_cred, password);
        let rc_stored = register(&setup, &mut rng, &rc_cred, seed);

        // Each envelope unlocks with its own secret on its own credential_id.
        assert!(try_login(
            &setup,
            &mut rng,
            &pw_cred,
            password,
            Some(pw_stored.clone())
        ));
        assert!(try_login(
            &setup,
            &mut rng,
            &rc_cred,
            seed,
            Some(rc_stored.clone())
        ));
    }

    /// Phase 1.1d: a stolen password envelope cannot answer a recovery
    /// challenge — the credential_id mismatch (kind byte) breaks the AKE.
    #[test]
    fn password_envelope_rejected_at_recovery_endpoint() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let acct = [0xCD; ACCOUNT_ID_LEN];

        let pw_cred = build_credential_id(&acct, KIND_PASSWORD);
        let rc_cred = build_credential_id(&acct, KIND_RECOVERY);
        let password = b"hunter2";

        let pw_stored = register(&setup, &mut rng, &pw_cred, password);

        // Try to authenticate at the RECOVERY credential_id with the
        // password envelope and the password itself. Must fail because
        // the credential_id used to seal the envelope (kind=password)
        // differs from the credential_id presented at login (kind=recovery).
        assert!(
            !try_login(&setup, &mut rng, &rc_cred, password, Some(pw_stored)),
            "cross-kind login must be rejected by AKE",
        );
    }

    /// Phase 1.16c3c3a: end-to-end password-reset math. Walks the
    /// same primitives `password_reset_start` / `password_reset_finish`
    /// drive in the live handler — minus the DB and HTTP plumbing —
    /// and asserts the canonical post-reset properties:
    ///
    ///   1. The mnemonic still works (recovery envelope untouched).
    ///   2. The NEW password works against the password envelope.
    ///   3. The OLD password no longer works.
    #[test]
    fn password_reset_replaces_password_envelope() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let acct = [0x55; ACCOUNT_ID_LEN];

        let pw_cred = build_credential_id(&acct, KIND_PASSWORD);
        let rc_cred = build_credential_id(&acct, KIND_RECOVERY);

        let old_password = b"correct horse battery staple";
        let new_password = b"hunter2-rotated";
        let seed: &[u8; 64] = b"\
            0123456789abcdef0123456789abcdef\
            0123456789abcdef0123456789abcdef";

        // Initial signup: enrol both envelopes.
        let _pw_stored_initial = register(&setup, &mut rng, &pw_cred, old_password);
        let rc_stored = register(&setup, &mut rng, &rc_cred, seed);

        // ============= /password/reset/start =============
        // Client: ClientLogin::start(seed) — proves recovery secret.
        let recovery_client_start = ClientLogin::<LakkoCipherSuite>::start(&mut rng, seed).unwrap();
        // Client: ClientRegistration::start(new_password) — begins
        // enrolling the new password envelope.
        let new_pw_client_start =
            ClientRegistration::<LakkoCipherSuite>::start(&mut rng, new_password).unwrap();

        // Server: ServerLogin against the recovery envelope.
        let recovery_server_start = ServerLogin::<LakkoCipherSuite>::start(
            &mut rng,
            &setup,
            Some(rc_stored.clone()),
            recovery_client_start.message,
            &rc_cred,
            ServerLoginStartParameters::default(),
        )
        .unwrap();
        // Server: ServerRegistration::start for the new password.
        let new_pw_server_start = ServerRegistration::<LakkoCipherSuite>::start(
            &setup,
            new_pw_client_start.message,
            &pw_cred,
        )
        .unwrap();

        // ============= /password/reset/finish =============
        // Client: ClientLogin::finish (recovery proof).
        let recovery_client_finish = recovery_client_start
            .state
            .finish(
                seed,
                recovery_server_start.message,
                ClientLoginFinishParameters::default(),
            )
            .unwrap();
        // Client: ClientRegistration::finish for the new password.
        let new_pw_client_finish = new_pw_client_start
            .state
            .finish(
                &mut rng,
                new_password,
                new_pw_server_start.message,
                ClientRegistrationFinishParameters::default(),
            )
            .unwrap();

        // Server: verify the recovery proof.
        let recovery_server_finish = recovery_server_start
            .state
            .finish(recovery_client_finish.message)
            .expect("recovery proof must verify");
        // Sanity: keys agreed on the recovery side.
        assert_eq!(
            recovery_client_finish.session_key.as_slice(),
            recovery_server_finish.session_key.as_slice(),
        );

        // Server: build the new password envelope, REPLACING the old.
        let new_pw_stored =
            ServerRegistration::<LakkoCipherSuite>::finish(new_pw_client_finish.message);

        // ============= post-reset assertions =============
        // 1. Recovery envelope still works with the same seed.
        assert!(
            try_login(&setup, &mut rng, &rc_cred, seed, Some(rc_stored)),
            "recovery envelope must survive password reset",
        );
        // 2. NEW password works against the rotated envelope.
        assert!(
            try_login(
                &setup,
                &mut rng,
                &pw_cred,
                new_password,
                Some(new_pw_stored.clone())
            ),
            "new password must unlock the rotated envelope",
        );
        // 3. OLD password no longer works against the rotated envelope.
        assert!(
            !try_login(
                &setup,
                &mut rng,
                &pw_cred,
                old_password,
                Some(new_pw_stored)
            ),
            "old password must NOT unlock the rotated envelope",
        );
    }

    /// Phase 1.16c3c3a: a wrong mnemonic seed at reset time fails the
    /// AKE finalization — same indistinguishable failure as
    /// `/recovery/login/finish` with a wrong seed.
    #[test]
    fn password_reset_with_wrong_seed_fails_at_finish() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let acct = [0x66; ACCOUNT_ID_LEN];
        let pw_cred = build_credential_id(&acct, KIND_PASSWORD);
        let rc_cred = build_credential_id(&acct, KIND_RECOVERY);

        let real_seed: &[u8; 64] = b"\
            aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
            aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let wrong_seed: &[u8; 64] = b"\
            bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\
            bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let _pw_stored = register(&setup, &mut rng, &pw_cred, b"oldpw");
        let rc_stored = register(&setup, &mut rng, &rc_cred, real_seed);

        // Client uses the WRONG seed for the recovery proof.
        let recovery_client_start =
            ClientLogin::<LakkoCipherSuite>::start(&mut rng, wrong_seed).unwrap();
        let recovery_server_start = ServerLogin::<LakkoCipherSuite>::start(
            &mut rng,
            &setup,
            Some(rc_stored),
            recovery_client_start.message,
            &rc_cred,
            ServerLoginStartParameters::default(),
        )
        .unwrap();

        // The handler typically rejects at the client's `.finish`
        // because the AKE no longer produces matching keys; if the
        // client somehow gets past that, the server's own
        // `state.finish(...)` rejects. Both paths produce an Err that
        // the handler maps to 401.
        let client_finish = recovery_client_start.state.finish(
            wrong_seed,
            recovery_server_start.message,
            ClientLoginFinishParameters::default(),
        );
        let rejected = match client_finish {
            Err(_) => true,
            Ok(cf) => recovery_server_start.state.finish(cf.message).is_err(),
        };
        assert!(rejected, "wrong seed must fail at the AKE finalization");
    }

    /// Phase 1.1d: a wrong BIP39 seed against the correct recovery
    /// envelope must fail.
    #[test]
    fn recovery_login_with_wrong_seed_fails() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let acct = [0xEF; ACCOUNT_ID_LEN];
        let cred = build_credential_id(&acct, KIND_RECOVERY);

        let real_seed: &[u8; 64] = b"\
            0000000000000000000000000000000\
            00000000000000000000000000000000\
            0";
        let wrong_seed: &[u8; 64] = b"\
            ffffffffffffffffffffffffffffffff\
            ffffffffffffffffffffffffffffffff";

        let stored = register(&setup, &mut rng, &cred, real_seed);
        assert!(!try_login(
            &setup,
            &mut rng,
            &cred,
            wrong_seed,
            Some(stored)
        ));
    }
}

/// ADR-0054 S4 over HTTP + a live MariaDB: signup keeps exactly one proof of
/// work (on start) and finish is bound to that start; login / reset starts
/// carry no proof and are limited per named account, enumeration-safely.
///
/// Gated on `LAKKO_AUTH_TEST_DATABASE_URL` (skips when unset).
#[cfg(test)]
mod signup_login_limits_e2e_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::http::{header, HeaderMap, Method, Request};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use http_body_util::BodyExt as _;
    use opaque_ke::{ClientLogin, ClientRegistration, ClientRegistrationFinishParameters};
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt as _;

    use crate::limits::{Category, Limits, Scope, Window};

    fn build_state(pool: MySqlPool, limits: Limits) -> crate::AppState {
        let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
        crate::AppState {
            db: pool,
            opaque_setup: Arc::new(ServerSetup::<LakkoCipherSuite>::new(&mut OsRng)),
            session_token_secret: Arc::new(token_secret),
            session_token_ttl_ms: 86_400_000,
            refresh_token_ttl_ms: 30 * 86_400_000,
            fresh_grant_ttl_ms: 600_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
            turn: None,
            totp_enc_key: None,
            vault_enc_key: None,
            room_cap: std::sync::Arc::new(crate::room_cap::RoomAdmissionCap::disabled()),
            backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
            anon_tokens: None,
            anon_cred: None,
            handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
            recovery_email: None,
            limits: Arc::new(limits),
        }
    }

    async fn post(
        state: &crate::AppState,
        uri: &str,
        ip: &str,
        body: &serde_json::Value,
    ) -> (StatusCode, HeaderMap, Bytes) {
        let req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-real-ip", ip)
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        let resp = crate::build_app(state.clone())
            .oneshot(req)
            .await
            .expect("router oneshot");
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, headers, bytes)
    }

    /// Fetch a challenge and solve it (the tests run at effort 0).
    async fn fresh_pow(state: &crate::AppState, ip: &str) -> serde_json::Value {
        let (st, _, body) = post(state, "/pow/challenge", ip, &serde_json::json!({})).await;
        assert_eq!(st, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let seed_b64 = v["seed"].as_str().unwrap().to_string();
        let seed = B64.decode(&seed_b64).unwrap();
        let mut nonce = [0u8; 16];
        loop {
            let mut challenge = Vec::with_capacity(32);
            challenge.extend_from_slice(&seed);
            challenge.extend_from_slice(&nonce);
            if let Ok(eq) = equix::EquiX::new(&challenge) {
                if let Some(sol) = eq.solve().first() {
                    return serde_json::json!({
                        "seed": seed_b64,
                        "nonce": B64.encode(nonce),
                        "solution": B64.encode(sol.to_bytes()),
                    });
                }
            }
            nonce[0] = nonce[0].wrapping_add(1);
            if nonce[0] == 0 {
                nonce[1] = nonce[1].wrapping_add(1);
            }
        }
    }

    async fn pool() -> Option<MySqlPool> {
        let url = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL").ok()?;
        std::env::set_var("LAKKO_SIGNUP_POW_EFFORT_BITS", "0");
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("migrations");
        Some(pool)
    }

    /// A real Ed25519 public key per tag: the key reset records the key it
    /// installs and refuses one that is not a usable point (ADR-0074).
    fn pk(tag: u8) -> [u8; 32] {
        lakko_crypto::signing::SigningKey::from_seed(&[tag; 32])
            .verifying_key()
            .to_bytes()
    }

    /// A fresh 16-byte account id, base64 as the wire carries it.
    fn new_account() -> ([u8; 16], String) {
        let id = *Uuid::now_v7().as_bytes();
        (id, B64.encode(id))
    }

    /// Client-side registration start + the finish closure's inputs.
    fn reg_start(secret: &[u8]) -> (opaque_ke::ClientRegistration<LakkoCipherSuite>, String) {
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut OsRng, secret).unwrap();
        let msg = B64.encode(cs.message.serialize());
        (cs.state, msg)
    }

    fn reg_finish(
        state: opaque_ke::ClientRegistration<LakkoCipherSuite>,
        secret: &[u8],
        response_b64: &str,
    ) -> String {
        let resp = opaque_ke::RegistrationResponse::<LakkoCipherSuite>::deserialize(
            &B64.decode(response_b64).unwrap(),
        )
        .unwrap();
        let fin = state
            .finish(
                &mut OsRng,
                secret,
                resp,
                ClientRegistrationFinishParameters::default(),
            )
            .unwrap();
        B64.encode(fin.message.serialize())
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn signup_keeps_one_proof_and_binds_finish_to_start() {
        let Some(pool) = pool().await else {
            eprintln!("skipping signup e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let ip = "198.51.100.10";
        let pw = b"correct horse battery staple";
        let seed = [0x42u8; 64];

        // (1) start without a proof is refused (the field is required).
        let (id, id_b64) = new_account();
        let (_, req) = reg_start(pw);
        let (st, _, _) = post(
            &state,
            "/signup/start",
            ip,
            &serde_json::json!({ "account_id": id_b64, "password_request": req }),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "no pow field");
        // A proof that does not verify is refused too.
        let (st, _, _) = post(
            &state,
            "/signup/start",
            ip,
            &serde_json::json!({
                "pow": { "seed": B64.encode([0u8; 16]), "nonce": B64.encode([0u8; 16]),
                         "solution": B64.encode([0u8; 16]) },
                "account_id": id_b64, "password_request": req,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "unknown challenge");

        // (2) finish without a start is refused, and creates nothing.
        let (cs, req) = reg_start(pw);
        let throwaway = ServerSetup::<LakkoCipherSuite>::new(&mut OsRng);
        let response = ServerRegistration::<LakkoCipherSuite>::start(
            &throwaway,
            RegistrationRequest::deserialize(&B64.decode(&req).unwrap()).unwrap(),
            &build_credential_id(&id, KIND_PASSWORD),
        )
        .unwrap();
        let upload = reg_finish(cs, pw, &B64.encode(response.message.serialize()));
        let (st, _, _) = post(
            &state,
            "/signup/finish",
            ip,
            &serde_json::json!({ "account_id": id_b64, "password_upload": upload }),
        )
        .await;
        assert_eq!(st, StatusCode::GONE, "finish without a start");
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE id = ?")
            .bind(id.as_slice())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(exists, 0);

        // (3) the real flow: one proof on start, none on finish.
        let (cs_pw, req_pw) = reg_start(pw);
        let (cs_rc, req_rc) = reg_start(&seed);
        let pow = fresh_pow(&state, ip).await;
        let (st, _, body) = post(
            &state,
            "/signup/start",
            ip,
            &serde_json::json!({
                "pow": pow, "account_id": id_b64,
                "password_request": req_pw, "recovery_request": req_rc,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let up_pw = reg_finish(cs_pw, pw, v["password_response"].as_str().unwrap());
        let up_rc = reg_finish(cs_rc, &seed, v["recovery_response"].as_str().unwrap());

        // A finish that drops the recovery upload the start announced: 400,
        // and the pending start survives (the transaction rolled back).
        let (st, _, _) = post(
            &state,
            "/signup/finish",
            ip,
            &serde_json::json!({ "account_id": id_b64, "password_upload": up_pw }),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "recovery mismatch");

        // A finish that still carries a legacy `pow` field: ignored.
        let (st, _, body) = post(
            &state,
            "/signup/finish",
            ip,
            &serde_json::json!({
                "pow": { "seed": "x", "nonce": "x", "solution": "x" },
                "account_id": id_b64, "password_upload": up_pw, "recovery_upload": up_rc,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["session_token"].as_str().is_some());

        // (4) the start is spent: a replayed finish is refused.
        let (st, _, _) = post(
            &state,
            "/signup/finish",
            ip,
            &serde_json::json!({
                "account_id": id_b64, "password_upload": up_pw, "recovery_upload": up_rc,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::GONE, "one start, one account");

        // (5) the per-address signup limit: 429 + Retry-After, never 401.
        let limited = build_state(
            pool.clone(),
            Limits::defaults().with_ip_window(Category::Signup, Window::new(2, 3600)),
        );
        let ip2 = "198.51.100.11";
        for _ in 0..2 {
            let (st, _, _) = post(&limited, "/signup/start", ip2, &serde_json::json!({})).await;
            assert_eq!(
                st,
                StatusCode::UNPROCESSABLE_ENTITY,
                "admitted by the bucket"
            );
        }
        let (st, headers, body) =
            post(&limited, "/signup/start", ip2, &serde_json::json!({})).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(headers[header::RETRY_AFTER], "1800");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], crate::limits::CODE_SIGNUP_RATE_LIMITED);
        assert_eq!(v["retry_after_secs"], 1800);
        // Another address is unaffected.
        let (st, _, _) = post(
            &limited,
            "/signup/start",
            "198.51.100.12",
            &serde_json::json!({}),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(id.as_slice())
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn login_and_reset_are_limited_per_named_account_enumeration_safely() {
        let Some(pool) = pool().await else {
            eprintln!("skipping login e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(
            pool.clone(),
            Limits::defaults()
                .with_name_window(Scope::LoginPassword, Window::new(3, 900))
                .with_name_window(Scope::PasswordReset, Window::new(1, 900)),
        );
        let ip = "198.51.100.20";
        let pw = b"hunter2hunter2";

        // A registered account.
        let (known, known_id_b64) = new_account();
        let (cs, req) = reg_start(pw);
        let pow = fresh_pow(&state, ip).await;
        let (st, _, body) = post(
            &state,
            "/signup/start",
            ip,
            &serde_json::json!({ "pow": pow, "account_id": known_id_b64, "password_request": req }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let upload = reg_finish(cs, pw, v["password_response"].as_str().unwrap());
        let (st, _, _) = post(
            &state,
            "/signup/finish",
            ip,
            &serde_json::json!({ "account_id": known_id_b64, "password_upload": upload }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        // ADR-0054 S5: the routes name the account by its login handle.
        let (known_handle, known_b64) = crate::login_handle::random_handle();
        crate::login_handle::bind_for_test(&pool, &known, &known_handle).await;
        let (_, unknown_b64) = crate::login_handle::random_handle();

        let login_start = |id: String, with_legacy_pow: bool| {
            let state = state.clone();
            async move {
                let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, pw).unwrap();
                let mut body = serde_json::json!({
                    "handle": id,
                    "request": B64.encode(cl.message.serialize()),
                });
                if with_legacy_pow {
                    body["pow"] = serde_json::json!({ "seed": "x", "nonce": "x", "solution": "x" });
                }
                post(&state, "/login/start", ip, &body).await
            }
        };

        // Under the limit both answer 200 (the unknown one via OPAQUE's fake
        // credentials); a body with a legacy `pow` field is accepted.
        for id in [&known_b64, &unknown_b64] {
            for i in 0..3 {
                let (st, _, body) = login_start(id.clone(), i == 0).await;
                assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
            }
        }
        // The next start is refused identically for both.
        let mut refusals = Vec::new();
        for id in [&known_b64, &unknown_b64] {
            let (st, headers, body) = login_start(id.clone(), false).await;
            assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
            let retry: u64 = headers[header::RETRY_AFTER]
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            assert!((1..=900).contains(&retry));
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["error"], crate::limits::CODE_TOO_MANY_ATTEMPTS);
            refusals.push(v["error"].clone());
        }
        assert_eq!(refusals[0], refusals[1]);

        // The recovery login keeps its own counter.
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, &[0x42u8; 64]).unwrap();
        let (st, _, _) = post(
            &state,
            "/recovery/login/start",
            ip,
            &serde_json::json!({
                "handle": known_b64,
                "request": B64.encode(cl.message.serialize()),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "separate scope");

        // Password reset: one per window here, for an unknown account too.
        for (i, expected) in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS]
            .into_iter()
            .enumerate()
        {
            let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, &[0x42u8; 64]).unwrap();
            let (_, req) = reg_start(b"new password");
            let (st, _, body) = post(
                &state,
                "/password/reset/start",
                ip,
                &serde_json::json!({
                    "handle": unknown_b64,
                    "recovery_request": B64.encode(cl.message.serialize()),
                    "new_password_request": req,
                }),
            )
            .await;
            assert_eq!(
                st,
                expected,
                "reset {i}: {}",
                String::from_utf8_lossy(&body)
            );
        }

        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(known.as_slice())
            .execute(&pool)
            .await
            .ok();
    }

    // ------------------------------------------------------------------
    // ADR-0054 S5 — login by name
    // ------------------------------------------------------------------

    /// A request with an optional bearer token.
    async fn send_json(
        state: &crate::AppState,
        method: Method,
        uri: &str,
        bearer: Option<&str>,
        body: Option<&serde_json::Value>,
    ) -> (StatusCode, Bytes) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-real-ip", "198.51.100.40");
        if let Some(t) = bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = match body {
            Some(b) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(b).unwrap()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let resp = crate::build_app(state.clone())
            .oneshot(req)
            .await
            .expect("router oneshot");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, bytes)
    }

    /// Create an account with a password and a recovery seed; returns its id
    /// and the session token signup issued.
    async fn signup_account(state: &crate::AppState, pw: &[u8], seed: &[u8]) -> ([u8; 16], String) {
        let (id, token, _) = signup_account_with_grant(state, pw, seed).await;
        (id, token)
    }

    /// [`signup_account`], also returning the fresh-auth grant the signup's
    /// finish minted (asserted here for every account these tests create:
    /// present, a `k1`, for this account at the genesis generation, expiring
    /// within the configured lifetime).
    async fn signup_account_with_grant(
        state: &crate::AppState,
        pw: &[u8],
        seed: &[u8],
    ) -> ([u8; 16], String, String) {
        let ip = "198.51.100.41";
        let (id, id_b64) = new_account();
        let (cs_pw, req_pw) = reg_start(pw);
        let (cs_rc, req_rc) = reg_start(seed);
        let pow = fresh_pow(state, ip).await;
        let (st, _, body) = post(
            state,
            "/signup/start",
            ip,
            &serde_json::json!({
                "pow": pow, "account_id": id_b64,
                "password_request": req_pw, "recovery_request": req_rc,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let up_pw = reg_finish(cs_pw, pw, v["password_response"].as_str().unwrap());
        let up_rc = reg_finish(cs_rc, seed, v["recovery_response"].as_str().unwrap());
        let (st, _, body) = post(
            state,
            "/signup/finish",
            ip,
            &serde_json::json!({
                "account_id": id_b64, "password_upload": up_pw, "recovery_upload": up_rc,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let grant = v["fresh_grant"]
            .as_str()
            .expect("signup finish mints a fresh-auth grant")
            .to_string();
        assert!(grant.starts_with("k1."), "{grant}");
        let now = crate::session::now_ms();
        let ttl = v["fresh_grant_expires_at_ms"].as_i64().unwrap() - now;
        assert!(ttl > 0 && ttl <= state.fresh_grant_ttl_ms, "{ttl}");
        let verified = state
            .session_token_secret
            .verify_fresh_grant(&grant, now)
            .unwrap();
        assert_eq!(verified.account_id, id);
        assert_eq!(verified.key_generation, 1);
        assert_eq!(v["key_generation"].as_u64(), Some(1));
        (id, v["session_token"].as_str().unwrap().to_string(), grant)
    }

    /// Run a full login by handle with the real OPAQUE client. `Ok(account
    /// id)` on success; `Err(())` when the client cannot finish (wrong
    /// secret or an unbound handle — the two are the same to the client).
    async fn login_by_handle(
        state: &crate::AppState,
        start_uri: &str,
        finish_uri: &str,
        handle_b64: &str,
        secret: &[u8],
    ) -> Result<String, ()> {
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, secret).unwrap();
        let (st, _, body) = post(
            state,
            start_uri,
            "198.51.100.42",
            &serde_json::json!({
                "handle": handle_b64,
                "request": B64.encode(cl.message.serialize()),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let response = opaque_ke::CredentialResponse::<LakkoCipherSuite>::deserialize(
            &B64.decode(v["response"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        let fin = cl
            .state
            .finish(
                secret,
                response,
                opaque_ke::ClientLoginFinishParameters::default(),
            )
            .map_err(|_| ())?;
        let (st, _, body) = post(
            state,
            finish_uri,
            "198.51.100.42",
            &serde_json::json!({
                "session_id": v["session_id"],
                "finalize": B64.encode(fin.message.serialize()),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["session_token"].as_str().is_some());
        Ok(v["account_id"].as_str().unwrap().to_string())
    }

    fn err_code(body: &[u8]) -> serde_json::Value {
        serde_json::from_slice::<serde_json::Value>(body).unwrap()["error"].clone()
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn login_handle_bind_replace_taken_delete_and_login_by_name() {
        let Some(pool) = pool().await else {
            eprintln!("skipping login-handle e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        // No minimum interval here: this test re-binds within seconds (the
        // interval has its own test below).
        let state = build_state(
            pool.clone(),
            Limits::defaults().with_handle_change_interval(0),
        );
        let pw = b"a password for login by name";
        let seed = [0x31u8; 64];
        let (a, a_token) = signup_account(&state, pw, &seed).await;
        let (b, b_token) = signup_account(&state, b"b's password", &[0x32u8; 64]).await;
        let (_, h1) = crate::login_handle::random_handle();
        let (_, h2) = crate::login_handle::random_handle();
        let (_, hb) = crate::login_handle::random_handle();
        let put = |token: String, handle: String| {
            let state = state.clone();
            async move { put_handle(&state, &token, &handle).await }
        };

        // Bind, then sign in by name (password and recovery), learning the
        // account id only from the finish response.
        let (st, body) = put(a_token.clone(), h1.clone()).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let a_b64 = B64.encode(a);
        assert_eq!(
            login_by_handle(&state, "/login/start", "/login/finish", &h1, pw).await,
            Ok(a_b64.clone())
        );
        assert_eq!(
            login_by_handle(
                &state,
                "/recovery/login/start",
                "/recovery/login/finish",
                &h1,
                &seed
            )
            .await,
            Ok(a_b64.clone())
        );
        assert_eq!(
            login_by_handle(&state, "/login/start", "/login/finish", &h1, b"wrong").await,
            Err(()),
            "a wrong password cannot finish"
        );

        // Replace: the old handle stops naming the account.
        let (st, _) = put(a_token.clone(), h2.clone()).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            login_by_handle(&state, "/login/start", "/login/finish", &h1, pw).await,
            Err(())
        );
        assert_eq!(
            login_by_handle(&state, "/login/start", "/login/finish", &h2, pw).await,
            Ok(a_b64.clone())
        );

        // Taken: B cannot take A's handle, and a refused bind keeps B's own.
        let (st, _) = put(b_token.clone(), hb.clone()).await;
        assert_eq!(st, StatusCode::OK);
        let (st, body) = put(b_token.clone(), h2.clone()).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(err_code(&body), "handle-taken");
        assert_eq!(
            login_by_handle(
                &state,
                "/login/start",
                "/login/finish",
                &hb,
                b"b's password"
            )
            .await,
            Ok(B64.encode(b))
        );
        assert_eq!(
            login_by_handle(&state, "/login/start", "/login/finish", &h2, pw).await,
            Ok(a_b64.clone()),
            "the refused bind changed nothing for A"
        );

        // Without a session, and with a malformed handle.
        let (st, _) = send_json(
            &state,
            Method::PUT,
            "/auth/login-handle",
            None,
            Some(&serde_json::json!({ "handle": h1 })),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, _) = put(a_token.clone(), "AAAA".to_string()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // Delete: the name stops resolving; a second delete removes nothing.
        let (st, body) = send_json(
            &state,
            Method::DELETE,
            "/auth/login-handle",
            Some(&a_token),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["removed"],
            true
        );
        let h2_bytes = crate::login_handle::decode_handle(&h2).ok().unwrap();
        assert!(
            !crate::login_handle::resolve(&pool, &crate::login_handle::TEST_PEPPER, &h2_bytes)
                .await
                .ok()
                .unwrap()
                .bound
        );
        let (_, body) = send_json(
            &state,
            Method::DELETE,
            "/auth/login-handle",
            Some(&a_token),
            None,
        )
        .await;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["removed"],
            false
        );

        // Account deletion removes the handle (FK cascade), freeing the name.
        let (st, _) = put(a_token.clone(), h1.clone()).await;
        assert_eq!(st, StatusCode::OK);
        let fresh = state
            .session_token_secret
            .issue_fresh_grant(&a, 1, crate::session::now_ms() + 60_000)
            .unwrap();
        let (st, _) = delete_account(&state, &a_token, Some(&fresh)).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        let left: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM login_handles WHERE account_id = ?")
                .bind(a.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(left, 0);
        let (st, _) = put(b_token.clone(), h1.clone()).await;
        assert_eq!(st, StatusCode::OK, "a freed name can be bound again");

        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(b.as_slice())
            .execute(&pool)
            .await
            .ok();
    }

    /// The OPRF evaluation that opens a credential response: its first 32
    /// bytes (RFC 9807 `evaluated_message`).
    async fn evaluation_for(
        state: &crate::AppState,
        uri: &str,
        handle_b64: &str,
        blind_seed: u64,
    ) -> (Vec<u8>, usize, Vec<String>) {
        use rand::SeedableRng as _;
        let mut rng = rand::rngs::StdRng::seed_from_u64(blind_seed);
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut rng, b"same guess").unwrap();
        let (st, _, body) = post(
            state,
            uri,
            "198.51.100.43",
            &serde_json::json!({
                "handle": handle_b64,
                "request": B64.encode(cl.message.serialize()),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        let resp = B64.decode(v["response"].as_str().unwrap()).unwrap();
        (resp[..32].to_vec(), resp.len(), keys)
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn an_unknown_handle_is_indistinguishable_from_a_known_one() {
        let Some(pool) = pool().await else {
            eprintln!("skipping login-handle e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let (k, _) = signup_account(&state, b"known pw", &[0x33u8; 64]).await;
        let (k_handle, known) = crate::login_handle::random_handle();
        crate::login_handle::bind_for_test(&pool, &k, &k_handle).await;
        let (u1_handle, unknown1) = crate::login_handle::random_handle();
        let (_, unknown2) = crate::login_handle::random_handle();

        // The code path is the same one: both resolve to an account id and
        // run the same lookups; only the internal flag differs.
        let p = crate::login_handle::TEST_PEPPER;
        assert!(
            crate::login_handle::resolve(&pool, &p, &k_handle)
                .await
                .ok()
                .unwrap()
                .bound
        );
        let u = crate::login_handle::resolve(&pool, &p, &u1_handle)
            .await
            .ok()
            .unwrap();
        assert!(!u.bound);
        assert_eq!(
            u.account_id,
            crate::login_handle::resolve(&pool, &p, &u1_handle)
                .await
                .ok()
                .unwrap()
                .account_id,
            "the stand-in is stable per handle"
        );

        for uri in ["/login/start", "/recovery/login/start"] {
            // Same status, same body shape, same response length.
            let (ek, lk, kk) = evaluation_for(&state, uri, &known, 7).await;
            let (e1, l1, k1) = evaluation_for(&state, uri, &unknown1, 7).await;
            assert_eq!((lk, &kk), (l1, &k1), "{uri}: shape");
            // A repeated name meets the same OPRF key: with the same blind the
            // evaluation repeats, for a known and an unknown name alike.
            assert_eq!(ek, evaluation_for(&state, uri, &known, 7).await.0, "{uri}");
            assert_eq!(
                e1,
                evaluation_for(&state, uri, &unknown1, 7).await.0,
                "{uri}"
            );
            // Different names meet different keys.
            let (e2, _, _) = evaluation_for(&state, uri, &unknown2, 7).await;
            assert_ne!(e1, e2, "{uri}");
            assert_ne!(e1, ek, "{uri}");
        }

        // A finalization that does not verify answers the same for both.
        let mut finishes = Vec::new();
        for h in [&known, &unknown1] {
            let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, b"guess").unwrap();
            let (_, _, body) = post(
                &state,
                "/login/start",
                "198.51.100.44",
                &serde_json::json!({ "handle": h, "request": B64.encode(cl.message.serialize()) }),
            )
            .await;
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let (st, _, body) = post(
                &state,
                "/login/finish",
                "198.51.100.44",
                &serde_json::json!({
                    "session_id": v["session_id"],
                    "finalize": B64.encode([0x42u8; 64]),
                }),
            )
            .await;
            finishes.push((st, body));
        }
        assert_eq!(finishes[0].0, StatusCode::UNAUTHORIZED);
        assert_eq!(finishes[0], finishes[1]);

        // A reset start answers the same shape for both.
        let mut shapes = Vec::new();
        for h in [&known, &unknown1] {
            let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, &[0x33u8; 64]).unwrap();
            let (_, req) = reg_start(b"new password");
            let (st, _, body) = post(
                &state,
                "/password/reset/start",
                "198.51.100.45",
                &serde_json::json!({
                    "handle": h,
                    "recovery_request": B64.encode(cl.message.serialize()),
                    "new_password_request": req,
                }),
            )
            .await;
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            let lens: Vec<usize> = ["recovery_response", "new_password_response"]
                .iter()
                .map(|f| B64.decode(v[*f].as_str().unwrap()).unwrap().len())
                .collect();
            shapes.push((st, keys, lens));
        }
        assert_eq!(shapes[0].0, StatusCode::OK);
        assert_eq!(shapes[0], shapes[1]);

        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(k.as_slice())
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn every_name_route_answers_503_without_the_pepper() {
        let Some(pool) = pool().await else {
            eprintln!("skipping login-handle e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let mut state = build_state(pool.clone(), Limits::defaults());
        state.handle_pepper = None;
        // The factor key is set so the pepper is what refuses.
        state.totp_enc_key = Some(Arc::new([1u8; 32]));
        let (_, h) = crate::login_handle::random_handle();
        let routes = [
            (
                "/login/start",
                serde_json::json!({ "handle": h, "request": "x" }),
            ),
            (
                "/recovery/login/start",
                serde_json::json!({ "handle": h, "request": "x" }),
            ),
            (
                "/password/reset/start",
                serde_json::json!({ "handle": h, "recovery_request": "x", "new_password_request": "x" }),
            ),
            (
                "/totp/recover",
                serde_json::json!({ "handle": h, "code": "000000" }),
            ),
        ];
        for (uri, body) in routes {
            let (st, _, bytes) = post(&state, uri, "198.51.100.46", &body).await;
            assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            assert_eq!(err_code(&bytes), "login-by-name-disabled", "{uri}");
        }
        // A bearer passes the middleware only for an account that exists
        // (ADR-0073: it reads the key generation), so the token is a real
        // account's.
        let (a, token) = raw_account(&pool, &state).await;
        for method in [Method::PUT, Method::DELETE] {
            let (st, bytes) = send_json(
                &state,
                method,
                "/auth/login-handle",
                Some(&token),
                Some(&serde_json::json!({ "handle": h })),
            )
            .await;
            assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(err_code(&bytes), "login-by-name-disabled");
        }
        drop_accounts(&pool, &[a]).await;
    }

    // ------------------------------------------------------------------
    // ADR-0054, what the security pass found
    // ------------------------------------------------------------------

    /// An account row and a session token for it, without the OPAQUE dance.
    async fn raw_account(pool: &MySqlPool, state: &crate::AppState) -> ([u8; 16], String) {
        let (id, _) = new_account();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(id.as_slice())
            .execute(pool)
            .await
            .unwrap();
        let now_ms = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let token = state
            .session_token_secret
            .issue(&id, 1, &[0u8; 32], 0, now_ms + 3_600_000)
            .unwrap();
        (id, token)
    }

    /// A fresh-auth grant for the account `token` names, as a password
    /// sign-in made just now would have returned.
    fn grant_for(state: &crate::AppState, token: &str) -> String {
        let now = crate::session::now_ms();
        let verified = state.session_token_secret.verify(token, now).unwrap();
        state
            .session_token_secret
            .issue_fresh_grant(&verified.account_id, verified.key_generation, now + 60_000)
            .unwrap()
    }

    /// `PUT /auth/login-handle` with a live fresh-auth grant, which a bind
    /// that changes the handle needs since ADR-0076 (a first bind ignores
    /// it). What these tests are about is everything after that gate.
    async fn put_handle(state: &crate::AppState, token: &str, handle: &str) -> (StatusCode, Bytes) {
        send_json(
            state,
            Method::PUT,
            "/auth/login-handle",
            Some(token),
            Some(&serde_json::json!({
                "handle": handle,
                "fresh_grant": grant_for(state, token),
            })),
        )
        .await
    }

    async fn drop_accounts(pool: &MySqlPool, ids: &[[u8; 16]]) {
        for id in ids {
            sqlx::query("DELETE FROM accounts WHERE id = ?")
                .bind(id.as_slice())
                .execute(pool)
                .await
                .ok();
        }
    }

    /// Finding 1: one account cannot sweep the handle registry through the
    /// 409. The 409 stays; every bind is counted per account, a change is
    /// refused within the minimum interval of the last one (restoring the
    /// previous handle excepted, a DELETE not reopening it), and the route
    /// has its own per-address bucket.
    #[tokio::test]
    #[allow(clippy::too_many_lines, clippy::many_single_char_names)]
    async fn login_handle_binds_are_rationed() {
        let Some(pool) = pool().await else {
            eprintln!("skipping login-handle e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let limited = |body: &Bytes| {
            let v: serde_json::Value = serde_json::from_slice(body).unwrap();
            assert_eq!(v["error"], crate::limits::CODE_HANDLE_BIND_LIMITED);
            assert!(v["retry_after_secs"].as_u64().unwrap() >= 1);
        };
        let (a, a_token) = raw_account(&pool, &state).await;
        let (b, b_token) = raw_account(&pool, &state).await;
        let (_, hb) = crate::login_handle::random_handle();
        let (_, h1) = crate::login_handle::random_handle();
        let (_, h2) = crate::login_handle::random_handle();
        assert_eq!(put_handle(&state, &b_token, &hb).await.0, StatusCode::OK);

        // A probes: a taken name is still 409 (the claim order needs it).
        let (st, body) = put_handle(&state, &a_token, &hb).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(err_code(&body), "handle-taken");
        assert_eq!(
            put_handle(&state, &a_token, &hb).await.0,
            StatusCode::CONFLICT
        );
        // A free name binds; the next change is refused for the interval,
        // whether the probed name is free or taken.
        assert_eq!(put_handle(&state, &a_token, &h1).await.0, StatusCode::OK);
        let (st, body) = put_handle(&state, &a_token, &h2).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        limited(&body);
        let (st, body) = put_handle(&state, &a_token, &hb).await;
        assert_eq!(
            st,
            StatusCode::TOO_MANY_REQUESTS,
            "no 409 inside the interval"
        );
        limited(&body);
        // Five binds spent: the sixth is refused whatever it asks for, even
        // re-binding the handle A already holds.
        let (st, body) = put_handle(&state, &a_token, &h1).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        limited(&body);
        let h1_bytes = crate::login_handle::decode_handle(&h1).ok().unwrap();
        let r = crate::login_handle::resolve(&pool, &crate::login_handle::TEST_PEPPER, &h1_bytes)
            .await
            .ok()
            .unwrap();
        assert_eq!(r.account_id, a, "the refusals changed nothing");

        // Restoring the handle held just before is exempt from the interval.
        let (c, c_token) = raw_account(&pool, &state).await;
        let (_, c0) = crate::login_handle::random_handle();
        let (_, c1) = crate::login_handle::random_handle();
        let (_, c2) = crate::login_handle::random_handle();
        assert_eq!(put_handle(&state, &c_token, &c0).await.0, StatusCode::OK);
        sqlx::query(
            "UPDATE login_handle_changes SET last_changed_at = NOW(3) - INTERVAL 1 HOUR \
             WHERE account_id = ?",
        )
        .bind(c.as_slice())
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(put_handle(&state, &c_token, &c1).await.0, StatusCode::OK);
        assert_eq!(
            put_handle(&state, &c_token, &c2).await.0,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            put_handle(&state, &c_token, &c0).await.0,
            StatusCode::OK,
            "a rename can be rolled back at once"
        );

        // A DELETE does not reopen the interval; the released handle can be
        // taken back.
        let (d, d_token) = raw_account(&pool, &state).await;
        let (_, d0) = crate::login_handle::random_handle();
        let (_, d1) = crate::login_handle::random_handle();
        assert_eq!(put_handle(&state, &d_token, &d0).await.0, StatusCode::OK);
        let (st, _) = send_json(
            &state,
            Method::DELETE,
            "/auth/login-handle",
            Some(&d_token),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            put_handle(&state, &d_token, &d1).await.0,
            StatusCode::TOO_MANY_REQUESTS,
            "release-and-rebind is still inside the interval"
        );
        assert_eq!(
            put_handle(&state, &d_token, &d0).await.0,
            StatusCode::OK,
            "the handle just released can be bound again"
        );

        // The per-address bucket on the route.
        let tight = build_state(
            pool.clone(),
            Limits::defaults().with_ip_window(Category::HandleBind, Window::new(1, 3600)),
        );
        let (e, e_token) = raw_account(&pool, &tight).await;
        let (_, e0) = crate::login_handle::random_handle();
        assert_eq!(put_handle(&tight, &e_token, &e0).await.0, StatusCode::OK);
        let (st, body) = put_handle(&tight, &e_token, &e0).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        limited(&body);

        drop_accounts(&pool, &[a, b, c, d, e]).await;
    }

    /// Finding 7: concurrent binds of one account are serialised, so none of
    /// them answers a spurious 409 (a unique violation on the account key
    /// rather than on the handle).
    #[tokio::test]
    async fn concurrent_binds_of_one_account_never_conflict() {
        let Some(pool) = pool().await else {
            eprintln!("skipping login-handle e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(
            pool.clone(),
            Limits::defaults()
                .with_handle_change_interval(0)
                .with_name_window(Scope::HandleBind, Window::new(100, 86_400))
                .with_ip_window(Category::HandleBind, Window::new(100, 3600)),
        );
        let (a, token) = raw_account(&pool, &state).await;
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let state = state.clone();
            let token = token.clone();
            let (_, h) = crate::login_handle::random_handle();
            tasks.spawn(async move { put_handle(&state, &token, &h).await });
        }
        while let Some(res) = tasks.join_next().await {
            let (st, body) = res.unwrap();
            assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        }
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM login_handles WHERE account_id = ?")
                .bind(a.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(rows, 1);
        drop_accounts(&pool, &[a]).await;
    }

    /// Finding 3: a request whose body does not decode is refused (400)
    /// before the named handle's counter is charged, so junk cannot lock a
    /// name.
    #[tokio::test]
    async fn a_malformed_start_does_not_spend_the_named_counter() {
        let Some(pool) = pool().await else {
            eprintln!("skipping login-handle e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(
            pool.clone(),
            Limits::defaults()
                .with_name_window(Scope::LoginPassword, Window::new(2, 900))
                .with_name_window(Scope::PasswordReset, Window::new(2, 900)),
        );
        let (_, handle) = crate::login_handle::random_handle();
        let valid_request = || {
            let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, b"guess").unwrap();
            B64.encode(cl.message.serialize())
        };
        for junk in ["!!!not base64", "AAAA"] {
            for _ in 0..3 {
                let (st, _, _) = post(
                    &state,
                    "/login/start",
                    "198.51.100.50",
                    &serde_json::json!({ "handle": handle, "request": junk }),
                )
                .await;
                assert_eq!(st, StatusCode::BAD_REQUEST);
                let (st, _, _) = post(
                    &state,
                    "/password/reset/start",
                    "198.51.100.50",
                    &serde_json::json!({
                        "handle": handle,
                        "recovery_request": valid_request(),
                        "new_password_request": junk,
                    }),
                )
                .await;
                assert_eq!(st, StatusCode::BAD_REQUEST);
            }
        }
        // The whole budget is still there, then it runs out.
        for expected in [
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let (st, _, _) = post(
                &state,
                "/login/start",
                "198.51.100.50",
                &serde_json::json!({ "handle": handle, "request": valid_request() }),
            )
            .await;
            assert_eq!(st, expected);
        }
        let (_, pw_req) = reg_start(b"new password");
        for expected in [
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let (st, _, _) = post(
                &state,
                "/password/reset/start",
                "198.51.100.50",
                &serde_json::json!({
                    "handle": handle,
                    "recovery_request": valid_request(),
                    "new_password_request": pw_req,
                }),
            )
            .await;
            assert_eq!(st, expected);
        }
    }

    // ------------------------------------------------------------------
    // Reset the account keys (ADR-0073 section 2 and 3)
    // ------------------------------------------------------------------

    /// Run the two reset calls with `token` and the key-reset `grant` for
    /// `seed` and `pubkey`; returns the finish answer (or the start's, when
    /// that is refused).
    async fn reset_keys(
        state: &crate::AppState,
        token: &str,
        grant: Option<&str>,
        seed: &[u8],
        pubkey: &[u8; 32],
    ) -> (StatusCode, Bytes) {
        let (cs, req) = reg_start(seed);
        let (st, body) = send_json(
            state,
            Method::POST,
            "/account/keys/reset/start",
            Some(token),
            Some(&serde_json::json!({ "fresh_grant": grant, "recovery_request": req })),
        )
        .await;
        if st != StatusCode::OK {
            return (st, body);
        }
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let upload = reg_finish(cs, seed, v["recovery_response"].as_str().unwrap());
        send_json(
            state,
            Method::POST,
            "/account/keys/reset/finish",
            Some(token),
            Some(&serde_json::json!({
                "fresh_grant": grant,
                "recovery_upload": upload,
                "new_signature_pubkey_b64": URL_SAFE_NO_PAD.encode(pubkey),
            })),
        )
        .await
    }

    /// `POST /account/keys/reset/finish` alone, with an upload prepared
    /// against the server's own setup (so no start call is spent).
    async fn reset_finish_only(
        state: &crate::AppState,
        token: &str,
        grant: Option<&str>,
        account: &[u8; 16],
        seed: &[u8],
        pubkey: &[u8; 32],
    ) -> (StatusCode, Bytes) {
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut OsRng, seed).unwrap();
        let response = ServerRegistration::<LakkoCipherSuite>::start(
            &state.opaque_setup,
            cs.message.clone(),
            &build_credential_id(account, KIND_RECOVERY),
        )
        .unwrap();
        let upload = reg_finish(cs.state, seed, &B64.encode(response.message.serialize()));
        send_json(
            state,
            Method::POST,
            "/account/keys/reset/finish",
            Some(token),
            Some(&serde_json::json!({
                "fresh_grant": grant,
                "recovery_upload": upload,
                "new_signature_pubkey_b64": URL_SAFE_NO_PAD.encode(pubkey),
            })),
        )
        .await
    }

    /// Prove the password just now: a full password login by `handle`,
    /// returning the fresh-auth grant its answer carries.
    async fn grant_by_password(state: &crate::AppState, handle: &str, pw: &[u8]) -> String {
        let v = password_login(state, handle, pw).await.unwrap();
        assert!(v["fresh_grant_expires_at_ms"].as_i64().unwrap() > crate::session::now_ms());
        v["fresh_grant"].as_str().unwrap().to_string()
    }

    /// Bind a fresh handle to the account of `token`, then
    /// [`grant_by_password`]. Returns the handle and the grant.
    async fn bind_and_grant(state: &crate::AppState, token: &str, pw: &[u8]) -> (String, String) {
        let (_, handle) = crate::login_handle::random_handle();
        assert_eq!(put_handle(state, token, &handle).await.0, StatusCode::OK);
        let grant = grant_by_password(state, &handle, pw).await;
        (handle, grant)
    }

    /// `GET /account/export` with `token`: the cheapest bearer route, and
    /// its `identity_version` is the generation the row holds.
    async fn export(state: &crate::AppState, token: &str) -> (StatusCode, Bytes) {
        send_json(state, Method::GET, "/account/export", Some(token), None).await
    }

    async fn refresh(state: &crate::AppState, refresh_token: &str) -> (StatusCode, Bytes) {
        send_json(
            state,
            Method::POST,
            "/auth/session/refresh",
            None,
            Some(&serde_json::json!({ "refresh_token": refresh_token })),
        )
        .await
    }

    async fn recovery_login(state: &crate::AppState, handle: &str, seed: &[u8]) -> Result<(), ()> {
        login_by_handle(
            state,
            "/recovery/login/start",
            "/recovery/login/finish",
            handle,
            seed,
        )
        .await
        .map(|_| ())
    }

    /// A full password login by handle; returns the whole finish body.
    async fn password_login(
        state: &crate::AppState,
        handle: &str,
        pw: &[u8],
    ) -> Result<serde_json::Value, ()> {
        opaque_login(state, "/login/start", "/login/finish", handle, pw).await
    }

    /// A full OPAQUE login by handle on the given route pair; returns the
    /// whole finish body.
    async fn opaque_login(
        state: &crate::AppState,
        start_uri: &str,
        finish_uri: &str,
        handle: &str,
        pw: &[u8],
    ) -> Result<serde_json::Value, ()> {
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, pw).unwrap();
        let (st, _, body) = post(
            state,
            start_uri,
            "198.51.100.42",
            &serde_json::json!({
                "handle": handle,
                "request": B64.encode(cl.message.serialize()),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let response = opaque_ke::CredentialResponse::<LakkoCipherSuite>::deserialize(
            &B64.decode(v["response"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        let fin = cl
            .state
            .finish(
                pw,
                response,
                opaque_ke::ClientLoginFinishParameters::default(),
            )
            .map_err(|_| ())?;
        let (st, _, body) = post(
            state,
            finish_uri,
            "198.51.100.42",
            &serde_json::json!({
                "session_id": v["session_id"],
                "finalize": B64.encode(fin.message.serialize()),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        Ok(serde_json::from_slice(&body).unwrap())
    }

    /// A full password reset by handle with the real OPAQUE client. `Err`
    /// when the client cannot finish the recovery proof (wrong seed).
    async fn reset_password(
        state: &crate::AppState,
        handle: &str,
        seed: &[u8],
        new_pw: &[u8],
    ) -> Result<(), ()> {
        let ip = "198.51.100.43";
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, seed).unwrap();
        let (cs_pw, req_pw) = reg_start(new_pw);
        let (st, _, body) = post(
            state,
            "/password/reset/start",
            ip,
            &serde_json::json!({
                "handle": handle,
                "recovery_request": B64.encode(cl.message.serialize()),
                "new_password_request": req_pw,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let response = opaque_ke::CredentialResponse::<LakkoCipherSuite>::deserialize(
            &B64.decode(v["recovery_response"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        let fin = cl
            .state
            .finish(
                seed,
                response,
                opaque_ke::ClientLoginFinishParameters::default(),
            )
            .map_err(|_| ())?;
        let upload = reg_finish(cs_pw, new_pw, v["new_password_response"].as_str().unwrap());
        let (st, _, body) = post(
            state,
            "/password/reset/finish",
            ip,
            &serde_json::json!({
                "session_id": v["session_id"],
                "recovery_finalize": B64.encode(fin.message.serialize()),
                "new_password_upload": upload,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["refresh_token"].as_str().is_some());
        assert!(
            v.get("fresh_grant").is_none(),
            "a password reset proves the phrase, not the password: no fresh-auth grant"
        );
        Ok(())
    }

    fn pair_of(v: &serde_json::Value) -> (String, String, u64) {
        (
            v["session_token"].as_str().unwrap().to_string(),
            v["refresh_token"].as_str().unwrap().to_string(),
            v["key_generation"].as_u64().unwrap(),
        )
    }

    /// Every login route lands a v3 pair at the account's generation; a
    /// reset bumps the generation, mints an attestation that verifies under
    /// the published attest key and binds the account, the journal and the
    /// new key, and from then on every credential of the old generation is
    /// refused on a bearer route and on refresh, while the pair the reset
    /// returned and the pairs refreshed from it work.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn a_key_reset_bumps_the_generation_and_signs_every_other_session_out() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let pw = b"the password";
        let old_seed = [0x51u8; 64];
        let new_seed = [0x52u8; 64];
        let new_pubkey = pk(0x5A);
        let (a, a_token) = signup_account(&state, pw, &old_seed).await;
        let (_, handle) = crate::login_handle::random_handle();
        assert_eq!(
            put_handle(&state, &a_token, &handle).await.0,
            StatusCode::OK
        );
        assert!(a_token.starts_with("v3."), "{a_token}");

        // "Another device": a password login, whose pair is at generation 1.
        let other = password_login(&state, &handle, pw).await.unwrap();
        let (other_access, other_refresh, other_gen) = pair_of(&other);
        assert_eq!(other_gen, 1);
        assert!(other_refresh.starts_with("r2."));
        assert_eq!(export(&state, &other_access).await.0, StatusCode::OK);
        let (st, body) = refresh(&state, &other_refresh).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (other_access2, _, gen2) = pair_of(&serde_json::from_slice(&body).unwrap());
        assert_eq!(gen2, 1);
        assert_eq!(export(&state, &other_access2).await.0, StatusCode::OK);

        // The reset, from the first device, with the password proven anew.
        let grant = grant_by_password(&state, &handle, pw).await;
        // ADR-0074: before the reset the genesis mint recorded the old key.
        let old_pubkey = pk(0x59);
        let genesis = |token: String, key: [u8; 32]| {
            let state = state.clone();
            async move {
                send_json(
                    &state,
                    Method::POST,
                    "/auth/genesis/attest",
                    Some(&token),
                    Some(&serde_json::json!({ "pubkey_b64": URL_SAFE_NO_PAD.encode(key) })),
                )
                .await
                .0
            }
        };
        assert_eq!(genesis(a_token.clone(), old_pubkey).await, StatusCode::OK);
        assert_eq!(
            genesis(a_token.clone(), new_pubkey).await,
            StatusCode::CONFLICT
        );

        let (st, body) = reset_keys(&state, &a_token, Some(&grant), &new_seed, &new_pubkey).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let (new_access, new_refresh, new_gen) = pair_of(&answer);
        assert_eq!(new_gen, 2);
        // The reset is the one writer that moves the recorded identity key:
        // the mint now attests the new key and refuses the old one.
        assert_eq!(
            crate::devices::identity_pubkey_of(&pool, &a).await.unwrap(),
            Some(new_pubkey)
        );
        assert_eq!(
            genesis(new_access.clone(), new_pubkey).await,
            StatusCode::OK
        );
        assert_eq!(
            genesis(new_access.clone(), old_pubkey).await,
            StatusCode::CONFLICT
        );
        // The caller's new pair names the device its bearer named (none, in
        // this test: the signup carried no `device_id_b64`).
        assert_eq!(
            state
                .session_token_secret
                .verify(&new_access, crate::session::now_ms())
                .unwrap()
                .device_id,
            [0u8; 32]
        );
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(2)
        );
        let attest = URL_SAFE_NO_PAD
            .decode(answer["key_reset_attest_b64"].as_str().unwrap())
            .unwrap();
        let opened = lakko_attest::verify_key_reset_binding(
            &attest,
            &state.attest_key.verifying,
            &lakko_attest::key_reset_account_ref(&a),
            &new_pubkey,
        )
        .expect("the attestation verifies under the attest key and binds the reset");
        assert_eq!(opened.key_generation, 2);
        let now = crate::session::now_ms();
        assert!(
            (now - opened.signed_at_unix_millis).abs() < 60_000,
            "signed_at is the mint time"
        );
        assert!(lakko_attest::verify_key_reset_binding(
            &attest,
            &state.attest_key.verifying,
            &lakko_attest::key_reset_account_ref(&a),
            &[0u8; 32],
        )
        .is_err());

        // Every credential of generation 1 is refused with the superseded
        // code: the other device's two access tokens, the resetting
        // device's own old token, and the other device's refresh
        // credential.
        for old in [&other_access, &other_access2, &a_token] {
            let (st, body) = export(&state, old).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED);
            assert_eq!(err_code(&body), "key-generation-superseded");
        }
        let (st, body) = refresh(&state, &other_refresh).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(err_code(&body), "key-generation-superseded");

        // The pair the reset returned works, and refreshes to a pair that
        // works, at generation 2.
        let (st, body) = export(&state, &new_access).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let exported: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(exported["identity_version"], 2);
        let (st, body) = refresh(&state, &new_refresh).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (refreshed_access, refreshed_refresh, refreshed_gen) =
            pair_of(&serde_json::from_slice(&body).unwrap());
        assert_eq!(refreshed_gen, 2);
        assert_eq!(export(&state, &refreshed_access).await.0, StatusCode::OK);
        assert_eq!(refresh(&state, &refreshed_refresh).await.0, StatusCode::OK);

        // A refresh credential that is not one: garbage, an access token,
        // and one of a deleted account, all `refresh-invalid`.
        for bad in [
            "r1.nope",
            new_access.as_str(),
            "v1.AAAAAAAAAAAAAAAAAAAAAA.1.AAAA",
        ] {
            let (st, body) = refresh(&state, bad).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED, "{bad}");
            assert_eq!(err_code(&body), "refresh-invalid", "{bad}");
        }
        let (b, b_token) = signup_account(&state, b"b", &[0x53u8; 64]).await;
        let (st, body) = send_json(
            &state,
            Method::POST,
            "/auth/session/refresh",
            None,
            Some(&serde_json::json!({ "refresh_token": "x" })),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(err_code(&body), "refresh-invalid");
        let b_refresh = state
            .session_token_secret
            .issue_refresh(&b, 1, &[0u8; 32], 0, crate::session::now_ms() + 60_000)
            .unwrap();
        drop_accounts(&pool, &[b]).await;
        let (st, body) = refresh(&state, &b_refresh).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(
            err_code(&body),
            "refresh-invalid",
            "a deleted account is indistinguishable from a bad credential"
        );
        assert_eq!(
            export(&state, &b_token).await.0,
            StatusCode::UNAUTHORIZED,
            "a bearer of a deleted account is the plain 401"
        );

        drop_accounts(&pool, &[a]).await;
    }

    /// A v1 token (the retired four-part shape) is refused everywhere a
    /// bearer is read, and a refresh credential is not a bearer.
    #[tokio::test]
    async fn v1_tokens_and_refresh_credentials_are_not_bearers() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let (a, a_token) = signup_account(&state, b"pw", &[0x54u8; 64]).await;
        let id_b64 = URL_SAFE_NO_PAD.encode(a);
        let exp = crate::session::now_ms() + 60_000;
        // The retired shapes: a four-part `v1` string and a five-part `v2`
        // one whose tag is the tag `v2` would have carried. Both are
        // malformed to the verifier by their segment count alone.
        let v1 = format!("v1.{id_b64}.{exp}.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let v2 = {
            use hmac::Mac as _;
            let message = format!("v2.{id_b64}.1.{exp}");
            let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&[7u8; 64]).unwrap();
            mac.update(message.as_bytes());
            format!(
                "{message}.{}",
                URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
            )
        };
        let refresh_as_bearer = state
            .session_token_secret
            .issue_refresh(&a, 1, &[0u8; 32], 0, exp)
            .unwrap();
        for bad in [v1.as_str(), v2.as_str(), refresh_as_bearer.as_str()] {
            let (st, body) = export(&state, bad).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED, "{bad}");
            assert!(body.is_empty(), "the plain 401 carries no body: {bad}");
        }
        assert_eq!(export(&state, &a_token).await.0, StatusCode::OK);
        drop_accounts(&pool, &[a]).await;
    }

    /// After a reset the new seed logs in through `/recovery/login` and
    /// resets the password; the old seed does neither, and a recovery login
    /// it had in flight cannot finish. The password envelope is untouched.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn a_key_reset_moves_recovery_to_the_new_seed_and_keeps_the_password() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let pw = b"the password before any reset";
        let old_seed = [0x61u8; 64];
        let new_seed = [0x62u8; 64];
        let (a, a_token) = signup_account(&state, pw, &old_seed).await;
        let (_, h) = crate::login_handle::random_handle();
        assert_eq!(put_handle(&state, &a_token, &h).await.0, StatusCode::OK);
        assert_eq!(recovery_login(&state, &h, &old_seed).await, Ok(()));
        assert_eq!(recovery_login(&state, &h, &new_seed).await, Err(()));

        // A recovery login the old seed has in flight when the reset lands.
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, &old_seed).unwrap();
        let (st, _, body) = post(
            &state,
            "/recovery/login/start",
            "198.51.100.44",
            &serde_json::json!({
                "handle": h, "request": B64.encode(cl.message.serialize()),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let in_flight: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let fin = cl
            .state
            .finish(
                &old_seed,
                opaque_ke::CredentialResponse::<LakkoCipherSuite>::deserialize(
                    &B64.decode(in_flight["response"].as_str().unwrap()).unwrap(),
                )
                .unwrap(),
                opaque_ke::ClientLoginFinishParameters::default(),
            )
            .unwrap();

        let grant = grant_by_password(&state, &h, pw).await;
        let (st, body) = reset_keys(&state, &a_token, Some(&grant), &new_seed, &pk(0x6A)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        let (st, _, _) = post(
            &state,
            "/recovery/login/finish",
            "198.51.100.44",
            &serde_json::json!({
                "session_id": in_flight["session_id"],
                "finalize": B64.encode(fin.message.serialize()),
            }),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "the old seed's in-flight login was dropped by the reset"
        );

        assert_eq!(recovery_login(&state, &h, &new_seed).await, Ok(()));
        assert_eq!(
            recovery_login(&state, &h, &old_seed).await,
            Err(()),
            "the old seed no longer opens the recovery envelope"
        );
        // The password envelope was not touched; its login lands at the new
        // generation.
        let logged_in = password_login(&state, &h, pw).await.unwrap();
        assert_eq!(logged_in["key_generation"], 2);
        let new_pw = b"the password after the reset";
        assert_eq!(
            reset_password(&state, &h, &old_seed, b"never installed").await,
            Err(()),
            "the old seed cannot reset the password"
        );
        assert_eq!(reset_password(&state, &h, &new_seed, new_pw).await, Ok(()));
        assert!(password_login(&state, &h, new_pw).await.is_ok());
        assert_eq!(password_login(&state, &h, pw).await, Err(()));
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM opaque_registrations WHERE account_id = ? AND kind = ?",
        )
        .bind(a.as_slice())
        .bind(KIND_RECOVERY)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 1, "replaced in place, never a second envelope");

        drop_accounts(&pool, &[a]).await;
    }

    /// Rows of every table a reset must clear, for `id`.
    async fn seed_rows(pool: &MySqlPool, id: [u8; 16]) {
        let mut credential_id = b"key-reset-test-".to_vec();
        credential_id.extend_from_slice(&id);
        sqlx::query(
            "INSERT INTO webauthn_credentials (credential_id, account_id, passkey_bytes) \
             VALUES (?, ?, ?)",
        )
        .bind(&credential_id)
        .bind(id.as_slice())
        .bind(&b"opaque"[..])
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO passkey_vaults (account_id, credential_id, body_enc) VALUES (?, ?, ?)",
        )
        .bind(id.as_slice())
        .bind(&credential_id)
        .bind(&[0u8; 44][..])
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO webauthn_states (state_id, account_id, kind, state_bytes, expires_at) \
             VALUES (?, ?, 1, ?, NOW(3) + INTERVAL 300 SECOND)",
        )
        .bind(id.as_slice())
        .bind(id.as_slice())
        .bind(&b"state"[..])
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO webauthn_action_grants (account_id, action_hash, expires_at) \
             VALUES (?, ?, NOW(3) + INTERVAL 300 SECOND)",
        )
        .bind(id.as_slice())
        .bind(&[7u8; 32][..])
        .execute(pool)
        .await
        .unwrap();
        // `grant_hash` is unique across accounts: derive it from the id.
        let mut grant_hash = [8u8; 32];
        grant_hash[..16].copy_from_slice(&id);
        sqlx::query(
            "INSERT INTO device_grants (account_id, grant_hash, expires_at) \
             VALUES (?, ?, NOW(3) + INTERVAL 300 SECOND)",
        )
        .bind(id.as_slice())
        .bind(&grant_hash[..])
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO device_requests (account_id, request_id, expires_at) \
             VALUES (?, ?, NOW(3) + INTERVAL 300 SECOND)",
        )
        .bind(id.as_slice())
        .bind(id.as_slice())
        .execute(pool)
        .await
        .unwrap();
    }
    /// Row counts of those tables for `id`, in `seed_rows` order.
    async fn counts(pool: &MySqlPool, id: [u8; 16]) -> Vec<i64> {
        let mut out = Vec::new();
        for q in [
            "SELECT COUNT(*) FROM webauthn_credentials WHERE account_id = ?",
            "SELECT COUNT(*) FROM passkey_vaults WHERE account_id = ?",
            "SELECT COUNT(*) FROM webauthn_states WHERE account_id = ?",
            "SELECT COUNT(*) FROM webauthn_action_grants WHERE account_id = ?",
            "SELECT COUNT(*) FROM device_grants WHERE account_id = ?",
            "SELECT COUNT(*) FROM device_requests WHERE account_id = ?",
        ] {
            let n: i64 = sqlx::query_scalar(q)
                .bind(id.as_slice())
                .fetch_one(pool)
                .await
                .unwrap();
            out.push(n);
        }
        out
    }
    /// The reset deletes the account's passkeys, their vault blobs, every
    /// in-flight WebAuthn state and action grant, and its device grants and
    /// requests, with the session and a fresh-auth grant (no passkey action
    /// grant); another account's rows stay.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn a_key_reset_deletes_passkeys_vaults_grants_and_requests() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let (a, a_token) = signup_account(&state, b"a", &[0x71u8; 64]).await;
        let (b, b_token) = signup_account(&state, b"b", &[0x72u8; 64]).await;

        seed_rows(&pool, a).await;
        seed_rows(&pool, b).await;
        assert_eq!(counts(&pool, a).await, vec![1; 6]);

        let (_, grant) = bind_and_grant(&state, &a_token, b"a").await;
        let (st, body) = reset_keys(&state, &a_token, Some(&grant), &[0x73u8; 64], &pk(0x7A)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            counts(&pool, a).await,
            vec![0; 6],
            "every row of A's old generation is gone"
        );
        assert_eq!(counts(&pool, b).await, vec![1; 6], "B's rows are untouched");
        // The password and the TOTP enrollment are kept, by decision; the
        // password's survival is asserted by the login in the sibling test.
        assert_eq!(
            crate::session::key_generation_of(&pool, &b).await.unwrap(),
            Some(1)
        );
        assert_eq!(export(&state, &b_token).await.0, StatusCode::OK);

        drop_accounts(&pool, &[a, b]).await;
    }

    /// No session, no reset; a session resets its OWN account whatever the
    /// body claims; a malformed body is a 400 and changes nothing.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn a_key_reset_needs_a_session_and_acts_on_its_own_account() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let a_seed = [0x81u8; 64];
        let b_seed = [0x82u8; 64];
        let b_new = [0x83u8; 64];
        let (a, a_token) = signup_account(&state, b"a's password", &a_seed).await;
        let (b, b_token) = signup_account(&state, b"b's password", &b_seed).await;
        let (_, ha) = crate::login_handle::random_handle();
        let (_, hb) = crate::login_handle::random_handle();
        assert_eq!(put_handle(&state, &a_token, &ha).await.0, StatusCode::OK);
        assert_eq!(put_handle(&state, &b_token, &hb).await.0, StatusCode::OK);

        let (cs, req) = reg_start(&b_new);
        for bearer in [None, Some("not.a.session.token")] {
            for (uri, body) in [
                (
                    "/account/keys/reset/start",
                    serde_json::json!({ "recovery_request": req }),
                ),
                (
                    "/account/keys/reset/finish",
                    serde_json::json!({ "recovery_upload": "AAAA", "new_signature_pubkey_b64": "AAAA" }),
                ),
            ] {
                let (st, _) = send_json(&state, Method::POST, uri, bearer, Some(&body)).await;
                assert_eq!(st, StatusCode::UNAUTHORIZED, "{uri} with {bearer:?}");
            }
        }

        // B names A in both bodies; the session decides, so B is reset.
        let b_grant = grant_by_password(&state, &hb, b"b's password").await;
        let a_grant = grant_by_password(&state, &ha, b"a's password").await;
        let a_b64 = B64.encode(a);
        let (st, body) = send_json(
            &state,
            Method::POST,
            "/account/keys/reset/start",
            Some(&b_token),
            Some(&serde_json::json!({
                "account_id": a_b64, "fresh_grant": b_grant, "recovery_request": req,
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let upload = reg_finish(cs, &b_new, v["recovery_response"].as_str().unwrap());
        let (st, _) = send_json(
            &state,
            Method::POST,
            "/account/keys/reset/finish",
            Some(&b_token),
            Some(&serde_json::json!({
                "account_id": a_b64,
                "fresh_grant": b_grant,
                "recovery_upload": upload,
                "new_signature_pubkey_b64": URL_SAFE_NO_PAD.encode(pk(0x8B)),
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(1)
        );
        assert_eq!(
            crate::session::key_generation_of(&pool, &b).await.unwrap(),
            Some(2)
        );
        assert_eq!(recovery_login(&state, &ha, &a_seed).await, Ok(()));
        assert_eq!(recovery_login(&state, &ha, &b_new).await, Err(()));
        assert_eq!(recovery_login(&state, &hb, &b_new).await, Ok(()));
        assert_eq!(recovery_login(&state, &hb, &b_seed).await, Err(()));

        // Malformed bodies are a 400 and change nothing: a bad upload, a
        // bad key.
        for body in [
            serde_json::json!({ "fresh_grant": a_grant, "recovery_upload": "AAAA", "new_signature_pubkey_b64": URL_SAFE_NO_PAD.encode([1u8; 32]) }),
            serde_json::json!({ "fresh_grant": a_grant, "recovery_upload": "AAAA", "new_signature_pubkey_b64": "AAAA" }),
        ] {
            let (st, _) = send_json(
                &state,
                Method::POST,
                "/account/keys/reset/finish",
                Some(&a_token),
                Some(&body),
            )
            .await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
        }
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut OsRng, &b_new).unwrap();
        let response = ServerRegistration::<LakkoCipherSuite>::start(
            &state.opaque_setup,
            cs.message.clone(),
            &build_credential_id(&a, KIND_RECOVERY),
        )
        .unwrap();
        let upload = reg_finish(cs.state, &b_new, &B64.encode(response.message.serialize()));
        let (st, _) = send_json(
            &state,
            Method::POST,
            "/account/keys/reset/finish",
            Some(&a_token),
            Some(&serde_json::json!({
                "fresh_grant": a_grant,
                "recovery_upload": upload,
                "new_signature_pubkey_b64": URL_SAFE_NO_PAD.encode([1u8; 31]),
            })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "a 31-byte key is refused");
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(1),
            "a refused finish bumps nothing"
        );
        assert_eq!(recovery_login(&state, &ha, &a_seed).await, Ok(()));
        assert_eq!(export(&state, &a_token).await.0, StatusCode::OK);

        drop_accounts(&pool, &[a, b]).await;
    }

    /// Both calls are counted against `RecoveryReplace` and the finish also
    /// against `KeyReset`; a refusal leaves the account as it was, another
    /// account is unaffected, a malformed call is free.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn key_resets_are_limited_per_account() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        // KeyReset admits one reset; RecoveryReplace admits one whole reset
        // (start + finish) plus one more start.
        let state = build_state(
            pool.clone(),
            Limits::defaults()
                .with_name_window(Scope::KeyReset, Window::new(1, 86_400))
                .with_name_window(Scope::RecoveryReplace, Window::new(3, 3600)),
        );
        let seed0 = [0x91u8; 64];
        let seed1 = [0x92u8; 64];
        let seed2 = [0x93u8; 64];
        let (a, a_token) = signup_account(&state, b"a's password", &seed0).await;
        let (b, b_token) = signup_account(&state, b"b's password", &seed0).await;
        let (_, ha) = crate::login_handle::random_handle();
        assert_eq!(put_handle(&state, &a_token, &ha).await.0, StatusCode::OK);

        // A malformed start does not spend either budget.
        let grant1 = grant_by_password(&state, &ha, b"a's password").await;
        let (st, _) = send_json(
            &state,
            Method::POST,
            "/account/keys/reset/start",
            Some(&a_token),
            Some(&serde_json::json!({ "fresh_grant": grant1, "recovery_request": "%%%" })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        let (st, body) = reset_keys(&state, &a_token, Some(&grant1), &seed1, &pk(0x9A)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (a_token2, _, _) = pair_of(&serde_json::from_slice(&body).unwrap());
        // A second reset needs the password proven again, at generation 2.
        let grant2 = grant_by_password(&state, &ha, b"a's password").await;

        // The second reset: start is still within RecoveryReplace (3rd
        // hit), finish is the 4th and refused there - and KeyReset would
        // refuse it too. The counters are per account and the refusal
        // leaves the credential and the generation as they were.
        let (st, body) = reset_keys(&state, &a_token2, Some(&grant2), &seed2, &pk(0x9B)).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], crate::limits::CODE_TOO_MANY_ATTEMPTS);
        assert!(v["retry_after_secs"].as_u64().unwrap() >= 1);
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(2)
        );
        assert_eq!(recovery_login(&state, &ha, &seed1).await, Ok(()));
        assert_eq!(recovery_login(&state, &ha, &seed2).await, Err(()));

        // B proves its password while the state that registered it is still
        // the one in scope (a rebuilt state has a new OPAQUE server setup).
        let (_, b_grant) = bind_and_grant(&state, &b_token, b"b's password").await;
        // With RecoveryReplace wide open, KeyReset alone refuses the finish.
        let state = build_state(
            pool.clone(),
            Limits::defaults().with_name_window(Scope::KeyReset, Window::new(1, 86_400)),
        );
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut OsRng, &seed2).unwrap();
        let response = ServerRegistration::<LakkoCipherSuite>::start(
            &state.opaque_setup,
            cs.message.clone(),
            &build_credential_id(&a, KIND_RECOVERY),
        )
        .unwrap();
        let upload = reg_finish(cs.state, &seed2, &B64.encode(response.message.serialize()));
        let (st, body) = send_json(
            &state,
            Method::POST,
            "/account/keys/reset/finish",
            Some(&a_token2),
            Some(&serde_json::json!({
                "fresh_grant": grant2,
                "recovery_upload": upload,
                "new_signature_pubkey_b64": URL_SAFE_NO_PAD.encode(pk(0x9C)),
            })),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::TOO_MANY_REQUESTS,
            "{}",
            String::from_utf8_lossy(&body)
        );
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(2)
        );

        assert_eq!(
            reset_keys(&state, &b_token, Some(&b_grant), &seed1, &pk(0x9D))
                .await
                .0,
            StatusCode::OK,
            "the counters are per account"
        );

        drop_accounts(&pool, &[a, b]).await;
    }

    /// An account with a passkey resets with its session and its password,
    /// no passkey gesture: the lost-everything path cannot assume the
    /// passkey survived, and the reset removes it.
    #[tokio::test]
    async fn a_passkey_account_resets_without_a_passkey_gesture() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let old_seed = [0xA1u8; 64];
        let new_seed = [0xA2u8; 64];
        let (a, a_token) = signup_account(&state, b"a's password", &old_seed).await;
        let (_, ha) = crate::login_handle::random_handle();
        assert_eq!(put_handle(&state, &a_token, &ha).await.0, StatusCode::OK);
        let mut credential_id = b"key-reset-passkey-test-".to_vec();
        credential_id.extend_from_slice(&a);
        sqlx::query(
            "INSERT INTO webauthn_credentials (credential_id, account_id, passkey_bytes) \
             VALUES (?, ?, ?)",
        )
        .bind(&credential_id)
        .bind(a.as_slice())
        .bind(&b"opaque"[..])
        .execute(&pool)
        .await
        .unwrap();

        let grant = grant_by_password(&state, &ha, b"a's password").await;
        let (st, body) = reset_keys(&state, &a_token, Some(&grant), &new_seed, &pk(0xAA)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(recovery_login(&state, &ha, &new_seed).await, Ok(()));
        assert_eq!(recovery_login(&state, &ha, &old_seed).await, Err(()));
        let passkeys: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_credentials WHERE account_id = ?")
                .bind(a.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(passkeys, 0);

        drop_accounts(&pool, &[a]).await;
    }

    // ------------------------------------------------------------------
    // The password or a passkey, proven just now (ADR-0073 amended
    // 2026-10-04 and 2026-10-06)
    // ------------------------------------------------------------------

    fn assert_password_required(st: StatusCode, body: &[u8], what: &str) {
        assert_eq!(st, StatusCode::FORBIDDEN, "{what}");
        assert_eq!(err_code(body), "fresh-auth-required", "{what}");
    }

    /// A session alone resets nothing. Without a grant, and with every kind
    /// of unusable grant, both calls answer `403 fresh-auth-required`; the
    /// generation, the passkeys and the recovery credential stay as they
    /// were, the session keeps working, and none of the refusals spent the
    /// owner's reset budget.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn a_key_reset_without_a_fresh_password_grant_is_refused_and_changes_nothing() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        // Budgets sized for exactly ONE whole reset: any charge by a refused
        // call would make the owner's reset at the end fail.
        let state = build_state(
            pool.clone(),
            Limits::defaults()
                .with_name_window(Scope::KeyReset, Window::new(1, 86_400))
                .with_name_window(Scope::RecoveryReplace, Window::new(2, 3600)),
        );
        let pw = b"a's password";
        let old_seed = [0xB1u8; 64];
        let new_seed = [0xB2u8; 64];
        let (a, a_token) = signup_account(&state, pw, &old_seed).await;
        let (b, b_token) = signup_account(&state, b"b's password", &[0xB3u8; 64]).await;
        seed_rows(&pool, a).await;
        let (ha, a_grant) = bind_and_grant(&state, &a_token, pw).await;
        let (_, b_grant) = bind_and_grant(&state, &b_token, b"b's password").await;

        let now = crate::session::now_ms();
        let secret = &state.session_token_secret;
        let expired = secret.issue_fresh_grant(&a, 1, now - 1).unwrap();
        let future_generation = secret.issue_fresh_grant(&a, 2, now + 60_000).unwrap();
        let foreign_secret = lakko_auth_token::TokenSecret::new(vec![9u8; 64])
            .unwrap()
            .issue_fresh_grant(&a, 1, now + 60_000)
            .unwrap();
        let access_as_grant = secret.issue(&a, 1, &[0u8; 32], 0, now + 60_000).unwrap();
        let refresh_as_grant = secret
            .issue_refresh(&a, 1, &[0u8; 32], 0, now + 60_000)
            .unwrap();
        let relabelled = format!("k1{}", a_token.strip_prefix("v3").unwrap());
        let bad: Vec<(&str, Option<&str>)> = vec![
            ("no grant", None),
            ("empty", Some("")),
            ("garbage", Some("k1.nope")),
            ("expired", Some(&expired)),
            ("another account's", Some(&b_grant)),
            (
                "a generation the account is not at",
                Some(&future_generation),
            ),
            ("tagged under another secret", Some(&foreign_secret)),
            ("an access token", Some(&access_as_grant)),
            ("the bearer itself", Some(&a_token)),
            ("a refresh credential", Some(&refresh_as_grant)),
            ("an access token relabelled k1", Some(&relabelled)),
        ];
        for (what, grant) in &bad {
            let (st, body) = reset_keys(&state, &a_token, *grant, &new_seed, &pk(0xBA)).await;
            assert_password_required(st, &body, &format!("start with {what}"));
            let (st, body) =
                reset_finish_only(&state, &a_token, *grant, &a, &new_seed, &pk(0xBA)).await;
            assert_password_required(st, &body, &format!("finish with {what}"));
        }
        // The grant is checked before the body: a session without the
        // password learns nothing else about the route.
        let (st, body) = send_json(
            &state,
            Method::POST,
            "/account/keys/reset/start",
            Some(&a_token),
            Some(&serde_json::json!({ "recovery_request": "%%%" })),
        )
        .await;
        assert_password_required(st, &body, "a malformed start without a grant");
        // A's own grant under B's session is another account's grant there.
        let (st, body) = reset_keys(&state, &b_token, Some(&a_grant), &new_seed, &pk(0xBA)).await;
        assert_password_required(st, &body, "A's grant with B's session");

        // Nothing changed.
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(1)
        );
        assert_eq!(
            crate::session::key_generation_of(&pool, &b).await.unwrap(),
            Some(1)
        );
        assert_eq!(
            counts(&pool, a).await,
            vec![1; 6],
            "passkeys and grants intact"
        );
        assert_eq!(recovery_login(&state, &ha, &old_seed).await, Ok(()));
        assert_eq!(recovery_login(&state, &ha, &new_seed).await, Err(()));
        assert_eq!(export(&state, &a_token).await.0, StatusCode::OK);

        // And nothing was charged: the owner's one reset still goes through.
        let (st, body) = reset_keys(&state, &a_token, Some(&a_grant), &new_seed, &pk(0xBA)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(2)
        );

        drop_accounts(&pool, &[a, b]).await;
    }

    /// A grant is spent by the reset it authorised: the same grant, and any
    /// other grant minted before that reset, is refused afterwards, and the
    /// pair the reset returns carries none. A new password login mints one
    /// that works.
    #[tokio::test]
    async fn a_grant_from_before_a_reset_is_refused_after_it() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let pw = b"a's password";
        let (a, a_token) = signup_account(&state, pw, &[0xC1u8; 64]).await;
        let (ha, grant) = bind_and_grant(&state, &a_token, pw).await;
        let sibling = grant_by_password(&state, &ha, pw).await;

        let (st, body) = reset_keys(&state, &a_token, Some(&grant), &[0xC2u8; 64], &pk(0xCA)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            answer.get("fresh_grant").is_none(),
            "the reset's own answer carries no grant"
        );
        let (token2, refresh2, _) = pair_of(&answer);

        for (what, old) in [("the spent grant", &grant), ("its sibling", &sibling)] {
            let (st, body) = reset_keys(&state, &token2, Some(old), &[0xC3u8; 64], &pk(0xCB)).await;
            assert_password_required(st, &body, &format!("start with {what}"));
            let (st, body) =
                reset_finish_only(&state, &token2, Some(old), &a, &[0xC3u8; 64], &pk(0xCB)).await;
            assert_password_required(st, &body, &format!("finish with {what}"));
        }
        // A refreshed session carries no grant either.
        let (st, body) = refresh(&state, &refresh2).await;
        assert_eq!(st, StatusCode::OK);
        let refreshed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(refreshed.get("fresh_grant").is_none());
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(2)
        );

        let fresh = grant_by_password(&state, &ha, pw).await;
        let (st, body) = reset_keys(&state, &token2, Some(&fresh), &[0xC3u8; 64], &pk(0xCB)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(3)
        );

        drop_accounts(&pool, &[a]).await;
    }

    /// Two finishes racing with grants of one generation: exactly one reset
    /// happens. The row lock is what makes the grant single-use.
    #[tokio::test]
    async fn two_resets_racing_on_one_generation_yield_one_reset() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let pw = b"a's password";
        let (a, a_token) = signup_account(&state, pw, &[0xD1u8; 64]).await;
        let (_, grant) = bind_and_grant(&state, &a_token, pw).await;

        let (key_a, key_b) = (pk(0xDA), pk(0xDB));
        let (first, second) = tokio::join!(
            reset_finish_only(&state, &a_token, Some(&grant), &a, &[0xD2u8; 64], &key_a),
            reset_finish_only(&state, &a_token, Some(&grant), &a, &[0xD3u8; 64], &key_b),
        );
        let mut statuses = [first.0, second.0];
        statuses.sort();
        // The loser is refused either by the grant (403: it reached the row
        // after the bump) or by the bearer check (401: its token was already
        // superseded when the middleware read the row).
        assert_eq!(statuses[0], StatusCode::OK, "{statuses:?}");
        assert!(
            statuses[1] == StatusCode::FORBIDDEN || statuses[1] == StatusCode::UNAUTHORIZED,
            "{statuses:?}"
        );
        assert_eq!(
            crate::session::key_generation_of(&pool, &a).await.unwrap(),
            Some(2),
            "one bump, not two"
        );

        drop_accounts(&pool, &[a]).await;
    }

    /// Of the OPAQUE routes the password login and the signup finish mint a
    /// grant (the signup's is asserted in `signup_account_with_grant` for
    /// every account these tests create, and used below): the
    /// recovery-phrase login's answer has none (password reset, refresh, the
    /// device redeem and the reset's own answer are asserted where those
    /// helpers run; the passkey login, the third route that mints one, in
    /// `passkey_vault.rs`), and a session landed that way cannot reset.
    #[tokio::test]
    async fn only_the_password_login_and_the_signup_mint_a_grant() {
        let Some(pool) = pool().await else {
            eprintln!("skipping key-reset e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let pw = b"a's password";
        let seed = [0xE1u8; 64];
        let (a, a_token, signup_grant) = signup_account_with_grant(&state, pw, &seed).await;
        // The signup's grant is not a session either.
        assert_eq!(
            export(&state, &signup_grant).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (_, ha) = crate::login_handle::random_handle();
        assert_eq!(put_handle(&state, &a_token, &ha).await.0, StatusCode::OK);

        let by_password = password_login(&state, &ha, pw).await.unwrap();
        let grant = by_password["fresh_grant"].as_str().unwrap();
        assert!(grant.starts_with("k1."), "{grant}");
        let expires = by_password["fresh_grant_expires_at_ms"].as_i64().unwrap();
        let ttl = expires - crate::session::now_ms();
        assert!(
            ttl > 0 && ttl <= state.fresh_grant_ttl_ms,
            "expiry is now + the configured TTL: {ttl}"
        );
        let verified = state
            .session_token_secret
            .verify_fresh_grant(grant, crate::session::now_ms())
            .unwrap();
        assert_eq!(verified.account_id, a);
        assert_eq!(verified.key_generation, 1);
        // The grant is not a session: it opens no bearer route and renews
        // nothing.
        assert_eq!(export(&state, grant).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(refresh(&state, grant).await.0, StatusCode::UNAUTHORIZED);

        let by_phrase = opaque_login(
            &state,
            "/recovery/login/start",
            "/recovery/login/finish",
            &ha,
            &seed,
        )
        .await
        .unwrap();
        assert!(by_phrase["session_token"].as_str().is_some());
        assert!(by_phrase.get("fresh_grant").is_none());
        assert!(by_phrase.get("fresh_grant_expires_at_ms").is_none());
        let phrase_token = by_phrase["session_token"].as_str().unwrap();
        let (st, body) = reset_keys(&state, phrase_token, None, &[0xE2u8; 64], &pk(0xEA)).await;
        assert_password_required(st, &body, "a recovery-login session");

        // A wrong password finishes nowhere, so mints nothing.
        assert_eq!(password_login(&state, &ha, b"wrong").await, Err(()));

        drop_accounts(&pool, &[a]).await;
    }

    // ------------------------------------------------------------------
    // The fresh-auth grant for delete account and add a device (ADR-0073
    // amended 2026-10-06)
    // ------------------------------------------------------------------

    /// `DELETE /account` with `token` and, when given, the fresh-auth grant
    /// in its `x-lakko-fresh-grant` header.
    async fn delete_account(
        state: &crate::AppState,
        token: &str,
        grant: Option<&str>,
    ) -> (StatusCode, Bytes) {
        let mut req = Request::builder()
            .method(Method::DELETE)
            .uri("/account")
            .header("x-real-ip", "198.51.100.40")
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(g) = grant {
            req = req.header(crate::session::FRESH_GRANT_HEADER, g);
        }
        let resp = crate::build_app(state.clone())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .expect("router oneshot");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, bytes)
    }

    /// Every kind of unusable grant for account `a` (at generation 1) under
    /// the session `a_token`; `other` is a live grant of another account.
    fn unusable_grants(
        state: &crate::AppState,
        a: &[u8; 16],
        a_token: &str,
        other: &str,
    ) -> Vec<(&'static str, Option<String>)> {
        let now = crate::session::now_ms();
        let secret = &state.session_token_secret;
        vec![
            ("no grant", None),
            ("empty", Some(String::new())),
            ("garbage", Some("k1.nope".into())),
            (
                "expired",
                Some(secret.issue_fresh_grant(a, 1, now - 1).unwrap()),
            ),
            ("another account's", Some(other.to_string())),
            (
                "a generation the account is not at",
                Some(secret.issue_fresh_grant(a, 2, now + 60_000).unwrap()),
            ),
            (
                "tagged under another secret",
                Some(
                    lakko_auth_token::TokenSecret::new(vec![9u8; 64])
                        .unwrap()
                        .issue_fresh_grant(a, 1, now + 60_000)
                        .unwrap(),
                ),
            ),
            (
                "an access token",
                Some(secret.issue(a, 1, &[0u8; 32], 0, now + 60_000).unwrap()),
            ),
            ("the bearer itself", Some(a_token.to_string())),
            (
                "a refresh credential",
                Some(
                    secret
                        .issue_refresh(a, 1, &[0u8; 32], 0, now + 60_000)
                        .unwrap(),
                ),
            ),
        ]
    }

    async fn account_exists(pool: &MySqlPool, id: &[u8; 16]) -> bool {
        crate::session::key_generation_of(pool, id)
            .await
            .unwrap()
            .is_some()
    }

    // ------------------------------------------------------------------
    // The fresh-auth grant for adding a passkey (ADR-0073 amended
    // 2026-10-06, second part)
    // ------------------------------------------------------------------

    const PASSKEY_RP_ID: &str = "lakko.app";

    fn passkey_origin() -> url::Url {
        url::Url::parse("https://lakko.app").expect("static origin")
    }

    /// [`build_state`] with the passkey routes configured.
    fn build_state_with_passkeys(pool: MySqlPool) -> crate::AppState {
        let mut state = build_state(pool, Limits::defaults());
        state.webauthn = Some(Arc::new(
            webauthn_rs::prelude::WebauthnBuilder::new(PASSKEY_RP_ID, &passkey_origin())
                .expect("builder")
                .rp_name("Lakko")
                .build()
                .expect("build"),
        ));
        state
    }

    type SoftAuthenticator = webauthn_authenticator_rs::WebauthnAuthenticator<
        webauthn_authenticator_rs::softpasskey::SoftPasskey,
    >;

    fn soft_authenticator() -> SoftAuthenticator {
        webauthn_authenticator_rs::WebauthnAuthenticator::new(
            webauthn_authenticator_rs::softpasskey::SoftPasskey::new(true),
        )
    }

    /// `POST /webauthn/register/start` with, when given, `fresh_grant`.
    async fn passkey_register_start(
        state: &crate::AppState,
        token: &str,
        grant: Option<&str>,
    ) -> (StatusCode, Bytes) {
        let mut body = serde_json::json!({ "user_name": "tester", "nickname": "soft" });
        if let Some(g) = grant {
            body["fresh_grant"] = serde_json::json!(g);
        }
        send_json(
            state,
            Method::POST,
            "/webauthn/register/start",
            Some(token),
            Some(&body),
        )
        .await
    }

    /// `POST /webauthn/register/finish` for `state_id` + `credential` with,
    /// when given, `fresh_grant`.
    async fn passkey_register_finish(
        state: &crate::AppState,
        token: &str,
        grant: Option<&str>,
        state_id: &serde_json::Value,
        credential: &serde_json::Value,
    ) -> (StatusCode, Bytes) {
        let mut body = serde_json::json!({
            "state_id": state_id, "nickname": "soft", "credential": credential,
        });
        if let Some(g) = grant {
            body["fresh_grant"] = serde_json::json!(g);
        }
        send_json(
            state,
            Method::POST,
            "/webauthn/register/finish",
            Some(token),
            Some(&body),
        )
        .await
    }

    /// A start with `grant` (must be accepted) and the software
    /// authenticator's answer to it: the state id and the credential JSON a
    /// finish takes.
    async fn passkey_ceremony(
        state: &crate::AppState,
        token: &str,
        grant: &str,
        wa: &mut SoftAuthenticator,
    ) -> (serde_json::Value, serde_json::Value) {
        let (st, body) = passkey_register_start(state, token, Some(grant)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let ccr: webauthn_rs::prelude::CreationChallengeResponse =
            serde_json::from_value(v["options"].clone()).expect("parse options");
        let rpkc = wa
            .do_registration(passkey_origin(), ccr)
            .expect("soft register");
        (v["state_id"].clone(), serde_json::to_value(&rpkc).unwrap())
    }

    /// The whole registration with `grant` on both calls; returns the new
    /// credential id (URL-safe base64).
    async fn add_passkey(
        state: &crate::AppState,
        token: &str,
        grant: &str,
        wa: &mut SoftAuthenticator,
    ) -> String {
        let (state_id, credential) = passkey_ceremony(state, token, grant, wa).await;
        let (st, body) =
            passkey_register_finish(state, token, Some(grant), &state_id, &credential).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["credential_id_b64"].as_str().unwrap().to_string()
    }

    /// `(passkeys, pending registration states)` of the account.
    async fn passkey_rows(pool: &MySqlPool, id: &[u8; 16]) -> (i64, i64) {
        let creds =
            sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_credentials WHERE account_id = ?")
                .bind(&id[..])
                .fetch_one(pool)
                .await
                .unwrap();
        let states =
            sqlx::query_scalar("SELECT COUNT(*) FROM webauthn_states WHERE account_id = ?")
                .bind(&id[..])
                .fetch_one(pool)
                .await
                .unwrap();
        (creds, states)
    }

    /// One passkey sign-in over the discoverable route with the credential
    /// `credential_id_b64` of `account`, the test standing in for a
    /// resident-key store; returns the finish answer.
    async fn passkey_sign_in(
        state: &crate::AppState,
        account: &[u8; 16],
        credential_id_b64: &str,
        wa: &mut SoftAuthenticator,
    ) -> serde_json::Value {
        let (st, body) = send_json(
            state,
            Method::POST,
            "/webauthn/auth/start",
            None,
            Some(&serde_json::json!({})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let mut options = v["options"].clone();
        options["publicKey"]["allowCredentials"] = serde_json::json!([{
            "type": "public-key", "id": credential_id_b64,
        }]);
        let rcr: webauthn_rs::prelude::RequestChallengeResponse =
            serde_json::from_value(options).expect("parse options");
        let pkc = wa
            .do_authentication(passkey_origin(), rcr)
            .expect("soft assertion");
        let mut credential = serde_json::to_value(&pkc).unwrap();
        credential["response"]["userHandle"] = serde_json::json!(URL_SAFE_NO_PAD.encode(account));
        let (st, body) = send_json(
            state,
            Method::POST,
            "/webauthn/auth/finish",
            None,
            Some(&serde_json::json!({ "state_id": v["state_id"], "credential": credential })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }

    /// A session alone adds no passkey (a passkey sign-in mints a grant, so
    /// it would otherwise mint itself the proof for everything else):
    /// without a grant and with every kind of unusable grant both
    /// `/webauthn/register/start` and `/finish` answer `403
    /// fresh-auth-required`, the start stores no state and the finish
    /// stores no credential and leaves the pending state in place. A
    /// signup-minted, a password-minted and a passkey-minted grant each add
    /// one; a grant is not spent by it.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn adding_a_passkey_needs_a_fresh_grant() {
        let Some(pool) = pool().await else {
            eprintln!("skipping passkey-grant e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state_with_passkeys(pool.clone());
        let pw = b"a's password";
        let (a, a_token, signup_grant) = signup_account_with_grant(&state, pw, &[0xD1u8; 64]).await;
        let (b, b_token, b_grant) =
            signup_account_with_grant(&state, b"b's password", &[0xD2u8; 64]).await;

        // Start: refused, and nothing stored.
        for (what, grant) in unusable_grants(&state, &a, &a_token, &b_grant) {
            let (st, body) = passkey_register_start(&state, &a_token, grant.as_deref()).await;
            assert_password_required(st, &body, what);
        }
        // Another account's session with A's grant.
        let (st, body) = passkey_register_start(&state, &b_token, Some(&signup_grant)).await;
        assert_password_required(st, &body, "another account's session");
        assert_eq!(passkey_rows(&pool, &a).await, (0, 0));
        assert_eq!(passkey_rows(&pool, &b).await, (0, 0));
        // The gate comes before input validation: a bad body with no grant
        // is the same 403, not a 400.
        let (st, body) = send_json(
            &state,
            Method::POST,
            "/webauthn/register/start",
            Some(&a_token),
            Some(&serde_json::json!({ "user_name": "", "nickname": "" })),
        )
        .await;
        assert_password_required(st, &body, "an invalid start without a grant");

        // Finish: a ceremony opened with a good grant is not finished
        // without one.
        let mut wa1 = soft_authenticator();
        let (state_id, credential) =
            passkey_ceremony(&state, &a_token, &signup_grant, &mut wa1).await;
        assert_eq!(passkey_rows(&pool, &a).await, (0, 1));
        for (what, grant) in unusable_grants(&state, &a, &a_token, &b_grant) {
            let (st, body) =
                passkey_register_finish(&state, &a_token, grant.as_deref(), &state_id, &credential)
                    .await;
            assert_password_required(st, &body, what);
        }
        assert_eq!(
            passkey_rows(&pool, &a).await,
            (0, 1),
            "a refused finish stores no passkey and keeps the pending state"
        );

        // The signup-minted grant finishes it.
        let (st, body) = passkey_register_finish(
            &state,
            &a_token,
            Some(&signup_grant),
            &state_id,
            &credential,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let first = v["credential_id_b64"].as_str().unwrap().to_string();
        assert_eq!(passkey_rows(&pool, &a).await, (1, 0));

        // A password-minted grant adds another, and is not spent by it.
        let (_, password_grant) = bind_and_grant(&state, &a_token, pw).await;
        add_passkey(&state, &a_token, &password_grant, &mut soft_authenticator()).await;
        add_passkey(&state, &a_token, &password_grant, &mut soft_authenticator()).await;
        assert_eq!(passkey_rows(&pool, &a).await, (3, 0));

        // A passkey-minted grant adds another, under the session that
        // sign-in landed.
        let login = passkey_sign_in(&state, &a, &first, &mut wa1).await;
        let passkey_token = login["session_token"].as_str().unwrap();
        let passkey_grant = login["fresh_grant"].as_str().unwrap();
        add_passkey(
            &state,
            passkey_token,
            passkey_grant,
            &mut soft_authenticator(),
        )
        .await;
        assert_eq!(passkey_rows(&pool, &a).await, (4, 0));

        drop_accounts(&pool, &[a, b]).await;
    }

    /// A session alone deletes no account, password-only or not: without a
    /// grant and with every kind of unusable grant `DELETE /account` answers
    /// `403 fresh-auth-required` and the account stays. A password-minted
    /// grant deletes it; a grant from before a key reset does not.
    #[tokio::test]
    async fn deleting_an_account_needs_a_fresh_grant() {
        let Some(pool) = pool().await else {
            eprintln!("skipping fresh-grant e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let state = build_state(pool.clone(), Limits::defaults());
        let pw = b"a's password";
        let (a, a_token) = signup_account(&state, pw, &[0xF1u8; 64]).await;
        let (b, b_token) = signup_account(&state, b"b's password", &[0xF2u8; 64]).await;
        let (_, a_grant) = bind_and_grant(&state, &a_token, pw).await;
        let (hb, b_grant) = bind_and_grant(&state, &b_token, b"b's password").await;

        for (what, grant) in unusable_grants(&state, &a, &a_token, &b_grant) {
            let (st, body) = delete_account(&state, &a_token, grant.as_deref()).await;
            assert_password_required(st, &body, &format!("delete with {what}"));
        }
        let (st, body) = delete_account(&state, &b_token, Some(&a_grant)).await;
        assert_password_required(st, &body, "A's grant with B's session");
        // The old shape (a bare DELETE, or the grant in a JSON body) is not
        // a way in.
        let (st, body) = send_json(
            &state,
            Method::DELETE,
            "/account",
            Some(&a_token),
            Some(&serde_json::json!({ "fresh_grant": a_grant })),
        )
        .await;
        assert_password_required(st, &body, "the grant in a body, not the header");
        assert!(account_exists(&pool, &a).await);
        assert!(account_exists(&pool, &b).await);
        assert_eq!(export(&state, &a_token).await.0, StatusCode::OK);

        // Deleting does not spend a grant another way: the same live grant
        // that was refused nowhere above deletes the account.
        let (st, body) = delete_account(&state, &a_token, Some(&a_grant)).await;
        assert_eq!(
            st,
            StatusCode::NO_CONTENT,
            "{}",
            String::from_utf8_lossy(&body)
        );
        assert!(!account_exists(&pool, &a).await);

        // A grant minted before a key reset is refused for delete after it.
        let sibling = grant_by_password(&state, &hb, b"b's password").await;
        let (st, body) =
            reset_keys(&state, &b_token, Some(&b_grant), &[0xF3u8; 64], &pk(0xFA)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (b_token2, _, _) = pair_of(&serde_json::from_slice(&body).unwrap());
        for (what, old) in [("the spent grant", &b_grant), ("its sibling", &sibling)] {
            let (st, body) = delete_account(&state, &b_token2, Some(old)).await;
            assert_password_required(st, &body, &format!("delete after a reset with {what}"));
        }
        assert!(account_exists(&pool, &b).await);
        let fresh = grant_by_password(&state, &hb, b"b's password").await;
        let (st, _) = delete_account(&state, &b_token2, Some(&fresh)).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        assert!(!account_exists(&pool, &b).await);

        drop_accounts(&pool, &[a, b]).await;
    }

    async fn open_device_request(state: &crate::AppState, handle: &str) -> String {
        let (st, _, body) = post(
            state,
            "/device/request",
            "198.51.100.43",
            &serde_json::json!({ "handle": handle }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["request_id_b64"].as_str().unwrap().to_string()
    }

    async fn answer_request(
        state: &crate::AppState,
        token: &str,
        id: &str,
        grant: Option<&str>,
    ) -> (StatusCode, Bytes) {
        send_json(
            state,
            Method::POST,
            &format!("/device/requests/{id}/pake"),
            Some(token),
            Some(&serde_json::json!({
                "fresh_grant": grant,
                "pake_b_b64": B64.encode([0x42u8; 33]),
            })),
        )
        .await
    }

    async fn record_wrong_code(
        state: &crate::AppState,
        token: &str,
        id: &str,
        grant: Option<&str>,
    ) -> (StatusCode, Bytes) {
        send_json(
            state,
            Method::POST,
            &format!("/device/requests/{id}/result"),
            Some(token),
            Some(&serde_json::json!({ "fresh_grant": grant, "outcome": "wrong_code" })),
        )
        .await
    }

    async fn mint_device_grant(
        state: &crate::AppState,
        token: &str,
        grant: Option<&str>,
    ) -> (StatusCode, Bytes) {
        send_json(
            state,
            Method::POST,
            "/device/authorize/start",
            Some(token),
            Some(&serde_json::json!({ "fresh_grant": grant })),
        )
        .await
    }

    /// `(device grants, wrong codes logged, requests answered, requests
    /// decided)` for `id`.
    async fn device_rows(pool: &MySqlPool, id: &[u8; 16]) -> (i64, i64, i64, i64) {
        let mut out = [0i64; 4];
        for (slot, q) in out.iter_mut().zip([
            "SELECT COUNT(*) FROM device_grants WHERE account_id = ?",
            "SELECT COUNT(*) FROM device_link_wrong_codes WHERE account_id = ?",
            "SELECT COUNT(*) FROM device_requests WHERE account_id = ? AND pake_b IS NOT NULL",
            "SELECT COUNT(*) FROM device_requests WHERE account_id = ? AND outcome IS NOT NULL",
        ]) {
            *slot = sqlx::query_scalar(q)
                .bind(id.as_slice())
                .fetch_one(pool)
                .await
                .unwrap();
        }
        (out[0], out[1], out[2], out[3])
    }

    /// A session alone lets no device in. Without a grant and with every
    /// kind of unusable grant, minting a device grant, answering a device
    /// request and recording its outcome each answer `403
    /// fresh-auth-required`; nothing is written, and neither the mint
    /// counter nor the wrong-code budget is charged. A password-minted grant
    /// does all three, is not spent by them (a second device within its
    /// lifetime), and is refused once a key reset has replaced its
    /// generation.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn adding_a_device_needs_a_fresh_grant() {
        let Some(pool) = pool().await else {
            eprintln!("skipping fresh-grant e2e: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        // A mint budget of exactly two: any charge by a refused call would
        // make the owner's second mint fail. The address buckets are opened
        // wide so they are not what answers.
        let state = build_state(
            pool.clone(),
            Limits::defaults()
                .with_name_window(Scope::DeviceGrantMint, Window::new(2, 900))
                .with_name_window(Scope::DeviceRequestTarget, Window::new(100, 600))
                .with_ip_window(Category::Attempt, Window::new(10_000, 60))
                .with_ip_window(Category::DeviceRequest, Window::new(10_000, 60)),
        );
        let pw = b"a's password";
        let (a, a_token) = signup_account(&state, pw, &[0xA1u8; 64]).await;
        let (b, b_token) = signup_account(&state, b"b's password", &[0xA2u8; 64]).await;
        let (ha, a_grant) = bind_and_grant(&state, &a_token, pw).await;
        let (_, b_grant) = bind_and_grant(&state, &b_token, b"b's password").await;

        // A new device asks; the request waits for the account's device.
        let id = open_device_request(&state, &ha).await;

        for (what, grant) in unusable_grants(&state, &a, &a_token, &b_grant) {
            let g = grant.as_deref();
            let (st, body) = mint_device_grant(&state, &a_token, g).await;
            assert_password_required(st, &body, &format!("mint with {what}"));
            let (st, body) = answer_request(&state, &a_token, &id, g).await;
            assert_password_required(st, &body, &format!("answer with {what}"));
            let (st, body) = record_wrong_code(&state, &a_token, &id, g).await;
            assert_password_required(st, &body, &format!("result with {what}"));
        }
        // The mint route had no body before the grant: an empty one and one
        // that is not JSON get the same refusal.
        let (st, body) = send_json(
            &state,
            Method::POST,
            "/device/authorize/start",
            Some(&a_token),
            None,
        )
        .await;
        assert_password_required(st, &body, "mint with an empty body");
        let (st, body) = send_json(
            &state,
            Method::POST,
            "/device/authorize/start",
            Some(&a_token),
            Some(&serde_json::json!("not an object")),
        )
        .await;
        assert_password_required(st, &body, "mint with a body that is not an object");
        // The grant is checked before the body's other fields.
        let (st, body) = send_json(
            &state,
            Method::POST,
            &format!("/device/requests/{id}/pake"),
            Some(&a_token),
            Some(&serde_json::json!({ "pake_b_b64": "%%%" })),
        )
        .await;
        assert_password_required(st, &body, "a malformed answer without a grant");
        let (st, body) = mint_device_grant(&state, &b_token, Some(&a_grant)).await;
        assert_password_required(st, &body, "A's grant with B's session");
        assert_eq!(
            device_rows(&pool, &a).await,
            (0, 0, 0, 0),
            "nothing written"
        );

        // With the grant: answer, then the new device's (wrong) guess.
        let (st, body) = answer_request(&state, &a_token, &id, Some(&a_grant)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (st, _, body) = post(
            &state,
            "/device/request/payload",
            "198.51.100.43",
            &serde_json::json!({
                "request_id_b64": id,
                "pake_a_b64": B64.encode([0x41u8; 33]),
                "sealed_payload_b64": B64.encode([1u8; 64]),
                "confirm_a_b64": B64.encode([2u8; 32]),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        // The outcome is now recordable, and a session without a grant
        // still cannot burn the account's wrong-code budget.
        for grant in [None, Some(b_grant.as_str())] {
            let (st, body) = record_wrong_code(&state, &a_token, &id, grant).await;
            assert_password_required(st, &body, "a recordable wrong code without a grant");
        }
        assert_eq!(device_rows(&pool, &a).await, (0, 0, 1, 0));
        let (st, body) = record_wrong_code(&state, &a_token, &id, Some(&a_grant)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(device_rows(&pool, &a).await, (0, 1, 1, 1));

        // The same grant is not spent: a second request, and two mints (the
        // whole budget, so no refusal above was charged), then the limit.
        let id2 = open_device_request(&state, &ha).await;
        let (st, body) = answer_request(&state, &a_token, &id2, Some(&a_grant)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let mut device_grants = Vec::new();
        for n in 0..2 {
            let (st, body) = mint_device_grant(&state, &a_token, Some(&a_grant)).await;
            assert_eq!(st, StatusCode::OK, "mint {n}");
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            device_grants.push(v["grant_token_b64"].as_str().unwrap().to_string());
        }
        let (st, _) = mint_device_grant(&state, &a_token, Some(&a_grant)).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        // The device let in lands a session and no fresh-auth grant.
        let (st, _, body) = post(
            &state,
            "/device/authorize/redeem",
            "198.51.100.43",
            &serde_json::json!({ "grant_token_b64": device_grants[0] }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let redeemed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(redeemed["session_token"].as_str().is_some());
        assert!(redeemed.get("fresh_grant").is_none());
        let new_device_token = redeemed["session_token"].as_str().unwrap();
        let (st, body) = mint_device_grant(&state, new_device_token, None).await;
        assert_password_required(st, &body, "the device just let in");

        // After a key reset the grant names a generation the account left.
        let sibling = grant_by_password(&state, &ha, pw).await;
        let (st, body) =
            reset_keys(&state, &a_token, Some(&a_grant), &[0xA3u8; 64], &pk(0xAA)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (a_token2, _, _) = pair_of(&serde_json::from_slice(&body).unwrap());
        let id3 = open_device_request(&state, &ha).await;
        for (what, old) in [("the spent grant", &a_grant), ("its sibling", &sibling)] {
            let (st, body) = mint_device_grant(&state, &a_token2, Some(old)).await;
            assert_password_required(st, &body, &format!("mint after a reset with {what}"));
            let (st, body) = answer_request(&state, &a_token2, &id3, Some(old)).await;
            assert_password_required(st, &body, &format!("answer after a reset with {what}"));
        }
        let fresh = grant_by_password(&state, &ha, pw).await;
        let (st, body) = answer_request(&state, &a_token2, &id3, Some(&fresh)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

        drop_accounts(&pool, &[a, b]).await;
    }
}
