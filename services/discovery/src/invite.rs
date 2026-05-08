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
//! ## What 1.18d1 leaves for d2 / d3
//!
//!   * **`max_uses` enforcement** — `used_count` column is in
//!     the schema but never consulted in d1; d2 wires the
//!     consume-counter path and a 410 Gone response when the
//!     ceiling is hit.
//!   * **Per-joiner de-dup (`HmacBloom`)** — d3 adds a side
//!     table per blob; today the server has no way to know
//!     whether the same joiner consumed the link twice.
//!   * **Owner-side revocation (`DELETE`)** — needs an auth
//!     gate that doesn't exist yet (the server can't trust a
//!     `DELETE /invite/{id}` without a session token from
//!     `services/auth`); deferred to d2.
//!   * **Expiry pruning** — the `idx_expiry` index supports a
//!     periodic `DELETE FROM invites WHERE expiry_ms != 0 AND
//!     expiry_ms < UNIX_TIMESTAMP()*1000` sweeper, but no
//!     scheduler exists yet. d2 ships it together with the
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

    sqlx::query(
        "INSERT INTO invites (blob_id, capability_bytes, owner_account_id, expiry_ms) \
         VALUES (?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE capability_bytes = VALUES(capability_bytes)",
    )
    .bind(&blob_id[..])
    .bind(&bytes)
    .bind(&owner_account_id[..])
    .bind(expiry_ms)
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
    if expiry_ms != 0 {
        let now_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(0);
        if now_ms >= expiry_ms {
            // Treat expired invites as "not found" — a 410 Gone
            // would let an attacker probe valid blob_ids by the
            // distinct status code. 404 is indistinguishable
            // from "this id was never issued".
            return Err((StatusCode::NOT_FOUND, "invite not found".to_string()));
        }
    }

    Ok(Json(FetchResponse {
        capability: B64.encode(&capability_bytes),
    }))
}

// ============================================================================
// Helpers
// ============================================================================

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
