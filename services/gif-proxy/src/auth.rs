// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Session-token verification for the `/gifs` routes.
//!
//! Same shape as `services/storage::auth`: the HMAC-SHA256 session token
//! `services/auth` issues, verified against `LAKKO_SESSION_TOKEN_SECRET`, with
//! an identical bare 401 for every failure. The one difference is logging:
//! nothing about the token or the failure reason is logged, only that a
//! request was refused.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::rate_limit::AccountKey;
use crate::AppState;

/// The verified account, inserted into request extensions. Used only as a
/// rate-limit and cache key; never logged.
#[derive(Clone, Copy)]
pub(crate) struct AuthenticatedAccount(pub(crate) AccountKey);

pub(crate) async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let now_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));

    // The key generation the v2 token carries (ADR-0073) is not applied here:
    // the proxy has no database to hold a floor and keeps the stateless
    // verify, so its revocation latency is the access-token TTL.
    let account: AccountKey = state
        .session_token_secret
        .verify(token, now_ms)
        .map_err(|_| {
            tracing::debug!("gif-proxy: session token refused");
            StatusCode::UNAUTHORIZED
        })?
        .account_id;

    request
        .extensions_mut()
        .insert(AuthenticatedAccount(account));
    Ok(next.run(request).await)
}
