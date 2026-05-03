// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! OPAQUE-PAKE (RFC draft `opaque`) auth flows for the auth service.
//!
//! The server never sees the plaintext password — client and server exchange
//! OPRF-blinded messages, and the server stores only an "envelope" from
//! which the password cannot be recovered without an MITM at registration
//! time (guaranteed by OPAQUE math).
//!
//! Phase 1.1b: registration (`/signup/start` + `/signup/finish`) — server
//! is stateless between start and finish.
//!
//! Phase 1.1c: login (`/login/start` + `/login/finish`) — server is
//! **stateful** between start and finish. `ServerLogin<CS>` is serialized
//! and stored in the `opaque_login_states` table keyed by session_id
//! (UUIDv7), TTL 2 minutes. A background pruner periodically deletes
//! expired rows. DB-backed (not in-memory) so the state survives service
//! restarts and works correctly under horizontal scale-out.
//!
//! The server-static state (OPRF + AKE keypairs) is generated once on
//! first start and persisted in the `opaque_server_setup` table.
//! Per-account registration material lives in `opaque_registrations`,
//! keyed by `account_id` (UUIDv7 raw bytes), with FK on `accounts(id)`.

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

/// Load the server-static OPAQUE setup from the DB; on first start,
/// generate a fresh one and persist it.
///
/// Singleton-row pattern — the `opaque_server_setup` table has a
/// CHECK(id=1) constraint, so it physically cannot hold more than one row.
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
fn decode_account_id(b64: &str) -> Result<Vec<u8>, (StatusCode, String)> {
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
    Ok(bytes)
}

// ============================================================================
// /signup/start
// ============================================================================

/// Registration-start request. The client has already generated an
/// `account_id` (typically UUIDv7) and a `RegistrationRequest` via
/// `ClientRegistration::start`.
#[derive(Deserialize)]
pub(crate) struct SignupStartRequest {
    /// Base64-encoded raw bytes of `account_id` (16 bytes, UUIDv7).
    pub(crate) account_id: String,
    /// Base64-encoded `RegistrationRequest` from the opaque-ke client.
    pub(crate) request: String,
}

/// Response to `/signup/start` — `RegistrationResponse` to be passed to
/// `ClientRegistration::finish`.
#[derive(Serialize)]
pub(crate) struct SignupStartResponse {
    /// Base64-encoded `RegistrationResponse` from the opaque-ke server.
    pub(crate) response: String,
}

/// `POST /signup/start` — first step of registration.
///
/// The server takes the client's `RegistrationRequest`, runs it through
/// `ServerRegistration::start` with the server-static setup and
/// credential_id=account_id, and returns the `RegistrationResponse`.
///
/// **The server stays stateless** between start and finish — OPAQUE math
/// binds the upload (which arrives in `/signup/finish`) to the response
/// via account_id and password, so no server-side state is required.
pub(crate) async fn signup_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<SignupStartRequest>,
) -> Result<Json<SignupStartResponse>, (StatusCode, String)> {
    let account_id_bytes = decode_account_id(&payload.account_id)?;

    let request_bytes = B64.decode(&payload.request).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid request base64: {e}"),
        )
    })?;
    let request_message = RegistrationRequest::<LakkoCipherSuite>::deserialize(&request_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid RegistrationRequest: {e:?}"),
            )
        })?;

    let server_start = ServerRegistration::<LakkoCipherSuite>::start(
        &state.opaque_setup,
        request_message,
        &account_id_bytes,
    )
    .map_err(|e| {
        tracing::error!(error = ?e, "ServerRegistration::start failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal opaque error".to_string(),
        )
    })?;

    Ok(Json(SignupStartResponse {
        response: B64.encode(server_start.message.serialize()),
    }))
}

// ============================================================================
// /signup/finish
// ============================================================================

/// Registration-finish request. The client has finished
/// `ClientRegistration` math and produced a `RegistrationUpload`.
#[derive(Deserialize)]
pub(crate) struct SignupFinishRequest {
    pub(crate) account_id: String,
    /// Base64-encoded `RegistrationUpload` (envelope + masking_key + client AKE pubkey).
    pub(crate) upload: String,
}

#[derive(Serialize)]
pub(crate) struct SignupFinishResponse {
    pub(crate) ok: bool,
}

/// `POST /signup/finish` — finalize registration.
///
/// In a single DB transaction: create an `accounts(id=account_id)` row and
/// store the serialized `ServerRegistration` in `opaque_registrations`. If
/// `account_id` is already registered, returns `409 Conflict`.
pub(crate) async fn signup_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<SignupFinishRequest>,
) -> Result<Json<SignupFinishResponse>, (StatusCode, String)> {
    let account_id_bytes = decode_account_id(&payload.account_id)?;

    let upload_bytes = B64.decode(&payload.upload).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid upload base64: {e}"),
        )
    })?;
    let upload_message = RegistrationUpload::<LakkoCipherSuite>::deserialize(&upload_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid RegistrationUpload: {e:?}"),
            )
        })?;

    let registration = ServerRegistration::<LakkoCipherSuite>::finish(upload_message);
    let registration_bytes = registration.serialize().to_vec();

    let mut tx = state.db.begin().await.map_err(|e| {
        tracing::error!(error = %e, "begin tx");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    // Insert account row. PK collision means account_id is already taken.
    let account_insert = sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&account_id_bytes)
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

    sqlx::query("INSERT INTO opaque_registrations (account_id, registration_bytes) VALUES (?, ?)")
        .bind(&account_id_bytes)
        .bind(&registration_bytes)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "insert opaque_registrations");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    tx.commit().await.map_err(|e| {
        tracing::error!(error = %e, "commit tx");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    tracing::info!(account_id = %B64.encode(&account_id_bytes), "signup completed");
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
) -> Result<Uuid, sqlx::Error> {
    let session_id = Uuid::now_v7();
    let state_bytes = state.serialize().to_vec();
    sqlx::query(
        "INSERT INTO opaque_login_states (session_id, account_id, state_bytes) \
         VALUES (?, ?, ?)",
    )
    .bind(session_id.as_bytes().as_slice())
    .bind(&account_id[..])
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
) -> Result<Option<(ServerLogin<LakkoCipherSuite>, [u8; ACCOUNT_ID_LEN])>, sqlx::Error> {
    // Atomically: fetch the row (with TTL check) and delete it.
    // MySQL/MariaDB has no `DELETE ... RETURNING`, so we use a transaction:
    //   1. SELECT ... FOR UPDATE
    //   2. DELETE
    let mut tx = pool.begin().await?;

    let row: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT state_bytes, account_id FROM opaque_login_states \
         WHERE session_id = ? \
           AND created_at >= NOW(3) - INTERVAL ? SECOND \
         FOR UPDATE",
    )
    .bind(session_id.as_bytes().as_slice())
    .bind(LOGIN_STATE_TTL_SECS)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((state_bytes, account_id_vec)) = row else {
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
    Ok(Some((server_login, acct)))
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
// /login/start
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct LoginStartRequest {
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

/// `POST /login/start` — first login step.
///
/// The server takes the client's `CredentialRequest`, looks up the
/// corresponding `ServerRegistration` in `opaque_registrations`, runs
/// `ServerLogin::start`, and returns `CredentialResponse` plus a session_id.
/// The state is stored in DB so any instance can answer `/login/finish`.
///
/// If the account is unknown, we still go through the motions
/// (`ServerLogin::start` with `password_file=None` produces a
/// cryptographically plausible response — the "fake credentials" path).
/// This prevents a username-enumeration timing channel.
pub(crate) async fn login_start(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginStartRequest>,
) -> Result<Json<LoginStartResponse>, (StatusCode, String)> {
    let account_id_bytes = decode_account_id(&payload.account_id)?;

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

    // Fetch stored ServerRegistration. If no row exists (unknown account),
    // pass `None` to ServerLogin::start — opaque-ke 3.0 takes the
    // fake-credentials path and produces a plausible response that can't
    // be distinguished from a real login with the wrong password.
    let stored: Option<ServerRegistration<LakkoCipherSuite>> = sqlx::query_as::<_, (Vec<u8>,)>(
        "SELECT registration_bytes FROM opaque_registrations WHERE account_id = ?",
    )
    .bind(&account_id_bytes[..])
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
        &account_id_bytes,
        ServerLoginStartParameters::default(),
    )
    .map_err(|e| {
        tracing::error!(error = ?e, "ServerLogin::start failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal opaque error".to_string(),
        )
    })?;

    // Convert Vec<u8> to [u8; 16] for DB (length validated above).
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    acct.copy_from_slice(&account_id_bytes);

    let session_id = insert_login_state(&state.db, &server_start.state, &acct)
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

// ============================================================================
// /login/finish
// ============================================================================

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
    /// session) is a separate sub-sprint after 1.1c.
    pub(crate) account_id: String,
}

/// `POST /login/finish` — finalize login.
///
/// Takes `CredentialFinalization` from the client and runs
/// `ServerLogin::finish`. If the math checks out, the password is correct,
/// and both sides have derived the same session_key. We return account_id;
/// proper session-token machinery (JWT signed by the auth-service key, or
/// a DB-backed `sessions` table) is a separate sub-sprint.
///
/// If session_id is unknown or expired (TTL 2 minutes), returns 401.
/// If `CredentialFinalization` doesn't validate, returns 401 (wrong password).
pub(crate) async fn login_finish(
    State(state): State<crate::AppState>,
    Json(payload): Json<LoginFinishRequest>,
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

    let (server_login, account_id) = cached;

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

    // ServerLogin::finish validates the client's AKE proof. Wrong password
    // means finish returns an error.
    let _result = server_login.finish(finalize_message).map_err(|e| {
        // Internal-only log — we never leak "user exists but wrong password"
        // vs "user doesn't exist" to the client.
        tracing::debug!(error = ?e, account_id = %B64.encode(account_id), "login finalization failed");
        (
            StatusCode::UNAUTHORIZED,
            "invalid credentials".to_string(),
        )
    })?;

    // At this point the server and client have derived a matching
    // session_key (both derive identically from the AKE handshake). Phase
    // 1.1c does NOT issue a session token — that's deferred. We just
    // confirm login succeeded.
    tracing::info!(account_id = %B64.encode(account_id), "login completed");

    Ok(Json(LoginFinishResponse {
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
        let credential_id = b"0123456789abcdef"; // 16 bytes — pretend UUIDv7
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

    /// Full flow: register → login with the correct password. Both sides
    /// must end up with matching session_keys.
    #[test]
    fn login_with_correct_password_succeeds() {
        let mut rng = OsRng;
        let server_setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let credential_id = b"0123456789abcdef";
        let password = b"correct horse battery staple";

        // ----- Registration (produces a stored ServerRegistration) -----
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut rng, password).unwrap();
        let ss =
            ServerRegistration::<LakkoCipherSuite>::start(&server_setup, cs.message, credential_id)
                .unwrap();
        let cf = cs
            .state
            .finish(
                &mut rng,
                password,
                ss.message,
                ClientRegistrationFinishParameters::default(),
            )
            .unwrap();
        let stored = ServerRegistration::<LakkoCipherSuite>::finish(cf.message);

        // ----- Login -----
        // Client: start.
        let cl_start = ClientLogin::<LakkoCipherSuite>::start(&mut rng, password).unwrap();

        // Server: start (with the stored registration).
        let sl_start = ServerLogin::<LakkoCipherSuite>::start(
            &mut rng,
            &server_setup,
            Some(stored),
            cl_start.message,
            credential_id,
            ServerLoginStartParameters::default(),
        )
        .unwrap();

        // Client: finish — produces the client session_key.
        let cl_finish = cl_start
            .state
            .finish(
                password,
                sl_start.message,
                ClientLoginFinishParameters::default(),
            )
            .unwrap();

        // Server: finish — produces the server session_key.
        let sl_finish = sl_start.state.finish(cl_finish.message).unwrap();

        // Core invariant: client and server agree on the session_key. If
        // this passes, authentication succeeded.
        assert_eq!(
            cl_finish.session_key.as_slice(),
            sl_finish.session_key.as_slice(),
        );
    }

    /// Login with the wrong password must fail at `ClientLogin::finish`
    /// (or `ServerLogin::finish`, depending on where the AKE proof breaks).
    #[test]
    fn login_with_wrong_password_fails() {
        let mut rng = OsRng;
        let server_setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let credential_id = b"0123456789abcdef";
        let real_password = b"correct horse battery staple";
        let wrong_password = b"hunter2";

        // ----- Register with real_password -----
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut rng, real_password).unwrap();
        let ss =
            ServerRegistration::<LakkoCipherSuite>::start(&server_setup, cs.message, credential_id)
                .unwrap();
        let cf = cs
            .state
            .finish(
                &mut rng,
                real_password,
                ss.message,
                ClientRegistrationFinishParameters::default(),
            )
            .unwrap();
        let stored = ServerRegistration::<LakkoCipherSuite>::finish(cf.message);

        // ----- Login with wrong_password -----
        let cl_start = ClientLogin::<LakkoCipherSuite>::start(&mut rng, wrong_password).unwrap();
        let sl_start = ServerLogin::<LakkoCipherSuite>::start(
            &mut rng,
            &server_setup,
            Some(stored),
            cl_start.message,
            credential_id,
            ServerLoginStartParameters::default(),
        )
        .unwrap();

        // ClientLogin::finish typically fails here when the server's
        // CredentialResponse doesn't match what the client expected — the
        // envelope can't be decrypted without the right password.
        let result = cl_start.state.finish(
            wrong_password,
            sl_start.message,
            ClientLoginFinishParameters::default(),
        );
        assert!(result.is_err(), "login with wrong password must fail");
    }

    /// `ServerLogin` serializes and deserializes byte-identically — proxy
    /// for the DB roundtrip (`insert_login_state` → `take_login_state`)
    /// without needing a real MariaDB. Real DB roundtrip is covered by
    /// integration tests against a live database.
    #[test]
    fn server_login_state_serializes_roundtrip() {
        let mut rng = OsRng;
        let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
        let cred = b"0123456789abcdef";
        let pw = b"x";

        // Mini-registration to produce a stored ServerRegistration.
        let cs = ClientRegistration::<LakkoCipherSuite>::start(&mut rng, pw).unwrap();
        let ss = ServerRegistration::<LakkoCipherSuite>::start(&setup, cs.message, cred).unwrap();
        let cf = cs
            .state
            .finish(
                &mut rng,
                pw,
                ss.message,
                ClientRegistrationFinishParameters::default(),
            )
            .unwrap();
        let stored = ServerRegistration::<LakkoCipherSuite>::finish(cf.message);

        // Login start, capturing the ServerLogin state.
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut rng, pw).unwrap();
        let sl = ServerLogin::<LakkoCipherSuite>::start(
            &mut rng,
            &setup,
            Some(stored),
            cl.message,
            cred,
            ServerLoginStartParameters::default(),
        )
        .unwrap();

        // Serialize + deserialize.
        let bytes = sl.state.serialize().to_vec();
        let restored =
            ServerLogin::<LakkoCipherSuite>::deserialize(&bytes).expect("ServerLogin deserialize");
        assert_eq!(bytes.as_slice(), restored.serialize().as_slice());
    }
}
