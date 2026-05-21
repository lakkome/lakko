// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Capability-based call-invite blob storage (Phase 3c3-server).
//!
//! Sibling of `invite.rs` for `lakko_contacts::CallInviteCapability`.
//! Owners POST verified bytes; joiners GET by content-addressed id.
//! Closes G-21 / G-22 from `docs/security/known-gaps.md` by giving
//! the multi-joiner + max_uses surface a real server-side row.
//!
//! ## Why a separate module from `invite.rs`
//!
//! The two capability schemas have different sizes, different
//! lifecycles (contact-invite seeds a `ContactRequest`; call-invite
//! bootstraps a fresh MLS group), and different signing key
//! verification details. Mirroring the structure but keeping the
//! file separate keeps each module's call graph linear + auditable.
//!
//! ## V1 scope
//!
//! Three handlers — `upload`, `fetch`, `consume`. The atomic
//! `consume` enforces `max_uses` via the same CASE-expression
//! ceiling pattern `invite::consume` uses, minus the
//! `joiner_tag`-based dedup side table. Per-joiner dedup for call
//! invites is the 3c3-server-followup sprint; V1 still gives the
//! basic anti-spam ceiling so a single capability can't be
//! consumed past its budget.
//!
//! ## Why no owner-revoke endpoint yet
//!
//! The contact-invite ecosystem includes `DELETE /discovery/invite/
//! {blob_id}` (1.18d2b) for owner-signed revocation. For call
//! invites the same primitive applies but the wire shape +
//! `revoke_canonical_bytes` analogue would need to live in
//! `lakko_contacts::call_invite` first. V1 ships without it; the
//! ephemeral nature of call invites (typically short expiry)
//! makes the gap less acute than for contact invites.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use lakko_contacts::call_invite::{decode_and_verify, CALL_ID_LEN};
use lakko_contacts::invite::BLOB_ID_LEN;
use lakko_contacts::ACCOUNT_ID_LEN;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AppState;

/// Server-side ceiling on the capability size. Pinned to the
/// `VARBINARY(8192)` column width in
/// `migrations/0007_call_invites.sql`. The 3c3 wire format
/// tops out around 4308 bytes with a 4 KiB embedded KP; the
/// headroom covers future `transparency_proof` or extra-bits
/// extensions.
const MAX_CAPABILITY_BYTES: usize = 8192;

/// Sentinel for "unlimited" `max_uses` — matches
/// `lakko_contacts::call_invite::CALL_SINGLE_USE` mapping
/// in the SQL CASE expression below.
const MAX_USES_UNLIMITED: u32 = u32::MAX;

// ============================================================================
// POST /discovery/call-invite — upload + verify + content-address
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    pub(crate) pow: crate::pow::PowProof,
    /// Standard-alphabet base64 of `lakko_contacts::call_invite::compose(...)`
    /// output. Server decodes + verifies the wire signature
    /// before storing.
    pub(crate) capability: String,
}

#[derive(Serialize)]
pub(crate) struct UploadResponse {
    /// base64url-no-pad of the SHA-256 of `capability_bytes`.
    /// Same bytes ⇒ same id; a retried upload is idempotent.
    /// The web client substitutes this directly into the
    /// `/c/<blob_id>` URL.
    pub(crate) blob_id: String,
}

/// `POST /discovery/call-invite` — upsert a verified call-invite
/// capability.
pub(crate) async fn upload(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let bytes = B64.decode(&payload.capability).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid capability base64: {e}"),
        )
    })?;
    if bytes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "capability must be non-empty".to_string(),
        ));
    }
    if bytes.len() > MAX_CAPABILITY_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "capability exceeds the {MAX_CAPABILITY_BYTES}-byte ceiling \
                 (got {} bytes)",
                bytes.len()
            ),
        ));
    }

    // Verify the wire signature server-side — same posture as
    // `invite::upload`. Rejects forged / tampered payloads at
    // ingress and gives us the parsed struct fields for the
    // denormalised index columns.
    let cap = decode_and_verify(&bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("call-invite did not verify: {e}"),
        )
    })?;

    let blob_id = blob_id_for(&bytes);
    let owner_account_id = cap.owner_account_id;

    // Anti-impersonation — the capability's embedded
    // `owner_account_id` must match the token's. Without this
    // check, an authenticated user could upload a call-invite
    // whose owner field is someone else's account.
    if owner_account_id != authed.0 {
        tracing::warn!(
            owner_account_id = %B64.encode(owner_account_id),
            token_account_id = %B64.encode(authed.0),
            "call-invite upload: capability owner_account_id mismatch"
        );
        return Err((
            StatusCode::FORBIDDEN,
            "capability owner_account_id mismatch with session token".to_string(),
        ));
    }

    let expiry_ms: u64 = cap.expiry_ms;
    let max_uses: u32 = cap.max_uses;
    let call_id: [u8; CALL_ID_LEN] = cap.call_id;

    sqlx::query(
        "INSERT INTO call_invites \
            (blob_id, capability_bytes, owner_account_id, call_id, expiry_ms, max_uses) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE \
            capability_bytes = VALUES(capability_bytes), \
            max_uses = VALUES(max_uses)",
    )
    .bind(&blob_id[..])
    .bind(&bytes)
    .bind(&owner_account_id[..])
    .bind(&call_id[..])
    .bind(expiry_ms)
    .bind(max_uses)
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "upsert call_invites");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    let blob_id_b64url = B64URL.encode(blob_id);
    tracing::info!(
        blob_id = %blob_id_b64url,
        owner_account_id = %B64.encode(owner_account_id),
        call_id = %B64.encode(call_id),
        expiry_ms = expiry_ms,
        bytes = bytes.len(),
        "call-invite upserted",
    );
    Ok(Json(UploadResponse {
        blob_id: blob_id_b64url,
    }))
}

// ============================================================================
// GET /discovery/call-invite/{blob_id} — fetch by content-addressed id
// ============================================================================

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    /// Standard-alphabet base64 of the stored capability bytes.
    /// Client re-hashes + re-verifies locally before consuming.
    pub(crate) capability: String,
}

/// `GET /discovery/call-invite/{blob_id}` — fetch by id.
///
/// 404 on missing or expired rows (server-side expiry filter
/// folded into 404 to deny status-code probing).
pub(crate) async fn fetch(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
) -> Result<Json<FetchResponse>, (StatusCode, String)> {
    let blob_id = B64URL.decode(&blob_id_b64url).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid blob_id base64url: {e}"),
        )
    })?;
    if blob_id.len() != BLOB_ID_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("blob_id must be {BLOB_ID_LEN} bytes after decode"),
        ));
    }

    let row: Option<(Vec<u8>, u64)> =
        sqlx::query_as("SELECT capability_bytes, expiry_ms FROM call_invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "fetch call_invites");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    let (capability_bytes, expiry_ms) =
        row.ok_or((StatusCode::NOT_FOUND, "call-invite not found".to_string()))?;

    if expiry_ms != 0 && current_unix_ms() >= expiry_ms {
        // Expired rows fold into 404 to deny attacker
        // status-code probing — same posture as `invite::fetch`.
        return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
    }

    Ok(Json(FetchResponse {
        capability: B64.encode(&capability_bytes),
    }))
}

// ============================================================================
// POST /discovery/call-invite/{blob_id}/consume — atomic max_uses bump
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct ConsumeRequest {
    pub(crate) pow: crate::pow::PowProof,
}

#[derive(Serialize)]
pub(crate) struct ConsumeResponse {
    /// Post-increment `used_count`.
    pub(crate) used_count: u32,
    /// Echo of the row's `max_uses` so the client can render
    /// "N of M uses" UI copy.
    pub(crate) max_uses: u32,
}

/// `POST /discovery/call-invite/{blob_id}/consume` — register
/// one consumption against the invite's `max_uses` ceiling.
///
/// The atomic UPDATE bumps `used_count` only when the row has
/// remaining budget. The CASE expression maps the wire-format
/// sentinels to SQL-comparable ceilings (matches the contact-
/// invite consume handler):
///
///   * `max_uses == 0`          → effective ceiling 1 (CALL_SINGLE_USE)
///   * `max_uses == 0xFFFFFFFF` → effective ceiling u32::MAX (unlimited)
///   * any other N              → effective ceiling N
///
/// HTTP outcomes:
///
///   * **200 OK** — fresh consume, counter bumped.
///   * **404 Not Found** — no row OR expired.
///   * **410 Gone** — budget exhausted.
///
/// V1 does NOT include per-joiner dedup (joiner-tag side
/// table). That's a 3c3-server-followup sprint — see G-21 in
/// `docs/security/known-gaps.md`. Multi-joiner repeats from
/// the same account count against the budget like distinct
/// joiners do; this is acceptable for V1 because the call-
/// invite lifecycle is much shorter than a contact invite.
pub(crate) async fn consume(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
    Json(payload): Json<ConsumeRequest>,
) -> Result<Json<ConsumeResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let blob_id = B64URL.decode(&blob_id_b64url).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid blob_id base64url: {e}"),
        )
    })?;
    if blob_id.len() != BLOB_ID_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("blob_id must be {BLOB_ID_LEN} bytes after decode"),
        ));
    }

    let now_ms = current_unix_ms();

    // Atomic conditional UPDATE — the CASE expression maps the
    // wire-format sentinels to SQL-comparable ceilings. Same
    // shape as `invite::consume`.
    let result = sqlx::query(
        "UPDATE call_invites \
         SET used_count = used_count + 1 \
         WHERE blob_id = ? \
           AND (expiry_ms = 0 OR expiry_ms > ?) \
           AND used_count < CASE \
                 WHEN max_uses = 0 THEN 1 \
                 WHEN max_uses = ? THEN ? \
                 ELSE max_uses \
               END",
    )
    .bind(&blob_id[..])
    .bind(now_ms)
    .bind(MAX_USES_UNLIMITED)
    .bind(MAX_USES_UNLIMITED)
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "consume call_invites UPDATE");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    if result.rows_affected() == 0 {
        // Either the row doesn't exist, it's expired, OR
        // the budget is exhausted. Disambiguate by SELECTing.
        let row: Option<(u64, u32, u32)> = sqlx::query_as(
            "SELECT expiry_ms, max_uses, used_count FROM call_invites WHERE blob_id = ?",
        )
        .bind(&blob_id[..])
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "consume call_invites disambiguate SELECT");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;
        let Some((expiry_ms, _max_uses, _used_count)) = row else {
            // No row at all.
            return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
        };
        if expiry_ms != 0 && now_ms >= expiry_ms {
            // Expired — fold into 404 same as `fetch`.
            return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
        }
        // Row exists, not expired → budget exhausted.
        return Err((
            StatusCode::GONE,
            "call-invite max_uses exhausted".to_string(),
        ));
    }

    // Re-SELECT to surface the post-bump counter + the static
    // ceiling. This is a second query but it's the same row
    // that just got UPDATEd, so it's a single buffer-pool hit.
    let (used_count, max_uses): (u32, u32) =
        sqlx::query_as("SELECT used_count, max_uses FROM call_invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_one(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "consume call_invites readback");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    Ok(Json(ConsumeResponse {
        used_count,
        max_uses,
    }))
}

// ============================================================================
// Helpers
// ============================================================================

fn current_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn blob_id_for(capability_bytes: &[u8]) -> [u8; BLOB_ID_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(capability_bytes);
    let digest = hasher.finalize();
    let mut out = [0_u8; BLOB_ID_LEN];
    out.copy_from_slice(&digest);
    out
}

// Statically prove the constants align with the imports we
// don't otherwise read at runtime.
const _: () = {
    let _ = ACCOUNT_ID_LEN; // imported for cross-module symmetry
};
