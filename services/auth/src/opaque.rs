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
//! Registration is stateless on the server between `/start` and `/finish`.
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
    /// PoW proof obtained via `POST /pow/challenge`. Verified and
    /// consumed before any expensive work.
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
/// **The server stays stateless** between start and finish — OPAQUE math
/// binds each upload (which arrives in `/signup/finish`) to its response.
pub(crate) async fn signup_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<SignupStartRequest>,
) -> Result<Json<SignupStartResponse>, (StatusCode, String)> {
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

    Ok(Json(SignupStartResponse {
        password_response,
        recovery_response,
    }))
}

// ============================================================================
// /signup/finish
// ============================================================================

/// Registration-finish request. Includes both uploads if the caller
/// started both flows in `/signup/start`.
#[derive(Deserialize)]
pub(crate) struct SignupFinishRequest {
    /// PoW proof — separate from the one consumed by `/signup/start`.
    /// Both endpoints write to MariaDB; both need anti-spam.
    pub(crate) pow: crate::pow::PowProof,
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
}

/// `POST /signup/finish` — finalize registration.
///
/// In a single DB transaction:
///
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
) -> Result<Json<SignupFinishResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

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

    // Insert account row. PK collision means account_id is already taken.
    let account_insert = sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&account_id[..])
        .execute(&mut *tx)
        .await;

    match account_insert {
        Ok(_) => {},
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
            return Err((
                StatusCode::CONFLICT,
                "account_id already registered".to_string(),
            ));
        },
        Err(e) => {
            tracing::error!(error = %e, "insert account");
            return Err((
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
    Ok(Json(SignupFinishResponse { ok: true }))
}

// ============================================================================
// Login state — DB-backed
// ============================================================================

/// TTL for in-flight login state. The client must complete `/login/finish`
/// within this window. Mitigates memory growth and replay attacks.
/// Typed as `i64` so it can be passed directly to SQL `INTERVAL ? SECOND`
/// without a cast.
const LOGIN_STATE_TTL_SECS: i64 = 120;

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

    let server_login = ServerLogin::<LakkoCipherSuite>::deserialize(&state_bytes)
        .expect("ServerLogin deserialize failed — DB row corrupted");
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    if account_id_vec.len() != ACCOUNT_ID_LEN {
        return Ok(None);
    }
    acct.copy_from_slice(&account_id_vec);
    Ok(Some((server_login, acct, kind)))
}

/// Spawn a background task that runs every `LOGIN_STATE_PRUNE_INTERVAL` and
/// deletes expired login states from the DB.
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
        }
    })
}

// ============================================================================
// Login: shared inner flow
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct LoginStartRequest {
    /// PoW proof obtained via `POST /pow/challenge`. Required on both
    /// `/login/start` and `/recovery/login/start`. The `*/finish`
    /// endpoints don't need it — they consume an existing session_id
    /// that the server only issues after a verified PoW.
    pub(crate) pow: crate::pow::PowProof,
    pub(crate) account_id: String,
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
    /// this for follow-up requests. A real session token (JWT or DB-backed
    /// session) is a separate sub-sprint.
    pub(crate) account_id: String,
}

/// Shared `/login/start` handler parameterised by `kind`.
///
/// Looks up `opaque_registrations(account_id, kind)`. If no row is found
/// (unknown account or no recovery enrolled), passes `None` to
/// `ServerLogin::start` — opaque-ke 3.0 takes the fake-credentials path
/// and produces a plausible response that cannot be distinguished from a
/// real login with the wrong secret. This blocks both account enumeration
/// AND recovery-enrolment enumeration.
async fn login_start_inner(
    state: &crate::AppState,
    payload: LoginStartRequest,
    kind: u8,
) -> Result<Json<LoginStartResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let account_id = decode_account_id(&payload.account_id)?;
    let credential_id = build_credential_id(&account_id, kind);

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
    .map(|(bytes,)| {
        ServerRegistration::<LakkoCipherSuite>::deserialize(&bytes)
            .expect("ServerRegistration deserialize failed — DB row corrupted")
    });

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

    Ok(Json(LoginFinishResponse {
        ok: true,
        account_id: B64.encode(account_id),
    }))
}

// ============================================================================
// /login/{start,finish} — password flow
// ============================================================================

/// `POST /login/start` — first login step (password flow).
pub(crate) async fn login_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginStartRequest>,
) -> Result<Json<LoginStartResponse>, (StatusCode, String)> {
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
) -> Result<Json<LoginStartResponse>, (StatusCode, String)> {
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
//        - verifies PoW,
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
// PoW lives only on /password/reset/start (matches the rest of the
// expensive endpoints: PoW gates the start, the start consumes the
// challenge, finish runs against an already-issued session_id).

#[derive(Deserialize)]
pub(crate) struct PasswordResetStartRequest {
    pub(crate) pow: crate::pow::PowProof,
    pub(crate) account_id: String,
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
) -> Result<Json<PasswordResetStartResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let account_id = decode_account_id(&payload.account_id)?;

    // Decode both client messages up-front; surfaces wire errors as
    // 400 before we touch the DB.
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
    .map(|(bytes,)| {
        ServerRegistration::<LakkoCipherSuite>::deserialize(&bytes)
            .expect("ServerRegistration deserialize failed — DB row corrupted")
    });

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

    Ok(Json(PasswordResetFinishResponse {
        ok: true,
        account_id: B64.encode(account_id),
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
