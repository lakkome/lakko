// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Session-token verification middleware for the per-account
//! backup endpoint (Phase 2a1b).
//!
//! Mirror of `services/discovery::auth` and
//! `services/ds::auth` — same `LAKKO_SESSION_TOKEN_SECRET`-keyed
//! HMAC-SHA256 verification, same 401-on-any-failure posture.
//!
//! ## Why per-route auth, not whole-service
//!
//! `services/storage` is still primarily a **blind** ciphertext
//! blob proxy — anyone can `PUT /blob/<hash>` because the SHA-256
//! check is the only invariant the server cares about. The
//! per-account backup endpoint is the first authenticated route;
//! we attach the middleware only there via
//! `.route_layer(...)`, leaving the content-addressed blob
//! routes auth-free.

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::AppState;

/// Authenticated principal — surfaces in axum request extensions
/// for handlers that need to bind business state to a specific
/// account. Wraps the raw 16-byte UUIDv7 the token's HMAC was
/// computed over.
#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedAccount(pub(crate) [u8; 16]);

/// Middleware factory. Apply via
/// `.route_layer(axum::middleware::from_fn_with_state(state.clone(), require_session))`.
///
/// Returns 401 on any failure mode (missing header, malformed
/// Bearer prefix, expired token, bad signature, version
/// mismatch). Body is identical for every failure — an attacker
/// cannot distinguish "your token expired" from "secret rotated"
/// via response-shape probing.
pub(crate) async fn require_session(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    admit(&state, request, next, false).await
}

/// [`require_session`], also accepting the `s1` restricted session of a
/// suspended account ([ADR-0089] step 3). Used on `DELETE /backup` alone, so a
/// suspended person who deletes the account also removes the backup.
///
/// [ADR-0089]: ../../../docs/adr/0089-reporting-and-moderation.md
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

    // The key generation the v2 token carries (ADR-0073) is not applied here:
    // storage has no database to hold a floor and keeps the stateless verify,
    // so its revocation latency is the access-token TTL.
    let account_id = match state.session_token_secret.verify(token, now_ms) {
        Ok(v) => v.account_id,
        Err(_) if restricted_too => {
            state
                .session_token_secret
                .verify_restricted(token, now_ms)
                .map_err(|e| {
                    tracing::warn!(error = %e, "session-token verify failed");
                    StatusCode::UNAUTHORIZED
                })?
                .account_id
        },
        Err(e) => {
            tracing::warn!(error = %e, "session-token verify failed");
            return Err(StatusCode::UNAUTHORIZED);
        },
    };

    request
        .extensions_mut()
        .insert(AuthenticatedAccount(account_id));
    Ok(next.run(request).await)
}
