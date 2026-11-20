// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The internal moderation listener ([ADR-0099] section 4, step 5).
//!
//! A channel is moderated only by a moderator's signed statement. The
//! moderation service logs it and forwards it here: a second listener bound
//! to `LAKKO_CHANNELS_INTERNAL_BIND` (default [`DEFAULT_INTERNAL_BIND`],
//! loopback) that nginx does not proxy, where every request presents
//! `x-lakko-internal-secret` equal to `LAKKO_CHANNELS_INTERNAL_SECRET`
//! (standard base64 of at least [`MIN_SECRET_LEN`] bytes, compared in
//! constant time). The service checks the statement itself with
//! `lakko_moderation::apply` against the moderator list under
//! `LAKKO_CHANNELS_MODERATION_ROOT_KEY`, so a compromised moderation service
//! cannot forge an action and the shared secret is transport authentication
//! only. With the secret or the root key unset the listener is not started.
//! (The ADR-0059 route `POST /internal/v1/channels/{id}/moderation`, which
//! took the secret as its authority, is gone.)
//!
//! Two routes:
//!
//! ```text
//! POST /internal/moderation/statement
//! {"signed_statement_b64", "moderator_list_b64", "log_index", "reason", "reason_salt_b64"}
//! POST /internal/moderation/list
//! {"moderator_list_b64"}
//! ```
//!
//! The list route (the moderation service pushes its list at startup)
//! raises the moderator-list floor, the highest list version seen: `200
//! {"list_version": n}`, `400 bad-list`, `409 stale-list` (older than the
//! floor).
//!
//! Actions (the target is the 16-byte channel id):
//!
//! - **ChannelDelist**: the listing goes and a new one is refused with
//!   `403 listing-delisted`;
//! - **ChannelBlockHandle**: the ADR-0059 block: the listing goes and a new
//!   one is refused with `403 listing-blocked`;
//! - **ChannelRemove**: the listing goes and every channel route for the id
//!   answers `410 {"code":"channel-removed","detail":..,"log_index":n}`
//!   ([`crate::api`]'s removal layer, which runs before the handlers);
//! - **ChannelRestore**: clears all three.
//!
//! The flags accumulate; a restore clears them. Answers, in order: `400
//! bad-list` (the list does not verify under the root); `409 stale-list`
//! for a list older than the floor, while a newer list raises the floor
//! first, kept even when the statement is then refused; the
//! `lakko_moderation::apply` statement refusals (`403 not-authorised`,
//! `400 bad-statement`, `400 stale-statement` / `400 future-statement` for
//! one issued more than 24 h before or 5 min after now, `400
//! wrong-service`, `400 reason-mismatch`); `409 stale-list` again when a
//! newer list arrived meanwhile; `200 {"applied":
//! false, "repeat": true}` for a statement applied before, `200 {"applied":
//! false, "superseded": true}` for one older by `issued_at_ms` than the
//! newest applied to the channel, else `200 {"applied": true}`. A statement
//! may name a channel id that does not exist yet.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_moderation::apply::{ApplyError, ApplyingService, ListBody, StatementBody};
use lakko_moderation::statement::Action;
use lakko_moderation::ModeratorList;
use subtle::ConstantTimeEq as _;

use crate::api::{unix_ms, AppState};
use crate::storage::{ChannelId, ChannelModeration, ModerationOutcome, ModerationStatement};

/// Default internal bind: loopback, the port after push-gw's internal 8092
/// in the service port plan (`infra/README.md`).
pub const DEFAULT_INTERNAL_BIND: &str = "127.0.0.1:8093";

/// The header the shared secret is presented in.
pub const SECRET_HEADER: &str = "x-lakko-internal-secret";

/// Minimum decoded length of the shared secret.
pub const MIN_SECRET_LEN: usize = 16;

/// Body limit on the internal listener: a signed statement, a moderator list
/// of up to 64 entries and a few short fields, in base64.
const MAX_INTERNAL_BODY_BYTES: usize = 256 * 1024;

const SECRET_VAR: &str = "LAKKO_CHANNELS_INTERNAL_SECRET";
const ROOT_KEY_VAR: &str = "LAKKO_CHANNELS_MODERATION_ROOT_KEY";

/// What the internal listener needs besides its bind (which is in
/// [`crate::Config`]).
#[derive(Clone)]
pub struct InternalConfig {
    /// The shared secret the moderation service presents.
    pub secret: Arc<Vec<u8>>,
    /// The pinned moderation root public key.
    pub root_key: [u8; 32],
}

impl std::fmt::Debug for InternalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalConfig").finish_non_exhaustive()
    }
}

/// Load `LAKKO_CHANNELS_INTERNAL_SECRET` and
/// `LAKKO_CHANNELS_MODERATION_ROOT_KEY`: `Ok(None)` when either is unset or
/// empty (the listener then stays off).
///
/// # Errors
///
/// A message naming the variable when one is set but malformed; the service
/// refuses to start on it.
pub fn load_internal() -> Result<Option<InternalConfig>, String> {
    parse_internal(
        std::env::var(SECRET_VAR).ok().as_deref(),
        std::env::var(ROOT_KEY_VAR).ok().as_deref(),
    )
}

/// [`load_internal`] over explicit values.
///
/// # Errors
///
/// See [`load_internal`].
pub fn parse_internal(
    secret: Option<&str>,
    root_key: Option<&str>,
) -> Result<Option<InternalConfig>, String> {
    let secret = parse_secret(secret)?;
    let root_key = match root_key.map(str::trim).filter(|v| !v.is_empty()) {
        None => None,
        Some(raw) => Some(
            URL_SAFE_NO_PAD
                .decode(raw)
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                .ok_or_else(|| format!("{ROOT_KEY_VAR} is not a base64url 32-byte key"))?,
        ),
    };
    Ok(match (secret, root_key) {
        (Some(secret), Some(root_key)) => Some(InternalConfig { secret, root_key }),
        _ => None,
    })
}

/// The shared secret: `Ok(None)` when unset or empty.
///
/// # Errors
///
/// When it is set but not standard base64 of at least [`MIN_SECRET_LEN`]
/// bytes.
pub fn parse_secret(raw: Option<&str>) -> Result<Option<Arc<Vec<u8>>>, String> {
    let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let bytes = B64
        .decode(raw.as_bytes())
        .map_err(|_| format!("{SECRET_VAR} is not valid base64"))?;
    if bytes.len() < MIN_SECRET_LEN {
        return Err(format!(
            "{SECRET_VAR} decodes to {} bytes; need >= {MIN_SECRET_LEN}",
            bytes.len()
        ));
    }
    Ok(Some(Arc::new(bytes)))
}

#[derive(Clone)]
struct InternalState {
    app: Arc<AppState>,
    config: InternalConfig,
}

/// The internal router: the statement route behind the secret gate, and
/// nothing else.
pub fn internal_router(app: Arc<AppState>, config: InternalConfig) -> Router {
    let state = InternalState { app, config };
    Router::new()
        .route("/internal/moderation/statement", post(apply_statement))
        .route("/internal/moderation/list", post(push_list))
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
    let matches: bool = state
        .config
        .secret
        .as_slice()
        .ct_eq(presented.as_slice())
        .into();
    if !matches {
        tracing::warn!("internal request refused: secret mismatch");
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

fn coded(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

fn refused(e: &ApplyError) -> Response {
    coded(
        StatusCode::from_u16(e.status()).unwrap_or(StatusCode::BAD_REQUEST),
        e.code(),
    )
}

/// Raise the floor to `list`'s version; the response of a list older than
/// the floor (`409 stale-list`) or of a store failure.
async fn raise_floor(state: &InternalState, list: &ModeratorList) -> Result<(), Response> {
    match state
        .app
        .store
        .raise_moderation_list_floor(list.list_version)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(refused(&ApplyError::StaleList)),
        Err(e) => {
            tracing::error!(error = %e, "moderator list floor failed");
            Err(coded(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        },
    }
}

/// `POST /internal/moderation/list`: raise the floor without a statement.
async fn push_list(State(state): State<InternalState>, Json(req): Json<ListBody>) -> Response {
    let list = match req.verify(&state.config.root_key) {
        Ok(l) => l,
        Err(e) => return refused(&e),
    };
    match raise_floor(&state, &list).await {
        Ok(()) => Json(serde_json::json!({ "list_version": list.list_version })).into_response(),
        Err(resp) => resp,
    }
}

const fn channel_action(action: Action) -> Option<ChannelModeration> {
    match action {
        Action::ChannelDelist => Some(ChannelModeration::Delist),
        Action::ChannelBlockHandle => Some(ChannelModeration::BlockHandle),
        Action::ChannelRemove => Some(ChannelModeration::Remove),
        Action::ChannelRestore => Some(ChannelModeration::Restore),
        _ => None,
    }
}

async fn apply_statement(
    State(state): State<InternalState>,
    Json(req): Json<StatementBody>,
) -> Response {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    // The list first, and the floor raised by it before the statement is
    // looked at: a newer list stays the floor even when the statement beside
    // it is refused.
    let list = match req.verified_list(&state.config.root_key) {
        Ok(l) => l,
        Err(e) => return refused(&e),
    };
    if let Err(resp) = raise_floor(&state, &list).await {
        return resp;
    }
    let checked = match req.check_against(&list, ApplyingService::Channels.actions(), now_ms) {
        Ok(c) => c,
        Err(e) => return refused(&e),
    };
    let statement = &checked.statement;
    // `ApplyingService::Channels.actions()` admits only the four channel
    // actions, each with a 16-byte channel target.
    let (Some(action), Ok(channel)) = (
        channel_action(statement.action),
        ChannelId::from_bytes(statement.target.bytes()),
    ) else {
        return coded(StatusCode::BAD_REQUEST, "wrong-service");
    };
    let record = ModerationStatement {
        statement_id: statement.statement_id,
        list_version: checked.list_version,
        channel,
        action,
        issued_at_ms: statement.issued_at_ms,
        log_index: req.log_index,
        reason: checked.reason.name(),
        now_ms: unix_ms(),
    };
    match state.app.store.apply_moderation(&record).await {
        Ok(outcome) => {
            let body = match outcome {
                ModerationOutcome::Changed => serde_json::json!({ "applied": true }),
                ModerationOutcome::Repeat => {
                    serde_json::json!({ "applied": false, "repeat": true })
                },
                ModerationOutcome::Superseded => {
                    serde_json::json!({ "applied": false, "superseded": true })
                },
                ModerationOutcome::StaleList => {
                    return coded(StatusCode::CONFLICT, "stale-list");
                },
            };
            tracing::info!(
                action = statement.action.name(),
                changed = outcome == ModerationOutcome::Changed,
                "moderation statement applied"
            );
            (StatusCode::OK, Json(body)).into_response()
        },
        Err(e) => {
            tracing::error!(error = %e, "moderation statement failed");
            coded(StatusCode::INTERNAL_SERVER_ERROR, "internal")
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

    #[test]
    fn the_listener_needs_both_secret_and_root() {
        let secret = B64.encode([7u8; 16]);
        let root = URL_SAFE_NO_PAD.encode([9u8; 32]);
        assert!(parse_internal(None, None).unwrap().is_none());
        assert!(parse_internal(Some(&secret), None).unwrap().is_none());
        assert!(parse_internal(None, Some(&root)).unwrap().is_none());
        let cfg = parse_internal(Some(&secret), Some(&root)).unwrap().unwrap();
        assert_eq!(cfg.root_key, [9u8; 32]);
        assert!(parse_internal(Some(&secret), Some("short")).is_err());
    }

    #[test]
    fn every_channel_action_maps_and_nothing_else() {
        for action in lakko_moderation::apply::ALL_ACTIONS {
            assert_eq!(
                channel_action(*action).is_some(),
                ApplyingService::Channels.actions().contains(action),
                "{action:?}"
            );
        }
    }
}
