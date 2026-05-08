// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Capability-based invite-link blob storage (Phase 1.18d1).
//!
//! Owners POST verified `InviteCapability` bytes here, get a
//! short content-addressed `blob_id` back, and embed
//! `https://lakko.me/i/<blob_id>` into the URL they share. Joiners
//! GET the bytes by id and run the existing 1.18a/b/c
//! `decodeAndVerify` flow locally.
//!
//! ## Why content-addressed
//!
//! `blob_id = SHA-256(capability_bytes)`. Same bytes ⇒ same id ⇒
//! a retried upload is a no-op via `INSERT … ON DUPLICATE KEY
//! UPDATE` (MariaDB upsert). The 32-byte hash also doubles as a
//! tamper detector: a malicious server cannot return substituted
//! bytes for a given `blob_id` without also changing the id, and
//! the joiner re-hashes the bytes locally before consuming
//! (V1.18d2 wires that check).
//!
//! ## Server-side verify
//!
//! We **decode_and_verify** the wire bytes before storing. Three
//! reasons:
//!   1. Junk uploads get rejected at ingress — keeps the table
//!      clean of forged signatures and oversized labels.
//!   2. The denormalised columns `owner_account_id` and
//!      `expiry_ms` come from the verified struct, so a malicious
//!      uploader cannot lie about the owner of an invite (e.g. to
//!      poison Alice's owner-revocation queue with bogus blobs
//!      attributed to her).
//!   3. The capability bytes are public by design — anyone with a
//!      link sees them — so coupling the server to
//!      `lakko-contacts` for verification adds no privacy risk.
//!
//! ## What 1.18d2+ leaves for follow-ups
//!
//!   * **`max_uses` enforcement** — ✅ wired in 1.18d2a via the
//!     atomic `consume` handler below; `0005_invite_max_uses.sql`
//!     denormalised the ceiling onto the row.
//!   * **Per-joiner de-dup (`HmacBloom`)** — d3 adds a side
//!     table per blob; today the server has no way to know
//!     whether the same joiner consumed the link twice.
//!   * **Owner-side revocation (`DELETE`)** — needs a signed
//!     request the consume / fetch path doesn't have today
//!     (the same Ed25519 key that signed the capability has the
//!     right to revoke; verify against the embedded
//!     `owner_signature_pubkey`). Deferred to 1.18d2b.
//!   * **Expiry pruning** — the `idx_expiry` index supports a
//!     periodic `DELETE FROM invites WHERE expiry_ms != 0 AND
//!     expiry_ms < UNIX_TIMESTAMP()*1000` sweeper, but no
//!     scheduler exists yet. 1.18d2c ships it together with the
//!     `pow` table's existing pruner pattern.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use lakko_contacts::invite::decode_and_verify;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AppState;

/// Server-side ceiling on the capability size. Pinned to the
/// `VARBINARY(2048)` column width in
/// `migrations/0004_invites.sql`. The 1.18a wire format tops out
/// at 224 bytes; the headroom covers the
/// `transparency_proof` extension ADR-0005 §13.3 anticipates
/// without another schema bump.
const MAX_CAPABILITY_BYTES: usize = 2048;

/// `blob_id` length — SHA-256 output, 32 bytes.
const BLOB_ID_LEN: usize = 32;

// ============================================================================
// POST /discovery/invite — upload + verify + content-address
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    pub(crate) pow: crate::pow::PowProof,
    /// Standard-alphabet base64 of `lakko_contacts::invite::compose(...)`
    /// output. Server decodes + verifies the wire signature
    /// before storing.
    pub(crate) capability: String,
}

#[derive(Serialize)]
pub(crate) struct UploadResponse {
    /// base64url-no-pad of the SHA-256 of `capability_bytes`.
    /// Same bytes ⇒ same id; a retried upload is idempotent.
    /// The web client substitutes this directly into the
    /// `/i/<blob_id>` URL.
    pub(crate) blob_id: String,
}

/// `POST /discovery/invite` — upsert a verified invite capability.
pub(crate) async fn upload(
    State(state): State<AppState>,
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

    // Verify the wire signature server-side. Rejects forged /
    // tampered / version-skewed payloads at ingress; also gives
    // us the parsed struct fields we denormalise into the row's
    // index columns (`owner_account_id`, `expiry_ms`).
    let cap = decode_and_verify(&bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("capability did not verify: {e}"),
        )
    })?;

    let blob_id = blob_id_for(&bytes);
    let owner_account_id = cap.owner_account_id;
    // SQL u64 column maps to MariaDB BIGINT UNSIGNED — sqlx
    // accepts u64 directly via the `chrono`-free numeric path.
    let expiry_ms: u64 = cap.expiry_ms;
    // 1.18d2a: denormalise `max_uses` so the consume handler
    // can enforce the ceiling in a single atomic UPDATE
    // without re-decoding `capability_bytes` on every call.
    // Sentinels (`0` = single-use, `0xFFFFFFFF` = unlimited)
    // are mapped to effective ceilings at consume time via a
    // SQL CASE expression — see `consume` below.
    let max_uses: u32 = cap.max_uses;

    sqlx::query(
        "INSERT INTO invites (blob_id, capability_bytes, owner_account_id, expiry_ms, max_uses) \
         VALUES (?, ?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE \
            capability_bytes = VALUES(capability_bytes), \
            max_uses = VALUES(max_uses)",
    )
    .bind(&blob_id[..])
    .bind(&bytes)
    .bind(&owner_account_id[..])
    .bind(expiry_ms)
    .bind(max_uses)
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "upsert invites");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    let blob_id_b64url = B64URL.encode(blob_id);
    tracing::info!(
        blob_id = %blob_id_b64url,
        owner_account_id = %B64.encode(owner_account_id),
        expiry_ms = expiry_ms,
        bytes = bytes.len(),
        "invite upserted",
    );
    Ok(Json(UploadResponse {
        blob_id: blob_id_b64url,
    }))
}

// ============================================================================
// GET /discovery/invite/{blob_id} — fetch by content-addressed id
// ============================================================================

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    /// Standard-alphabet base64 of the stored capability bytes.
    /// The client decodes + re-verifies locally before acting on
    /// the bytes.
    pub(crate) capability: String,
}

/// `GET /discovery/invite/{blob_id}` — fetch by id.
///
/// Returns **404 Not Found** when no row exists for the id, or
/// when the stored capability has expired (server-side enforces
/// expiry as a courtesy, but the client re-checks via
/// `InviteCapability.isValidNow` regardless — the client clock
/// is the only one that matters for the user-facing UX).
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
        sqlx::query_as("SELECT capability_bytes, expiry_ms FROM invites WHERE blob_id = ?")
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

    let (capability_bytes, expiry_ms) =
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
        capability: B64.encode(&capability_bytes),
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
    pub(crate) pow: crate::pow::PowProof,
}

#[derive(Serialize)]
pub(crate) struct ConsumeResponse {
    /// Post-increment `used_count`. The client uses this to
    /// surface "1 of 10 uses" copy in the UI when the invite has
    /// a finite `max_uses`.
    pub(crate) used_count: u32,
    /// Echo of the row's `max_uses` (sentinel mapping is
    /// preserved — `0` for single-use, `u32::MAX` for unlimited,
    /// any other N for an N-use ceiling). Lets the joiner UI
    /// render the same "single-use" / "up to N uses" /
    /// "unlimited" string the inviter saw on generation.
    pub(crate) max_uses: u32,
}

/// `POST /discovery/invite/{blob_id}/consume` — register one
/// consumption against the invite's `max_uses` ceiling.
///
/// Atomic semantics: a single UPDATE bumps `used_count` only
/// when the row exists, has not expired, and has remaining
/// budget. The CASE expression maps the wire-format sentinels
/// to SQL-comparable ceilings:
///
///   * `max_uses == 0`           → effective ceiling 1 (SINGLE_USE)
///   * `max_uses == 0xFFFFFFFF`  → effective ceiling u32::MAX (unlimited)
///   * any other N               → effective ceiling N
///
/// Outcomes:
///
///   * **200 OK** with `{ used_count, max_uses }` — counter
///     bumped successfully.
///   * **404 Not Found** — no row for the id, OR the row has
///     expired. We deliberately fold expired-blob into 404 (same
///     posture as `fetch`) so an attacker cannot probe valid
///     blob_ids by status code.
///   * **410 Gone** — row exists, not expired, but the budget
///     is exhausted. Distinct from 404 because the joiner
///     already had a peek at the bytes via GET — surfacing
///     "this invite has been used up" to the UI is more
///     informative than a generic "not found".
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

    // Single-shot atomic UPDATE: bump used_count iff the row is
    // alive AND has remaining budget. Result rows-affected
    // disambiguates the success path from the "no row / expired
    // / exhausted" path; a follow-up SELECT teases apart the
    // failure cases for the right HTTP status.
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
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "consume invites UPDATE");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    if update_result.rows_affected() == 1 {
        // Success — read back the (post-increment) state for
        // the response body.
        let row: (u32, u32) =
            sqlx::query_as("SELECT used_count, max_uses FROM invites WHERE blob_id = ?")
                .bind(&blob_id[..])
                .fetch_one(&state.db)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, "consume invites read-back");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal db error".to_string(),
                    )
                })?;
        tracing::info!(
            blob_id = %blob_id_b64url,
            used_count = row.0,
            max_uses = row.1,
            "invite consumed",
        );
        return Ok(Json(ConsumeResponse {
            used_count: row.0,
            max_uses: row.1,
        }));
    }

    // 0 rows updated — disambiguate. SELECT the row regardless
    // of expiry / budget; if absent → 404, if expired → 404
    // (folded), else → 410 Gone.
    let row: Option<(u64, u32, u32)> =
        sqlx::query_as("SELECT expiry_ms, max_uses, used_count FROM invites WHERE blob_id = ?")
            .bind(&blob_id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "consume invites disambiguation");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    let Some((expiry_ms, _max_uses, _used_count)) = row else {
        return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
    };
    if expiry_ms != 0 && now_ms >= expiry_ms {
        // Expired — fold into 404 to keep the response indistinguishable
        // from a never-issued blob_id.
        return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
    }
    // Row exists + not expired ⇒ budget exhausted.
    Err((StatusCode::GONE, "invite max_uses reached".to_string()))
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

/// Derive the content-addressed `blob_id` from the capability
/// wire bytes — a plain SHA-256.
fn blob_id_for(capability_bytes: &[u8]) -> [u8; BLOB_ID_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(capability_bytes);
    hasher.finalize().into()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_id_is_sha256_of_capability_bytes() {
        let bytes = b"a-test-capability-stand-in";
        let id = blob_id_for(bytes);
        assert_eq!(id.len(), BLOB_ID_LEN);

        // Independent SHA-256 with the same input must match.
        let mut h = Sha256::new();
        h.update(bytes);
        let expected: [u8; BLOB_ID_LEN] = h.finalize().into();
        assert_eq!(id, expected);
    }

    #[test]
    fn same_bytes_yield_same_blob_id() {
        let bytes = vec![0xAAu8; 200];
        assert_eq!(blob_id_for(&bytes), blob_id_for(&bytes));
    }

    #[test]
    fn different_bytes_yield_different_blob_id() {
        // SHA-256 collision space is 2^256 — practically zero
        // chance of an accidental tie on two single-byte-different
        // inputs.
        let a = [0xAAu8; 200];
        let mut b = a;
        b[0] = 0xBB;
        assert_ne!(blob_id_for(&a), blob_id_for(&b));
    }

    /// The `MAX_CAPABILITY_BYTES` constant pins the
    /// `VARBINARY(...)` width declared in the migration. Drift
    /// surfaces as a test failure rather than a silent
    /// "everything past N bytes 500s in production".
    #[test]
    fn max_capability_bytes_matches_column_width() {
        let migration = include_str!("../migrations/0004_invites.sql");
        assert!(
            migration.contains(&format!("VARBINARY({MAX_CAPABILITY_BYTES})")),
            "MAX_CAPABILITY_BYTES ({MAX_CAPABILITY_BYTES}) must match the \
             VARBINARY width declared in 0004_invites.sql",
        );
    }

    #[test]
    fn blob_id_len_matches_column_width() {
        let migration = include_str!("../migrations/0004_invites.sql");
        assert!(
            migration.contains(&format!("BINARY({BLOB_ID_LEN})")),
            "BLOB_ID_LEN ({BLOB_ID_LEN}) must match the BINARY width \
             declared in 0004_invites.sql",
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

    /// 0005 migration drift sentinel — the consume handler binds
    /// `max_uses` as `INT UNSIGNED`. A future migration that
    /// changes the column type would surface here.
    #[test]
    fn max_uses_column_type_is_int_unsigned() {
        let migration = include_str!("../migrations/0005_invite_max_uses.sql");
        assert!(
            migration.contains("max_uses INT UNSIGNED"),
            "0005 migration must declare `max_uses INT UNSIGNED`",
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

    /// Verifies the upload path's signature gate: hand-crafting a
    /// well-formed `InviteCapability` with `lakko-contacts::invite`
    /// and confirming `decode_and_verify` accepts it; flipping a
    /// signature byte and confirming rejection. This is the same
    /// invariant the upload handler relies on — pinning it here
    /// ensures a future change to `lakko-contacts` that loosens
    /// signature checking surfaces in the discovery test suite,
    /// not just on the client side.
    #[test]
    fn server_side_decode_and_verify_round_trip() {
        use lakko_contacts::invite::compose;
        use lakko_crypto::agreement::SecretKey as X25519SecretKey;
        use lakko_crypto::signing::SigningKey;

        let signing = SigningKey::from_seed(&[7u8; 32]);
        let account_id = [0xA1u8; 16];
        let x25519 = X25519SecretKey::generate().public_key();

        let bytes = compose(&signing, &account_id, &x25519, 0, 0, false, "alice@home").unwrap();
        let cap = decode_and_verify(&bytes).expect("verified");
        assert_eq!(cap.owner_account_id, account_id);
        assert_eq!(cap.label, "alice@home");

        // Tamper the trailing signature byte.
        let mut tampered = bytes.clone();
        let n = tampered.len();
        tampered[n - 1] ^= 0x01;
        assert!(decode_and_verify(&tampered).is_err());
    }
}
