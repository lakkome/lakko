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
//!   * **`KIND_RECOVERY`** — derived from a BIP39 24-word mnemonic seed
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
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

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

/// Issue a fresh session token for `account_id`. Returns the
/// dot-formatted token + the absolute unix-ms expiry (mirror of
/// the wire shape every `*FinishResponse` now carries).
///
/// Wraps the `lakko-auth-token` crate's `TokenSecret::issue` with
/// the server-side TTL math: `now_ms + state.session_token_ttl_ms`,
/// clamped to the i64 range so a misconfigured TTL of years
/// doesn't overflow the millisecond arithmetic.
fn issue_token_for(
    state: &crate::AppState,
    account_id: &[u8; 16],
) -> Result<(String, i64), (StatusCode, String)> {
    let now_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let expires_at_ms = now_ms.saturating_add(state.session_token_ttl_ms);
    let token = state
        .session_token_secret
        .issue(account_id, expires_at_ms)
        .map_err(|e| {
            tracing::error!(error = %e, "session-token issuance failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal token error".to_string(),
            )
        })?;
    Ok((token, expires_at_ms))
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
const KIND_PASSWORD: u8 = 1;

/// OPAQUE registration kind: derived from the BIP39 mnemonic seed.
const KIND_RECOVERY: u8 = 2;

/// Flow tag for `opaque_login_states` rows created during a password
/// reset. The OPAQUE math underneath uses `KIND_RECOVERY`'s
/// credential_id (the user is proving knowledge of the recovery
/// secret), but the row tag is distinct so neither `/login/finish`
/// nor `/recovery/login/finish` can consume a state created for a
/// reset (anti confused-deputy, mirrors the existing kind guard
/// between `/login/finish` and `/recovery/login/finish`).
const KIND_PASSWORD_RESET: u8 = 3;

/// Build the OPAQUE credential_id by appending the registration kind to
/// `account_id`. Domain-separates the password and recovery flows so an
/// envelope from one flow cannot answer a challenge in the other.
fn build_credential_id(account_id: &[u8; ACCOUNT_ID_LEN], kind: u8) -> [u8; CREDENTIAL_ID_LEN] {
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
fn decode_registration_request(
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
fn decode_registration_upload(
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
fn opaque_registration_start(
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
}

#[derive(Serialize)]
pub(crate) struct SignupFinishResponse {
    pub(crate) ok: bool,
    /// Phase 1.5b: session bearer token. Same shape as the one
    /// `/login/finish` returns — the client treats signup +
    /// auto-login as one ceremony and stashes the token for
    /// follow-up writes to `services/discovery`.
    pub(crate) session_token: String,
    /// Token expiry as a unix-millisecond timestamp. The client
    /// uses this to schedule a silent re-login before the token
    /// dies.
    pub(crate) session_expires_at_ms: i64,
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

    tracing::info!(
        account_id = %B64.encode(account_id),
        with_recovery,
        "signup completed",
    );
    let (session_token, session_expires_at_ms) = issue_token_for(&state, &account_id)?;
    Ok(Json(SignupFinishResponse {
        ok: true,
        session_token,
        session_expires_at_ms,
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

/// Fetch a `ServerLogin` state by session_id and **delete the row in the
/// same transaction** (one-shot — replaying the same session_id on a second
/// `/login/finish` returns 401).
///
/// Returns `None` if session_id is unknown or expired (TTL elapsed).
async fn take_login_state(
    pool: &MySqlPool,
    session_id: Uuid,
) -> Result<Option<(ServerLogin<LakkoCipherSuite>, [u8; ACCOUNT_ID_LEN], u8)>, sqlx::Error> {
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

    sqlx::query("DELETE FROM opaque_login_states WHERE session_id = ?")
        .bind(session_id.as_bytes().as_slice())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let server_login = ServerLogin::<LakkoCipherSuite>::deserialize(&state_bytes).map_err(|e| {
        tracing::error!(error = ?e, "opaque_login_states row does not deserialize");
        sqlx::Error::Protocol("corrupted opaque_login_states row".to_string())
    })?;
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    if account_id_vec.len() != ACCOUNT_ID_LEN {
        return Ok(None);
    }
    acct.copy_from_slice(&account_id_vec);
    Ok(Some((server_login, acct, kind)))
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
}

#[derive(Serialize)]
pub(crate) struct LoginFinishResponse {
    pub(crate) ok: bool,
    /// Base64-encoded account_id (16 raw UUIDv7 bytes) — the client pins
    /// this for follow-up requests.
    pub(crate) account_id: String,
    /// Phase 1.5b: HMAC-SHA256 bearer token. The client presents
    /// this in the `Authorization: Bearer <token>` header on
    /// protected `lakko-svc-discovery` writes (register, identity
    /// upload, keypackage upload, invite upload).
    pub(crate) session_token: String,
    /// Token expiry as a unix-millisecond timestamp. Mirrors the
    /// `session_token`'s internal `expires_at_ms` field; surfaced
    /// separately so the client doesn't have to parse the token.
    pub(crate) session_expires_at_ms: i64,
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
async fn login_finish_inner(
    state: &crate::AppState,
    payload: LoginFinishRequest,
    expected_kind: u8,
) -> Result<Json<LoginFinishResponse>, (StatusCode, String)> {
    let session_id = Uuid::parse_str(&payload.session_id).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid session_id (not a UUID): {e}"),
        )
    })?;

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

    let (server_login, account_id, kind) = cached;

    if kind != expected_kind {
        // The session was started for a different flow. Treat as 401 —
        // we already deleted the row in `take_login_state`, so an attacker
        // gets one shot at most.
        tracing::warn!(
            session_kind = kind,
            expected_kind,
            account_id = %B64.encode(account_id),
            "login_finish: session kind mismatch (cross-flow attempt)",
        );
        return Err((
            StatusCode::UNAUTHORIZED,
            "session_id was started for a different flow".to_string(),
        ));
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

    tracing::info!(kind, account_id = %B64.encode(account_id), "login completed");

    let (session_token, session_expires_at_ms) = issue_token_for(state, &account_id)?;
    Ok(Json(LoginFinishResponse {
        ok: true,
        account_id: B64.encode(account_id),
        session_token,
        session_expires_at_ms,
    }))
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
pub(crate) async fn login_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginFinishRequest>,
) -> Result<Json<LoginFinishResponse>, (StatusCode, String)> {
    login_finish_inner(&state, payload, KIND_PASSWORD).await
}

// ============================================================================
// /recovery/login/{start,finish} — BIP39 mnemonic flow
// ============================================================================

/// `POST /recovery/login/start` — first step of recovery login.
///
/// The "password" input on the client side is a 64-byte seed derived
/// from a 24-word BIP39 mnemonic via PBKDF2-HMAC-SHA512 per the BIP39
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
) -> Result<Json<LoginFinishResponse>, (StatusCode, String)> {
    login_finish_inner(&state, payload, KIND_RECOVERY).await
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
}

#[derive(Serialize)]
pub(crate) struct PasswordResetFinishResponse {
    pub(crate) ok: bool,
    /// Base64-encoded `account_id` echo, same shape as
    /// [`LoginFinishResponse::account_id`].
    pub(crate) account_id: String,
    /// Phase 1.5b: bearer token for the rotated password. Same
    /// shape as [`LoginFinishResponse::session_token`]. The
    /// recovery flow issues a fresh token because the prior
    /// password (and any tokens issued against it) is now stale
    /// from the user's PoV — though server-side those tokens
    /// still verify until their TTL expires (stateless tokens
    /// don't revoke).
    pub(crate) session_token: String,
    /// Token expiry as a unix-millisecond timestamp.
    pub(crate) session_expires_at_ms: i64,
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
pub(crate) async fn password_reset_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<PasswordResetFinishRequest>,
) -> Result<Json<PasswordResetFinishResponse>, (StatusCode, String)> {
    let session_id = Uuid::parse_str(&payload.session_id).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid session_id (not a UUID): {e}"),
        )
    })?;

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

    let (server_login, account_id, kind) = cached;

    if kind != KIND_PASSWORD_RESET {
        // Caller tried to finalize a regular login session via the
        // password-reset endpoint. The row is already deleted, so an
        // attacker gets one shot at most; surface 401.
        tracing::warn!(
            session_kind = kind,
            account_id = %B64.encode(account_id),
            "password_reset_finish: session kind mismatch (cross-flow attempt)",
        );
        return Err((
            StatusCode::UNAUTHORIZED,
            "session_id was started for a different flow".to_string(),
        ));
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

    // 3. Replace the password envelope. ON DUPLICATE KEY UPDATE relies
    //    on the (account_id, kind) composite primary key; the
    //    `updated_at` column auto-bumps via the table's
    //    `ON UPDATE CURRENT_TIMESTAMP` clause.
    sqlx::query(
        "INSERT INTO opaque_registrations (account_id, kind, registration_bytes) \
         VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE registration_bytes = VALUES(registration_bytes)",
    )
    .bind(&account_id[..])
    .bind(KIND_PASSWORD)
    .bind(&new_bytes)
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "upsert opaque_registrations (password_reset)");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    tracing::info!(
        account_id = %B64.encode(account_id),
        "password reset completed (mnemonic-driven)",
    );

    let (session_token, session_expires_at_ms) = issue_token_for(&state, &account_id)?;
    Ok(Json(PasswordResetFinishResponse {
        ok: true,
        account_id: B64.encode(account_id),
        session_token,
        session_expires_at_ms,
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
        // 64-byte BIP39 seed (real clients derive this from a 24-word
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
    use http_body_util::BodyExt as _;
    use opaque_ke::{ClientLogin, ClientRegistration, ClientRegistrationFinishParameters};
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::Arc;
    use tower::ServiceExt as _;

    use crate::limits::{Category, Limits, Scope, Window};

    fn build_state(pool: MySqlPool, limits: Limits) -> crate::AppState {
        let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
        crate::AppState {
            db: pool,
            opaque_setup: Arc::new(ServerSetup::<LakkoCipherSuite>::new(&mut OsRng)),
            session_token_secret: Arc::new(token_secret),
            session_token_ttl_ms: 86_400_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
            turn: None,
            totp_enc_key: None,
            recovery_contact_pepper: None,
            vault_enc_key: None,
            otp_sender: Arc::new(crate::otp_sender::LogOnlyOtpSender),
            throttle: crate::throttle::ThrottleConfig::disabled(),
            backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
            anon_tokens: None,
            handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
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
        (id, v["session_token"].as_str().unwrap().to_string())
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
            async move {
                send_json(
                    &state,
                    Method::PUT,
                    "/auth/login-handle",
                    Some(&token),
                    Some(&serde_json::json!({ "handle": handle })),
                )
                .await
            }
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
        let (st, _) = send_json(&state, Method::DELETE, "/account", Some(&a_token), None).await;
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
        // The factor keys are set so the pepper is what refuses.
        state.totp_enc_key = Some(Arc::new([1u8; 32]));
        state.vault_enc_key = Some(Arc::new([2u8; 32]));
        state.recovery_contact_pepper = Some(Arc::new([3u8; 32]));
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
            ("/vault/recover/start", serde_json::json!({ "handle": h })),
            (
                "/vault/recover/finish",
                serde_json::json!({ "handle": h, "verifier_b64": B64.encode([0u8; 32]) }),
            ),
            (
                "/recovery-contact/recover/start",
                serde_json::json!({ "handle": h, "kind": 1, "contact": "a@example.com" }),
            ),
            (
                "/recovery-contact/recover/finish",
                serde_json::json!({ "handle": h, "kind": 1, "code": "000000" }),
            ),
        ];
        for (uri, body) in routes {
            let (st, _, bytes) = post(&state, uri, "198.51.100.46", &body).await;
            assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            assert_eq!(err_code(&bytes), "login-by-name-disabled", "{uri}");
        }
        let token = state
            .session_token_secret
            .issue(&[9u8; 16], i64::MAX / 2)
            .unwrap();
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
            .issue(&id, now_ms + 3_600_000)
            .unwrap();
        (id, token)
    }

    async fn put_handle(state: &crate::AppState, token: &str, handle: &str) -> (StatusCode, Bytes) {
        send_json(
            state,
            Method::PUT,
            "/auth/login-handle",
            Some(token),
            Some(&serde_json::json!({ "handle": handle })),
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
}
