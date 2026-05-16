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
//! - **Registration** (2a2a, this sub-sprint) — the user is already
//!   logged in (the start/finish pair is gated by the session-token
//!   middleware). The browser / iOS / Android authenticator mints a
//!   fresh (private-key, credential-id, public-key) tuple bound to
//!   the lakko relying-party id; the server persists the public-key
//!   half so a future authentication round can verify a signed
//!   challenge.
//!
//! - **Authentication** (2a2b) — anonymous; user presents the
//!   passkey to obtain a session token without going through the
//!   OPAQUE password round-trip. Lands in a follow-up sub-sprint.
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    CreationChallengeResponse, CredentialID, Passkey, PasskeyRegistration,
    RegisterPublicKeyCredential, Webauthn, WebauthnBuilder,
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
mod state_kind {
    pub(super) const REGISTRATION: u8 = 0;
    // 1 = authentication, 2 = discoverable — land in 2a2b.
}

/// Bearer-authenticated endpoint that prepares a fresh passkey
/// registration challenge. The caller has already gone through
/// OPAQUE login (or signup); the auth middleware stamps the
/// account_id into request extensions.
#[axum::debug_handler]
pub(crate) async fn register_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<RegisterStartReq>,
) -> Result<Json<RegisterStartRes>, ApiError> {
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
    let (ccr, reg_state): (CreationChallengeResponse, PasskeyRegistration) = webauthn
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

/// Finalise the registration: verify the attestation, persist the
/// public-key half. Caller passes back the `state_id` we issued plus
/// the authenticator's `PublicKeyCredential` JSON.
#[axum::debug_handler]
pub(crate) async fn register_finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<RegisterFinishReq>,
) -> Result<Json<RegisterFinishRes>, ApiError> {
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
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;

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

    sqlx::query("DELETE FROM webauthn_credentials WHERE credential_id = ? AND account_id = ?")
        .bind(&credential_id)
        .bind(auth.0.as_slice())
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;
    Ok(StatusCode::NO_CONTENT)
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
    let webauthn = WebauthnBuilder::new(&rp_id, &origin_url)
        .map_err(|e| anyhow::anyhow!("WebauthnBuilder::new: {e}"))?
        .rp_name(&rp_name)
        .build()
        .map_err(|e| anyhow::anyhow!("WebauthnBuilder::build: {e}"))?;
    Ok(Some(Arc::new(webauthn)))
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

fn random_state_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    id
}

fn parse_state_id(s: &str) -> Result<[u8; 16], ApiError> {
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

fn hex_lower(bytes: &[u8]) -> String {
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
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = sqlx::query("DELETE FROM webauthn_states WHERE expires_at < NOW(3)")
                .execute(&pool)
                .await;
            match result {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned expired webauthn_states");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "webauthn_states prune failed"),
            }
        }
    })
}

// ----------------------------------------------------------------
// Middleware
// ----------------------------------------------------------------

/// Bearer-auth middleware. Same shape as
/// `services/storage::auth::require_session`. Stamps an
/// [`AuthenticatedAccount`] into request extensions on success;
/// returns 401 on any failure.
pub(crate) async fn require_session(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    use axum::http::header;
    let header_value = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            tracing::debug!("webauthn auth middleware: missing Authorization header");
            StatusCode::UNAUTHORIZED
        })?;
    let token = header_value.strip_prefix("Bearer ").ok_or_else(|| {
        tracing::warn!("webauthn auth middleware: Authorization missing Bearer prefix");
        StatusCode::UNAUTHORIZED
    })?;
    let now_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let account_id_bytes = state
        .session_token_secret
        .verify(token, now_ms)
        .map_err(|e| {
            tracing::warn!(error = %e, "webauthn auth: session-token verify failed");
            StatusCode::UNAUTHORIZED
        })?;
    let account_id: [u8; 16] = account_id_bytes.as_slice().try_into().map_err(|_| {
        tracing::error!(
            "webauthn auth: account_id length {} after verify (expected 16)",
            account_id_bytes.len()
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    request
        .extensions_mut()
        .insert(AuthenticatedAccount(account_id));
    Ok(next.run(request).await)
}

// ----------------------------------------------------------------
// Wire shapes
// ----------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct RegisterStartReq {
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

// ----------------------------------------------------------------
// Error → HTTP mapping
// ----------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum ApiError {
    Disabled,
    BadInput(&'static str),
    StateMissing,
    Db(sqlx::Error),
    Webauthn(webauthn_rs::prelude::WebauthnError),
    SerdeJson(serde_json::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let (code, msg) = match self {
            Self::Disabled => (StatusCode::SERVICE_UNAVAILABLE, "webauthn-disabled"),
            Self::BadInput(m) => (StatusCode::BAD_REQUEST, m),
            Self::StateMissing => (StatusCode::GONE, "state-missing-or-expired"),
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
