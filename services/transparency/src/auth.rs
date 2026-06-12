// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Session-token verification middleware.
//!
//! `services/auth` issues HMAC-SHA256 bearer tokens on every
//! `*/finish` endpoint; this middleware verifies them via the
//! shared `LAKKO_SESSION_TOKEN_SECRET` secret and stashes the
//! authenticated `account_id` (16 raw UUIDv7 bytes) in axum
//! request extensions for downstream handlers to consume.
//!
//! Identical shape to `services/discovery`'s `auth.rs`. The append
//! endpoint sits behind this gate for S1; S2 will have the discovery
//! service call the append endpoint with its own bearer.

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::AppState;

/// Authenticated principal — surfaces in axum request extensions
/// for handlers that need to bind business state to a specific
/// account.
///
/// Wraps the raw 16-byte account_id (NOT base64-encoded, NOT
/// json-stringified — just the bytes the token's HMAC was
/// computed over).
#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedAccount(#[allow(dead_code)] pub(crate) [u8; 16]);

/// Middleware factory. Apply via
/// `.route_layer(axum::middleware::from_fn_with_state(state.clone(), require_session))`.
///
/// Returns 401 on any failure mode (missing header, malformed
/// Bearer prefix, expired token, bad signature, version mismatch).
/// **Body is identical** for every failure so an attacker cannot
/// distinguish failure causes via response-shape probing — all
/// surface as a bare 401.
///
/// Logs the failure cause to `tracing::warn!` for operator
/// diagnostics; clients see only the opaque 401.
pub(crate) async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let header_value = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            tracing::debug!("session-token middleware: missing Authorization header");
            StatusCode::UNAUTHORIZED
        })?;

    let token = header_value.strip_prefix("Bearer ").ok_or_else(|| {
        tracing::warn!("session-token middleware: Authorization header missing 'Bearer ' prefix");
        StatusCode::UNAUTHORIZED
    })?;

    let now_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));

    let account_id_bytes = state
        .session_token_secret
        .verify(token, now_ms)
        .map_err(|e| {
            tracing::warn!(error = %e, "session-token verify failed");
            StatusCode::UNAUTHORIZED
        })?;

    // `verify` checks length already, but we re-check at the
    // boundary to convert into the fixed-size array the handlers
    // expect. A length mismatch here would be a `lakko-auth-token`
    // crate bug.
    let account_id: [u8; 16] = account_id_bytes.as_slice().try_into().map_err(|_| {
        tracing::error!(
            "session-token middleware: account_id length {} after verify (expected 16)",
            account_id_bytes.len()
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    request
        .extensions_mut()
        .insert(AuthenticatedAccount(account_id));
    Ok(next.run(request).await)
}
