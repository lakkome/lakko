// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Anonymous credential issuance for channel polls ([ADR-0092] section 4.2).
//!
//! One account, one vote per channel poll: auth issues each account a
//! keyed-verification anonymous credential (`lakko-anon-cred`) per 30-day
//! period, over the account's fixed credential secret `s`. Auth never sees
//! `s`; it stores the commitment `C = s*G` the account registered once.
//!
//! | Method | Path | Gate | |
//! |---|---|---|---|
//! | `PUT` | `/auth/anon-credential/commitment` | bearer | `{registration_b64}`: register `C` with a proof of knowledge of `s`. `204` when stored or already the same; `409 commitment-already-registered` for another `C`. |
//! | `POST` | `/auth/anon-credential` | bearer | `{request_b64}`: a proof of knowledge of the `s` behind the stored `C`, bound to the current period's key. `200 {period, response_b64, public_key_b64, period_ends_at}`. |
//! | `GET` | `/auth/anon-credential/key/{period}` | none | `{period, public_key_b64}` for the previous, current or next period. |
//!
//! Every byte string is base64url without padding.
//!
//! Issuance is repeatable within a period: a second device of the account,
//! or a reinstall, gets an equivalent credential for the same `s`, which buys
//! no second vote (the tag depends on `s` and the poll only). Each issuance is
//! recorded in `anon_credential_issued (account_id, period)` (one row per
//! pair) and both bearer routes are counted against the account's
//! [`Scope::AnonCredential`] window.
//!
//! **Key reset and recall** (ADR-0073, ADR-0074) delete both rows inside
//! their transaction ([`crate::key_reset::delete_superseded_state`]): the
//! person's new seed gives a new `s`, which registers a new `C`. Account
//! deletion cascades. A key rotation keeps the lineage seed, so `s` and `C`
//! stay.
//!
//! # Configuration
//!
//! `LAKKO_ANON_CRED_MASTER_SECRET` — 64 hex characters, shared with
//! `services/channels` only (not the anonymous-token master). Unset: every
//! route here answers `503 anon-credential-disabled`. Malformed: startup
//! fails.
//!
//! [ADR-0092]: ../../../docs/adr/0092-attachment-kinds.md

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_cred::{
    period_at, verify_registration, AnonCredError, Commitment, CommitmentRegistration,
    IssuanceRequest, IssuerSecretKey, MasterSecret, PERIOD_SECS,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::limits::{LimitError, Scope};
use crate::webauthn::AuthenticatedAccount;
use crate::AppState;

/// Env var of the credential master secret.
pub(crate) const MASTER_VAR: &str = "LAKKO_ANON_CRED_MASTER_SECRET";

/// Issuance configuration: the master secret shared with the verifier.
#[derive(Debug, Clone)]
pub(crate) struct AnonCredIssuer {
    master: MasterSecret,
}

impl AnonCredIssuer {
    /// Build from a master secret (tests and startup).
    pub(crate) const fn new(master: MasterSecret) -> Self {
        Self { master }
    }
}

/// Load the issuer from the environment. `Ok(None)` when unset or blank.
///
/// # Errors
///
/// A value that is not 64 hex characters.
pub(crate) fn load_anon_credential() -> anyhow::Result<Option<AnonCredIssuer>> {
    match std::env::var(MASTER_VAR) {
        Ok(v) if !v.trim().is_empty() => MasterSecret::from_hex(&v)
            .map(|m| Some(AnonCredIssuer::new(m)))
            .map_err(|_| anyhow::anyhow!("{MASTER_VAR} must be 64 hex characters")),
        _ => Ok(None),
    }
}

/// Log the startup state, loudly when dormant.
pub(crate) fn log_startup(issuer: Option<&AnonCredIssuer>) {
    if issuer.is_some() {
        tracing::info!("anonymous-credential issuance enabled");
    } else {
        tracing::warn!(
            "anonymous-credential issuance DISABLED: LAKKO_ANON_CRED_MASTER_SECRET is not set; \
             /auth/anon-credential* answers 503 and no channel poll vote can be cast"
        );
    }
}

/// Unix seconds now.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Unix milliseconds now.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

// ----------------------------------------------------------------
// Errors
// ----------------------------------------------------------------

/// Refusals of the credential routes, each with a stable code.
#[derive(Debug)]
pub(crate) enum CredApiError {
    /// No master secret (503 `anon-credential-disabled`).
    Disabled,
    /// A body field that is not base64url or not the right layout (400
    /// `malformed`).
    Malformed,
    /// A proof that does not verify (400 `bad-proof`).
    BadProof,
    /// An issuance request for another period than the current one (400
    /// `period-not-issuable`).
    PeriodNotIssuable { current_period: u64 },
    /// A key asked for a period outside previous..=next (404
    /// `period-out-of-range`).
    PeriodOutOfRange,
    /// Another commitment is registered (409
    /// `commitment-already-registered`).
    CommitmentConflict,
    /// No commitment is registered yet (409 `commitment-not-registered`).
    NoCommitment,
    /// A limiter refused.
    Limit(LimitError),
    /// Store or crypto failure (500 `internal`).
    Internal,
}

impl From<LimitError> for CredApiError {
    fn from(e: LimitError) -> Self {
        Self::Limit(e)
    }
}

impl IntoResponse for CredApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Disabled => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "anon-credential-disabled" }),
            ),
            Self::Malformed => (StatusCode::BAD_REQUEST, json!({ "error": "malformed" })),
            Self::BadProof => (StatusCode::BAD_REQUEST, json!({ "error": "bad-proof" })),
            Self::PeriodNotIssuable { current_period } => (
                StatusCode::BAD_REQUEST,
                json!({ "error": "period-not-issuable", "current_period": current_period }),
            ),
            Self::PeriodOutOfRange => (
                StatusCode::NOT_FOUND,
                json!({ "error": "period-out-of-range" }),
            ),
            Self::CommitmentConflict => (
                StatusCode::CONFLICT,
                json!({ "error": "commitment-already-registered" }),
            ),
            Self::NoCommitment => (
                StatusCode::CONFLICT,
                json!({ "error": "commitment-not-registered" }),
            ),
            Self::Limit(e) => return e.into_response(),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "internal" }),
            ),
        };
        (status, Json(body)).into_response()
    }
}

fn issuer(state: &AppState) -> Result<&Arc<AnonCredIssuer>, CredApiError> {
    state.anon_cred.as_ref().ok_or(CredApiError::Disabled)
}

fn decode_b64(s: &str) -> Result<Vec<u8>, CredApiError> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| CredApiError::Malformed)
}

fn db(e: &sqlx::Error) -> CredApiError {
    tracing::error!(error = %e, "anon-credential store error; refusing");
    CredApiError::Internal
}

/// The stored commitment of `account`, if any.
async fn stored_commitment(
    pool: &sqlx::MySqlPool,
    account: &[u8; 16],
) -> Result<Option<Commitment>, CredApiError> {
    let row: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT commitment FROM anon_credential_commitments WHERE account_id = ?",
    )
    .bind(&account[..])
    .fetch_optional(pool)
    .await
    .map_err(|e| db(&e))?;
    row.map(|bytes| {
        Commitment::decode(&bytes).map_err(|_| {
            tracing::error!("stored anon-credential commitment does not decode");
            CredApiError::Internal
        })
    })
    .transpose()
}

// ----------------------------------------------------------------
// PUT /auth/anon-credential/commitment
// ----------------------------------------------------------------

/// Body of `PUT /auth/anon-credential/commitment`.
#[derive(Deserialize)]
pub(crate) struct RegisterReq {
    /// The `CommitmentRegistration` (98 bytes), base64url.
    pub registration_b64: String,
}

/// `PUT /auth/anon-credential/commitment`: register the account's
/// commitment once.
///
/// Order: decode, charge the account's window, verify the proof, then store
/// (insert-if-absent) and compare. A repeat with the same commitment is a
/// `204`; another commitment is a `409`, until a key reset or recall deletes
/// the row.
pub(crate) async fn register_commitment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<RegisterReq>,
) -> Result<StatusCode, CredApiError> {
    issuer(&state)?;
    let registration = CommitmentRegistration::decode(&decode_b64(&req.registration_b64)?)
        .map_err(|_| CredApiError::Malformed)?;
    crate::limits::charge_name(&state.db, &state.limits, Scope::AnonCredential, &auth.0).await?;
    let commitment = verify_registration(&registration).map_err(|_| CredApiError::BadProof)?;
    let bytes = commitment.to_bytes();
    sqlx::query(
        "INSERT IGNORE INTO anon_credential_commitments (account_id, commitment, created_at_ms) \
         VALUES (?, ?, ?)",
    )
    .bind(&auth.0[..])
    .bind(&bytes[..])
    .bind(now_ms())
    .execute(&state.db)
    .await
    .map_err(|e| db(&e))?;
    match stored_commitment(&state.db, &auth.0).await? {
        Some(c) if c == commitment => Ok(StatusCode::NO_CONTENT),
        Some(_) => Err(CredApiError::CommitmentConflict),
        // The account row vanished between the insert and the read (deleted).
        None => Err(CredApiError::Internal),
    }
}

// ----------------------------------------------------------------
// POST /auth/anon-credential
// ----------------------------------------------------------------

/// Body of `POST /auth/anon-credential`.
#[derive(Deserialize)]
pub(crate) struct IssueReq {
    /// The `IssuanceRequest` (73 bytes), base64url.
    pub request_b64: String,
}

/// Answer of `POST /auth/anon-credential`.
#[derive(Serialize)]
pub(crate) struct IssueRes {
    /// The period the credential is for (the current one).
    pub period: u64,
    /// The `IssuanceResponse` (233 bytes), base64url.
    pub response_b64: String,
    /// The period's public key (96 bytes), base64url. Informational: a
    /// client checks the response against the key it fetched from the key
    /// route.
    pub public_key_b64: String,
    /// Unix second at which the period ends.
    pub period_ends_at: u64,
}

/// `POST /auth/anon-credential`: issue the current period's credential for
/// the stored commitment.
pub(crate) async fn issue_credential(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<IssueReq>,
) -> Result<Json<IssueRes>, CredApiError> {
    let issuer = issuer(&state)?;
    let request = IssuanceRequest::decode(&decode_b64(&req.request_b64)?)
        .map_err(|_| CredApiError::Malformed)?;
    let current = period_at(now_secs());
    if request.period() != current {
        return Err(CredApiError::PeriodNotIssuable {
            current_period: current,
        });
    }
    crate::limits::charge_name(&state.db, &state.limits, Scope::AnonCredential, &auth.0).await?;
    let stored = stored_commitment(&state.db, &auth.0)
        .await?
        .ok_or(CredApiError::NoCommitment)?;
    let key = IssuerSecretKey::derive(&issuer.master, current).map_err(|e| {
        tracing::error!(code = e.code(), "anon-credential key derivation failed");
        CredApiError::Internal
    })?;
    let response = key.issue(&request, &stored).map_err(|e| match e {
        AnonCredError::BadProof | AnonCredError::WrongPeriod => CredApiError::BadProof,
        other => {
            tracing::error!(code = other.code(), "anon-credential issuance failed");
            CredApiError::Internal
        },
    })?;
    sqlx::query(
        "INSERT IGNORE INTO anon_credential_issued (account_id, period, issued_at_ms) \
         VALUES (?, ?, ?)",
    )
    .bind(&auth.0[..])
    .bind(current)
    .bind(now_ms())
    .execute(&state.db)
    .await
    .map_err(|e| db(&e))?;
    Ok(Json(IssueRes {
        period: current,
        response_b64: URL_SAFE_NO_PAD.encode(response.to_bytes()),
        public_key_b64: URL_SAFE_NO_PAD.encode(key.public_key().to_bytes()),
        period_ends_at: current.saturating_add(1).saturating_mul(PERIOD_SECS),
    }))
}

// ----------------------------------------------------------------
// GET /auth/anon-credential/key/{period}
// ----------------------------------------------------------------

/// Answer of the key route.
#[derive(Serialize)]
pub(crate) struct KeyRes {
    /// The period.
    pub period: u64,
    /// The period's public key (96 bytes), base64url.
    pub public_key_b64: String,
}

/// `GET /auth/anon-credential/key/{period}` (anonymous): the public key of
/// the previous, current or next period.
pub(crate) async fn public_key(
    State(state): State<AppState>,
    Path(period): Path<u64>,
) -> Result<Response, CredApiError> {
    let issuer = issuer(&state)?;
    let current = period_at(now_secs());
    if period.saturating_add(1) < current || period > current.saturating_add(1) {
        return Err(CredApiError::PeriodOutOfRange);
    }
    let key = IssuerSecretKey::derive(&issuer.master, period).map_err(|e| {
        tracing::error!(code = e.code(), "anon-credential key derivation failed");
        CredApiError::Internal
    })?;
    let mut resp = Json(KeyRes {
        period,
        public_key_b64: URL_SAFE_NO_PAD.encode(key.public_key().to_bytes()),
    })
    .into_response();
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    Ok(resp)
}

/// Drop issuance rows older than the previous period, hourly.
pub(crate) fn spawn_pruner(pool: sqlx::MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let floor = period_at(now_secs()).saturating_sub(1);
            match sqlx::query("DELETE FROM anon_credential_issued WHERE period < ?")
                .bind(floor)
                .execute(&pool)
                .await
            {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(
                        removed = r.rows_affected(),
                        "anon-credential issuance rows pruned"
                    );
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "anon-credential issuance prune failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests;
