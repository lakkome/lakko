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
//! ## Removal that holds (ADR-0074 section 2.2)
//!
//! A device removed from the account keeps a valid access token for up to
//! the access lifetime, and it holds the account seed, so it can mint a
//! binding for any device key. Three rules keep it out of the directory from
//! the moment of the delete:
//!
//! * **The device gate.** An upload's `device_id` must be the device the
//!   session token names (`403 device-mismatch`); a session that names no
//!   device cannot publish. So a token cannot publish under a fresh device
//!   key, and a new token needs a new sign-in.
//! * **Tombstones.** Every delete of one device's row records the id in
//!   `keypackage_tombstones`; an upload under a tombstoned id is refused
//!   (`403 device-revoked`) for [`TOMBSTONE_RETENTION_MS`].
//! * **The delete gate.** A session deletes its OWN device's row freely.
//!   Deleting ANOTHER device's row needs an account statement (`lakko-attest`,
//!   kind `device-revoke`, over that device id) in the
//!   `x-lakko-account-statement` header, verified under the account's
//!   published `identity_bundles.signature_pubkey`; else `403
//!   account-key-required`. So a removed device cannot delete the others'
//!   rows with its token alone. (It holds the account seed and could sign a
//!   statement; taking the seed away is a key rotation, not a removal.)
//!
//! Both writes are under the ADR-0073 generation floor.
//!
//! The server still parses no KeyPackage and does not check that `device_id`
//! is the KeyPackage's leaf key.
//!
//! ## Wire shape
//!
//! Identical to `identity_bundle.rs`: standard-base64 keys on
//! request / response bodies, base64url-without-padding on the
//! GET path segment. The fetched `keypackage_bytes` is opaque
//! (TLS-encoded MLS) — the client deserialises it via
//! `lakko-mls::key_package::deserialize`.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use lakko_attest::{
    device_revoke_body, verify_account_statement, AccountStatementKind, ACCOUNT_STATEMENT_HEADER,
};
use lakko_auth_token::UNBOUND_DEVICE_ID;
use lakko_crypto::signing::VerifyingKey;
use sqlx::MySqlPool;

use crate::key_reset::RouteError;
use crate::AppState;

/// How long a deleted device id stays unpublishable: 30 days, the refresh
/// credential's default lifetime, so every session the removed device could
/// hold has expired by the time its tombstone goes. Chosen, not measured.
pub(crate) const TOMBSTONE_RETENTION_MS: i64 = 30 * 24 * 3600 * 1000;

/// How often the tombstone pruner runs.
const TOMBSTONE_PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn db_error(e: sqlx::Error) -> RouteError {
    tracing::error!(error = %e, "keypackages");
    RouteError::Text(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal db error".to_string(),
    )
}

/// `true` when `(account_id, device_id)` was deleted from the directory
/// within [`TOMBSTONE_RETENTION_MS`].
async fn is_tombstoned(
    db: &MySqlPool,
    account_id: &[u8; ACCOUNT_ID_LEN],
    device_id: &[u8; DEVICE_ID_LEN],
) -> Result<bool, RouteError> {
    let deleted_at: Option<i64> = sqlx::query_scalar(
        "SELECT deleted_at_ms FROM keypackage_tombstones WHERE account_id = ? AND device_id = ?",
    )
    .bind(&account_id[..])
    .bind(&device_id[..])
    .fetch_optional(db)
    .await
    .map_err(db_error)?;
    Ok(deleted_at.is_some_and(|at| at > now_ms().saturating_sub(TOMBSTONE_RETENTION_MS)))
}

/// The ADR-0074 gate of a KeyPackage publish: `device_id` is the device the
/// session names and not the all-zero "no device" id (`403 device-mismatch`),
/// and is not tombstoned (`403 device-revoked`).
async fn require_publishable_device(
    db: &MySqlPool,
    account_id: &[u8; ACCOUNT_ID_LEN],
    device_id: &[u8; DEVICE_ID_LEN],
    session_device: &[u8; DEVICE_ID_LEN],
) -> Result<(), RouteError> {
    if *device_id == UNBOUND_DEVICE_ID || device_id != session_device {
        tracing::warn!("keypackage upload refused — device_id is not the session's device",);
        return Err(crate::key_reset::json_error(
            StatusCode::FORBIDDEN,
            crate::key_reset::CODE_DEVICE_MISMATCH,
        ));
    }
    if is_tombstoned(db, account_id, device_id).await? {
        tracing::warn!("keypackage upload refused — the device id was removed from the directory",);
        return Err(crate::key_reset::json_error(
            StatusCode::FORBIDDEN,
            crate::key_reset::CODE_DEVICE_REVOKED,
        ));
    }
    Ok(())
}

/// Background pruner of `keypackage_tombstones`: drops rows older than
/// [`TOMBSTONE_RETENTION_MS`], hourly. Hygiene only: the upload gate compares
/// the age itself.
pub(crate) fn spawn_tombstone_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TOMBSTONE_PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let cutoff = now_ms().saturating_sub(TOMBSTONE_RETENTION_MS);
            match sqlx::query("DELETE FROM keypackage_tombstones WHERE deleted_at_ms <= ?")
                .bind(cutoff)
                .execute(&pool)
                .await
            {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned keypackage tombstones");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "keypackage_tombstones prune failed"),
            }
        }
    })
}

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
///
/// ADR-0073 section 4.1: one of the two identity writes under the generation
/// floor — a session token issued under a key generation below the account's
/// `identity_bundles.key_generation` is refused `401
/// {"error":"key-generation-superseded"}` before anything is written.
///
/// ADR-0074 section 2.2, after the floor: the body's `device_id` must be the
/// device the token names and not the all-zero "no device" id (`403
/// {"error":"device-mismatch"}`), and must not be tombstoned (`403
/// {"error":"device-revoked"}`).
pub(crate) async fn upload(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    axum::Extension(generation): axum::Extension<crate::auth::SessionKeyGeneration>,
    axum::Extension(session_device): axum::Extension<crate::auth::SessionDevice>,
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, RouteError> {
    let token_generation = generation.0;
    let account_id = decode_account_id(&payload.account_id)?;
    let device_id = decode_device_id(&payload.device_id)?;

    // Phase 1.5b anti-impersonation: KeyPackage uploads must be
    // bound to the token's account. Otherwise an authenticated
    // user could overwrite someone else's KeyPackage to fork them
    // into a wrong-init-key 1×1 group on next contact.
    if account_id != authed.0 {
        tracing::warn!("keypackage upload: body account_id does not match token's");
        return Err(RouteError::Text(
            StatusCode::FORBIDDEN,
            "account_id mismatch between body and session token".to_string(),
        ));
    }

    // ADR-0073: the generation floor. Read before the device cap, so a
    // superseded device learns the one code it acts on and writes nothing.
    if crate::key_reset::floor_refuses(&state.db, &account_id, token_generation).await? {
        tracing::warn!(
            token_generation,
            "keypackage upload refused — session token's key generation superseded",
        );
        return Err(crate::key_reset::json_error(
            StatusCode::UNAUTHORIZED,
            crate::key_reset::CODE_KEY_GENERATION_SUPERSEDED,
        ));
    }

    // ADR-0074: the device gate, then the tombstone.
    require_publishable_device(&state.db, &account_id, &device_id, &session_device.0).await?;

    let kp_bytes = B64.decode(&payload.keypackage).map_err(|e| {
        RouteError::Text(
            StatusCode::BAD_REQUEST,
            format!("invalid keypackage base64: {e}"),
        )
    })?;
    if kp_bytes.is_empty() {
        return Err(RouteError::Text(
            StatusCode::BAD_REQUEST,
            "keypackage must be non-empty".to_string(),
        ));
    }
    if kp_bytes.len() > MAX_KEYPACKAGE_BYTES {
        return Err(RouteError::Text(
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
        RouteError::Text(
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
        return Err(RouteError::Text(
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

    tracing::info!(bytes = kp_bytes.len(), "keypackage upserted",);
    Ok(Json(UploadResponse { ok: true }))
}

// ============================================================================
// DELETE /discovery/keypackage/device/{device_id_b64url}   — one device's row
// ============================================================================

/// Verify the `x-lakko-account-statement` header of a delete of another
/// device's row: a `device-revoke` statement for `device_id`, signed by the
/// account's published identity key. Every failure (no header, bad base64, no
/// identity bundle, a malformed statement, another kind, another account,
/// outside the time window, a bad signature, another device) is the one
/// `403 account-key-required`.
async fn require_revoke_statement(
    db: &MySqlPool,
    headers: &HeaderMap,
    account_id: &[u8; ACCOUNT_ID_LEN],
    device_id: &[u8; DEVICE_ID_LEN],
) -> Result<(), RouteError> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    let refused = || {
        crate::key_reset::json_error(
            StatusCode::FORBIDDEN,
            crate::key_reset::CODE_ACCOUNT_KEY_REQUIRED,
        )
    };
    let statement = headers
        .get(ACCOUNT_STATEMENT_HEADER)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| B64URL.decode(v).ok())
        .ok_or_else(refused)?;
    let published: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT signature_pubkey FROM identity_bundles WHERE account_id = ?")
            .bind(&account_id[..])
            .fetch_optional(db)
            .await
            .map_err(db_error)?;
    let verifying = published
        .and_then(|bytes| VerifyingKey::from_bytes(&bytes).ok())
        .ok_or_else(refused)?;
    verify_account_statement(
        &statement,
        &verifying,
        AccountStatementKind::DeviceRevoke,
        account_id,
        &device_revoke_body(device_id),
        now_ms(),
    )
    .map_err(|e| {
        tracing::debug!(error = %e, "keypackage delete: account statement refused");
        refused()
    })?;
    Ok(())
}

/// `DELETE /discovery/keypackage/device/{device_id_b64url}` — remove one
/// device's KeyPackage for the authenticated account (ADR-0050 device
/// removal) and tombstone its id (ADR-0074 section 2.2).
///
/// Checks, in order: the path id decodes to 32 bytes (`400`); the ADR-0073
/// generation floor (`401 key-generation-superseded`: a session of a
/// superseded generation deletes nothing); then by whose row it is:
///
/// - the session's own device: nothing more;
/// - another device (or the session names no device): the
///   `x-lakko-account-statement` header, else `403 account-key-required`.
///
/// The delete and the tombstone are one transaction. Idempotent: a missing
/// row is a `204` too, and still (re)writes the tombstone.
pub(crate) async fn delete(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    axum::Extension(generation): axum::Extension<crate::auth::SessionKeyGeneration>,
    axum::Extension(session_device): axum::Extension<crate::auth::SessionDevice>,
    Path(device_id_b64url): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, RouteError> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    let bytes = B64URL.decode(&device_id_b64url).map_err(|e| {
        RouteError::Text(
            StatusCode::BAD_REQUEST,
            format!("invalid device_id base64url: {e}"),
        )
    })?;
    let device_id: [u8; DEVICE_ID_LEN] = bytes.as_slice().try_into().map_err(|_| {
        RouteError::Text(
            StatusCode::BAD_REQUEST,
            format!("device_id must be {DEVICE_ID_LEN} bytes after decode"),
        )
    })?;
    let account_id = authed.0;

    if crate::key_reset::floor_refuses(&state.db, &account_id, generation.0).await? {
        tracing::warn!(
            token_generation = generation.0,
            "keypackage delete refused — session token's key generation superseded",
        );
        return Err(crate::key_reset::json_error(
            StatusCode::UNAUTHORIZED,
            crate::key_reset::CODE_KEY_GENERATION_SUPERSEDED,
        ));
    }

    let own_device = device_id != UNBOUND_DEVICE_ID && device_id == session_device.0;
    if !own_device {
        require_revoke_statement(&state.db, &headers, &account_id, &device_id).await?;
    }

    let mut tx = state.db.begin().await.map_err(db_error)?;
    sqlx::query("DELETE FROM keypackages WHERE account_id = ? AND device_id = ?")
        .bind(&account_id[..])
        .bind(&device_id[..])
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    sqlx::query(
        "INSERT INTO keypackage_tombstones (account_id, device_id, deleted_at_ms) \
         VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE deleted_at_ms = GREATEST(deleted_at_ms, VALUES(deleted_at_ms))",
    )
    .bind(&account_id[..])
    .bind(&device_id[..])
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;

    tracing::info!("keypackage deleted and device id tombstoned",);
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

/// ADR-0074 sprint T1 route tests for the KeyPackage directory, driven
/// through the real router: the device gate, the tombstones, the delete gate
/// and the generation floor on the delete. DB-gated on
/// `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skip, not fail, when unset).
#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::anon_token_route_tests::{state, test_pool};
    use crate::key_reset::{
        CODE_ACCOUNT_KEY_REQUIRED, CODE_DEVICE_MISMATCH, CODE_DEVICE_REVOKED,
        CODE_KEY_GENERATION_SUPERSEDED,
    };
    use crate::{build_app, AppState};
    use axum::body::Body;
    use axum::http::Request;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    use http_body_util::BodyExt as _;
    use lakko_attest::{AccountStatement, KeyResetAttestation};
    use lakko_crypto::signing::SigningKey;
    use tower::ServiceExt as _;

    const DEVICE_A: [u8; 32] = [0xA1; 32];
    const DEVICE_B: [u8; 32] = [0xB2; 32];
    const DEVICE_C: [u8; 32] = [0xC3; 32];

    fn attest_key() -> SigningKey {
        SigningKey::from_seed(&[0x66; 32])
    }

    /// A fresh account id per test so parallel tests never share a row.
    fn fresh_account(tag: u8) -> [u8; 16] {
        let mut out = [tag; 16];
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        out[8..].copy_from_slice(&now.to_be_bytes());
        out
    }

    fn error_code(body: &str) -> String {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// A `device-revoke` statement for `device`, signed by `key` at `at`.
    fn statement(key: &SigningKey, account: &[u8; 16], device: &[u8; 32], at: i64) -> String {
        B64URL.encode(
            AccountStatement::new(
                AccountStatementKind::DeviceRevoke,
                *account,
                at,
                [0x5A; 16],
                device,
            )
            .sign(key),
        )
    }

    struct Client {
        app: axum::Router,
        state: AppState,
        pool: MySqlPool,
    }

    impl Client {
        fn new(pool: MySqlPool) -> Self {
            let mut state = state(pool.clone(), false);
            state.key_reset_attest_key = Some(attest_key().verifying_key());
            Self {
                app: build_app(state.clone()),
                state,
                pool,
            }
        }

        fn token(&self, account: &[u8; 16], generation: u64, device: &[u8; 32]) -> String {
            let now = now_ms();
            self.state
                .session_token_secret
                .issue(account, generation, device, now, now + 60_000)
                .unwrap()
        }

        async fn call(
            &self,
            method: &str,
            uri: &str,
            token: &str,
            statement: Option<&str>,
            body: serde_json::Value,
        ) -> (StatusCode, String) {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header("x-real-ip", "192.0.2.66")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"));
            if let Some(s) = statement {
                b = b.header(ACCOUNT_STATEMENT_HEADER, s);
            }
            let resp = self
                .app
                .clone()
                .oneshot(b.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }

        /// Publish the identity bundle with `key` as the account key.
        async fn publish_identity(
            &self,
            account: &[u8; 16],
            generation: u64,
            key: &SigningKey,
            extra: serde_json::Value,
        ) -> StatusCode {
            let mut body = serde_json::json!({
                "account_id": B64.encode(account),
                "signature_pubkey": B64.encode(key.verifying_key().to_bytes()),
                "x25519_pubkey": B64.encode([0x33u8; 32]),
            });
            if let (Some(dst), Some(src)) = (body.as_object_mut(), extra.as_object()) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
            self.call(
                "POST",
                "/discovery/identity",
                &self.token(account, generation, &DEVICE_A),
                None,
                body,
            )
            .await
            .0
        }

        /// `POST /discovery/keypackage` for `body_device` with `token`.
        async fn publish(
            &self,
            account: &[u8; 16],
            token: &str,
            body_device: &[u8; 32],
        ) -> (StatusCode, String) {
            self.call(
                "POST",
                "/discovery/keypackage",
                token,
                None,
                serde_json::json!({
                    "account_id": B64.encode(account),
                    "device_id": B64.encode(body_device),
                    "keypackage": B64.encode([body_device[0]; 64]),
                }),
            )
            .await
        }

        async fn delete(
            &self,
            token: &str,
            device: &[u8; 32],
            statement: Option<&str>,
        ) -> (StatusCode, String) {
            self.call(
                "DELETE",
                &format!("/discovery/keypackage/device/{}", B64URL.encode(device)),
                token,
                statement,
                serde_json::Value::Null,
            )
            .await
        }

        /// The device ids the directory serves for `account`.
        async fn listed(&self, account: &[u8; 16]) -> Vec<Vec<u8>> {
            sqlx::query_scalar(
                "SELECT device_id FROM keypackages WHERE account_id = ? ORDER BY device_id",
            )
            .bind(&account[..])
            .fetch_all(&self.pool)
            .await
            .unwrap()
        }

        async fn tombstones(&self, account: &[u8; 16]) -> i64 {
            sqlx::query_scalar("SELECT COUNT(*) FROM keypackage_tombstones WHERE account_id = ?")
                .bind(&account[..])
                .fetch_one(&self.pool)
                .await
                .unwrap()
        }

        async fn cleanup(&self, account: &[u8; 16]) {
            for sql in [
                "DELETE FROM identity_bundles WHERE account_id = ?",
                "DELETE FROM keypackages WHERE account_id = ?",
                "DELETE FROM keypackage_tombstones WHERE account_id = ?",
            ] {
                sqlx::query(sql)
                    .bind(&account[..])
                    .execute(&self.pool)
                    .await
                    .unwrap();
            }
        }
    }

    /// A session publishes only the device it names: not another device, and
    /// nothing at all while it names none.
    #[tokio::test]
    async fn a_session_publishes_only_its_own_device() {
        let Some(pool) = test_pool("a_session_publishes_only_its_own_device").await else {
            return;
        };
        let c = Client::new(pool);
        let acct = fresh_account(0x71);
        let a = c.token(&acct, 1, &DEVICE_A);
        let unbound = c.token(&acct, 1, &UNBOUND_DEVICE_ID);

        let (status, text) = c.publish(&acct, &a, &DEVICE_B).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert_eq!(error_code(&text), CODE_DEVICE_MISMATCH);

        for device in [DEVICE_A, UNBOUND_DEVICE_ID] {
            let (status, text) = c.publish(&acct, &unbound, &device).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
            assert_eq!(error_code(&text), CODE_DEVICE_MISMATCH);
        }
        assert!(c.listed(&acct).await.is_empty(), "no refusal wrote a row");

        let (status, text) = c.publish(&acct, &a, &DEVICE_A).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        // A replace by the same device is still an upsert.
        assert_eq!(c.publish(&acct, &a, &DEVICE_A).await.0, StatusCode::OK);
        assert_eq!(c.listed(&acct).await, vec![DEVICE_A.to_vec()]);

        // Another account's session naming the same device id is refused by
        // the account binding, before the device gate.
        let other = fresh_account(0x72);
        let (status, _) = c
            .publish(&acct, &c.token(&other, 1, &DEVICE_A), &DEVICE_A)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        c.cleanup(&acct).await;
    }

    /// The removal: a device that holds the account key deletes another
    /// device's row; the removed device cannot put itself back, cannot
    /// publish under a fresh key, and cannot delete the others' rows.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn a_removed_device_stays_out_of_the_directory() {
        let Some(pool) = test_pool("a_removed_device_stays_out_of_the_directory").await else {
            return;
        };
        let c = Client::new(pool);
        let acct = fresh_account(0x73);
        let key = SigningKey::from_seed(&[0x74; 32]);
        let a = c.token(&acct, 1, &DEVICE_A);
        let b = c.token(&acct, 1, &DEVICE_B);

        assert_eq!(
            c.publish_identity(&acct, 1, &key, serde_json::json!({}))
                .await,
            StatusCode::OK
        );
        assert_eq!(c.publish(&acct, &a, &DEVICE_A).await.0, StatusCode::OK);
        assert_eq!(c.publish(&acct, &b, &DEVICE_B).await.0, StatusCode::OK);
        assert_eq!(c.listed(&acct).await.len(), 2);

        // Deleting ANOTHER device's row with the session alone is refused:
        // no header, garbage, and every statement that does not verify.
        let other_key = SigningKey::from_seed(&[0x75; 32]);
        let other_acct = fresh_account(0x76);
        let now = now_ms();
        let wrong_kind = B64URL.encode(
            AccountStatement::new(
                AccountStatementKind::SessionUpgrade,
                acct,
                now,
                [0; 16],
                &DEVICE_B,
            )
            .sign(&key),
        );
        let refused: Vec<(&str, Option<String>)> = vec![
            ("no header", None),
            ("not base64url", Some("!!!".to_string())),
            ("wrong length", Some(B64URL.encode([1u8; 137]))),
            (
                "another key",
                Some(statement(&other_key, &acct, &DEVICE_B, now)),
            ),
            (
                "another device",
                Some(statement(&key, &acct, &DEVICE_A, now)),
            ),
            (
                "another account",
                Some(statement(&key, &other_acct, &DEVICE_B, now)),
            ),
            (
                "too old",
                Some(statement(&key, &acct, &DEVICE_B, now - 121_000)),
            ),
            (
                "from the future",
                Some(statement(&key, &acct, &DEVICE_B, now + 121_000)),
            ),
            ("another kind", Some(wrong_kind)),
        ];
        for (why, header) in &refused {
            let (status, text) = c.delete(&a, &DEVICE_B, header.as_deref()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{why}: {text}");
            assert_eq!(error_code(&text), CODE_ACCOUNT_KEY_REQUIRED, "{why}");
        }
        assert_eq!(c.listed(&acct).await.len(), 2, "no refusal deleted a row");
        assert_eq!(c.tombstones(&acct).await, 0, "no refusal wrote a tombstone");

        // With the account key: 204, the row is gone and the id tombstoned.
        let (status, text) = c
            .delete(
                &a,
                &DEVICE_B,
                Some(&statement(&key, &acct, &DEVICE_B, now_ms())),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
        assert_eq!(c.listed(&acct).await, vec![DEVICE_A.to_vec()]);
        assert_eq!(c.tombstones(&acct).await, 1);

        // The removed device's still-valid token cannot re-publish its row.
        let (status, text) = c.publish(&acct, &b, &DEVICE_B).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert_eq!(error_code(&text), CODE_DEVICE_REVOKED);
        // Nor publish under a fresh device key it minted from the seed.
        let (status, text) = c.publish(&acct, &b, &DEVICE_C).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert_eq!(error_code(&text), CODE_DEVICE_MISMATCH);
        // Nor delete another device's row with its token alone.
        let (status, text) = c.delete(&b, &DEVICE_A, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert_eq!(error_code(&text), CODE_ACCOUNT_KEY_REQUIRED);
        assert_eq!(c.listed(&acct).await, vec![DEVICE_A.to_vec()]);

        // The tombstoned id stays dead for a NEW session naming it too.
        let b_again = c.token(&acct, 1, &DEVICE_B);
        let (status, text) = c.publish(&acct, &b_again, &DEVICE_B).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert_eq!(error_code(&text), CODE_DEVICE_REVOKED);

        // The same device id under ANOTHER account is another device.
        assert_eq!(
            c.publish(&other_acct, &c.token(&other_acct, 1, &DEVICE_B), &DEVICE_B)
                .await
                .0,
            StatusCode::OK
        );

        // Deleting again is idempotent, and the kept device still publishes.
        let (status, _) = c
            .delete(
                &a,
                &DEVICE_B,
                Some(&statement(&key, &acct, &DEVICE_B, now_ms())),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(c.tombstones(&acct).await, 1);
        assert_eq!(c.publish(&acct, &a, &DEVICE_A).await.0, StatusCode::OK);

        // Only when the tombstone is older than the retention window is the
        // id publishable again.
        sqlx::query(
            "UPDATE keypackage_tombstones SET deleted_at_ms = ? \
             WHERE account_id = ? AND device_id = ?",
        )
        .bind(now_ms() - TOMBSTONE_RETENTION_MS + 60_000)
        .bind(&acct[..])
        .bind(&DEVICE_B[..])
        .execute(&c.pool)
        .await
        .unwrap();
        assert_eq!(
            c.publish(&acct, &b_again, &DEVICE_B).await.0,
            StatusCode::FORBIDDEN,
            "a minute inside the window"
        );
        sqlx::query(
            "UPDATE keypackage_tombstones SET deleted_at_ms = ? \
             WHERE account_id = ? AND device_id = ?",
        )
        .bind(now_ms() - TOMBSTONE_RETENTION_MS - 1)
        .bind(&acct[..])
        .bind(&DEVICE_B[..])
        .execute(&c.pool)
        .await
        .unwrap();
        assert_eq!(
            c.publish(&acct, &b_again, &DEVICE_B).await.0,
            StatusCode::OK
        );

        c.cleanup(&acct).await;
        c.cleanup(&other_acct).await;
    }

    /// A session deletes its own device's row with no statement, and the id
    /// is tombstoned like any other; an account with no published identity
    /// key cannot delete another device's row at all.
    #[tokio::test]
    async fn a_device_deletes_its_own_row_freely_and_that_is_all() {
        let Some(pool) = test_pool("a_device_deletes_its_own_row_freely_and_that_is_all").await
        else {
            return;
        };
        let c = Client::new(pool);
        let acct = fresh_account(0x77);
        let key = SigningKey::from_seed(&[0x78; 32]);
        let a = c.token(&acct, 1, &DEVICE_A);
        let b = c.token(&acct, 1, &DEVICE_B);
        let unbound = c.token(&acct, 1, &UNBOUND_DEVICE_ID);
        assert_eq!(c.publish(&acct, &a, &DEVICE_A).await.0, StatusCode::OK);
        assert_eq!(c.publish(&acct, &b, &DEVICE_B).await.0, StatusCode::OK);

        // No identity bundle: even a well-signed statement verifies under
        // nothing.
        let (status, text) = c
            .delete(
                &a,
                &DEVICE_B,
                Some(&statement(&key, &acct, &DEVICE_B, now_ms())),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert_eq!(error_code(&text), CODE_ACCOUNT_KEY_REQUIRED);

        // A session that names no device owns no row: it needs the statement
        // for any id, the all-zero one included.
        for device in [DEVICE_A, UNBOUND_DEVICE_ID] {
            let (status, text) = c.delete(&unbound, &device, None).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
            assert_eq!(error_code(&text), CODE_ACCOUNT_KEY_REQUIRED);
        }
        // A malformed path id is a 400.
        let (status, _) = c
            .call(
                "DELETE",
                "/discovery/keypackage/device/AAAA",
                &a,
                None,
                serde_json::Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(c.listed(&acct).await.len(), 2);

        // Its own row: no statement.
        let (status, text) = c.delete(&b, &DEVICE_B, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
        assert_eq!(c.listed(&acct).await, vec![DEVICE_A.to_vec()]);
        let (status, text) = c.publish(&acct, &b, &DEVICE_B).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert_eq!(error_code(&text), CODE_DEVICE_REVOKED);

        // The account erasure takes the tombstones with the rows.
        assert_eq!(c.tombstones(&acct).await, 1);
        let (status, _) = c
            .call(
                "DELETE",
                "/discovery/account",
                &a,
                None,
                serde_json::Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(c.tombstones(&acct).await, 0);
        assert!(c.listed(&acct).await.is_empty());
    }

    /// The delete is under the generation floor (the ADR-0073 residual), and
    /// an attested key change clears the account's tombstones with its rows.
    #[tokio::test]
    async fn the_delete_is_under_the_generation_floor_and_a_key_change_clears_tombstones() {
        let Some(pool) = test_pool(
            "the_delete_is_under_the_generation_floor_and_a_key_change_clears_tombstones",
        )
        .await
        else {
            return;
        };
        let c = Client::new(pool);
        let acct = fresh_account(0x79);
        let old_key = SigningKey::from_seed(&[0x7A; 32]);
        let new_key = SigningKey::from_seed(&[0x7B; 32]);
        let a1 = c.token(&acct, 1, &DEVICE_A);
        let b1 = c.token(&acct, 1, &DEVICE_B);

        assert_eq!(
            c.publish_identity(&acct, 1, &old_key, serde_json::json!({}))
                .await,
            StatusCode::OK
        );
        assert_eq!(c.publish(&acct, &a1, &DEVICE_A).await.0, StatusCode::OK);
        assert_eq!(c.publish(&acct, &b1, &DEVICE_B).await.0, StatusCode::OK);
        // Device C is removed under the old generation: a tombstone.
        let (status, _) = c
            .delete(
                &a1,
                &DEVICE_C,
                Some(&statement(&old_key, &acct, &DEVICE_C, now_ms())),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(c.tombstones(&acct).await, 1);

        // The account's keys are reset: an attested change at generation 2.
        let attestation = B64.encode(
            KeyResetAttestation::for_account(&acct, 2, new_key.verifying_key().to_bytes(), 1_000)
                .sign(&attest_key()),
        );
        assert_eq!(
            c.publish_identity(
                &acct,
                2,
                &new_key,
                serde_json::json!({ "key_reset_attest": attestation }),
            )
            .await,
            StatusCode::OK
        );
        assert!(
            c.listed(&acct).await.is_empty(),
            "the reset drops every row"
        );
        assert_eq!(c.tombstones(&acct).await, 0, "and every tombstone");

        let a2 = c.token(&acct, 2, &DEVICE_A);
        assert_eq!(c.publish(&acct, &a2, &DEVICE_A).await.0, StatusCode::OK);
        // An id tombstoned before the reset is publishable under the new
        // generation.
        let c2 = c.token(&acct, 2, &DEVICE_C);
        assert_eq!(c.publish(&acct, &c2, &DEVICE_C).await.0, StatusCode::OK);

        // A session of the old generation deletes nothing: not its own row
        // id, not another device's with a statement by the OLD account key.
        for (token, device, header) in [
            (&a1, DEVICE_A, None),
            (
                &b1,
                DEVICE_A,
                Some(statement(&old_key, &acct, &DEVICE_A, now_ms())),
            ),
            (
                &b1,
                DEVICE_C,
                Some(statement(&new_key, &acct, &DEVICE_C, now_ms())),
            ),
        ] {
            let (status, text) = c.delete(token, &device, header.as_deref()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");
            assert_eq!(error_code(&text), CODE_KEY_GENERATION_SUPERSEDED);
        }
        assert_eq!(c.listed(&acct).await.len(), 2);
        assert_eq!(c.tombstones(&acct).await, 0);

        // At the current generation the statement must be by the NEW key:
        // the old account key no longer removes devices here.
        let (status, text) = c
            .delete(
                &a2,
                &DEVICE_C,
                Some(&statement(&old_key, &acct, &DEVICE_C, now_ms())),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert_eq!(error_code(&text), CODE_ACCOUNT_KEY_REQUIRED);
        let (status, _) = c
            .delete(
                &a2,
                &DEVICE_C,
                Some(&statement(&new_key, &acct, &DEVICE_C, now_ms())),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(c.listed(&acct).await, vec![DEVICE_A.to_vec()]);

        c.cleanup(&acct).await;
    }
}
