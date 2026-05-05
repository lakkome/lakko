// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Public MLS KeyPackage directory (Phase 1.16e1).
//!
//! Sending a Welcome to start a 1×1 MLS group needs the
//! recipient's KeyPackage (RFC 9420 §10) — specifically its
//! `init_key`, which is the HPKE public key for the DHKEM step
//! that seeds the new group's epoch secret. KeyPackages are
//! public by design — anyone with the recipient's `account_id`
//! can fetch one.
//!
//! ## Phase-1 simplifications
//!
//! * **One row per account**, replaceable on every login. The
//!   architecture plan §3 calls for last-resort + multiple
//!   KeyPackages with consume-on-use; we'll add that with a
//!   schema bump in a follow-up. For MVP messaging, one
//!   refreshed-on-login KeyPackage is enough — forward-secrecy
//!   on new groups regresses (the same `init_key` may seed two
//!   groups if a sender races within the same login session),
//!   but that's a documented MVP trade-off.
//! * **Same Phase-1 trust gap** as `/discovery/register` and
//!   `/discovery/identity` — PoW-gated, claim-trusting. Phase
//!   1.5b will gate the upload on an auth-issued session token.
//!
//! ## Wire shape
//!
//! Identical to `identity_bundle.rs`: standard-base64 keys on
//! request / response bodies, base64url-without-padding on the
//! GET path segment. The fetched `keypackage_bytes` is opaque
//! (TLS-encoded MLS) — the client deserialises it via
//! `lakko-mls::key_package::deserialize`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::AppState;

const ACCOUNT_ID_LEN: usize = 16;
/// Server-side ceiling on the KeyPackage size. Matches the
/// `VARBINARY(2048)` column width in
/// `migrations/0003_keypackages.sql` so an oversize payload is
/// rejected here rather than blowing up at the SQL layer.
const MAX_KEYPACKAGE_BYTES: usize = 2048;

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

// ============================================================================
// POST /discovery/keypackage   — upsert
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    pub(crate) pow: crate::pow::PowProof,
    pub(crate) account_id: String,
    /// Base64 of the TLS-encoded MLS KeyPackage. Server treats
    /// these bytes as opaque — verification is the responsibility
    /// of the recipient client.
    pub(crate) keypackage: String,
}

#[derive(Serialize)]
pub(crate) struct UploadResponse {
    pub(crate) ok: bool,
}

/// `POST /discovery/keypackage` — upsert.
pub(crate) async fn upload(
    State(state): State<AppState>,
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let account_id = decode_account_id(&payload.account_id)?;

    let kp_bytes = B64.decode(&payload.keypackage).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid keypackage base64: {e}"),
        )
    })?;
    if kp_bytes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "keypackage must be non-empty".to_string(),
        ));
    }
    if kp_bytes.len() > MAX_KEYPACKAGE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "keypackage exceeds the {MAX_KEYPACKAGE_BYTES}-byte ceiling \
                 (got {} bytes)",
                kp_bytes.len()
            ),
        ));
    }

    sqlx::query(
        "INSERT INTO keypackages (account_id, keypackage_bytes) \
         VALUES (?, ?) \
         ON DUPLICATE KEY UPDATE keypackage_bytes = VALUES(keypackage_bytes)",
    )
    .bind(&account_id[..])
    .bind(&kp_bytes)
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "upsert keypackages");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    tracing::info!(
        account_id = %B64.encode(account_id),
        bytes = kp_bytes.len(),
        "keypackage upserted",
    );
    Ok(Json(UploadResponse { ok: true }))
}

// ============================================================================
// GET /discovery/keypackage/{account_id_b64url}   — fetch
// ============================================================================

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    /// Base64-encoded TLS bytes of the stored KeyPackage. The
    /// client feeds this to `Group::add_member` after decoding.
    pub(crate) keypackage: String,
}

/// `GET /discovery/keypackage/{account_id_b64url}` — fetch.
///
/// Returns **404 Not Found** when no KeyPackage is registered.
/// Path encodes `account_id` as base64url-without-padding (same
/// reason as `/discovery/identity/{...}` — axum rejects `+`
/// and `/` in path segments).
pub(crate) async fn fetch(
    State(state): State<AppState>,
    Path(account_id_b64url): Path<String>,
) -> Result<Json<FetchResponse>, (StatusCode, String)> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;

    let bytes = B64URL.decode(&account_id_b64url).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid account_id base64url: {e}"),
        )
    })?;
    if bytes.len() != ACCOUNT_ID_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("account_id must be {ACCOUNT_ID_LEN} bytes after decode"),
        ));
    }

    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT keypackage_bytes FROM keypackages WHERE account_id = ?")
            .bind(&bytes[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "fetch keypackages");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    let (kp,) = row.ok_or((StatusCode::NOT_FOUND, "keypackage not found".to_string()))?;

    Ok(Json(FetchResponse {
        keypackage: B64.encode(&kp),
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_account_id_validates_length() {
        // Right length round-trips.
        let ok = B64.encode([0xAAu8; ACCOUNT_ID_LEN]);
        let bytes = decode_account_id(&ok).unwrap();
        assert_eq!(bytes.len(), ACCOUNT_ID_LEN);

        // Too short.
        let err = decode_account_id(&B64.encode([0u8; 8])).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Too long.
        let err = decode_account_id(&B64.encode([0u8; 32])).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Garbage.
        let err = decode_account_id("not-base64!!!").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    /// The `MAX_KEYPACKAGE_BYTES` constant matches the column
    /// width in the migration. Pinning it as a test constant
    /// surfaces a future drift (e.g. someone bumps the
    /// `VARBINARY` to 4096 in the SQL but forgets to bump the
    /// Rust constant) as a compile-or-fail rather than a
    /// silent "everything past 2048 bytes 500s in production".
    #[test]
    fn max_keypackage_bytes_matches_column_width() {
        // The migration is a string literal that the test reads
        // from disk. If the schema changes, this test should
        // be updated together — it's here to catch drift.
        let migration = include_str!("../migrations/0003_keypackages.sql");
        assert!(
            migration.contains(&format!("VARBINARY({MAX_KEYPACKAGE_BYTES})")),
            "MAX_KEYPACKAGE_BYTES ({MAX_KEYPACKAGE_BYTES}) must match the \
             VARBINARY width declared in 0003_keypackages.sql",
        );
    }
}
