// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Shared-secret gate for `POST /witness/cosign` (ADR-0020).
//!
//! Only the transparency operator drives the cosign endpoint, so it is gated on
//! a shared secret presented as `Authorization: Bearer <secret-b64>` — the same
//! posture the transparency append endpoint uses. This is a DoS / abuse gate,
//! NOT the security boundary: the witness's real defence is the consistency
//! check it runs before co-signing (an operator with the secret still cannot
//! get a co-signature on a forked root).

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use subtle::ConstantTimeEq as _;

use crate::AppState;

/// Minimum decoded length for the cosign shared secret.
pub(crate) const MIN_COSIGN_SECRET_LEN: usize = 16;

/// Load + validate the cosign secret from `LAKKO_WITNESS_COSIGN_SECRET`.
///
/// `Ok(None)` when unset (the cosign endpoint then serves 503); `Err` when set
/// but malformed (not base64 / too short) — fail fast rather than boot into a
/// state where every cosign is silently rejected.
///
/// # Errors
///
/// Returns an error string if the value is not valid base64 or decodes to fewer
/// than [`MIN_COSIGN_SECRET_LEN`] bytes.
pub(crate) fn load_cosign_secret() -> Result<Option<Arc<Vec<u8>>>, String> {
    let Ok(raw) = std::env::var("LAKKO_WITNESS_COSIGN_SECRET") else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let bytes = B64
        .decode(raw.as_bytes())
        .map_err(|e| format!("LAKKO_WITNESS_COSIGN_SECRET is not valid base64: {e}"))?;
    if bytes.len() < MIN_COSIGN_SECRET_LEN {
        return Err(format!(
            "LAKKO_WITNESS_COSIGN_SECRET decodes to {} bytes; need >= {MIN_COSIGN_SECRET_LEN}",
            bytes.len()
        ));
    }
    Ok(Some(Arc::new(bytes)))
}

/// Middleware gating `POST /witness/cosign` on the shared secret.
///
/// - secret unset on the server → **503** (no anonymous-cosign fallback),
/// - missing / non-Bearer / non-base64 / mismatching secret → **401**.
pub(crate) async fn require_cosign_secret(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(secret) = state.cosign_secret.as_ref() else {
        tracing::error!(
            "cosign rejected: LAKKO_WITNESS_COSIGN_SECRET unset — the cosign endpoint is disabled"
        );
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };

    let header_value = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            tracing::warn!("cosign rejected: missing Authorization header");
            StatusCode::UNAUTHORIZED
        })?;

    let presented_b64 = header_value.strip_prefix("Bearer ").ok_or_else(|| {
        tracing::warn!("cosign rejected: Authorization header missing 'Bearer ' prefix");
        StatusCode::UNAUTHORIZED
    })?;

    let presented = B64.decode(presented_b64.as_bytes()).map_err(|_| {
        tracing::warn!("cosign rejected: bearer value not valid base64");
        StatusCode::UNAUTHORIZED
    })?;

    let matches: bool = secret.as_slice().ct_eq(presented.as_slice()).into();
    if !matches {
        tracing::warn!("cosign rejected: cosign secret mismatch");
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
}
