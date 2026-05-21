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
//! ## Scope
//!
//! Three handlers — `upload`, `fetch`, `consume`. The atomic
//! `consume` enforces `max_uses` via the same CASE-expression
//! ceiling pattern `invite::consume` uses, PLUS per-joiner
//! dedup via the `call_invite_consumers` side table (3c3-
//! multi-use, mirror of `invite_consumers`). The dedup
//! `INSERT IGNORE` and the counter UPDATE live in a single
//! transaction so a stale dedup row never outlives a failed
//! bump. Joiner-tag computation reuses
//! `lakko_contacts::invite::compute_joiner_tag` — the salt
//! depends on `capability_bytes`, so contact-invite and
//! call-invite tags for the same joiner don't collide even
//! though the function is shared.
//!
//! ## Owner-signed revocation (3c3-revoke)
//!
//! `DELETE /discovery/call-invite/{blob_id}` runs the same
//! authorisation gate as the contact-invite revoke: the
//! body carries `signed_at_ms` + Ed25519 signature over
//! `call_revoke_canonical_bytes(blob_id, signed_at_ms)`,
//! and the server verifies against the embedded
//! `owner_signature_pubkey` recovered from the stored
//! capability. Cross-kind isolation is guaranteed by the
//! domain separator
//! ([`CALL_INVITE_REVOKE_DOMAIN_V1`](lakko_contacts::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1)):
//! a captured contact-invite revoke signature cannot be
//! replayed against a call-invite blob (or vice versa).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use lakko_contacts::call_invite::{
    call_revoke_canonical_bytes, decode_and_verify, CALL_ID_LEN, CALL_REVOKE_REPLAY_WINDOW_MS,
};
use lakko_contacts::invite::{BLOB_ID_LEN, JOINER_TAG_LEN};
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
    /// Standard-alphabet base64 of the 32-byte joiner_tag.
    /// Joiner computes via `lakko_contacts::invite::compute_joiner_tag(
    /// capability_bytes, joiner_account_id)` (the same helper
    /// the contact-invite ecosystem uses — salt is per-blob so
    /// tags don't collide across invite kinds).
    pub(crate) joiner_tag: String,
}

#[derive(Serialize)]
pub(crate) struct ConsumeResponse {
    /// Post-increment `used_count` — OR the existing value
    /// when `already_consumed` is `true` (no bump on repeat).
    pub(crate) used_count: u32,
    /// Echo of the row's `max_uses`.
    pub(crate) max_uses: u32,
    /// `true` when the `(blob_id, joiner_tag)` PK on
    /// `call_invite_consumers` already had a row. The UI
    /// surfaces "you've already accepted this invite" copy
    /// and skips the MLS bootstrap.
    pub(crate) already_consumed: bool,
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
/// Per-joiner dedup landed in 3c3-multi-use via the
/// `call_invite_consumers` side table (PK on `(blob_id,
/// joiner_tag)`, FK CASCADE on `call_invites.blob_id`). The
/// consume handler runs an `INSERT IGNORE` against the table
/// inside the same transaction as the counter bump; a repeat
/// from the same joiner returns 200 with `already_consumed =
/// true` and does NOT bump the counter. Joiner-tag
/// construction is server-blind (`lakko_contacts::invite::
/// compute_joiner_tag` — HMAC over a per-blob salt); the
/// server stores opaque 32-byte tags only.
#[allow(clippy::too_many_lines)] // single-transaction lifecycle
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

    let joiner_tag = B64.decode(&payload.joiner_tag).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid joiner_tag base64: {e}"),
        )
    })?;
    if joiner_tag.len() != JOINER_TAG_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("joiner_tag must be {JOINER_TAG_LEN} bytes after decode"),
        ));
    }

    let now_ms = current_unix_ms();

    // Single transaction — dedup-insert + counter bump must
    // live or die together. Same shape as `invite::consume`.
    let mut tx = state.db.begin().await.map_err(|e| {
        tracing::error!(error = %e, "consume call_invites BEGIN");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    // Step 1 — dedup insert. INSERT IGNORE lets us
    // discriminate "first time" from "already consumed" by
    // `rows_affected`. The FK to `call_invites.blob_id`
    // catches "no such blob" — if the parent row doesn't
    // exist, this insert fails with a foreign-key violation
    // which we map to 404.
    let insert_result =
        sqlx::query("INSERT IGNORE INTO call_invite_consumers (blob_id, joiner_tag) VALUES (?, ?)")
            .bind(&blob_id[..])
            .bind(&joiner_tag[..])
            .execute(&mut *tx)
            .await;

    let insert_rows = match insert_result {
        Ok(r) => r.rows_affected(),
        Err(e) => {
            let _ = tx.rollback().await;
            let msg = format!("{e}");
            if msg.contains("foreign key constraint") {
                return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
            }
            tracing::error!(error = %e, "consume call_invite_consumers INSERT");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            ));
        },
    };

    if insert_rows == 0 {
        // Already consumed by this joiner. Read back the
        // current counter + ceiling so the UI can render
        // consistent copy.
        let row: Option<(u32, u32)> =
            sqlx::query_as("SELECT used_count, max_uses FROM call_invites WHERE blob_id = ?")
                .bind(&blob_id[..])
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, "consume call_invites already-consumed readback");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal db error".to_string(),
                    )
                })?;
        tx.commit().await.map_err(|e| {
            tracing::error!(error = %e, "consume call_invites already-consumed COMMIT");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;
        let (used_count, max_uses) =
            row.ok_or((StatusCode::NOT_FOUND, "call-invite not found".to_string()))?;
        return Ok(Json(ConsumeResponse {
            used_count,
            max_uses,
            already_consumed: true,
        }));
    }

    // Step 2 — atomic conditional UPDATE on the parent row.
    // Same CASE-expression sentinel mapping as before.
    let update_result = sqlx::query(
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
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "consume call_invites UPDATE");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    if update_result.rows_affected() == 0 {
        // Counter didn't bump → either expired or
        // exhausted. Roll back so the dedup-insert from
        // step 1 is undone (this joiner can re-attempt
        // later — though `max_uses` is static at compose
        // time, so realistically this only matters if the
        // invite was concurrently consumed past the
        // ceiling by other joiners during step 2's window).
        let _ = tx.rollback().await;

        // Disambiguate "expired" vs "exhausted" via a
        // SELECT outside the rolled-back transaction.
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
            return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
        };
        if expiry_ms != 0 && now_ms >= expiry_ms {
            return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
        }
        return Err((
            StatusCode::GONE,
            "call-invite max_uses exhausted".to_string(),
        ));
    }

    // Re-SELECT inside the transaction so the readback
    // reflects the bumped counter.
    let (used_count, max_uses): (u32, u32) =
        sqlx::query_as("SELECT used_count, max_uses FROM call_invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "consume call_invites readback");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    tx.commit().await.map_err(|e| {
        tracing::error!(error = %e, "consume call_invites COMMIT");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    Ok(Json(ConsumeResponse {
        used_count,
        max_uses,
        already_consumed: false,
    }))
}

// ============================================================================
// DELETE /discovery/call-invite/{blob_id} — owner-signed revocation
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct RevokeRequest {
    /// UNIX millis at which the body was signed. Must be
    /// within `CALL_REVOKE_REPLAY_WINDOW_MS` of the server
    /// clock.
    pub(crate) signed_at_ms: u64,
    /// Standard-alphabet base64 of the 64-byte Ed25519
    /// signature over
    /// `lakko_contacts::call_invite::call_revoke_canonical_bytes`.
    pub(crate) signature: String,
}

/// `DELETE /discovery/call-invite/{blob_id}` — owner-signed
/// delete. Mirror of `invite::revoke` for the call-invite
/// ecosystem. Same response model:
///
///   * **204 No Content** — row deleted (idempotent on success).
///   * **404 Not Found** — folded over "no row" /
///     "signature did not verify" / "owner pubkey could not
///     be recovered". Denies attacker status-code probing.
///   * **400 Bad Request** — body shape failures (bad
///     base64, wrong-sized signature, `signed_at_ms`
///     outside the replay window).
///
/// Cross-kind isolation: the canonical bytes use
/// `CALL_INVITE_REVOKE_DOMAIN_V1` instead of the contact-
/// invite domain, so a captured contact-invite revoke
/// signature cannot be replayed here.
#[allow(clippy::too_many_lines)] // single-shot lifecycle —
                                 // splitting the body shape checks into helpers would scatter
                                 // the error-mapping across functions; matches `invite::revoke`
                                 // posture which uses the same shape.
pub(crate) async fn revoke(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
    // Session-token-gated via route_layer in `main.rs`.
    // Ed25519 signature on the body proves ownership of the
    // capability; the session token proves "logged-in
    // Lakko user" — together strictly stronger than either
    // alone.
    axum::Extension(_authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<RevokeRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
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

    // Replay window check before any DB work — cheap DoS
    // gate.
    let now_ms = current_unix_ms();
    if !within_replay_window(payload.signed_at_ms, now_ms) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "signed_at_ms {} outside the ±{}ms replay window (now = {})",
                payload.signed_at_ms, CALL_REVOKE_REPLAY_WINDOW_MS, now_ms,
            ),
        ));
    }

    let signature_bytes = B64.decode(&payload.signature).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid signature base64: {e}"),
        )
    })?;
    if signature_bytes.len() != lakko_crypto::signing::SIGNATURE_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "signature must be {} bytes",
                lakko_crypto::signing::SIGNATURE_LEN,
            ),
        ));
    }
    let signature = lakko_crypto::signing::Signature::from_bytes(&signature_bytes)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("malformed signature: {e}")))?;

    // Recover the owner's verifying key from the stored
    // capability. "No row" folds into 404 (see doc comment).
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT capability_bytes FROM call_invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "revoke call_invites SELECT");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;
    let Some((capability_bytes,)) = row else {
        return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
    };

    let cap = match decode_and_verify(&capability_bytes) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(
                error = %e,
                blob_id = %blob_id_b64url,
                "revoke: stored call-invite did not re-verify",
            );
            return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
        },
    };

    let mut blob_id_arr = [0_u8; BLOB_ID_LEN];
    blob_id_arr.copy_from_slice(&blob_id);
    let canonical = call_revoke_canonical_bytes(&blob_id_arr, payload.signed_at_ms);
    if cap
        .owner_signature_pubkey
        .verify(&canonical, &signature)
        .is_err()
    {
        // Signature mismatch — fold into 404.
        return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
    }

    // Authorised. Delete the row — the FK CASCADE on
    // `call_invite_consumers` reaps the dedup side-table
    // rows for free.
    sqlx::query("DELETE FROM call_invites WHERE blob_id = ?")
        .bind(&blob_id[..])
        .execute(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "revoke call_invites DELETE");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    tracing::info!(
        blob_id = %blob_id_b64url,
        signed_at_ms = payload.signed_at_ms,
        "call-invite revoked",
    );
    Ok(StatusCode::NO_CONTENT)
}

// ============================================================================
// Helpers
// ============================================================================

fn within_replay_window(signed_at_ms: u64, now_ms: u64) -> bool {
    let lo = now_ms.saturating_sub(CALL_REVOKE_REPLAY_WINDOW_MS);
    let hi = now_ms.saturating_add(CALL_REVOKE_REPLAY_WINDOW_MS);
    signed_at_ms >= lo && signed_at_ms <= hi
}

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
