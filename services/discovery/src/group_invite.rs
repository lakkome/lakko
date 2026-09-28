// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group join-ticket blob ledger (ADR-0028, G-3).
//!
//! Server-side anti-abuse ledger for `GroupInviteCapability` links — the group
//! analogue of [`crate::call_invite`]. An owner/admin POSTs the verified link
//! bytes; a prospective joiner GETs them by a random server-minted id (the
//! `/g/<blob_id>` pointer form, the indirection over the inline
//! `/g/<base64url>` link of ADR-0028 Phase 1-3) and registers one consume
//! against the link's soft `max_uses` ceiling.
//!
//! Routes:
//!
//! - `POST /discovery/group-invite`                       — upload + verify + mint a random id (session-gated).
//! - `GET  /discovery/group-invite/{blob_id}`             — fetch by id (open, per-IP rate-limited).
//! - `POST /discovery/group-invite/{blob_id}/consume`     — atomic per-joiner consume (open).
//!
//! ## The ledger is NOT the security boundary
//!
//! ADR-0028 puts the security boundary at the admin approval gate (ADR-0027
//! role-gated, peer-verified): a leaked link lets a stranger *ask* to join,
//! never *in*. So `max_uses` here is a SOFT anti-spam knob — how many strangers
//! may queue an external-join request — not an access control. It is NOT part
//! of the signed `GroupInviteCapability` wire (which carries only `group_id` +
//! `issuer_signature_pubkey` + `expiry_ms`); the uploader supplies it in the
//! POST body, and lying about it only changes how many requests reach the
//! admin, who still approves each one individually.
//!
//! ## No owner-account binding, no revoke (v1)
//!
//! Unlike a call-invite, a group-invite carries no `owner_account_id` — the
//! issuer is identified solely by its Ed25519 verifying key. So upload has no
//! anti-impersonation account-match check (there is no account field to match);
//! the session-token gate is a pure DoS / "logged-in user" rate-limit. Owner-
//! signed revocation is deferred — `GroupInviteCapability` has no revoke domain
//! yet; expiry + the periodic pruner are the v1 lifecycle controls.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use lakko_contacts::group_invite::{
    decode_and_verify, group_revoke_canonical_bytes, GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS,
};
use lakko_contacts::invite::JOINER_TAG_LEN;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Server-side ceiling on the capability size. Pinned to the `VARBINARY(512)`
/// column width in `migrations/0014_group_invites.sql`. The ADR-0028 wire
/// format tops out near 363 bytes (1 schema + 2 gid_len + 256 gid + 32
/// issuer_pubkey + 8 expiry + 64 signature); the headroom covers a future
/// schema bump.
const MAX_CAPABILITY_BYTES: usize = 512;

/// Sentinel for "unlimited" `max_uses` — matches the SQL CASE expression in the
/// consume handler.
const MAX_USES_UNLIMITED: u32 = u32::MAX;

// ============================================================================
// POST /discovery/group-invite — upload + verify + mint a random id
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    pub(crate) pow: crate::pow::PowProof,
    /// Standard-alphabet base64 of `lakko_contacts::group_invite::compose(...)`
    /// output. The server decodes + verifies the wire signature before storing.
    pub(crate) capability: String,
    /// SOFT consume ceiling — `0` = single-use, `0xFFFFFFFF` = unlimited, any
    /// other N = at most N consumes. NOT part of the signed capability and NOT
    /// a security boundary (see the module docs): it is the issuer's own
    /// anti-spam throttle on how many strangers may queue a request.
    pub(crate) max_uses: u32,
}

#[derive(Serialize)]
pub(crate) struct UploadResponse {
    /// base64url-no-pad of the random [`lakko_contacts::invite::BLOB_ID_LEN`]-byte
    /// ledger id (8 characters), minted fresh for this upload: the same
    /// capability uploaded twice gets two ids. The client substitutes this
    /// directly into the `/g/<blob_id>` URL.
    pub(crate) blob_id: String,
}

/// `POST /discovery/group-invite` — store a verified group-invite capability under a fresh id.
pub(crate) async fn upload(
    State(state): State<AppState>,
    // Session-token-gated via the route_layer in `main.rs`. There is no
    // account-to-capability binding to enforce (the group-invite carries no
    // owner_account_id), so the token is a pure DoS / logged-in-user gate; we
    // log the uploader for audit only.
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
                "capability exceeds the {MAX_CAPABILITY_BYTES}-byte ceiling (got {} bytes)",
                bytes.len()
            ),
        ));
    }

    // Verify the wire signature server-side — rejects forged / tampered
    // payloads at ingress and gives us the parsed fields for the denormalised
    // index columns.
    let cap = decode_and_verify(&bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("group-invite did not verify: {e}"),
        )
    })?;

    let issuer_pubkey = cap.issuer_signature_pubkey.to_bytes();
    let group_id = cap.group_id;
    let expiry_ms: u64 = cap.expiry_ms;
    let max_uses: u32 = payload.max_uses;

    // A random id, never an upsert: a collision is retried under a fresh id
    // and can never overwrite another link's row (the short-link amendment
    // of ADR-0028, see `crate::ledger_id`).
    let db = state.db.clone();
    let blob_id = crate::ledger_id::insert_with_fresh_id("group_invites", |id| {
        let db = db.clone();
        let bytes = bytes.clone();
        let group_id = group_id.clone();
        async move {
            sqlx::query(
                "INSERT INTO group_invites \
                    (blob_id, capability_bytes, issuer_pubkey, group_id, expiry_ms, max_uses) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(id.to_vec())
            .bind(bytes)
            .bind(issuer_pubkey.to_vec())
            .bind(group_id)
            .bind(expiry_ms)
            .bind(max_uses)
            .execute(&db)
            .await
        }
    })
    .await?;

    let blob_id_b64url = B64URL.encode(blob_id);
    tracing::info!(
        blob_id = %blob_id_b64url,
        uploader_account_id = %B64.encode(authed.0),
        issuer_pubkey = %B64.encode(issuer_pubkey),
        expiry_ms = expiry_ms,
        max_uses = max_uses,
        bytes = bytes.len(),
        "group-invite stored",
    );
    Ok(Json(UploadResponse {
        blob_id: blob_id_b64url,
    }))
}

// ============================================================================
// GET /discovery/group-invite/{blob_id} — fetch by id
// ============================================================================

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    /// Standard-alphabet base64 of the stored capability bytes. The client
    /// re-verifies the signature locally before requesting to join, and a link
    /// carrying a preview secret opens the preview only on the capability it
    /// was minted for (the preview key binds the group id and issuer).
    pub(crate) capability: String,
}

/// `GET /discovery/group-invite/{blob_id}` — fetch by id.
///
/// 404 on missing or expired rows (the server-side expiry filter is folded into
/// 404 to deny status-code probing).
pub(crate) async fn fetch(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
) -> Result<Json<FetchResponse>, (StatusCode, String)> {
    let blob_id = crate::ledger_id::parse_blob_id(&blob_id_b64url)?;

    let row: Option<(Vec<u8>, u64)> =
        sqlx::query_as("SELECT capability_bytes, expiry_ms FROM group_invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "fetch group_invites");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    let (capability_bytes, expiry_ms) =
        row.ok_or((StatusCode::NOT_FOUND, "group-invite not found".to_string()))?;

    if expiry_ms != 0 && current_unix_ms() >= expiry_ms {
        // Expired rows fold into 404 to deny attacker status-code probing.
        return Err((StatusCode::NOT_FOUND, "group-invite not found".to_string()));
    }

    Ok(Json(FetchResponse {
        capability: B64.encode(&capability_bytes),
    }))
}

// ============================================================================
// POST /discovery/group-invite/{blob_id}/consume — atomic per-joiner consume
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct ConsumeRequest {
    pub(crate) pow: crate::pow::PowProof,
    /// Standard-alphabet base64 of the 32-byte joiner_tag. The joiner computes
    /// it via `lakko_contacts::invite::compute_joiner_tag(capability_bytes,
    /// joiner_account_id)` — the same helper the contact- and call-invite
    /// ecosystems use; the salt is per-blob so tags don't collide across invite
    /// kinds, and the server only ever stores the opaque tag (server-blind).
    pub(crate) joiner_tag: String,
}

#[derive(Serialize)]
pub(crate) struct ConsumeResponse {
    /// Post-increment `used_count` — OR the existing value when
    /// `already_consumed` is `true` (no bump on a repeat).
    pub(crate) used_count: u32,
    /// Echo of the row's `max_uses`.
    pub(crate) max_uses: u32,
    /// `true` when the `(blob_id, joiner_tag)` PK on `group_invite_consumers`
    /// already had a row. The client surfaces "you've already requested to join
    /// this group" copy and skips re-posting the external-join proposal.
    pub(crate) already_consumed: bool,
}

/// `POST /discovery/group-invite/{blob_id}/consume` — register one consumption
/// against the link's `max_uses` ceiling.
///
/// The atomic UPDATE bumps `used_count` only when the row has remaining budget.
/// The CASE expression maps the wire-format sentinels to SQL-comparable
/// ceilings (identical to the call-invite consume handler):
///
///   * `max_uses == 0`          -> effective ceiling 1 (single-use)
///   * `max_uses == 0xFFFFFFFF` -> effective ceiling u32::MAX (unlimited)
///   * any other N              -> effective ceiling N
///
/// HTTP outcomes:
///
///   * **200 OK** — fresh consume, counter bumped (or a dedup repeat with
///     `already_consumed = true`).
///   * **404 Not Found** — no row OR expired.
///   * **410 Gone** — budget exhausted.
///
/// Per-joiner dedup uses the `group_invite_consumers` side table (PK on
/// `(blob_id, joiner_tag)`, FK CASCADE on `group_invites.blob_id`) inside the
/// same transaction as the counter bump; a repeat from the same joiner returns
/// 200 with `already_consumed = true` and does NOT bump the counter.
#[allow(clippy::too_many_lines)] // single-transaction lifecycle — mirrors call_invite::consume
pub(crate) async fn consume(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
    Json(payload): Json<ConsumeRequest>,
) -> Result<Json<ConsumeResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let blob_id = crate::ledger_id::parse_blob_id(&blob_id_b64url)?;

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

    // Single transaction — dedup-insert + counter bump must live or die
    // together. Same shape as `call_invite::consume`.
    let mut tx = state.db.begin().await.map_err(|e| {
        tracing::error!(error = %e, "consume group_invites BEGIN");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    // Step 1 — dedup insert. INSERT IGNORE lets us discriminate "first time"
    // from "already consumed" by `rows_affected`. The FK to
    // `group_invites.blob_id` catches "no such blob" — if the parent row
    // doesn't exist, this insert fails with a foreign-key violation which we map
    // to 404.
    let insert_result = sqlx::query(
        "INSERT IGNORE INTO group_invite_consumers (blob_id, joiner_tag) VALUES (?, ?)",
    )
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
                return Err((StatusCode::NOT_FOUND, "group-invite not found".to_string()));
            }
            tracing::error!(error = %e, "consume group_invite_consumers INSERT");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            ));
        },
    };

    if insert_rows == 0 {
        // Already consumed by this joiner. Read back the current counter +
        // ceiling so the client can render consistent copy.
        let row: Option<(u32, u32)> =
            sqlx::query_as("SELECT used_count, max_uses FROM group_invites WHERE blob_id = ?")
                .bind(&blob_id[..])
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, "consume group_invites already-consumed readback");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal db error".to_string(),
                    )
                })?;
        tx.commit().await.map_err(|e| {
            tracing::error!(error = %e, "consume group_invites already-consumed COMMIT");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;
        let (used_count, max_uses) =
            row.ok_or((StatusCode::NOT_FOUND, "group-invite not found".to_string()))?;
        return Ok(Json(ConsumeResponse {
            used_count,
            max_uses,
            already_consumed: true,
        }));
    }

    // Step 2 — atomic conditional UPDATE on the parent row. Same CASE-expression
    // sentinel mapping as the call-invite consume handler.
    let update_result = sqlx::query(
        "UPDATE group_invites \
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
        tracing::error!(error = %e, "consume group_invites UPDATE");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    if update_result.rows_affected() == 0 {
        // Counter didn't bump → either expired or exhausted. Roll back so the
        // dedup-insert from step 1 is undone.
        let _ = tx.rollback().await;

        // Disambiguate "expired" vs "exhausted" via a SELECT outside the
        // rolled-back transaction.
        let row: Option<(u64, u32, u32)> = sqlx::query_as(
            "SELECT expiry_ms, max_uses, used_count FROM group_invites WHERE blob_id = ?",
        )
        .bind(&blob_id[..])
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "consume group_invites disambiguate SELECT");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;
        let Some((expiry_ms, _max_uses, _used_count)) = row else {
            return Err((StatusCode::NOT_FOUND, "group-invite not found".to_string()));
        };
        if expiry_ms != 0 && now_ms >= expiry_ms {
            return Err((StatusCode::NOT_FOUND, "group-invite not found".to_string()));
        }
        return Err((
            StatusCode::GONE,
            "group-invite max_uses exhausted".to_string(),
        ));
    }

    // Re-SELECT inside the transaction so the readback reflects the bumped
    // counter.
    let (used_count, max_uses): (u32, u32) =
        sqlx::query_as("SELECT used_count, max_uses FROM group_invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "consume group_invites readback");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    tx.commit().await.map_err(|e| {
        tracing::error!(error = %e, "consume group_invites COMMIT");
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
// DELETE /discovery/group-invite/{blob_id} — issuer-signed revocation
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct RevokeRequest {
    /// UNIX millis at which the body was signed. Must be within
    /// `GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS` of the server clock.
    pub(crate) signed_at_ms: u64,
    /// Standard-alphabet base64 of the 64-byte Ed25519 signature over
    /// `lakko_contacts::group_invite::group_revoke_canonical_bytes`.
    pub(crate) signature: String,
}

/// `DELETE /discovery/group-invite/{blob_id}` — issuer-signed delete (ADR-0028
/// revoke). Mirror of `call_invite::revoke`:
///
///   * **204 No Content** — row deleted (idempotent on success).
///   * **404 Not Found** — folded over "no row" / "signature did not verify" /
///     "issuer pubkey could not be recovered" (denies status-code probing).
///   * **400 Bad Request** — bad base64 / wrong-sized signature / `signed_at_ms`
///     outside the replay window.
///
/// The issuer's verifying key is recovered from the stored capability's
/// `issuer_signature_pubkey` (the key that minted the link); the signature is
/// checked against the group-invite revoke domain, so a captured call-invite /
/// contact-invite revoke signature cannot be replayed here.
#[allow(clippy::too_many_lines)] // single-shot lifecycle — mirrors call_invite::revoke
pub(crate) async fn revoke(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
    // Session-token-gated via route_layer in `main.rs`. The Ed25519 signature on
    // the body proves issuer ownership; the token proves "logged-in Lakko user".
    axum::Extension(_authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<RevokeRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let blob_id = crate::ledger_id::parse_blob_id(&blob_id_b64url)?;

    // Replay window check before any DB work — cheap DoS gate.
    let now_ms = current_unix_ms();
    if !within_replay_window(payload.signed_at_ms, now_ms) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "signed_at_ms {} outside the ±{}ms replay window (now = {})",
                payload.signed_at_ms, GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS, now_ms,
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

    // Recover the issuer's verifying key from the stored capability. "No row"
    // folds into 404.
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT capability_bytes FROM group_invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "revoke group_invites SELECT");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;
    let Some((capability_bytes,)) = row else {
        return Err((StatusCode::NOT_FOUND, "group-invite not found".to_string()));
    };

    let cap = match decode_and_verify(&capability_bytes) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(
                error = %e,
                blob_id = %blob_id_b64url,
                "revoke: stored group-invite did not re-verify",
            );
            return Err((StatusCode::NOT_FOUND, "group-invite not found".to_string()));
        },
    };

    let canonical = group_revoke_canonical_bytes(&blob_id, payload.signed_at_ms);
    if cap
        .issuer_signature_pubkey
        .verify(&canonical, &signature)
        .is_err()
    {
        // Signature mismatch — fold into 404.
        return Err((StatusCode::NOT_FOUND, "group-invite not found".to_string()));
    }

    // Authorised. Delete the row — the FK CASCADE on `group_invite_consumers`
    // reaps the dedup side-table rows for free.
    sqlx::query("DELETE FROM group_invites WHERE blob_id = ?")
        .bind(&blob_id[..])
        .execute(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "revoke group_invites DELETE");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    tracing::info!(blob_id = %blob_id_b64url, "group-invite revoked");
    Ok(StatusCode::NO_CONTENT)
}

// ============================================================================
// Internal helpers (mirror call_invite.rs)
// ============================================================================

fn within_replay_window(signed_at_ms: u64, now_ms: u64) -> bool {
    let lo = now_ms.saturating_sub(GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS);
    let hi = now_ms.saturating_add(GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS);
    signed_at_ms >= lo && signed_at_ms <= hi
}

fn current_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
// ============================================================================
// Background expiry pruner (ADR-0028, G-3)
// ============================================================================

/// How often the group-invite-expiry pruner wakes up. Matches the
/// `invite::spawn_pruner` cadence (1 h) — a group-invite link lives about as
/// long as a contact invite, so the same asymmetry versus the 60 s PoW-challenge
/// prune applies.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Spawn a background task that periodically deletes expired group-invite blobs.
///
/// This closes the storage-bloat side of the `group_invites` table: without it,
/// an expired row lingers until an issuer-signed revoke ([`revoke`]) DELETEs it,
/// even though [`fetch`] / [`consume`] already fold an expired row into 404 (so
/// the accumulation is a bloat concern, not a security one). The
/// `idx_group_invite_expiry` index from `migrations/0014_group_invites.sql` is
/// the load-bearing piece — without it the recurring
/// `WHERE expiry_ms != 0 AND expiry_ms < ?` scan would do a full-table seq each
/// tick. The FK CASCADE on `group_invite_consumers.blob_id`
/// (`migrations/0015_group_invite_consumers.sql`) reaps the per-joiner dedup
/// children of a deleted parent for free.
///
/// `expiry_ms = 0` is the `GROUP_INVITE_NEVER_EXPIRES` sentinel (matching the
/// [`fetch`] / [`consume`] read-path check) — those rows are deliberately
/// preserved.
///
/// Mirror of [`crate::invite::spawn_pruner`] — the same interval / loop / log /
/// carry-on shape. Aborted when the service stops; the `_group_invite_pruner`
/// handle in `main.rs` owns the abort.
pub(crate) fn spawn_pruner(pool: sqlx::MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        // The first tick fires immediately — skip it so a startup-time prune
        // doesn't race with the first upload.
        interval.tick().await;
        loop {
            interval.tick().await;
            let now_ms = current_unix_ms();
            // `expiry_ms` is `BIGINT UNSIGNED` — matching the `u64` binds
            // elsewhere in this module (upload / fetch / consume).
            let result =
                sqlx::query("DELETE FROM group_invites WHERE expiry_ms != 0 AND expiry_ms < ?")
                    .bind(now_ms)
                    .execute(&pool)
                    .await;
            match result {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned expired group-invites");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "group-invites prune failed"),
            }
        }
    })
}

// ============================================================================
// Tests
// ============================================================================
//
// Mirror of `call_invite::tests` — pure-helper coverage + migration-shape drift
// sentinels. The async handlers (`upload` / `fetch` / `consume`) need a live
// MariaDB and are exercised by the integration suite the maintainer drives
// against a docker-composed `discovery` instance; here we pin the helpers + the
// host-side / SQL-side constant alignment.

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::invite::BLOB_ID_LEN;

    /// Pruner cadence pin — mirrors `invite::prune_interval_is_one_hour`. The
    /// constant is also named in the `spawn_pruner` doc; pin it in code so a
    /// future bump (e.g. shortening on a hot table) is a deliberate choice, not
    /// a silent edit.
    #[test]
    fn prune_interval_is_one_hour() {
        assert_eq!(PRUNE_INTERVAL, std::time::Duration::from_secs(3600));
    }

    /// `MAX_CAPABILITY_BYTES` pins the `VARBINARY(...)` width declared in
    /// `0018_short_invite_ids.sql`. Drift surfaces as a test failure rather than a
    /// silent "everything past N bytes 500s in production".
    #[test]
    fn max_capability_bytes_matches_column_width() {
        let migration = include_str!("../migrations/0018_short_invite_ids.sql");
        assert!(
            migration.contains(&format!("VARBINARY({MAX_CAPABILITY_BYTES})")),
            "MAX_CAPABILITY_BYTES ({MAX_CAPABILITY_BYTES}) must match the \
             VARBINARY width declared in 0018_short_invite_ids.sql",
        );
    }

    #[test]
    fn blob_id_len_matches_column_width() {
        let migration = include_str!("../migrations/0018_short_invite_ids.sql");
        let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalised.contains(&format!("blob_id BINARY({BLOB_ID_LEN})")),
            "BLOB_ID_LEN ({BLOB_ID_LEN}) must match the BINARY width \
             declared in 0018_short_invite_ids.sql",
        );
    }

    /// The consume handler binds `MAX_USES_UNLIMITED` directly into the SQL CASE
    /// expression — pin the value so a future rename / bump surfaces here first.
    #[test]
    fn max_uses_unlimited_matches_u32_max() {
        assert_eq!(MAX_USES_UNLIMITED, u32::MAX);
    }

    /// 0014 migration shape pin — the denormalised index columns the upload
    /// host-side bind layout relies on (`issuer_pubkey`, `group_id`,
    /// `expiry_ms`, `max_uses`). A future migration that drops one would
    /// silently break the INSERT.
    #[test]
    fn group_invites_migration_columns() {
        let migration = include_str!("../migrations/0018_short_invite_ids.sql");
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS group_invites"),
            "0018 migration must create `group_invites`",
        );
        for col in [
            "issuer_pubkey",
            "group_id",
            "expiry_ms",
            "max_uses",
            "used_count",
        ] {
            assert!(
                migration.contains(col),
                "0018 migration must declare the `{col}` column",
            );
        }
    }

    /// 0015 migration shape pin — the consume handler relies on the side table
    /// with PK `(blob_id, joiner_tag)` + FK CASCADE on `group_invites.blob_id`.
    /// A future migration that loosens either constraint would silently break
    /// dedup or leak orphan rows; surface here.
    #[test]
    fn group_invite_consumers_migration_shape() {
        let migration = include_str!("../migrations/0018_short_invite_ids.sql");
        let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS group_invite_consumers"),
            "0018 migration must create `group_invite_consumers`",
        );
        assert!(
            normalised.contains(&format!("joiner_tag BINARY({JOINER_TAG_LEN})")),
            "0018 migration must declare joiner_tag as BINARY({JOINER_TAG_LEN})",
        );
        assert!(
            migration.contains("PRIMARY KEY (blob_id, joiner_tag)"),
            "0018 migration must PK on (blob_id, joiner_tag)",
        );
        assert!(
            migration.contains(
                "FOREIGN KEY (blob_id) REFERENCES group_invites(blob_id) ON DELETE CASCADE",
            ),
            "0018 migration must FK CASCADE on group_invites.blob_id",
        );
    }
}
