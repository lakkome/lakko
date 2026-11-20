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
//! - `POST /discovery/group-invite`                       — upload a sealed capability + mint a random id (session-gated).
//! - `GET  /discovery/group-invite/{blob_id}`             — fetch by id (open, per-IP rate-limited).
//! - `POST /discovery/group-invite/{blob_id}/consume`     — atomic per-joiner consume (open).
//!
//! ## The ledger is NOT the security boundary
//!
//! ADR-0028 puts the security boundary at the admin approval gate (ADR-0027
//! role-gated, peer-verified): a leaked link lets a stranger *ask* to join,
//! never *in*. So `max_uses` here is a SOFT anti-spam knob — how many strangers
//! may queue an external-join request — not an access control. It is NOT part
//! of the signed `GroupInviteCapability` wire; the uploader supplies it in the
//! POST body, and lying about it only changes how many requests reach the
//! admin, who still approves each one individually.
//!
//! ## The ledger stores ciphertext (2026-09-28 security pass)
//!
//! The capability arrives SEALED under a key derived from the 12-byte link
//! secret in the URL fragment (`lakko_contacts::link_seal`), the same secret
//! that opens the preview inside it. The server can neither read nor verify
//! it; a `/g/` link that lost its fragment used to accept whatever capability
//! the ledger returned, and now opens nothing, and a blob the ledger
//! substitutes fails the joiner's AEAD open. `revocation_pubkey` (for the
//! revoke) and `expiry_ms` (for the fetch filter and the pruner) arrive in clear
//! beside the blob; the `group_id` the server used to copy out is no longer
//! stored at all.
//!
//! ## No owner, no issuer; revocation is signed by a per-link key
//!
//! The row stores neither the uploading account nor the issuer's identity
//! key (ADR-0078 section 10: together with the knock journal the issuer key
//! named every group a person invited into). The session-token gate is a
//! pure DoS / "logged-in user" rate-limit and the account is never stored.
//! Revocation (`DELETE /discovery/group-invite/{blob_id}`) carries an Ed25519
//! signature by the per-link revocation key the uploader named in clear,
//! over `group_revoke_canonical_bytes` (domain
//! `lakko-group-invite-revoke/v1`, the 6-byte id and a timestamp inside
//! `GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS`). Expiry and the periodic pruner
//! remain the other lifecycle controls.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use lakko_contacts::group_invite::{
    group_revoke_canonical_bytes, GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS, MAX_SEALED_LEN,
};
use lakko_contacts::invite::JOINER_TAG_LEN;
use lakko_contacts::link_seal::LinkKind;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Server-side ceiling on the sealed blob: the codec's bound on a sealed `/g/`
/// link (`lakko_contacts::group_invite::MAX_SEALED_LEN`, the 1965-byte bound
/// on a capability plus the 30-byte seal, 1995 bytes), derived rather than
/// written as a literal so a codec change moves it. The `VARBINARY` width of
/// `group_invites.sealed_capability` (migration
/// `0038_invite_preview_photo_width.sql`) matches it; a test pins both, and
/// another seals the largest links a client can mint and checks they fit.
const MAX_SEALED_BYTES: usize = MAX_SEALED_LEN;

/// Sentinel for "unlimited" `max_uses` — matches the SQL CASE expression in the
/// consume handler.
const MAX_USES_UNLIMITED: u32 = u32::MAX;

// ============================================================================
// POST /discovery/group-invite — upload a sealed capability + mint a random id
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    /// Standard-alphabet base64 of the sealed link blob
    /// (`lakko_contacts::group_invite::seal_link`). Opaque to the server.
    pub(crate) sealed: String,
    /// Standard-alphabet base64 of the 32-byte per-link revocation key
    /// (ADR-0078 section 10) — the key a revoke must verify against.
    pub(crate) revocation_pubkey: String,
    /// The capability's `expiry_ms` (`0` = never expires), in clear for the
    /// fetch filter and the pruner.
    pub(crate) expiry_ms: u64,
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
    /// blob uploaded twice gets two ids. The client puts it in the
    /// `/g/<blob_id>#<secret>` URL.
    pub(crate) blob_id: String,
}

/// `POST /discovery/group-invite` — store a sealed group-invite capability under a fresh id.
pub(crate) async fn upload(
    State(state): State<AppState>,
    // Session-token-gated via the route_layer in `main.rs` as a pure DoS /
    // logged-in-user gate; the uploader is neither stored nor logged.
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    let sealed =
        crate::ledger_id::parse_sealed_upload(&payload.sealed, LinkKind::Group, MAX_SEALED_BYTES)?;
    let revocation_pk = crate::ledger_id::parse_revocation_pubkey(&payload.revocation_pubkey)?;
    let expiry_ms: u64 = payload.expiry_ms;
    let max_uses: u32 = payload.max_uses;

    // A random id, never an upsert: a collision is retried under a fresh id
    // and can never overwrite another link's row (the short-link amendment
    // of ADR-0028, see `crate::ledger_id`).
    let db = state.db.clone();
    let blob_id = crate::ledger_id::insert_with_fresh_id("group_invites", |id| {
        let db = db.clone();
        let sealed = sealed.clone();
        async move {
            sqlx::query(
                "INSERT INTO group_invites \
                    (blob_id, sealed_capability, revocation_pk, expiry_ms, max_uses) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(id.to_vec())
            .bind(sealed)
            .bind(revocation_pk.to_vec())
            .bind(expiry_ms)
            .bind(max_uses)
            .execute(&db)
            .await
        }
    })
    .await?;

    let blob_id_b64url = B64URL.encode(blob_id);
    // ADR-0078 section 11: no ledger id, account or key in a log line.
    tracing::info!(max_uses, bytes = sealed.len(), "group-invite stored");
    Ok(Json(UploadResponse {
        blob_id: blob_id_b64url,
    }))
}

// ============================================================================
// GET /discovery/group-invite/{blob_id} — fetch by id
// ============================================================================

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    /// Standard-alphabet base64 of the stored sealed blob. The client opens it
    /// with the link secret, which verifies the capability inside and then
    /// opens its preview; without the secret it opens nothing.
    pub(crate) sealed: String,
    /// No uses left. A page with no session reads the link as dead; a signed-in
    /// client still reads it, and `consume` says whether this person used it.
    pub(crate) used_up: bool,
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

    let row: Option<(Vec<u8>, u64, u32, u32)> = sqlx::query_as(
        "SELECT sealed_capability, expiry_ms, max_uses, used_count \
         FROM group_invites WHERE blob_id = ?",
    )
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

    let (sealed, expiry_ms, max_uses, used_count) =
        row.ok_or((StatusCode::NOT_FOUND, "group-invite not found".to_string()))?;

    if expiry_ms != 0 && current_unix_ms() >= expiry_ms {
        // Expired rows fold into 404 to deny attacker status-code probing.
        return Err((StatusCode::NOT_FOUND, "group-invite not found".to_string()));
    }

    Ok(Json(FetchResponse {
        sealed: B64.encode(&sealed),
        used_up: crate::ledger_id::link_used_up(max_uses, used_count),
    }))
}

// ============================================================================
// POST /discovery/group-invite/{blob_id}/consume — atomic per-joiner consume
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct ConsumeRequest {
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
///     "stored revocation key malformed" (denies status-code probing).
///   * **400 Bad Request** — bad base64 / wrong-sized signature / `signed_at_ms`
///     outside the replay window.
///
/// The verifying key is the per-link `revocation_pk` the uploader stored beside
/// the sealed blob (the server cannot read the capability); the
/// signature is checked against the group-invite revoke domain, so a captured call-invite /
/// contact-invite revoke signature cannot be replayed here.
#[allow(clippy::too_many_lines)] // single-shot lifecycle — mirrors call_invite::revoke
pub(crate) async fn revoke(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
    // Session-token-gated via route_layer in `main.rs` as a rate-limit gate;
    // the signature by the link's revocation key is the authorisation.
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

    // The revocation key stored beside the sealed blob. "No row" folds into 404.
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT revocation_pk FROM group_invites WHERE blob_id = ?")
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
    let Some(revocation_key) = row.and_then(|(k,)| crate::ledger_id::stored_revocation_key(&k))
    else {
        return Err((StatusCode::NOT_FOUND, "group-invite not found".to_string()));
    };

    let canonical = group_revoke_canonical_bytes(&blob_id, payload.signed_at_ms);
    if revocation_key.verify(&canonical, &signature).is_err() {
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

    tracing::info!("group-invite revoked");
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
/// long as a contact invite.
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
    use crate::metrics::{job_run, jobs};
    lakko_metrics::job_registered(jobs::GROUP_INVITE_PRUNE);
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
            job_run(jobs::GROUP_INVITE_PRUNE, &result);
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

    /// `MAX_SEALED_BYTES` pins the `group_invites.sealed_capability` width,
    /// declared in `0031_ownerless_invite_ledgers.sql` and widened by
    /// `0036_subscriber_invite_width.sql` and
    /// `0038_invite_preview_photo_width.sql` (ADR-0087). Drift surfaces as a
    /// test failure rather than a silent "everything past N bytes 500s in
    /// production".
    #[test]
    fn max_sealed_bytes_matches_column_width() {
        let widened = include_str!("../migrations/0038_invite_preview_photo_width.sql");
        let normalised = widened.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalised.contains(&format!(
                "MODIFY sealed_capability VARBINARY({MAX_SEALED_BYTES})"
            )),
            "MAX_SEALED_BYTES ({MAX_SEALED_BYTES}) must match group_invites.sealed_capability \
             in 0038_invite_preview_photo_width.sql",
        );
    }

    /// The derivation lands where it was computed by hand: the 1965-byte
    /// bound on a v3 capability with a photo in its preview plus the 30-byte
    /// seal.
    #[test]
    fn max_sealed_bytes_is_the_computed_maximum() {
        assert_eq!(lakko_contacts::group_invite::MAX_WIRE_LEN, 1965);
        assert_eq!(MAX_SEALED_BYTES, 1995);
    }

    /// The largest photo a preview carries: a still and a video at their
    /// size bounds (ADR-0087 section 18.4).
    fn maximal_photo() -> lakko_contacts::avatar::Avatar {
        use lakko_contacts::avatar::{Avatar, AvatarRef, AvatarVideo};
        let reference = |byte: u8, side: u16, size: u32| AvatarRef {
            blob_id: [byte; 32],
            key: [byte; 32],
            nonce: [byte; 12],
            size,
            width: side,
            height: side,
        };
        Avatar {
            still: reference(0x10, 640, 256 * 1024),
            video: Some(AvatarVideo {
                reference: reference(0x20, 480, 4 * 1024 * 1024),
                duration_ms: 10_000,
            }),
        }
    }

    fn maximal_capability(expiry_ms: u64, secret: &[u8; 12]) -> Vec<u8> {
        use lakko_contacts::group_invite::{compose_with_preview, GroupInvitePreview};
        use lakko_contacts::MAX_GROUP_ID_LEN;
        use lakko_crypto::signing::SigningKey;

        // 63 four-byte scalars plus one three-byte scalar: 64 scalars and 255
        // bytes, the display-name rule's two ceilings at once.
        let long_name = format!("{}{}", "\u{1F600}".repeat(63), '\u{20AC}');
        assert_eq!(long_name.len(), 255);
        let preview = GroupInvitePreview {
            group_name: Some(long_name.clone()),
            inviter_name: Some(long_name),
            member_count: u16::MAX,
            channel_id: None,
            subscriber: None,
            avatar: Some(maximal_photo()),
        };
        compose_with_preview(
            &SigningKey::from_seed(&[0x61; 32]),
            &vec![0x5a; MAX_GROUP_ID_LEN],
            expiry_ms,
            Some(&lakko_contacts::group_invite::GroupKnockKey {
                public_key: [0x4B; 32],
                version: 1,
            }),
            &preview,
            secret,
        )
        .expect("maximal capability composes")
    }

    /// The largest subscriber invitation (ADR-0087 section 7: a 16-byte
    /// channel id, a private channel's keys, the longest name and
    /// description).
    fn maximal_subscriber_capability(secret: &[u8; 12]) -> Vec<u8> {
        use lakko_contacts::group_invite::{
            compose_with_preview, ChannelAccess, GroupInvitePreview, SubscriberInvite,
        };
        use lakko_crypto::signing::SigningKey;

        let channel_id = [0xC7; 16];
        let preview = GroupInvitePreview {
            group_name: Some(format!("{}\u{20AC}", "\u{1F600}".repeat(63))),
            inviter_name: None,
            member_count: 0,
            channel_id: Some(channel_id),
            subscriber: Some(SubscriberInvite {
                access: ChannelAccess::Private {
                    invite_seed: [0x11; 32],
                    admission_public_key: [0x22; 32],
                    admission_version: u16::MAX,
                },
                description: "\u{1F600}".repeat(256),
            }),
            avatar: Some(maximal_photo()),
        };
        compose_with_preview(
            &SigningKey::from_seed(&[0x61; 32]),
            &channel_id,
            0,
            None,
            &preview,
            secret,
        )
        .expect("maximal subscriber capability composes")
    }

    /// The codec's ceiling is an upper bound: it adds the largest group id
    /// and the knock key to the largest preview, which only a subscriber
    /// invitation carries, and a subscriber invitation names a 16-byte
    /// channel id and no knock key. So the largest subscriber link sits
    /// exactly that far below the ceiling, and the largest group link fits
    /// too: every link a client can mint fits the ledger.
    #[test]
    fn a_maximal_sealed_link_fits_the_ceiling() {
        use lakko_contacts::group_invite::{generate_link_key, open_link, seal_link};
        use lakko_contacts::knock::KNOCK_KEY_LEN;
        use lakko_contacts::MAX_GROUP_ID_LEN;

        let secret = generate_link_key();
        let wire = maximal_subscriber_capability(&secret);
        let sealed = seal_link(&secret, &wire).unwrap();
        // The knock field is the key plus its two-byte version.
        assert_eq!(
            sealed.len(),
            MAX_SEALED_BYTES - (MAX_GROUP_ID_LEN - 16) - (KNOCK_KEY_LEN + 2)
        );
        assert_eq!(open_link(&secret, &sealed).unwrap().1, wire);

        let group_wire = maximal_capability(0, &secret);
        let group_sealed = seal_link(&secret, &group_wire).unwrap();
        assert!(group_sealed.len() <= MAX_SEALED_BYTES);
        assert_eq!(open_link(&secret, &group_sealed).unwrap().1, group_wire);
    }

    /// A maximal sealed link survives the real ledger: upload stores it,
    /// fetch returns it byte for byte, the fragment's secret opens it and its
    /// preview, another secret opens nothing, and only the link's revocation
    /// key revokes it (not the issuer key that signed the capability). Gated on
    /// `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skips when unset).
    #[tokio::test]
    async fn a_maximal_sealed_link_round_trips_through_the_ledger() {
        use axum::extract::{Path, State};
        use axum::Json;
        use lakko_contacts::group_invite::{
            generate_link_key, group_sign_revocation, open_link, seal_link,
        };
        use lakko_crypto::signing::SigningKey;

        let Some(db) =
            crate::anon_token_route_tests::test_pool("a_maximal_sealed_link_round_trips").await
        else {
            return;
        };
        let state = crate::anon_token_route_tests::state(db, false);

        let secret = generate_link_key();
        let expiry_ms = current_unix_ms() + 3_600_000;
        let wire = maximal_capability(expiry_ms, &secret);
        let sealed = seal_link(&secret, &wire).unwrap();
        let issuer = SigningKey::from_seed(&[0x61; 32]);
        let revocation = SigningKey::generate();

        let Json(stored) = upload(
            State(state.clone()),
            Json(UploadRequest {
                sealed: B64.encode(&sealed),
                revocation_pubkey: B64.encode(revocation.verifying_key().to_bytes()),
                expiry_ms,
                max_uses: 1,
            }),
        )
        .await
        .expect("a maximal sealed link uploads");
        let Json(fetched) = fetch(State(state.clone()), Path(stored.blob_id.clone()))
            .await
            .expect("the stored blob fetches");
        assert!(!fetched.used_up, "a fresh link has uses left");
        sqlx::query("UPDATE group_invites SET used_count = 1 WHERE blob_id = ?")
            .bind(&crate::ledger_id::parse_blob_id(&stored.blob_id).unwrap()[..])
            .execute(&state.db)
            .await
            .unwrap();
        let Json(spent) = fetch(State(state.clone()), Path(stored.blob_id.clone()))
            .await
            .expect("a used-up link is still served, flagged");
        assert!(spent.used_up, "the row at its ceiling reads as used up");
        let fetched = B64.decode(fetched.sealed).unwrap();
        assert_eq!(fetched, sealed);
        let (cap, opened) = open_link(&secret, &fetched).expect("the secret opens it");
        assert_eq!(opened, wire);
        assert!(cap.open_preview(&secret).unwrap().is_some());
        assert!(open_link(&generate_link_key(), &fetched).is_err());

        let blob_id = crate::ledger_id::parse_blob_id(&stored.blob_id).unwrap();
        let now = current_unix_ms();
        assert!(revoke(
            State(state.clone()),
            Path(stored.blob_id.clone()),
            Json(RevokeRequest {
                signed_at_ms: now,
                signature: B64.encode(group_sign_revocation(&issuer, &blob_id, now)),
            }),
        )
        .await
        .is_err());
        let ok = revoke(
            State(state.clone()),
            Path(stored.blob_id.clone()),
            Json(RevokeRequest {
                signed_at_ms: now,
                signature: B64.encode(group_sign_revocation(&revocation, &blob_id, now)),
            }),
        )
        .await
        .expect("the link's revocation key revokes");
        assert_eq!(ok, StatusCode::NO_CONTENT);
        assert!(fetch(State(state), Path(stored.blob_id)).await.is_err());
    }

    /// One byte over the ceiling, a blob of another kind, and a bad revocation
    /// key are refused before any insert.
    #[tokio::test]
    async fn malformed_uploads_are_refused() {
        use axum::extract::State;
        use axum::Json;
        use lakko_contacts::link_seal::{generate_link_secret, seal};

        let Some(db) = crate::anon_token_route_tests::test_pool("one_byte_over_the_ceiling").await
        else {
            return;
        };
        let state = crate::anon_token_route_tests::state(db, false);
        let key = B64.encode(
            lakko_crypto::signing::SigningKey::generate()
                .verifying_key()
                .to_bytes(),
        );
        let own = seal(LinkKind::Group, &generate_link_secret(), b"x").unwrap();
        for (sealed, issuer, want) in [
            (
                vec![b'g'; MAX_SEALED_BYTES + 1],
                key.clone(),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
            (
                seal(LinkKind::Contact, &generate_link_secret(), b"x").unwrap(),
                key.clone(),
                StatusCode::BAD_REQUEST,
            ),
            (own, B64.encode([0u8; 31]), StatusCode::BAD_REQUEST),
        ] {
            let Err((status, _)) = upload(
                State(state.clone()),
                Json(UploadRequest {
                    sealed: B64.encode(sealed),
                    revocation_pubkey: issuer,
                    expiry_ms: 0,
                    max_uses: 1,
                }),
            )
            .await
            else {
                panic!("a malformed upload must be refused");
            };
            assert_eq!(status, want);
        }
    }

    #[test]
    fn blob_id_len_matches_column_width() {
        let migration = include_str!("../migrations/0031_ownerless_invite_ledgers.sql");
        let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalised.contains(&format!("blob_id BINARY({BLOB_ID_LEN})")),
            "BLOB_ID_LEN ({BLOB_ID_LEN}) must match the BINARY width \
             declared in 0031_ownerless_invite_ledgers.sql",
        );
    }

    /// The consume handler binds `MAX_USES_UNLIMITED` directly into the SQL CASE
    /// expression — pin the value so a future rename / bump surfaces here first.
    #[test]
    fn max_uses_unlimited_matches_u32_max() {
        assert_eq!(MAX_USES_UNLIMITED, u32::MAX);
    }

    /// 0022 migration shape pin — the columns the upload bind layout relies on
    /// (`sealed_capability`, `revocation_pk`, `expiry_ms`, `max_uses`). A
    /// future migration that drops one would silently break the INSERT.
    #[test]
    fn group_invites_migration_columns() {
        let migration = include_str!("../migrations/0031_ownerless_invite_ledgers.sql");
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS group_invites"),
            "0031 migration must create `group_invites`",
        );
        for col in [
            "sealed_capability",
            "revocation_pk",
            "expiry_ms",
            "max_uses",
            "used_count",
        ] {
            assert!(
                migration.contains(col),
                "0031 migration must declare the `{col}` column",
            );
        }
    }

    /// 0015 migration shape pin — the consume handler relies on the side table
    /// with PK `(blob_id, joiner_tag)` + FK CASCADE on `group_invites.blob_id`.
    /// A future migration that loosens either constraint would silently break
    /// dedup or leak orphan rows; surface here.
    #[test]
    fn group_invite_consumers_migration_shape() {
        let migration = include_str!("../migrations/0031_ownerless_invite_ledgers.sql");
        let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS group_invite_consumers"),
            "0031 migration must create `group_invite_consumers`",
        );
        assert!(
            normalised.contains(&format!("joiner_tag BINARY({JOINER_TAG_LEN})")),
            "0031 migration must declare joiner_tag as BINARY({JOINER_TAG_LEN})",
        );
        assert!(
            migration.contains("PRIMARY KEY (blob_id, joiner_tag)"),
            "0031 migration must PK on (blob_id, joiner_tag)",
        );
        assert!(
            migration.contains(
                "FOREIGN KEY (blob_id) REFERENCES group_invites(blob_id) ON DELETE CASCADE",
            ),
            "0031 migration must FK CASCADE on group_invites.blob_id",
        );
    }
}
