// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Session-token verification middleware (Phase 1.5b).
//!
//! `services/auth` issues HMAC-SHA256 bearer tokens on every
//! `*/finish` endpoint; this middleware verifies them via the
//! shared `LAKKO_SESSION_TOKEN_SECRET` secret and stashes the
//! authenticated `account_id` (16 raw UUIDv7 bytes) in axum
//! request extensions for downstream handlers to consume.
//!
//! ## Why a separate middleware module
//!
//! `axum::middleware::from_fn_with_state` is the canonical shape
//! for this kind of gate. We could inline it in `main.rs` but
//! the verify path has enough texture (Bearer-header parsing,
//! `now_ms` snapshot, error mapping to 401, extensions plumbing)
//! to warrant its own file. Routes that need authentication call
//! `.route_layer(middleware::from_fn_with_state(state.clone(), require_session))`;
//! lookups + GETs stay on the plain router with no layer.
//!
//! ## What this middleware does NOT enforce
//!
//! Per-endpoint **business** invariants — e.g. "the token's
//! account_id matches the request body's `account_id` field" —
//! belong in each handler, not here. The middleware only proves
//! "the caller holds a valid token for SOME account"; handlers
//! that mutate state for a specific account (registry::register,
//! identity_bundle::upload, keypackage::upload, invite::upload)
//! must additionally check the body's account_id against the
//! extracted [`AuthenticatedAccount`].

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
pub(crate) struct AuthenticatedAccount(pub(crate) [u8; 16]);

/// The key generation the session token was issued under (ADR-0073),
/// stashed beside [`AuthenticatedAccount`] so the two identity writes can
/// apply the generation floor of `identity_bundles.key_generation`. A
/// property of the credential, not of the principal, hence its own
/// extension; every other handler ignores it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionKeyGeneration(pub(crate) u64);

/// The device the session token names (ADR-0074 section 0.4): the device's
/// conversation leaf signature key, which is the `device_id` of the KeyPackage
/// directory. All zero for a session that names no device yet. Asserted by
/// the client when the session landed at auth, not proven; what it buys here
/// is that a session can only publish the device it was issued for, so a
/// removed device's still-valid token cannot publish under a fresh device
/// key. A property of the credential, hence its own extension.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionDevice(pub(crate) [u8; 32]);

/// Middleware factory. Apply via
/// `.route_layer(axum::middleware::from_fn_with_state(state.clone(), require_session))`.
///
/// Returns 401 on any failure mode (missing header, malformed
/// Bearer prefix, expired token, bad signature, version mismatch).
/// **Body is identical** for every failure so an attacker cannot
/// distinguish "your token expired" from "your secret got
/// rotated" via response-shape probing — both surface as
/// `"unauthorized"`.
///
/// Logs the failure cause to `tracing::warn!` for operator
/// diagnostics; clients see only the opaque 401.
pub(crate) async fn require_session(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    admit(&state, request, next, false).await
}

/// [`require_session`], also accepting the `s1` restricted session of a
/// suspended account ([ADR-0098] step 3). Used on `DELETE /discovery/account`
/// alone, so a suspended person who deletes the account at auth also removes
/// its username, identity bundle and KeyPackages here.
///
/// [ADR-0098]: ../../../docs/adr/0098-reporting-and-moderation.md
pub(crate) async fn require_session_or_restricted(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    admit(&state, request, next, true).await
}

async fn admit(
    state: &AppState,
    mut request: Request,
    next: Next,
    restricted_too: bool,
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

    // The key generation the v3 token carries (ADR-0073) is not compared
    // here: discovery's stateless verify accepts any generation within the
    // expiry. It is handed on, and the two identity writes apply the floor
    // of their own row (`key_reset::floor_refuses`).
    let verified = match state.session_token_secret.verify(token, now_ms) {
        Ok(v) => v,
        Err(_) if restricted_too => state
            .session_token_secret
            .verify_restricted(token, now_ms)
            .map_err(|e| {
                tracing::warn!(error = %e, "session-token verify failed");
                StatusCode::UNAUTHORIZED
            })?,
        Err(e) => {
            tracing::warn!(error = %e, "session-token verify failed");
            return Err(StatusCode::UNAUTHORIZED);
        },
    };

    request
        .extensions_mut()
        .insert(AuthenticatedAccount(verified.account_id));
    request
        .extensions_mut()
        .insert(SessionKeyGeneration(verified.key_generation));
    request
        .extensions_mut()
        .insert(SessionDevice(verified.device_id));
    Ok(next.run(request).await)
}
