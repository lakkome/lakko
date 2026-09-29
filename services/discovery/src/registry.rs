// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Username registry — the table mapping `OPRF(K, normalize(@username))`
//! to `account_id`. Plaintext usernames are never stored.
//!
//! ## Authorisation
//!
//! `/discovery/register` is session-token-gated (Phase 1.5b) and the
//! body's `account_id` must equal the token's; it is counted per account
//! by the write limiter and, since ADR-0054 S3, carries no proof of work.
//! `/discovery/lookup` is anonymous and rate-limited per source address, and
//! since ADR-0060 answers only for a person who chose Everyone;
//! the enumeration step it follows (`/discovery/oprf`) spends an
//! anonymous token.
//!
//!   * Usernames are immutable (no UPDATE / DELETE in the schema).
//!   * First-come-first-served via `INSERT IGNORE` semantics; the API
//!     surfaces 409 Conflict on a hash collision.
//!   * Squatting damage is recoverable by a manual table reset before
//!     beta launch.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// VOPRF Ristretto255-SHA512 output is 64 bytes (one SHA-512 digest).
const OPRF_HASH_LEN: usize = 64;
/// `account_id` length in bytes (UUIDv7 raw, same as `services/auth`).
const ACCOUNT_ID_LEN: usize = 16;

/// Decode a fixed-length base64 input.
fn decode_fixed<const N: usize>(
    b64: &str,
    name: &'static str,
) -> Result<[u8; N], (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid {name} base64: {e}"),
        )
    })?;
    if bytes.len() != N {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{name} must be {N} bytes after base64 decode"),
        ));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

// ============================================================================
// /discovery/register
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct RegisterRequest {
    /// Base64-encoded UUIDv7 raw bytes (16 bytes).
    pub(crate) account_id: String,
    /// Base64-encoded VOPRF output bytes (64 bytes for Ristretto255-SHA512).
    /// The client computes this locally after a `/discovery/oprf` round trip.
    pub(crate) oprf_hash: String,
}

#[derive(Serialize)]
pub(crate) struct RegisterResponse {
    pub(crate) ok: bool,
    /// ADR-0060: standard base64 of the 153-byte username certificate
    /// (`lakko_attest::username_cert`) binding this OPRF hash to the account.
    pub(crate) username_certificate_b64: String,
}

/// `POST /discovery/register` — bind an OPRF-hashed username to an
/// `account_id`. Usernames are immutable in Phase 1.5: a hash that's
/// already present returns **409 Conflict** regardless of which
/// `account_id` is presented.
pub(crate) async fn register(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, (StatusCode, String)> {
    let account_id = decode_fixed::<ACCOUNT_ID_LEN>(&payload.account_id, "account_id")?;
    let oprf_hash = decode_fixed::<OPRF_HASH_LEN>(&payload.oprf_hash, "oprf_hash")?;

    // Phase 1.5b anti-impersonation: the body's account_id must
    // match the token's. Otherwise an authenticated user could
    // register a username under someone else's account_id (the
    // resulting state would be useless to the attacker, but it
    // would burn the username for the victim — first-come-first-
    // served is the only resolution in Phase 1).
    if account_id != authed.0 {
        tracing::warn!(
            body_account_id = %B64.encode(account_id),
            token_account_id = %B64.encode(authed.0),
            "register: body account_id does not match token's"
        );
        return Err((
            StatusCode::FORBIDDEN,
            "account_id mismatch between body and session token".to_string(),
        ));
    }

    let result = sqlx::query("INSERT INTO usernames (oprf_hash, account_id) VALUES (?, ?)")
        .bind(&oprf_hash[..])
        .bind(&account_id[..])
        .execute(&state.db)
        .await;

    match result {
        Ok(_) => {
            tracing::info!(
                account_id = %B64.encode(account_id),
                "username registered",
            );
            Ok(Json(RegisterResponse {
                ok: true,
                username_certificate_b64: crate::visibility::sign_certificate(
                    &state.username_cert_key,
                    oprf_hash,
                    account_id,
                ),
            }))
        },
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => Err((
            StatusCode::CONFLICT,
            "username already registered".to_string(),
        )),
        Err(e) => {
            tracing::error!(error = %e, "insert usernames");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            ))
        },
    }
}

// ============================================================================
// /discovery/lookup
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct LookupRequest {
    pub(crate) oprf_hash: String,
}

#[derive(Serialize)]
pub(crate) struct LookupResponse {
    /// Base64-encoded UUIDv7 raw bytes — the `account_id` registered
    /// under this OPRF-hashed username.
    pub(crate) account_id: String,
}

/// `POST /discovery/lookup` — resolve an OPRF-hashed username to its
/// `account_id`. Returns **404 Not Found** for unknown hashes, and for a
/// username whose owner is not in Everyone (ADR-0060: the same status and
/// body, so a lookup cannot tell hidden from unregistered).
///
/// Exact-match only: there is intentionally no prefix search /
/// autocomplete from the server (which would either need plaintext
/// access or leak partial information about the username space).
/// Clients implement local autocomplete from their own contact list.
pub(crate) async fn lookup(
    State(state): State<AppState>,
    Json(payload): Json<LookupRequest>,
) -> Result<Json<LookupResponse>, (StatusCode, String)> {
    let oprf_hash = decode_fixed::<OPRF_HASH_LEN>(&payload.oprf_hash, "oprf_hash")?;

    // ADR-0060: only a person in Everyone answers; a hidden username gets the
    // very response an unregistered one does.
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT account_id FROM usernames WHERE oprf_hash = ? AND is_public = TRUE")
            .bind(&oprf_hash[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "lookup usernames");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    match row {
        Some((account_id,)) => Ok(Json(LookupResponse {
            account_id: B64.encode(account_id),
        })),
        None => Err((StatusCode::NOT_FOUND, "username not found".to_string())),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_fixed_validates_length() {
        // Right length.
        let s = B64.encode([0u8; ACCOUNT_ID_LEN]);
        decode_fixed::<ACCOUNT_ID_LEN>(&s, "account_id").unwrap();

        // Too short.
        let s = B64.encode([0u8; 8]);
        let err = decode_fixed::<ACCOUNT_ID_LEN>(&s, "account_id").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Right length for OPRF hash.
        let s = B64.encode([0u8; OPRF_HASH_LEN]);
        decode_fixed::<OPRF_HASH_LEN>(&s, "oprf_hash").unwrap();

        // Bad base64.
        let err = decode_fixed::<ACCOUNT_ID_LEN>("not-valid!!!", "account_id").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
}
