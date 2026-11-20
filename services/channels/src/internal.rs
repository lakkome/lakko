// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The internal moderation listener ([ADR-0059] section 7).
//!
//! The operator removes a listing, or blocks a channel from being listed
//! again, only here: a second listener bound to
//! `LAKKO_CHANNELS_INTERNAL_BIND` (default [`DEFAULT_INTERNAL_BIND`],
//! loopback) that nginx does not proxy, where every request must present
//! `x-lakko-internal-secret` equal to `LAKKO_CHANNELS_INTERNAL_SECRET` (the
//! push-gw internal-listener pattern: base64 of at least
//! [`MIN_SECRET_LEN`] bytes, compared in constant time). With the secret
//! unset the listener is not started at all.
//!
//! One route:
//!
//! ```text
//! POST /internal/v1/channels/{channel_id}/moderation
//! {"action": "remove" | "block" | "unblock", "reason": "<1-64 of [a-z0-9-]>"}
//! ```
//!
//! `remove` deletes the listing (the owner may list again); `block` deletes
//! it and refuses any later listing with `403 listing-blocked`; `unblock`
//! lifts the block. The reason code is kept in the moderation row. A block
//! may name a channel id that does not exist yet.
//!
//! [ADR-0059]: ../../../docs/adr/0059-channel-discover.md

use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::Deserialize;
use subtle::ConstantTimeEq as _;

use crate::api::{err, unix_ms, AppState, ErrJson};
use crate::directory::is_reason_code;
use crate::storage::{ChannelId, ModerationAction};

/// Default internal bind: loopback, the port after push-gw's internal 8092
/// in the service port plan (`infra/README.md`).
pub const DEFAULT_INTERNAL_BIND: &str = "127.0.0.1:8093";

/// The header the shared secret is presented in.
pub const SECRET_HEADER: &str = "x-lakko-internal-secret";

/// Minimum decoded length of the shared secret.
pub const MIN_SECRET_LEN: usize = 16;

/// Body limit on the internal listener: two short fields.
const MAX_INTERNAL_BODY_BYTES: usize = 1024;

/// Load and validate `LAKKO_CHANNELS_INTERNAL_SECRET`: `Ok(None)` when unset
/// or empty (the listener then stays off).
///
/// # Errors
///
/// A message naming the variable when it is set but not base64 of at least
/// [`MIN_SECRET_LEN`] bytes; the service refuses to start on it.
pub fn load_internal_secret() -> Result<Option<Arc<Vec<u8>>>, String> {
    parse_secret(
        std::env::var("LAKKO_CHANNELS_INTERNAL_SECRET")
            .ok()
            .as_deref(),
    )
}

/// [`load_internal_secret`] over an explicit value.
///
/// # Errors
///
/// See [`load_internal_secret`].
pub fn parse_secret(raw: Option<&str>) -> Result<Option<Arc<Vec<u8>>>, String> {
    let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let bytes = B64
        .decode(raw.as_bytes())
        .map_err(|_| "LAKKO_CHANNELS_INTERNAL_SECRET is not valid base64".to_string())?;
    if bytes.len() < MIN_SECRET_LEN {
        return Err(format!(
            "LAKKO_CHANNELS_INTERNAL_SECRET decodes to {} bytes; need >= {MIN_SECRET_LEN}",
            bytes.len()
        ));
    }
    Ok(Some(Arc::new(bytes)))
}

#[derive(Clone)]
struct InternalState {
    app: Arc<AppState>,
    secret: Arc<Vec<u8>>,
}

/// Body of the moderation route.
#[derive(Deserialize)]
pub struct ModerationRequest {
    /// `remove`, `block` or `unblock`.
    pub action: String,
    /// A reason code, 1-64 of `[a-z0-9-]`.
    pub reason: String,
}

/// The internal router: the moderation route behind the secret gate, and
/// nothing else.
pub fn internal_router(app: Arc<AppState>, secret: Arc<Vec<u8>>) -> Router {
    let state = InternalState { app, secret };
    Router::new()
        .route(
            "/internal/v1/channels/{channel_id}/moderation",
            post(moderate),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_internal_secret,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_INTERNAL_BODY_BYTES,
        ))
        // Requests by route template, method and status class (ADR-0096).
        .layer(lakko_metrics::red_layer("channels"))
        .with_state(state)
}

async fn require_internal_secret(
    State(state): State<InternalState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(presented) = request
        .headers()
        .get(SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        tracing::warn!("internal request refused: missing secret header");
        return Err(StatusCode::UNAUTHORIZED);
    };
    let Ok(presented) = B64.decode(presented.trim().as_bytes()) else {
        tracing::warn!("internal request refused: secret header not base64");
        return Err(StatusCode::UNAUTHORIZED);
    };
    // Constant time over the bytes; a length mismatch returns early, and the
    // length of a secret is not the secret.
    let matches: bool = state.secret.as_slice().ct_eq(presented.as_slice()).into();
    if !matches {
        tracing::warn!("internal request refused: secret mismatch");
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

async fn moderate(
    State(state): State<InternalState>,
    Path(channel_id): Path<String>,
    Json(req): Json<ModerationRequest>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            ErrJson(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    let action = match req.action.as_str() {
        "remove" => ModerationAction::Remove,
        "block" => ModerationAction::Block,
        "unblock" => ModerationAction::Unblock,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                ErrJson(err("bad-action", "action must be remove, block or unblock")),
            )
                .into_response()
        },
    };
    if !is_reason_code(&req.reason) {
        return (
            StatusCode::BAD_REQUEST,
            ErrJson(err("bad-reason", "reason must be 1-64 of [a-z0-9-]")),
        )
            .into_response();
    }
    match state
        .app
        .store
        .moderate(channel, action, &req.reason, unix_ms())
        .await
    {
        Ok(()) => {
            tracing::info!(action = %req.action, reason = %req.reason, "moderation applied");
            StatusCode::NO_CONTENT.into_response()
        },
        Err(e) => {
            tracing::error!(error = %e, "moderation failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrJson(err("internal", "unexpected error")),
            )
                .into_response()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_secret_must_be_base64_of_sixteen_bytes() {
        assert!(parse_secret(None).unwrap().is_none());
        assert!(parse_secret(Some("  ")).unwrap().is_none());
        assert!(parse_secret(Some("not base64!")).is_err());
        assert!(parse_secret(Some(&B64.encode([1u8; 15]))).is_err());
        assert_eq!(
            parse_secret(Some(&B64.encode([1u8; 16])))
                .unwrap()
                .unwrap()
                .len(),
            16
        );
    }
}
