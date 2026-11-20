// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Capability-based invite-link blob storage (Phase 1.18d1).
//!
//! Owners POST a SEALED `InviteCapability` here, get a short random
//! `blob_id` back, and share `https://lakko.me/i/<blob_id>#<secret>`.
//! Joiners GET the sealed bytes by id and open them locally with the
//! secret from the link fragment (`lakko_contacts::invite::open_link`),
//! which also verifies the capability's signature.
//!
//! ## Why a random id (2026-09-27, the ADR-0028 short-link amendment)
//!
//! The id used to be `SHA-256(capability_bytes)` (43 URL
//! characters, idempotent by content). It is now a random 6-byte
//! handle (8 characters) the server mints at upload; see
//! `crate::ledger_id`. The insert is never an upsert, so a
//! colliding id is retried and never overwrites another row.
//! Guessing live ids (2^48) is bounded by the per-IP rate limit on
//! the open fetch / consume routes.
//!
//! ## The ledger stores ciphertext (2026-09-28 security pass)
//!
//! The capability is sealed under a key derived from a 12-byte secret
//! that rides only in the link's URL fragment
//! (`lakko_contacts::link_seal`). The server therefore cannot decode or
//! verify it, and it no longer tries: a guessed id yields a blob that
//! does not open, and a substituted blob (a malicious discovery serving
//! another capability under a live id) fails the joiner's AEAD open,
//! which closes the substitution this module used to document as open.
//! What the server still needs arrives in clear upload fields:
//!
//!   * `revocation_pubkey` — a key minted for this one link
//!     (`lakko_contacts::invite::revocation_public_key`); a revoke must be
//!     signed by its seed, which the issuing device keeps beside the link
//!     secret.
//!   * `expiry_ms`, `max_uses` — the fetch expiry filter, the pruner and
//!     the consume ceiling. An uploader who lies about them only changes
//!     how their own link behaves.
//!
//! ## No owner, no issuer (ADR-0078 section 10)
//!
//! The row used to carry the uploading session's account id and the
//! issuer's identity key, which tied an account to every link it handed out.
//! It carries neither: the session token on the upload and revoke routes is
//! a rate-limit gate only and is never stored. The owner-list route that
//! needed the owner column is gone; a client lists its own links from the
//! record it keeps for each link it minted. Account deletion leaves rows to
//! their expiry, their last use or a revocation.
//!
//! ## What 1.18d2+ leaves for follow-ups
//!
//!   * **`max_uses` enforcement** — ✅ wired in 1.18d2a via the
//!     atomic `consume` handler below; `0005_invite_max_uses.sql`
//!     denormalised the ceiling onto the row.
//!   * **Per-joiner de-dup** — ✅ wired in 1.18d3b via the
//!     `invite_consumers` side table (PK on
//!     `(blob_id, joiner_tag)`, FK CASCADE on
//!     `invites.blob_id`). The consume handler runs an
//!     `INSERT IGNORE` against the table inside the same
//!     transaction as the counter bump; a repeat from the
//!     same joiner returns 200 with `already_consumed = true`
//!     and does NOT bump the counter. Joiner-tag construction
//!     is server-blind (HMAC over a per-blob salt — see
//!     `lakko_contacts::invite::compute_joiner_tag`); the
//!     server stores opaque 32-byte tags only.
//!   * **Owner-side revocation (`DELETE`)** — ✅ wired in
//!     1.18d2b via the `revoke` handler below; same Ed25519
//!     key that signed the capability has the right to delete
//!     the row. Replay-protected by a `signed_at_ms` window;
//!     "not found" + "bad signature" both fold into 404 to
//!     deny status-code probing.
//!   * **Expiry pruning** — ✅ wired in 1.18d2c via
//!     `spawn_pruner` below. Hourly `DELETE FROM invites WHERE
//!     expiry_ms != 0 AND expiry_ms < ?` keeps storage bloat
//!     contained; `idx_expiry` from `0004_invites.sql` makes
//!     the recurring sweep cheap. `expiry_ms = 0` rows
//!     (NEVER_EXPIRES) are preserved by design.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use lakko_contacts::invite::{
    revoke_canonical_bytes, JOINER_TAG_LEN, MAX_SEALED_LEN, REVOKE_REPLAY_WINDOW_MS,
};
use lakko_contacts::link_seal::LinkKind;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Server-side ceiling on the sealed blob: the longest sealed `/i/` link the
/// codec can produce (`lakko_contacts::invite::MAX_SEALED_LEN`, 693 bytes
/// since the ADR-0079 inviter preview), derived rather than written as a
/// literal. Pinned to the `VARBINARY` width of `invites.sealed_capability` in
/// `migrations/0031_invite_preview_width.sql` by a test.
const MAX_SEALED_BYTES: usize = MAX_SEALED_LEN;

// `BLOB_ID_LEN` is sourced from `lakko_contacts::invite` so the
// canonical-bytes layout (1.18d2b) and the row schema width
// stay pinned to the same constant. See the import above.

// ============================================================================
// POST /discovery/invite — upload a sealed capability + mint a random id
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    /// Standard-alphabet base64 of the sealed link blob
    /// (`lakko_contacts::invite::seal_link` over the composed capability).
    /// Opaque to the server.
    pub(crate) sealed: String,
    /// Standard-alphabet base64 of the 32-byte per-link revocation key
    /// (ADR-0078 section 10) — the key a revoke must verify against. It
    /// names nobody.
    pub(crate) revocation_pubkey: String,
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
    /// `/i/<blob_id>#<secret>` URL.
    pub(crate) blob_id: String,
}

/// `POST /discovery/invite` — store a sealed invite capability under a fresh id.
///
/// The session token (checked by the route layer) rations uploads per
/// account; the account is not stored.
pub(crate) async fn upload(
    State(state): State<AppState>,
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    let sealed = crate::ledger_id::parse_sealed_upload(
        &payload.sealed,
        LinkKind::Contact,
        MAX_SEALED_BYTES,
    )?;
    let revocation_pk = crate::ledger_id::parse_revocation_pubkey(&payload.revocation_pubkey)?;
    let expiry_ms: u64 = payload.expiry_ms;
    // 1.18d2a: the consume handler enforces the ceiling in a single atomic
    // UPDATE. Sentinels (`0` = single-use, `0xFFFFFFFF` = unlimited) are
    // mapped to effective ceilings at consume time via a SQL CASE expression.
    let max_uses: u32 = payload.max_uses;

    // A random id, never an upsert: a collision is retried under a fresh id
    // and can never overwrite another link's row (the short-link amendment
    // of ADR-0028, see `crate::ledger_id`).
    let db = state.db.clone();
    let blob_id = crate::ledger_id::insert_with_fresh_id("invites", |id| {
        let db = db.clone();
        let sealed = sealed.clone();
        async move {
            sqlx::query(
                "INSERT INTO invites \
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
    tracing::info!(bytes = sealed.len(), "invite stored");
    Ok(Json(UploadResponse {
        blob_id: blob_id_b64url,
    }))
}

// ============================================================================
// GET /discovery/invite/{blob_id} — fetch by id
// ============================================================================

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    /// Standard-alphabet base64 of the stored sealed blob. The client opens
    /// it with the link secret and verifies the capability inside.
    pub(crate) sealed: String,
    /// No uses left. A page with no session reads the link as dead; a signed-in
    /// client still reads it, and `consume` says whether this person used it.
    pub(crate) used_up: bool,
}

/// `GET /discovery/invite/{blob_id}` — fetch by id.
///
/// Returns **404 Not Found** when no row exists for the id, or
/// when the row's clear `expiry_ms` has passed (a courtesy filter;
/// the client re-checks the signed expiry inside the capability via
/// `InviteCapability.isValidNow` — the client clock is the one that
/// matters for the user-facing UX).
pub(crate) async fn fetch(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
) -> Result<Json<FetchResponse>, (StatusCode, String)> {
    let blob_id = crate::ledger_id::parse_blob_id(&blob_id_b64url)?;

    let row: Option<(Vec<u8>, u64, u32, u32)> = sqlx::query_as(
        "SELECT sealed_capability, expiry_ms, max_uses, used_count \
         FROM invites WHERE blob_id = ?",
    )
    .bind(&blob_id[..])
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "fetch invites");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    let (sealed, expiry_ms, max_uses, used_count) =
        row.ok_or((StatusCode::NOT_FOUND, "invite not found".to_string()))?;

    // Server-side expiry filter — `expiry_ms == 0` is the
    // `NEVER_EXPIRES` sentinel (1.18a). For non-zero values we
    // compare against the current wall clock.
    if expiry_ms != 0 && current_unix_ms() >= expiry_ms {
        // Treat expired invites as "not found" — a 410 Gone
        // would let an attacker probe valid blob_ids by the
        // distinct status code. 404 is indistinguishable from
        // "this id was never issued".
        return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
    }

    Ok(Json(FetchResponse {
        sealed: B64.encode(&sealed),
        used_up: crate::ledger_id::link_used_up(max_uses, used_count),
    }))
}

// ============================================================================
// POST /discovery/invite/{blob_id}/consume — atomic max_uses enforcement
// ============================================================================

/// Sentinel for "unlimited" `max_uses` — matches
/// `lakko_contacts::invite` and the JS `MaxUsesPreset::Unlimited`
/// preset. SQL enforcement uses this constant directly.
const MAX_USES_UNLIMITED: u32 = u32::MAX;

#[derive(Deserialize)]
pub(crate) struct ConsumeRequest {
    /// Standard-alphabet base64 of the 32-byte
    /// `compute_joiner_tag(capability_bytes,
    /// joiner_account_id)` output (1.18d3a), computed by the
    /// joiner over the PLAINTEXT capability the sealed link
    /// opened to. The server needs nothing else from it. Server stores
    /// this opaque tag in `invite_consumers` keyed by
    /// `(blob_id, joiner_tag)`; a re-consume from the same
    /// joiner conflicts on the PK and surfaces as
    /// `already_consumed` in the response.
    pub(crate) joiner_tag: String,
}

#[derive(Serialize)]
pub(crate) struct ConsumeResponse {
    /// Post-increment `used_count` — OR the existing counter
    /// when `already_consumed` is `true` (no bump on a repeat
    /// consume from the same joiner). The client uses this to
    /// surface "1 of 10 uses" copy in the UI when the invite has
    /// a finite `max_uses`.
    pub(crate) used_count: u32,
    /// Echo of the row's `max_uses` (sentinel mapping is
    /// preserved — `0` for single-use, `u32::MAX` for unlimited,
    /// any other N for an N-use ceiling). Lets the joiner UI
    /// render the same "single-use" / "up to N uses" /
    /// "unlimited" string the inviter saw on generation.
    pub(crate) max_uses: u32,
    /// `true` when the side-table PK on `(blob_id, joiner_tag)`
    /// already had a row — i.e. this joiner has consumed the
    /// invite before. The handler returns 200 OK in this case
    /// rather than 410 Gone because the joiner's own past
    /// consume is not an error condition; the UI surfaces
    /// "you've already accepted this invite" + Decline-only
    /// copy and skips the ContactRequest send.
    pub(crate) already_consumed: bool,
}

/// `POST /discovery/invite/{blob_id}/consume` — register one
/// consumption against the invite's `max_uses` ceiling, with
/// per-joiner dedup (Phase 1.18d3b).
///
/// Atomic flow within a single transaction:
///
///   1. `INSERT IGNORE INTO invite_consumers (blob_id, joiner_tag)`
///      — server-blind dedup. If `rows_affected == 0` the same
///      `(blob_id, joiner_tag)` pair has been seen before; we
///      idempotently return 200 with `already_consumed = true`
///      and DO NOT bump the counter. If `rows_affected == 1`
///      this is a fresh joiner against this invite and we
///      proceed to step 2.
///   2. The 1.18d2a atomic UPDATE bumps `used_count` only when
///      the row is alive AND has remaining budget. The CASE
///      expression maps the wire-format sentinels to
///      SQL-comparable ceilings:
///
///         * `max_uses == 0`           → effective ceiling 1 (SINGLE_USE)
///         * `max_uses == 0xFFFFFFFF`  → effective ceiling u32::MAX (unlimited)
///         * any other N               → effective ceiling N
///
///   3. If the bump took, COMMIT and return 200; if not, ROLLBACK
///      so the consumer-row insert from step 1 is undone (the
///      joiner can re-attempt later — though `max_uses` is
///      static at compose time, so realistically this only
///      matters if the invite is concurrently consumed past the
///      ceiling by other joiners during step 2's window).
///
/// HTTP outcomes:
///
///   * **200 OK** with `{ used_count, max_uses, already_consumed }`
///     — either a fresh consume (`already_consumed = false`,
///     counter bumped) or a repeat from the same joiner
///     (`already_consumed = true`, counter unchanged). The UI
///     branches on the flag.
///   * **404 Not Found** — no row for the id, OR the row has
///     expired. Folded together so an attacker cannot probe
///     valid blob_ids by status code (same posture as `fetch`).
///   * **410 Gone** — row exists, not expired, but the budget
///     is exhausted by other joiners. Distinct from 404.
#[allow(clippy::too_many_lines)] // single-transaction lifecycle —
                                 // extracting the dedup-insert / bump / disambiguation paths into
                                 // helpers would scatter the open `tx` across functions and obscure
                                 // the rollback ordering. The handler does one cohesive thing
                                 // (consume an invite atomically); preserving its linear shape is
                                 // worth the line count.
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

    // Single transaction — the dedup-insert and the counter
    // bump have to live or die together so a concurrent
    // exhaustion doesn't leave a stranded consumer-row behind.
    let mut tx = state.db.begin().await.map_err(|e| {
        tracing::error!(error = %e, "consume invites BEGIN");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    // Step 1 — dedup insert. INSERT IGNORE lets us discriminate
    // "first time" from "already consumed" by `rows_affected`
    // without raising / catching a duplicate-key error class.
    // The FK to `invites.blob_id` is what catches "no such
    // blob" — if the parent row doesn't exist, this insert
    // fails, and we map the error to 404 (folded into the
    // standard not-found path below).
    let insert_result =
        sqlx::query("INSERT IGNORE INTO invite_consumers (blob_id, joiner_tag) VALUES (?, ?)")
            .bind(&blob_id[..])
            .bind(&joiner_tag[..])
            .execute(&mut *tx)
            .await;

    let insert_rows = match insert_result {
        Ok(r) => r.rows_affected(),
        Err(e) => {
            // FK violation on a non-existent invite or transient DB
            // error. We rollback (best-effort) and proceed to the
            // SELECT-disambiguation block below — if the SELECT
            // also can't find the row, the standard 404 fold
            // surfaces; otherwise we propagate as 500.
            let _ = tx.rollback().await;
            // Distinguish FK violation (1452) from other errors
            // by string-matching the lowercase MySQL error message
            // — we don't pull the sqlx-mysql backend type for
            // numeric error codes here because the rest of the
            // crate goes through the same `tracing::error!`
            // pattern on opaque DB errors.
            let msg = format!("{e}");
            if msg.contains("foreign key constraint") {
                return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
            }
            tracing::error!(error = %e, "consume invite_consumers INSERT");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            ));
        },
    };

    if insert_rows == 0 {
        // Already consumed by this joiner. Read back the
        // current counter state for the response body, COMMIT
        // (a no-op transaction is fine), and return 200 idempotent.
        let row_result: Result<(u32, u32), _> =
            sqlx::query_as("SELECT used_count, max_uses FROM invites WHERE blob_id = ?")
                .bind(&blob_id[..])
                .fetch_one(&mut *tx)
                .await;
        let (used_count, max_uses) = match row_result {
            Ok(r) => r,
            // MariaDB's INSERT IGNORE downgrades the foreign-key violation of
            // a consume against an absent invite to a warning with zero rows
            // affected, so "no such invite" arrives here rather than in the
            // error arm above: fold it into the documented 404.
            Err(sqlx::Error::RowNotFound) => {
                let _ = tx.rollback().await;
                return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
            },
            Err(e) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %e, "consume invites read-back (already-consumed)");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                ));
            },
        };
        tx.commit().await.map_err(|e| {
            tracing::error!(error = %e, "consume invites COMMIT (already-consumed)");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;
        tracing::info!(
            used_count,
            max_uses,
            "invite already consumed by this joiner"
        );
        return Ok(Json(ConsumeResponse {
            used_count,
            max_uses,
            already_consumed: true,
        }));
    }

    // Step 2 — atomic UPDATE bumps used_count iff the row is
    // alive AND has remaining budget. Same shape as the 1.18d2a
    // standalone version, just nested in the transaction now.
    let update_result = sqlx::query(
        "UPDATE invites \
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
        tracing::error!(error = %e, "consume invites UPDATE");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    if update_result.rows_affected() == 1 {
        // Success — read back the post-increment state.
        let row: (u32, u32) =
            sqlx::query_as("SELECT used_count, max_uses FROM invites WHERE blob_id = ?")
                .bind(&blob_id[..])
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, "consume invites read-back");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal db error".to_string(),
                    )
                })?;
        tx.commit().await.map_err(|e| {
            tracing::error!(error = %e, "consume invites COMMIT");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;
        tracing::info!(used_count = row.0, max_uses = row.1, "invite consumed");
        return Ok(Json(ConsumeResponse {
            used_count: row.0,
            max_uses: row.1,
            already_consumed: false,
        }));
    }

    // 0 rows updated — disambiguate. SELECT the row regardless
    // of expiry / budget; if absent → 404, if expired → 404
    // (folded), else → 410 Gone. ROLLBACK so the consumer-row
    // insert from step 1 doesn't persist for an exhausted
    // invite.
    let row: Option<(u64, u32, u32)> =
        sqlx::query_as("SELECT expiry_ms, max_uses, used_count FROM invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "consume invites disambiguation");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;
    let _ = tx.rollback().await;

    let Some((expiry_ms, _max_uses, _used_count)) = row else {
        return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
    };
    if expiry_ms != 0 && now_ms >= expiry_ms {
        return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
    }
    Err((StatusCode::GONE, "invite max_uses reached".to_string()))
}

// ============================================================================
// DELETE /discovery/invite/{blob_id} — owner-side revocation (Phase 1.18d2b)
// ============================================================================

// `REVOKE_DOMAIN_V1` and `REVOKE_REPLAY_WINDOW_MS` live in
// `lakko_contacts::invite` (Phase 1.18d2b-web): the canonical
// layout is a protocol detail the web client and server both
// need to drive sign/verify against, and a single source of
// truth keeps them from drifting. Imported above.

#[derive(Deserialize)]
pub(crate) struct RevokeRequest {
    /// UNIX millis at which the body was signed. Must be within
    /// `REVOKE_REPLAY_WINDOW_MS` of the server clock.
    pub(crate) signed_at_ms: u64,
    /// Standard-alphabet base64 of the 64-byte Ed25519 signature
    /// over [`revoke_canonical_bytes`].
    pub(crate) signature: String,
}

/// `DELETE /discovery/invite/{blob_id}` — owner-signed delete.
///
/// Authorisation gate: the per-link revocation key the uploader
/// supplied beside the blob (ADR-0078 section 10) is the only key
/// permitted to revoke. Replay protection: `signed_at_ms` must
/// be within `REVOKE_REPLAY_WINDOW_MS` of the server clock.
///
/// Response model:
///
///   * **204 No Content** — the row was deleted (or was already
///     gone — DELETE is idempotent on success path).
///   * **404 Not Found** — folded over the union of "no row" /
///     "signature did not verify" / "owner pubkey could not be
///     recovered". Keeps the endpoint indistinguishable from a
///     never-issued blob_id under attacker probing — the only
///     status-code signal the prober gets is the same as a
///     random unknown id.
///   * **400 Bad Request** — body shape failures (bad base64,
///     wrong-sized signature, `signed_at_ms` outside the replay
///     window). These ARE distinct from 404 because they
///     indicate a buggy client, not a probing attacker.
pub(crate) async fn revoke(
    State(state): State<AppState>,
    Path(blob_id_b64url): Path<String>,
    // Phase 1.5b: a session token is required (route_layer'd in
    // `build_app`) as a rate-limit / DoS gate only; the signature
    // on the body is the authorisation, and the account is not
    // recorded anywhere.
    Json(payload): Json<RevokeRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let blob_id = crate::ledger_id::parse_blob_id(&blob_id_b64url)?;

    // Replay window. Reject before doing any DB work — this is
    // also a cheap DoS gate (an attacker can't make us SELECT
    // by spamming `signed_at_ms = 0`).
    let now_ms = current_unix_ms();
    if !within_replay_window(payload.signed_at_ms, now_ms) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "signed_at_ms {} outside the ±{}ms replay window (now = {})",
                payload.signed_at_ms, REVOKE_REPLAY_WINDOW_MS, now_ms,
            ),
        ));
    }

    // Decode signature up front for the same reason — no DB hit
    // before the body shape is validated.
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

    // Look up the revocation key stored beside the sealed blob. We
    // deliberately fold "no row" into the same 404 the
    // signature-failure path returns below — see the function-level
    // doc.
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT revocation_pk FROM invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "revoke invites SELECT");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;
    let Some(revocation_key) = row.and_then(|(k,)| crate::ledger_id::stored_revocation_key(&k))
    else {
        return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
    };

    // Build the canonical bytes the link's revocation key signed and verify.
    let canonical = revoke_canonical_bytes(&blob_id, payload.signed_at_ms);
    if revocation_key.verify(&canonical, &signature).is_err() {
        // Signature mismatch — fold into 404 (see doc comment).
        return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
    }

    // Authorised. Delete the row. We don't tombstone — ids are
    // server-minted random handles an uploader cannot choose, so a
    // revoked id comes back only if a later upload draws the same
    // 48 bits by chance; a clean DELETE is fine.
    sqlx::query("DELETE FROM invites WHERE blob_id = ?")
        .bind(&blob_id[..])
        .execute(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "revoke invites DELETE");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    tracing::info!("invite revoked");
    Ok(StatusCode::NO_CONTENT)
}

// ============================================================================
// Helpers
// ============================================================================

/// Wall-clock UNIX milliseconds, saturating to `0` on the
/// vanishingly unlikely path where `SystemTime::now()` is before
/// `UNIX_EPOCH`. Same shape as `fetch` — extracted so the two
/// handlers keep telling the same time.
fn current_unix_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(0)
}

/// Map the wire-format `max_uses` sentinels (1.18a) to an
/// effective ceiling the consume handler enforces. The SQL CASE
/// expression in the consume UPDATE is the actual enforcement;
/// this Rust mirror exists purely so a unit test can pin the
/// sentinel mapping. Any drift between the two surfaces here as
/// a test failure rather than as a per-row enforcement mismatch
/// in production.
///
///   * `0`           → 1            (`SINGLE_USE`)
///   * `0xFFFFFFFF`  → `u32::MAX`   (unlimited)
///   * any other N   → N
#[cfg(test)]
fn effective_max_uses(max_uses: u32) -> u32 {
    match max_uses {
        0 => 1,
        MAX_USES_UNLIMITED => MAX_USES_UNLIMITED,
        n => n,
    }
}

/// `true` iff `signed_at_ms` is within `±REVOKE_REPLAY_WINDOW_MS`
/// of `now_ms`. Symmetric so client / server clock skew is
/// tolerated equally in both directions.
fn within_replay_window(signed_at_ms: u64, now_ms: u64) -> bool {
    signed_at_ms.abs_diff(now_ms) <= REVOKE_REPLAY_WINDOW_MS
}

// ============================================================================
// Background expiry pruner (Phase 1.18d2c)
// ============================================================================

/// How often the invite-expiry pruner wakes up. Invites
/// typically live for hours or days (the UI presets are 1d /
/// 7d / 30d), so an hourly sweep is proportionate — much
/// shorter and we'd be hammering the DB for little value, much
/// longer and the storage-bloat side leaks across deploys.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Spawn a background task that periodically deletes expired
/// invite blobs (Phase 1.18d2c). Closes the storage-bloat side
/// of the table; the `idx_expiry` index from
/// `0004_invites.sql` is the load-bearing piece (without it
/// the periodic `WHERE expiry_ms != 0 AND expiry_ms < ?`
/// scan would do a full-table seq each tick).
///
/// `expiry_ms = 0` is the `NEVER_EXPIRES` sentinel from
/// `lakko_contacts::invite` — those rows are deliberately
/// preserved (the inviter explicitly opted in to a
/// stalking-risk shape per ADR-0005 §13.3; the codec layer
/// surfaces a UI warning, the storage layer does not
/// second-guess).
///
/// Shape: interval + loop + log + carry on, at a cadence
/// matched to the typical invite lifetime. Aborted when the
/// service stops; the `_invite_pruner` handle in `main.rs`
/// owns the abort.
pub(crate) fn spawn_pruner(pool: sqlx::MySqlPool) -> tokio::task::JoinHandle<()> {
    use crate::metrics::{job_run, jobs};
    lakko_metrics::job_registered(jobs::INVITE_PRUNE);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        // The first tick fires immediately — skip it. We don't
        // want a startup-time prune racing with the first
        // upload.
        interval.tick().await;
        loop {
            interval.tick().await;
            let now_ms = current_unix_ms();
            // `expiry_ms` column is `BIGINT UNSIGNED` —
            // matching the `u64` binds elsewhere in this
            // module (upload / fetch / consume).
            let result = sqlx::query("DELETE FROM invites WHERE expiry_ms != 0 AND expiry_ms < ?")
                .bind(now_ms)
                .execute(&pool)
                .await;
            job_run(jobs::INVITE_PRUNE, &result);
            match result {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::info!(rows = r.rows_affected(), "pruned expired invites",);
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "invites prune failed"),
            }
        }
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::invite::BLOB_ID_LEN;

    /// The `MAX_SEALED_BYTES` constant pins the
    /// `VARBINARY(...)` width declared in the migration. Drift
    /// surfaces as a test failure rather than a silent
    /// "everything past N bytes 500s in production".
    #[test]
    fn max_sealed_bytes_matches_column_width() {
        // ADR-0079: the column was widened for the inviter preview by 0032.
        let widened = include_str!("../migrations/0032_invite_preview_width.sql");
        let widened = widened.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            widened.contains(&format!(
                "MODIFY sealed_capability VARBINARY({MAX_SEALED_BYTES}) NOT NULL COMMENT \
                 'lakko_contacts::link_seal sealed InviteCapability"
            )),
            "MAX_SEALED_BYTES ({MAX_SEALED_BYTES}) must match invites.sealed_capability \
             in 0032_invite_preview_width.sql",
        );
        assert_eq!(MAX_SEALED_BYTES, 693);
    }

    /// ADR-0078 section 10: no ledger table has an owner or an issuer
    /// column, in the migration that last created them.
    #[test]
    fn no_ledger_table_has_an_owner_or_issuer_column() {
        let migration = include_str!("../migrations/0031_ownerless_invite_ledgers.sql");
        let ddl: String = migration
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        for table in ["invites", "call_invites", "group_invites"] {
            assert!(
                ddl.contains(&format!("CREATE TABLE IF NOT EXISTS {table} (")),
                "0031 must recreate {table}"
            );
        }
        for forbidden in [
            "owner_account_id",
            "issuer_pubkey",
            "owner",
            "issuer",
            "account",
        ] {
            assert!(
                !ddl.contains(forbidden),
                "no ledger column may name an owner, an issuer or an account: {forbidden}"
            );
        }
        assert_eq!(ddl.matches("revocation_pk").count(), 3);
    }

    #[test]
    fn blob_id_len_matches_column_width() {
        let migration = include_str!("../migrations/0031_ownerless_invite_ledgers.sql");
        assert!(
            migration.contains(&format!("BINARY({BLOB_ID_LEN})")),
            "BLOB_ID_LEN ({BLOB_ID_LEN}) must match the BINARY width \
             declared in 0031_ownerless_invite_ledgers.sql",
        );
    }

    /// `effective_max_uses` is the host-side mirror of the SQL
    /// CASE expression in the consume UPDATE. Any drift between
    /// the two surfaces here as a test failure rather than as a
    /// per-row enforcement mismatch in production.
    #[test]
    fn effective_max_uses_maps_sentinels() {
        // SINGLE_USE sentinel → effective 1
        assert_eq!(effective_max_uses(0), 1);
        // Unlimited sentinel → u32::MAX (enforcement no-op)
        assert_eq!(effective_max_uses(MAX_USES_UNLIMITED), MAX_USES_UNLIMITED);
        // Explicit N → N
        assert_eq!(effective_max_uses(1), 1);
        assert_eq!(effective_max_uses(10), 10);
        assert_eq!(effective_max_uses(42), 42);
    }

    /// The consume handler's atomic UPDATE relies on
    /// `MAX_USES_UNLIMITED` being exactly `u32::MAX`. A future
    /// constant rename / value bump would break the SQL CASE
    /// expression silently — pin it.
    #[test]
    fn max_uses_unlimited_matches_u32_max() {
        assert_eq!(MAX_USES_UNLIMITED, u32::MAX);
    }

    /// 0022 migration drift sentinel — the consume handler binds
    /// `max_uses` as `INT UNSIGNED`. A future migration that
    /// changes the column type would surface here.
    #[test]
    fn max_uses_column_type_is_int_unsigned() {
        let migration = include_str!("../migrations/0031_ownerless_invite_ledgers.sql");
        let normalised = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalised.contains("max_uses INT UNSIGNED NOT NULL DEFAULT 0"),
            "0031 migration must declare `max_uses INT UNSIGNED`",
        );
        // The default value is load-bearing: existing rows from
        // 1.18d1 (no max_uses column) get backfilled to `0`,
        // which the sentinel mapping reads as SINGLE_USE — the
        // most-conservative interpretation of the original
        // 1.18d1 default. Pin so the default doesn't drift to
        // something more permissive (e.g. u32::MAX) silently.
        assert!(
            migration.contains("DEFAULT 0"),
            "max_uses default must be 0 (SINGLE_USE sentinel)",
        );
    }

    /// A maximal sealed invite survives the real ledger: upload stores the
    /// sealed blob with its clear fields, fetch returns it byte for byte,
    /// the link secret opens it, the row names no account, and a revoke
    /// verifies only against the link's own revocation key. Gated
    /// on `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skips when unset).
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn a_sealed_invite_round_trips_through_the_ledger() {
        use axum::extract::{Path, State};
        use axum::Json;
        use lakko_contacts::invite::{compose, open_link, seal_link, sign_revocation};
        use lakko_contacts::link_seal::generate_link_secret;
        use lakko_crypto::agreement::SecretKey as X25519SecretKey;
        use lakko_crypto::signing::SigningKey;

        let Some(db) =
            crate::anon_token_route_tests::test_pool("a_sealed_invite_round_trips").await
        else {
            return;
        };
        let state = crate::anon_token_route_tests::state(db, false);

        let signing = SigningKey::generate();
        let revocation = SigningKey::generate();
        let account_id = [0x31u8; lakko_contacts::ACCOUNT_ID_LEN];
        let label = "L".repeat(lakko_contacts::invite::MAX_LABEL_LEN);
        let expiry_ms = current_unix_ms() + 3_600_000;
        let wire = compose(
            &signing,
            &account_id,
            &X25519SecretKey::generate().public_key(),
            expiry_ms,
            3,
            true,
            &label,
            &[0x1C; 16],
            // ADR-0078: the grant's mailbox binding must be the owner's.
            &lakko_contacts::reach::ReachGrant::derive(
                &signing.to_seed(),
                lakko_contacts::reach::ReachClass::Link,
                &[0x1C; 16],
                0,
            )
            .unwrap(),
            // ADR-0079: the longest inviter preview, so the row is the
            // widest sealed blob the column must hold.
            &lakko_contacts::invite::InvitePreview {
                display_name: format!("{}\u{20AC}", "\u{1F600}".repeat(63)),
                username: "a".repeat(32),
            },
        )
        .unwrap();
        let secret = generate_link_secret();
        let sealed = seal_link(&secret, &wire).unwrap();
        assert_eq!(sealed.len(), MAX_SEALED_BYTES);
        assert_eq!(sealed.len(), MAX_SEALED_BYTES);

        let Json(stored) = upload(
            State(state.clone()),
            Json(UploadRequest {
                sealed: B64.encode(&sealed),
                revocation_pubkey: B64.encode(revocation.verifying_key().to_bytes()),
                expiry_ms,
                max_uses: 3,
            }),
        )
        .await
        .expect("a maximal sealed invite uploads");
        let Json(fetched) = fetch(State(state.clone()), Path(stored.blob_id.clone()))
            .await
            .expect("the stored blob fetches");
        let fetched = B64.decode(fetched.sealed).unwrap();
        assert_eq!(fetched, sealed);
        let (cap, opened) = open_link(&secret, &fetched).expect("the secret opens it");
        assert_eq!(opened, wire);
        assert_eq!(cap.label, label);

        // The stored row holds the revocation key and nothing that names
        // the account or its identity key.
        let blob_id = crate::ledger_id::parse_blob_id(&stored.blob_id).unwrap();
        let (stored_pk,): (Vec<u8>,) =
            sqlx::query_as("SELECT revocation_pk FROM invites WHERE blob_id = ?")
                .bind(blob_id.to_vec())
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(stored_pk, revocation.verifying_key().to_bytes().to_vec());
        assert_ne!(stored_pk, signing.verifying_key().to_bytes().to_vec());

        // A revoke signed by the identity key is a 404; the link key's is a 204.
        let now = current_unix_ms();
        let Err((status, _)) = revoke(
            State(state.clone()),
            Path(stored.blob_id.clone()),
            Json(RevokeRequest {
                signed_at_ms: now,
                signature: B64.encode(sign_revocation(&signing, &blob_id, now)),
            }),
        )
        .await
        else {
            panic!("a foreign key must not revoke");
        };
        assert_eq!(status, StatusCode::NOT_FOUND);
        let ok = revoke(
            State(state.clone()),
            Path(stored.blob_id.clone()),
            Json(RevokeRequest {
                signed_at_ms: now,
                signature: B64.encode(sign_revocation(&revocation, &blob_id, now)),
            }),
        )
        .await
        .expect("the link's revocation key revokes");
        assert_eq!(ok, StatusCode::NO_CONTENT);
        assert!(fetch(State(state), Path(stored.blob_id)).await.is_err());
    }

    /// An upload whose blob is not an `/i/` seal, is over the ceiling, or
    /// carries a revocation key that is not one is refused before any insert.
    #[tokio::test]
    async fn malformed_uploads_are_refused() {
        use axum::extract::State;
        use axum::Json;
        use lakko_contacts::link_seal::{generate_link_secret, seal, LinkKind};

        let Some(db) = crate::anon_token_route_tests::test_pool("invite_malformed_uploads").await
        else {
            return;
        };
        let state = crate::anon_token_route_tests::state(db, false);
        let key = B64.encode(
            lakko_crypto::signing::SigningKey::generate()
                .verifying_key()
                .to_bytes(),
        );
        let call_blob = seal(LinkKind::Call, &generate_link_secret(), b"x").unwrap();
        let own_blob = seal(LinkKind::Contact, &generate_link_secret(), b"x").unwrap();
        for (sealed, revocation_pubkey, want) in [
            (call_blob, key.clone(), StatusCode::BAD_REQUEST),
            (
                vec![b'i'; MAX_SEALED_BYTES + 1],
                key.clone(),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
            (own_blob, B64.encode([1u8; 31]), StatusCode::BAD_REQUEST),
        ] {
            let Err((status, _)) = upload(
                State(state.clone()),
                Json(UploadRequest {
                    sealed: B64.encode(sealed),
                    revocation_pubkey,
                    expiry_ms: 0,
                    max_uses: 0,
                }),
            )
            .await
            else {
                panic!("a malformed upload must be refused");
            };
            assert_eq!(status, want);
        }
    }

    // The canonical-bytes layout, the replay-window constant
    // value, and the sign / verify round-trip are owned by
    // `lakko-contacts::invite::tests` (Phase 1.18d2b-web). The
    // server `revoke` handler verifies against the same shared
    // helper; a layout drift surfaces as a `lakko-contacts`
    // test failure first.

    /// 1.18d3b migration shape pin. The consume handler relies
    /// on the side table existing with PK `(blob_id,
    /// joiner_tag)` and FK CASCADE on `invites.blob_id`. A
    /// future migration that loosens either constraint would
    /// silently break dedup or leak orphan rows; surface here.
    #[test]
    fn invite_consumers_migration_shape() {
        let migration = include_str!("../migrations/0031_ownerless_invite_ledgers.sql");
        // Side-table exists.
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS invite_consumers"),
            "0022 migration must create `invite_consumers`",
        );
        // Tag column matches `JOINER_TAG_LEN`.
        assert!(
            migration.contains(&format!("joiner_tag   BINARY({JOINER_TAG_LEN})"))
                || migration.contains(&format!("joiner_tag BINARY({JOINER_TAG_LEN})")),
            "0022 migration must declare joiner_tag as BINARY({JOINER_TAG_LEN})",
        );
        // PK is the dedup key the consume handler reads.
        assert!(
            migration.contains("PRIMARY KEY (blob_id, joiner_tag)"),
            "0022 migration must PK on (blob_id, joiner_tag)",
        );
        // FK CASCADE is what gives us free cleanup on revoke /
        // expiry-prune.
        assert!(
            migration
                .contains("FOREIGN KEY (blob_id) REFERENCES invites(blob_id) ON DELETE CASCADE"),
            "0022 migration must FK CASCADE on invites.blob_id",
        );
    }

    /// 1.18d2c pruner cadence pin. The constant is also
    /// referenced in the function-level doc above; pin in code
    /// so a future bump (e.g. shortening to 5 min on a hot
    /// table) is a deliberate choice, not a silent edit.
    #[test]
    fn prune_interval_is_one_hour() {
        assert_eq!(PRUNE_INTERVAL, std::time::Duration::from_secs(3600));
    }

    /// `within_replay_window` is server-side only — the helper
    /// doesn't exist on the client side at all (the client
    /// trusts its own clock, the server is the one filtering),
    /// so the symmetric-window test belongs here.
    #[test]
    fn within_replay_window_is_symmetric() {
        let now = 1_700_000_000_000u64;
        // Inside: ±5 minutes.
        assert!(within_replay_window(now - 5 * 60 * 1000, now));
        assert!(within_replay_window(now + 5 * 60 * 1000, now));
        // Boundary: exactly ±10 minutes — accepted (≤).
        assert!(within_replay_window(now - REVOKE_REPLAY_WINDOW_MS, now));
        assert!(within_replay_window(now + REVOKE_REPLAY_WINDOW_MS, now));
        // Outside: > 10 minutes.
        assert!(!within_replay_window(
            now - REVOKE_REPLAY_WINDOW_MS - 1,
            now
        ));
        assert!(!within_replay_window(
            now + REVOKE_REPLAY_WINDOW_MS + 1,
            now
        ));
    }
}
