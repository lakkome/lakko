// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Public identity-bundle directory.
//!
//! The discovery service already maps `username → account_id`
//! via [`crate::registry`]. To send a
//! `lakko-contacts::ContactRequest`, the sender ALSO needs the
//! recipient's:
//!
//!   * **Ed25519 identity public key** — pins the
//!     `from_pubkey` field on ContactRequest signature
//!     verification.
//!   * **X25519 public key** — the HPKE-Base sealed envelope's
//!     recipient key for the optional intro message inside the
//!     ContactRequest.
//!
//! Both are public; putting them in a queryable directory leaks
//! no secret. One row per `account_id`; uploaded at signup,
//! replaceable via UPSERT on key rotation (Phase 1.16d3c+).
//!
//! ## Known Phase-1 trust gap
//!
//! `POST /discovery/identity` currently TRUSTS the client's
//! claim of `account_id`, same as `POST /discovery/register`.
//! A PoW token is required, but no proof of ownership beyond
//! that. Phase 1.5b will gate this endpoint on an auth-issued
//! session token; until then, the squatting risk is identical
//! to `/discovery/register` and recoverable by table reset
//! before beta launch.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// `account_id` length in bytes (UUIDv7 raw, matches the rest of
/// the service).
const ACCOUNT_ID_LEN: usize = 16;
/// Ed25519 public key length (RFC 8032 §5.1).
const SIGNATURE_PUBKEY_LEN: usize = 32;
/// X25519 public key length (RFC 7748 §5).
const X25519_PUBKEY_LEN: usize = 32;
/// Maximum supersession-proof length the server will store (G-4, ADR-0016).
/// The current `lakko-transparency` proof is 145 bytes; the cap leaves a
/// little headroom and matches the `VARBINARY(150)` column. The server caps
/// the length only to bound storage — it never parses the bytes (invariant
/// #2); validation is the receiving peer's job.
const MAX_SUPERSESSION_PROOF_LEN: usize = 150;

/// A fetched `identity_bundles` row: `(signature_pubkey, x25519_pubkey,
/// supersession_proof?)` — the proof column is nullable (G-4, ADR-0016).
type IdentityBundleRow = (Vec<u8>, Vec<u8>, Option<Vec<u8>>);

fn decode_fixed<const N: usize>(
    b64: &str,
    name: &'static str,
) -> Result<[u8; N], (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid {name} base64: {e}"),
        )
    })?;
    if bytes.len() != N {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{name} must be {N} bytes after base64 decode"),
        ));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Decode the optional G-4 supersession proof (ADR-0016). `None` (the field
/// absent) stays `None`; the upsert then PRESERVES any already-stored proof
/// (via `COALESCE`), so a non-rotation re-upload does not strip a still-valid
/// proof. A present value is base64-decoded and length-capped to
/// [`MAX_SUPERSESSION_PROOF_LEN`]
/// — the server does NOT parse or verify the bytes (invariant #2); that is the
/// receiving peer's job via `verify_supersession`.
fn decode_optional_proof(b64: Option<&str>) -> Result<Option<Vec<u8>>, (StatusCode, String)> {
    let Some(b64) = b64 else {
        return Ok(None);
    };
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid supersession_proof base64: {e}"),
        )
    })?;
    if bytes.len() > MAX_SUPERSESSION_PROOF_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("supersession_proof exceeds {MAX_SUPERSESSION_PROOF_LEN} bytes"),
        ));
    }
    Ok(Some(bytes))
}

// ============================================================================
// POST /discovery/identity   — upsert
// ============================================================================

/// Upload (or replace) the identity bundle for the claimed
/// `account_id`. PoW-gated, claim-trusting (see file-level
/// docstring for the Phase-1 caveat).
#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    pub(crate) pow: crate::pow::PowProof,
    /// Base64 of the raw 16-byte UUIDv7.
    pub(crate) account_id: String,
    /// Base64 of the 32-byte Ed25519 public key.
    pub(crate) signature_pubkey: String,
    /// Base64 of the 32-byte X25519 public key (HPKE init_key).
    pub(crate) x25519_pubkey: String,
    /// G-4 (ADR-0016): base64 of an optional old-key-signed supersession
    /// proof bridging this rotation. Absent for a normal (non-rotation)
    /// upload, which clears any previously stored proof. The server stores
    /// it opaque and never parses it (invariant #2).
    #[serde(default)]
    pub(crate) supersession_proof: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct UploadResponse {
    pub(crate) ok: bool,
}

/// `POST /discovery/identity` — upsert.
///
/// Replaces an existing row atomically via MariaDB
/// `INSERT … ON DUPLICATE KEY UPDATE`. The
/// `updated_at` column auto-bumps via the table's
/// `ON UPDATE CURRENT_TIMESTAMP` clause.
pub(crate) async fn upload(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let account_id = decode_fixed::<ACCOUNT_ID_LEN>(&payload.account_id, "account_id")?;
    let signature_pubkey =
        decode_fixed::<SIGNATURE_PUBKEY_LEN>(&payload.signature_pubkey, "signature_pubkey")?;
    let x25519_pubkey = decode_fixed::<X25519_PUBKEY_LEN>(&payload.x25519_pubkey, "x25519_pubkey")?;

    // G-4 (ADR-0016): decode the optional supersession proof (opaque to the
    // server). `None` PRESERVES any stored proof (the `COALESCE` below), so a
    // routine non-rotation re-upload (login refresh, KeyPackage rotation)
    // does not strip a still-valid proof before peers have pulled it; a new
    // rotation supplies a fresh proof that replaces it, and the proof
    // self-expires via its own 14-day window regardless.
    let supersession_proof = decode_optional_proof(payload.supersession_proof.as_deref())?;

    // Phase 1.5b anti-impersonation: only the account_id's owner
    // can upload its identity bundle. Same check shape as
    // `registry::register`.
    if account_id != authed.0 {
        tracing::warn!(
            body_account_id = %B64.encode(account_id),
            token_account_id = %B64.encode(authed.0),
            "identity_bundle upload: body account_id does not match token's"
        );
        return Err((
            StatusCode::FORBIDDEN,
            "account_id mismatch between body and session token".to_string(),
        ));
    }

    sqlx::query(
        "INSERT INTO identity_bundles \
            (account_id, signature_pubkey, x25519_pubkey, supersession_proof) \
         VALUES (?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE \
            signature_pubkey   = VALUES(signature_pubkey), \
            x25519_pubkey      = VALUES(x25519_pubkey), \
            supersession_proof = COALESCE(VALUES(supersession_proof), supersession_proof)",
    )
    .bind(&account_id[..])
    .bind(&signature_pubkey[..])
    .bind(&x25519_pubkey[..])
    .bind(supersession_proof.as_deref())
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "upsert identity_bundles");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    tracing::info!(
        account_id = %B64.encode(account_id),
        "identity bundle upserted",
    );
    Ok(Json(UploadResponse { ok: true }))
}

// ============================================================================
// GET /discovery/identity/{account_id_b64url}   — fetch
// ============================================================================

/// Public response shape. Keys are base64-encoded with the
/// standard alphabet for parity with the rest of the service's
/// wire format.
#[derive(Serialize)]
pub(crate) struct FetchResponse {
    pub(crate) signature_pubkey: String,
    pub(crate) x25519_pubkey: String,
    /// G-4 (ADR-0016): base64 of the stored supersession proof, when one is
    /// present (i.e. the account rotated recently). Omitted from the JSON
    /// when absent. Opaque server bytes — the peer verifies it locally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) supersession_proof: Option<String>,
}

/// `GET /discovery/identity/{account_id_b64url}` — fetch.
///
/// No auth — identity public keys are public by definition. Same
/// rate-limit story as the rest of the discovery service:
/// per-IP `limit_req` at the nginx layer in production.
///
/// Returns **404 Not Found** when the account has no bundle
/// registered. The path encodes `account_id` as
/// **base64url-without-padding** (URL-safe alphabet) — the more
/// common standard alphabet uses `+` and `/`, which axum would
/// reject in a path segment.
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

    let row: Option<IdentityBundleRow> = sqlx::query_as(
        "SELECT signature_pubkey, x25519_pubkey, supersession_proof FROM identity_bundles \
         WHERE account_id = ?",
    )
    .bind(&bytes[..])
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "fetch identity_bundles");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    let (sig, x25519, proof) = row.ok_or((
        StatusCode::NOT_FOUND,
        "identity bundle not found".to_string(),
    ))?;

    if sig.len() != SIGNATURE_PUBKEY_LEN || x25519.len() != X25519_PUBKEY_LEN {
        // Schema enforces fixed lengths; a row that doesn't match
        // is corruption, not a client error.
        tracing::error!(
            sig_len = sig.len(),
            x25519_len = x25519.len(),
            "identity_bundles row has unexpected key lengths",
        );
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "stored bundle has unexpected key lengths".to_string(),
        ));
    }

    Ok(Json(FetchResponse {
        signature_pubkey: B64.encode(&sig),
        x25519_pubkey: B64.encode(&x25519),
        // Opaque pass-through — the server re-encodes the stored bytes
        // without interpreting them; the peer verifies the proof locally.
        supersession_proof: proof.map(|p| B64.encode(&p)),
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_fixed_validates_lengths() {
        // Right length.
        let ok = B64.encode([0u8; 32]);
        let bytes = decode_fixed::<32>(&ok, "x").unwrap();
        assert_eq!(bytes.len(), 32);

        // Too short.
        let short = B64.encode([0u8; 16]);
        let err = decode_fixed::<32>(&short, "x").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Too long.
        let long = B64.encode([0u8; 64]);
        let err = decode_fixed::<32>(&long, "x").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Non-base64.
        let err = decode_fixed::<32>("not-base64!!!", "x").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn decode_optional_proof_handles_absent_present_and_oversized() {
        // Absent -> None (the upsert then PRESERVES any stored proof).
        assert!(decode_optional_proof(None).unwrap().is_none());

        // Present, within the cap (the real proof is 145 bytes) -> decoded.
        let proof = B64.encode([7u8; 145]);
        let decoded = decode_optional_proof(Some(&proof)).unwrap().unwrap();
        assert_eq!(decoded.len(), 145);
        assert!(decoded.iter().all(|&b| b == 7));

        // Boundary: exactly the cap is accepted.
        let at_cap = B64.encode([0u8; MAX_SUPERSESSION_PROOF_LEN]);
        assert_eq!(
            decode_optional_proof(Some(&at_cap)).unwrap().unwrap().len(),
            MAX_SUPERSESSION_PROOF_LEN
        );

        // Over the cap -> 400 (bounds storage; not a crypto check).
        let oversized = B64.encode([0u8; MAX_SUPERSESSION_PROOF_LEN + 1]);
        let err = decode_optional_proof(Some(&oversized)).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Non-base64 -> 400.
        let err = decode_optional_proof(Some("not-base64!!!")).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn fetch_path_validates_base64url() {
        // The fetch handler decodes its `Path<String>` via
        // base64url-without-padding because axum rejects `/` and
        // `+` in path segments. Verify the alphabet round-trips
        // here so the contract surfaces in tests rather than
        // showing up as a 400 in production.
        use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
        let id = [0xABu8; ACCOUNT_ID_LEN];
        let encoded = B64URL.encode(id);
        // Standard-alphabet artefacts (`+`, `/`, `=`) must NOT appear.
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
        let decoded = B64URL.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), ACCOUNT_ID_LEN);
        assert_eq!(decoded.as_slice(), &id[..]);
    }
}
