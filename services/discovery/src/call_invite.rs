// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Capability-based call-invite blob storage (Phase 3c3-server).
//!
//! Sibling of `invite.rs` for `lakko_contacts::CallInviteCapability`.
//! Owners POST verified bytes; joiners GET by a random server-minted id
//! (the ADR-0028 short-link amendment, see `crate::ledger_id`).
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
//! depends on the (plaintext) capability bytes, so contact-invite
//! and call-invite tags for the same joiner don't collide even
//! though the function is shared.
//!
//! ## The ledger stores ciphertext (2026-09-28 security pass)
//!
//! The capability arrives SEALED under a key derived from a
//! 12-byte secret carried only in the link fragment
//! (`lakko_contacts::link_seal`); the link is `/c/<id>#<secret>`.
//! Before this, a 48-bit id was a bearer: whoever fetched it held
//! a valid capability, and under ADR-0032's auto admission that
//! was a seat in the call. Now a fetched blob opens nothing
//! without the secret, and a blob the ledger substitutes fails the
//! joiner's AEAD open. The server can no longer read the
//! capability, so what it needs arrives in clear upload fields
//! (`issuer_pubkey` for the revoke, `expiry_ms` and `max_uses`);
//! `owner_account_id` is the session's account, and the `call_id`
//! the server used to copy out is no longer stored at all.
//!
//! ## Owner-signed revocation (3c3-revoke)
//!
//! `DELETE /discovery/call-invite/{blob_id}` runs the same
//! authorisation gate as the contact-invite revoke: the
//! body carries `signed_at_ms` + Ed25519 signature over
//! `call_revoke_canonical_bytes(blob_id, signed_at_ms)`,
//! and the server verifies against the clear `issuer_pubkey`
//! stored beside the sealed blob. Cross-kind isolation is guaranteed by the
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
    call_revoke_canonical_bytes, CALL_REVOKE_REPLAY_WINDOW_MS, MAX_SEALED_LEN,
};
use lakko_contacts::invite::JOINER_TAG_LEN;
use lakko_contacts::link_seal::LinkKind;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Server-side ceiling on the sealed blob: the longest sealed `/c/` link
/// the codec can produce (`lakko_contacts::call_invite::MAX_SEALED_LEN`,
/// 4338 bytes with a 4 KiB embedded KeyPackage), derived rather than written
/// as a literal. Pinned to `call_invites.sealed_capability` in
/// `migrations/0022_sealed_invite_links.sql` by a test.
const MAX_SEALED_BYTES: usize = MAX_SEALED_LEN;

/// Sentinel for "unlimited" `max_uses` — matches
/// `lakko_contacts::call_invite::CALL_SINGLE_USE` mapping
/// in the SQL CASE expression below.
const MAX_USES_UNLIMITED: u32 = u32::MAX;

// ============================================================================
// POST /discovery/call-invite — upload a sealed capability + mint a random id
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    /// Standard-alphabet base64 of the sealed link blob
    /// (`lakko_contacts::call_invite::seal_link`). Opaque to the server.
    pub(crate) sealed: String,
    /// Standard-alphabet base64 of the 32-byte Ed25519 key that signed the
    /// capability — the key an owner-signed revoke must verify against.
    pub(crate) issuer_pubkey: String,
    /// The capability's `expiry_ms` (`0` = never expires), in clear for the
    /// fetch filter and the pruner.
    pub(crate) expiry_ms: u64,
    /// The capability's `max_uses` sentinel, in clear for the consume ceiling.
    pub(crate) max_uses: u32,
}

#[derive(Serialize)]
pub(crate) struct UploadResponse {
    /// base64url-no-pad of the random [`lakko_contacts::invite::BLOB_ID_LEN`]-byte
    /// ledger id (8 characters), minted fresh for this upload: the same
    /// blob uploaded twice gets two ids. The client puts it in the
    /// `/c/<blob_id>#<secret>` URL.
    pub(crate) blob_id: String,
}

/// `POST /discovery/call-invite` — store a sealed call-invite
/// capability under a fresh id.
pub(crate) async fn upload(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    let sealed =
        crate::ledger_id::parse_sealed_upload(&payload.sealed, LinkKind::Call, MAX_SEALED_BYTES)?;
    let issuer_pubkey = crate::ledger_id::parse_issuer_pubkey(&payload.issuer_pubkey)?;
    // The row belongs to the uploading session's account (account erasure
    // deletes by it). The capability's own `owner_account_id` is sealed.
    let owner_account_id = authed.0;
    let expiry_ms: u64 = payload.expiry_ms;
    let max_uses: u32 = payload.max_uses;

    // A random id, never an upsert: a collision is retried under a fresh id
    // and can never overwrite another link's row (the short-link amendment
    // of ADR-0028, see `crate::ledger_id`).
    let db = state.db.clone();
    let blob_id = crate::ledger_id::insert_with_fresh_id("call_invites", |id| {
        let db = db.clone();
        let sealed = sealed.clone();
        async move {
            sqlx::query(
                "INSERT INTO call_invites \
                    (blob_id, sealed_capability, owner_account_id, issuer_pubkey, \
                     expiry_ms, max_uses) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(id.to_vec())
            .bind(sealed)
            .bind(owner_account_id.to_vec())
            .bind(issuer_pubkey.to_vec())
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
        owner_account_id = %B64.encode(owner_account_id),
        expiry_ms = expiry_ms,
        bytes = sealed.len(),
        "call-invite stored",
    );
    Ok(Json(UploadResponse {
        blob_id: blob_id_b64url,
    }))
}

// ============================================================================
// GET /discovery/call-invite/{blob_id} — fetch by id
// ============================================================================

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    /// Standard-alphabet base64 of the stored sealed blob. The client opens
    /// it with the link secret and verifies the capability inside.
    pub(crate) sealed: String,
}

/// `GET /discovery/call-invite/{blob_id}` — fetch by id.
///
/// 404 on missing or expired rows (server-side expiry filter
/// folded into 404 to deny status-code probing).
pub(crate) async fn fetch(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
) -> Result<Json<FetchResponse>, (StatusCode, String)> {
    let blob_id = crate::ledger_id::parse_blob_id(&blob_id_b64url)?;

    let row: Option<(Vec<u8>, u64)> =
        sqlx::query_as("SELECT sealed_capability, expiry_ms FROM call_invites WHERE blob_id = ?")
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

    let (sealed, expiry_ms) =
        row.ok_or((StatusCode::NOT_FOUND, "call-invite not found".to_string()))?;

    if expiry_ms != 0 && current_unix_ms() >= expiry_ms {
        // Expired rows fold into 404 to deny attacker
        // status-code probing — same posture as `invite::fetch`.
        return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
    }

    Ok(Json(FetchResponse {
        sealed: B64.encode(&sealed),
    }))
}

// ============================================================================
// POST /discovery/call-invite/{blob_id}/consume — atomic max_uses bump
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct ConsumeRequest {
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
///     "signature did not verify" / "stored issuer key
///     malformed". Denies attacker status-code probing.
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
    let blob_id = crate::ledger_id::parse_blob_id(&blob_id_b64url)?;

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

    // The issuer key stored beside the sealed blob. "No row"
    // folds into 404 (see doc comment).
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT issuer_pubkey FROM call_invites WHERE blob_id = ?")
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
    let Some(issuer_key) = row.and_then(|(k,)| crate::ledger_id::stored_issuer_key(&k)) else {
        return Err((StatusCode::NOT_FOUND, "call-invite not found".to_string()));
    };

    let canonical = call_revoke_canonical_bytes(&blob_id, payload.signed_at_ms);
    if issuer_key.verify(&canonical, &signature).is_err() {
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

// ============================================================================
// Tests
// ============================================================================
//
// Mirror of `invite::tests` — pure-helper coverage + migration-
// shape drift sentinels. The async handlers (`upload` / `fetch` /
// `consume` / `revoke`) need a live MariaDB and are exercised by
// the integration suite the maintainer drives against a docker-
// composed `discovery` instance; here we pin the helpers + the
// host-side / SQL-side constant alignment.

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::invite::BLOB_ID_LEN;

    // -------------------------------------------------------------------------
    // Constants vs. migration shape — drift sentinels.
    // -------------------------------------------------------------------------

    /// The `MAX_SEALED_BYTES` constant pins the
    /// `call_invites.sealed_capability` width declared in
    /// `0022_sealed_invite_links.sql`.
    #[test]
    fn max_sealed_bytes_matches_column_width() {
        let migration = include_str!("../migrations/0022_sealed_invite_links.sql");
        let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalised.contains(&format!("sealed_capability VARBINARY({MAX_SEALED_BYTES})")),
            "MAX_SEALED_BYTES ({MAX_SEALED_BYTES}) must match call_invites.sealed_capability",
        );
        assert_eq!(MAX_SEALED_BYTES, 4338);
    }

    #[test]
    fn blob_id_len_matches_column_width() {
        let migration = include_str!("../migrations/0022_sealed_invite_links.sql");
        // Whitespace-tolerant — same hand-alignment as
        // `call_invites_migration_indexes`.
        let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalised.contains(&format!("blob_id BINARY({BLOB_ID_LEN})")),
            "BLOB_ID_LEN ({BLOB_ID_LEN}) must match the BINARY width \
             declared in 0022_sealed_invite_links.sql",
        );
    }

    /// The consume handler's atomic UPDATE binds
    /// `MAX_USES_UNLIMITED` directly into the SQL CASE
    /// expression. A future constant rename / value bump would
    /// silently change the unlimited-budget semantics — pin
    /// the value here so the regression surfaces in the test
    /// suite first.
    #[test]
    fn max_uses_unlimited_matches_u32_max() {
        assert_eq!(MAX_USES_UNLIMITED, u32::MAX);
    }

    /// 0008 migration shape pin — the consume handler relies
    /// on the side table existing with PK `(blob_id,
    /// joiner_tag)` + FK CASCADE on `call_invites.blob_id`. A
    /// future migration that loosens either constraint would
    /// silently break dedup or leak orphan rows; surface here.
    #[test]
    fn call_invite_consumers_migration_shape() {
        let migration = include_str!("../migrations/0022_sealed_invite_links.sql");
        let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        // Side-table exists.
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS call_invite_consumers"),
            "0022 migration must create `call_invite_consumers`",
        );
        // Tag column matches `JOINER_TAG_LEN` (the helper
        // shared with the contact-invite ecosystem — see
        // `lakko_contacts::invite::JOINER_TAG_LEN`).
        assert!(
            normalised.contains(&format!("joiner_tag BINARY({JOINER_TAG_LEN})")),
            "0022 migration must declare joiner_tag as BINARY({JOINER_TAG_LEN})",
        );
        // PK is the dedup key the consume handler reads.
        assert!(
            migration.contains("PRIMARY KEY (blob_id, joiner_tag)"),
            "0022 migration must PK on (blob_id, joiner_tag)",
        );
        // FK CASCADE is what gives us free cleanup on revoke.
        assert!(
            migration.contains(
                "FOREIGN KEY (blob_id) REFERENCES call_invites(blob_id) ON DELETE CASCADE",
            ),
            "0022 migration must FK CASCADE on call_invites.blob_id",
        );
    }

    /// 0007 migration shape pin — denormalised index columns +
    /// the per-call query indexes the call-invite UX relies on
    /// (owner-list, expiry pruner, call-id lookup). A future
    /// migration that drops one of these would silently turn
    /// hot queries into table scans.
    #[test]
    fn call_invites_migration_indexes() {
        let migration = include_str!("../migrations/0022_sealed_invite_links.sql");
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS call_invites"),
            "0022 migration must create `call_invites`",
        );
        // Whitespace-tolerant — the migration is hand-aligned
        // for readability (varies between 1–11 spaces between
        // column name + type), so normalise the column-decl
        // segment by collapsing runs of spaces before
        // searching.
        let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        // The server no longer stores the call id (it cannot read the
        // sealed capability, and nothing looked rows up by it).
        let table = normalised
            .split("CREATE TABLE IF NOT EXISTS call_invites (")
            .nth(1)
            .and_then(|rest| rest.split(") ENGINE").next())
            .expect("call_invites table");
        assert!(
            !table.contains("call_id"),
            "call_invites must not store call_id"
        );
        assert!(
            table.contains("issuer_pubkey BINARY(32)"),
            "call_invites must store the clear issuer_pubkey",
        );
        // The host-side bind layout requires `expiry_ms`,
        // `max_uses`, `owner_account_id` — surface here if a
        // future migration drops one of them.
        assert!(
            migration.contains("expiry_ms"),
            "0022 migration must declare expiry_ms column",
        );
        assert!(
            migration.contains("max_uses"),
            "0022 migration must declare max_uses column",
        );
        assert!(
            migration.contains("owner_account_id"),
            "0022 migration must declare owner_account_id column",
        );
    }

    // -------------------------------------------------------------------------
    // within_replay_window — server-side helper (the client
    // doesn't filter timestamps locally). Symmetric inclusive
    // window of ±CALL_REVOKE_REPLAY_WINDOW_MS.
    // -------------------------------------------------------------------------

    #[test]
    fn within_replay_window_is_symmetric() {
        let now = 1_700_000_000_000u64;
        // Inside: ±5 minutes.
        assert!(within_replay_window(now - 5 * 60 * 1000, now));
        assert!(within_replay_window(now + 5 * 60 * 1000, now));
        // Boundary: exactly ±10 minutes — accepted (inclusive
        // ≤ on both sides; pinned here so a future tightening
        // to strict-less-than surfaces as a test failure
        // before it shows up as silent revoke / consume
        // rejections on the boundary).
        assert!(within_replay_window(
            now - CALL_REVOKE_REPLAY_WINDOW_MS,
            now
        ));
        assert!(within_replay_window(
            now + CALL_REVOKE_REPLAY_WINDOW_MS,
            now
        ));
        // Outside: > 10 minutes.
        assert!(!within_replay_window(
            now - CALL_REVOKE_REPLAY_WINDOW_MS - 1,
            now,
        ));
        assert!(!within_replay_window(
            now + CALL_REVOKE_REPLAY_WINDOW_MS + 1,
            now,
        ));
    }

    /// Saturating arithmetic boundary check — when `now_ms` is
    /// very small (e.g. test fixtures using `now = 100`), the
    /// `lo = now - window` subtraction must NOT wrap around to
    /// near-u64::MAX. Pin the saturating semantics.
    #[test]
    fn within_replay_window_saturates_on_small_now() {
        // `now = 100ms` is well below the 10-minute window;
        // `lo` would underflow to near-u64::MAX without
        // saturating_sub. The signed_at_ms = 0 case must
        // still be accepted (it's within the boundary on the
        // upper side: 0 <= 100 + window).
        let now = 100u64;
        assert!(within_replay_window(0, now));
        assert!(within_replay_window(50, now));
    }

    /// A maximal sealed call invite survives the real ledger and opens under
    /// its secret only; an issuer-signed revoke verifies against the clear
    /// key. Gated on `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skips when unset).
    #[tokio::test]
    async fn a_sealed_call_invite_round_trips_through_the_ledger() {
        use axum::extract::{Path, State};
        use axum::{Extension, Json};
        use lakko_contacts::call_invite::{
            call_sign_revocation, compose, open_link, seal_link, CallInviteMedia, CALL_ID_LEN,
            MAX_KEY_PACKAGE_LEN,
        };
        use lakko_contacts::invite::MAX_LABEL_LEN;
        use lakko_contacts::link_seal::generate_link_secret;
        use lakko_crypto::signing::SigningKey;

        let Some(db) =
            crate::anon_token_route_tests::test_pool("a_sealed_call_invite_round_trips").await
        else {
            return;
        };
        let state = crate::anon_token_route_tests::state(db, false);
        let signing = SigningKey::generate();
        let account_id = [0x41u8; 16];
        let expiry_ms = current_unix_ms() + 3_600_000;
        let wire = compose(
            &signing,
            &account_id,
            &[0x10u8; CALL_ID_LEN],
            expiry_ms,
            u32::MAX,
            CallInviteMedia::audio_video(),
            &vec![0xCD; MAX_KEY_PACKAGE_LEN],
            &"t".repeat(MAX_LABEL_LEN),
        )
        .unwrap();
        let secret = generate_link_secret();
        let sealed = seal_link(&secret, &wire).unwrap();
        assert_eq!(sealed.len(), MAX_SEALED_BYTES);

        let Json(stored) = upload(
            State(state.clone()),
            Extension(crate::auth::AuthenticatedAccount(account_id)),
            Json(UploadRequest {
                sealed: B64.encode(&sealed),
                issuer_pubkey: B64.encode(signing.verifying_key().to_bytes()),
                expiry_ms,
                max_uses: u32::MAX,
            }),
        )
        .await
        .expect("a maximal sealed call invite uploads");
        let Json(fetched) = fetch(State(state.clone()), Path(stored.blob_id.clone()))
            .await
            .expect("fetch");
        let fetched = B64.decode(fetched.sealed).unwrap();
        assert_eq!(fetched, sealed);
        assert_eq!(open_link(&secret, &fetched).unwrap().1, wire);
        assert!(open_link(&generate_link_secret(), &fetched).is_err());

        let blob_id = crate::ledger_id::parse_blob_id(&stored.blob_id).unwrap();
        let now = current_unix_ms();
        let ok = revoke(
            State(state.clone()),
            Path(stored.blob_id.clone()),
            Extension(crate::auth::AuthenticatedAccount(account_id)),
            Json(RevokeRequest {
                signed_at_ms: now,
                signature: B64.encode(call_sign_revocation(&signing, &blob_id, now)),
            }),
        )
        .await
        .expect("the issuer revokes");
        assert_eq!(ok, StatusCode::NO_CONTENT);
        assert!(fetch(State(state), Path(stored.blob_id)).await.is_err());
    }
}
