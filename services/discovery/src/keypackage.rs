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
//! ## Shape (ADR-0050)
//!
//! * **One row per `(account_id, device_id)`**, where `device_id` is the
//!   device leaf's 32-byte Ed25519 signature key. A contact adding an
//!   account fetches the whole list and adds every device. The server
//!   never parses the bytes and never checks that `device_id` matches the
//!   KeyPackage's leaf key — the peer verifies the leaf's device binding
//!   before adding it, which is the check that matters.
//! * Replace-on-upload per device, no consume-on-fetch. A device
//!   republishes after every Welcome it consumes.
//! * At most [`MAX_DEVICES_PER_ACCOUNT`] rows per account; the composite key
//!   would otherwise be an unbounded write channel for an authenticated
//!   account.
//! * Upload and delete are bearer-bound to the token's account; fetch is
//!   public by design (KeyPackages are public artefacts).
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
/// The device leaf signature key length (ADR-0050).
const DEVICE_ID_LEN: usize = 32;
/// Server-side ceiling on the KeyPackage size. Matches the
/// `VARBINARY(2048)` column width in
/// `migrations/0017_keypackages_per_device.sql` so an oversize payload is
/// rejected here rather than blowing up at the SQL layer.
const MAX_KEYPACKAGE_BYTES: usize = 2048;
/// Ceiling on distinct devices per account. Mirrors
/// `lakko_mls::group_proposals::MAX_LEAVES_PER_ACCOUNT` (ADR-0050): a peer's
/// gate refuses a ninth leaf anyway, so the directory refuses a ninth row.
const MAX_DEVICES_PER_ACCOUNT: i64 = 8;

fn decode_device_id(b64: &str) -> Result<[u8; DEVICE_ID_LEN], (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid device_id base64: {e}"),
        )
    })?;
    if bytes.len() != DEVICE_ID_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("device_id must be {DEVICE_ID_LEN} bytes after base64 decode"),
        ));
    }
    let mut out = [0u8; DEVICE_ID_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

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
    /// Base64 of the device leaf's 32-byte Ed25519 signature key (ADR-0050).
    pub(crate) device_id: String,
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
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let account_id = decode_account_id(&payload.account_id)?;
    let device_id = decode_device_id(&payload.device_id)?;

    // Phase 1.5b anti-impersonation: KeyPackage uploads must be
    // bound to the token's account. Otherwise an authenticated
    // user could overwrite someone else's KeyPackage to fork them
    // into a wrong-init-key 1×1 group on next contact.
    if account_id != authed.0 {
        tracing::warn!(
            body_account_id = %B64.encode(account_id),
            token_account_id = %B64.encode(authed.0),
            "keypackage upload: body account_id does not match token's"
        );
        return Err((
            StatusCode::FORBIDDEN,
            "account_id mismatch between body and session token".to_string(),
        ));
    }

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

    let db_err = |e: sqlx::Error| {
        tracing::error!(error = %e, "keypackages");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    };

    // ADR-0050 device cap. A replace of an existing device never counts;
    // a new device past the cap is refused. Not one transaction: two
    // concurrent first-uploads can overshoot by one, which the peer's own
    // per-account gate still refuses at the group.
    // Two plain COUNTs rather than one SUM(CASE ...): MariaDB types a SUM as
    // DECIMAL, which sqlx refuses to decode as i64.
    let (existing,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM keypackages WHERE account_id = ?")
            .bind(&account_id[..])
            .fetch_one(&state.db)
            .await
            .map_err(db_err)?;
    let (this_device,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM keypackages WHERE account_id = ? AND device_id = ?")
            .bind(&account_id[..])
            .bind(&device_id[..])
            .fetch_one(&state.db)
            .await
            .map_err(db_err)?;
    if this_device == 0 && existing >= MAX_DEVICES_PER_ACCOUNT {
        return Err((
            StatusCode::CONFLICT,
            format!("this account already has {MAX_DEVICES_PER_ACCOUNT} devices; remove one first"),
        ));
    }

    sqlx::query(
        "INSERT INTO keypackages (account_id, device_id, keypackage_bytes) \
         VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE keypackage_bytes = VALUES(keypackage_bytes)",
    )
    .bind(&account_id[..])
    .bind(&device_id[..])
    .bind(&kp_bytes)
    .execute(&state.db)
    .await
    .map_err(db_err)?;

    tracing::info!(
        account_id = %B64.encode(account_id),
        bytes = kp_bytes.len(),
        "keypackage upserted",
    );
    Ok(Json(UploadResponse { ok: true }))
}

// ============================================================================
// DELETE /discovery/keypackage/device/{device_id_b64url}   — one device's row
// ============================================================================

/// `DELETE /discovery/keypackage/device/{device_id_b64url}` — remove one device's
/// KeyPackage for the authenticated account (ADR-0050 device removal).
/// Idempotent: a missing row is a 204 too.
pub(crate) async fn delete(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Path(device_id_b64url): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    let bytes = B64URL.decode(&device_id_b64url).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid device_id base64url: {e}"),
        )
    })?;
    if bytes.len() != DEVICE_ID_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("device_id must be {DEVICE_ID_LEN} bytes after decode"),
        ));
    }
    sqlx::query("DELETE FROM keypackages WHERE account_id = ? AND device_id = ?")
        .bind(authed.0.as_slice())
        .bind(&bytes[..])
        .execute(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "delete keypackage");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

// ============================================================================
// GET /discovery/keypackage/{account_id_b64url}   — fetch
// ============================================================================

/// One device's entry in [`FetchResponse`].
#[derive(Serialize)]
pub(crate) struct DeviceKeyPackage {
    /// Base64 of the device leaf's 32-byte signature key.
    pub(crate) device_id: String,
    /// Base64-encoded TLS bytes of the stored KeyPackage. The
    /// client feeds this to `Group::add_member` after decoding.
    pub(crate) keypackage: String,
}

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    /// Every device of the account that has a KeyPackage published,
    /// oldest first (ADR-0050). A contact adds all of them.
    pub(crate) keypackages: Vec<DeviceKeyPackage>,
}

/// `GET /discovery/keypackage/{account_id_b64url}` — fetch the account's
/// device KeyPackages.
///
/// Returns **404 Not Found** when the account has none.
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

    let rows: Vec<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT device_id, keypackage_bytes FROM keypackages \
         WHERE account_id = ? ORDER BY created_at ASC, device_id ASC",
    )
    .bind(&bytes[..])
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "fetch keypackages");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    if rows.is_empty() {
        return Err((StatusCode::NOT_FOUND, "keypackage not found".to_string()));
    }

    Ok(Json(FetchResponse {
        keypackages: rows
            .into_iter()
            .map(|(device_id, kp)| DeviceKeyPackage {
                device_id: B64.encode(&device_id),
                keypackage: B64.encode(&kp),
            })
            .collect(),
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
    fn decode_device_id_validates_length() {
        assert!(decode_device_id(&B64.encode([7u8; DEVICE_ID_LEN])).is_ok());
        assert_eq!(
            decode_device_id(&B64.encode([0u8; 16])).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            decode_device_id("!!").unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn max_keypackage_bytes_matches_column_width() {
        // The migration is a string literal that the test reads
        // from disk. If the schema changes, this test should
        // be updated together — it's here to catch drift.
        let migration = include_str!("../migrations/0017_keypackages_per_device.sql");
        assert!(
            migration.contains(&format!("VARBINARY({MAX_KEYPACKAGE_BYTES})")),
            "MAX_KEYPACKAGE_BYTES ({MAX_KEYPACKAGE_BYTES}) must match the \
             VARBINARY width declared in 0017_keypackages_per_device.sql",
        );
    }
}
