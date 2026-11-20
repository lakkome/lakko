// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The moderator's blob delete ([ADR-0089] section 4, step 5).
//!
//! A moderator deletes a blob named in a report's evidence by a signed
//! statement. The moderation service logs it and forwards it to storage's
//! internal listener (`LAKKO_STORAGE_INTERNAL_BIND`, default
//! [`DEFAULT_INTERNAL_BIND`], loopback, never proxied by nginx). Every
//! request presents `x-lakko-internal-secret` = `LAKKO_STORAGE_INTERNAL_SECRET`
//! (standard base64 of at least [`MIN_SECRET_LEN`] bytes, compared in
//! constant time). Storage checks the statement itself with
//! `lakko_moderation::apply` against the moderator list under
//! `LAKKO_STORAGE_MODERATION_ROOT_KEY`, so a compromised moderation service
//! cannot delete a blob; no proof of ownership is needed under a moderator's
//! statement. With the secret or the root key unset the listener does not
//! start.
//!
//! `GET /internal/moderation/blob/{blob_hex}` (same secret) returns a
//! blob's ciphertext for the moderators' panel, which proxies it to a
//! moderator's browser to decrypt evidence with the key the report carries.
//! No pass, not an access (the blob's life is not extended); `404
//! blob-not-found` as for any gone blob, a moderator-deleted one included.
//!
//! `POST /internal/moderation/statement` with
//! `lakko_moderation::apply::StatementBody`. BlobDelete (a 32-byte blob id)
//! tombstones the id (kept forever: ids are ciphertext hashes), removes its
//! index row and, when the row was on this node or there was none, its file.
//! An upload of the id then answers `410 blob-removed` and a read `404
//! blob-not-found` as for any gone blob. Answers, in order: `400 bad-list`
//! (the list does not verify under the root); `409 stale-list` for a list
//! older than the floor (the newest list version seen), while a newer list
//! raises the floor first, kept even when the statement is then refused;
//! the `lakko_moderation::apply` statement refusals (`403 not-authorised`,
//! `400 bad-statement`, `400 stale-statement` / `400 future-statement` for
//! one issued more than 24 h before or 5 min after now, `400
//! wrong-service`, `400 reason-mismatch`); `409 stale-list` again when a
//! newer list arrived meanwhile; `200 {"applied":
//! false, "repeat": true}` for a statement applied before, `200 {"applied":
//! false, "already_removed": true}` for an id another statement removed,
//! else `200 {"applied": true}`. BlobDelete has no opposite action, so there
//! is no per-target order to keep.
//!
//! `POST /internal/moderation/list` with `lakko_moderation::apply::ListBody`
//! (`{"moderator_list_b64"}`, pushed by the moderation service at its
//! startup) raises the floor: `200 {"list_version": n}`, `400 bad-list`,
//! `409 stale-list` (older than the floor).
//!
//! [ADR-0089]: ../../../docs/adr/0089-reporting-and-moderation.md

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_moderation::apply::{ApplyError, ApplyingService, ListBody, StatementBody};
use lakko_moderation::ModeratorList;
use subtle::ConstantTimeEq as _;

use crate::blob::{blob_path, remove_blob_file};
use crate::index::{today, BlobId, ModerationDelete, ModerationOutcome};
use crate::AppState;

/// Default bind of the internal listener: loopback, not proxied by nginx.
pub(crate) const DEFAULT_INTERNAL_BIND: &str = "127.0.0.1:8098";
/// The header the shared secret is presented in.
pub(crate) const SECRET_HEADER: &str = "x-lakko-internal-secret";
/// Minimum decoded length of the shared secret.
pub(crate) const MIN_SECRET_LEN: usize = 16;
/// Body limit of the internal route: a signed statement, a moderator list
/// of up to 64 entries and a few short fields, in base64.
const MAX_INTERNAL_BODY_BYTES: usize = 256 * 1024;

const BIND_VAR: &str = "LAKKO_STORAGE_INTERNAL_BIND";
const SECRET_VAR: &str = "LAKKO_STORAGE_INTERNAL_SECRET";
const ROOT_KEY_VAR: &str = "LAKKO_STORAGE_MODERATION_ROOT_KEY";

/// What the internal listener needs.
#[derive(Clone)]
pub(crate) struct InternalConfig {
    /// Where it listens.
    pub(crate) bind: SocketAddr,
    /// The shared secret the moderation service presents.
    pub(crate) secret: Arc<Vec<u8>>,
    /// The pinned moderation root public key.
    pub(crate) root_key: [u8; 32],
}

impl std::fmt::Debug for InternalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalConfig")
            .field("bind", &self.bind)
            .finish_non_exhaustive()
    }
}

/// Read the internal listener's configuration from the environment.
/// `Ok(None)` when the secret or the root key is unset.
///
/// # Errors
///
/// A message naming the variable when one is set but malformed.
pub(crate) fn load_internal() -> Result<Option<InternalConfig>, String> {
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    parse_internal(
        var(BIND_VAR).as_deref(),
        var(SECRET_VAR).as_deref(),
        var(ROOT_KEY_VAR).as_deref(),
    )
}

/// [`load_internal`] over explicit values.
///
/// # Errors
///
/// See [`load_internal`].
pub(crate) fn parse_internal(
    bind: Option<&str>,
    secret: Option<&str>,
    root_key: Option<&str>,
) -> Result<Option<InternalConfig>, String> {
    let bind: SocketAddr = bind
        .unwrap_or(DEFAULT_INTERNAL_BIND)
        .parse()
        .map_err(|_| format!("{BIND_VAR} is not host:port"))?;
    let secret = match secret {
        None => None,
        Some(raw) => {
            let bytes = STANDARD
                .decode(raw)
                .map_err(|_| format!("{SECRET_VAR} is not valid base64"))?;
            if bytes.len() < MIN_SECRET_LEN {
                return Err(format!(
                    "{SECRET_VAR} decodes to {} bytes; need >= {MIN_SECRET_LEN}",
                    bytes.len()
                ));
            }
            Some(bytes)
        },
    };
    let root_key = match root_key {
        None => None,
        Some(raw) => Some(
            URL_SAFE_NO_PAD
                .decode(raw)
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                .ok_or_else(|| format!("{ROOT_KEY_VAR} is not a base64url 32-byte key"))?,
        ),
    };
    match (secret, root_key) {
        (Some(secret), Some(root_key)) => Ok(Some(InternalConfig {
            bind,
            secret: Arc::new(secret),
            root_key,
        })),
        _ => Ok(None),
    }
}

#[derive(Clone)]
struct InternalState {
    app: AppState,
    config: InternalConfig,
}

/// The internal router, behind the secret gate.
pub(crate) fn internal_router(app: AppState, config: InternalConfig) -> Router {
    let state = InternalState { app, config };
    Router::new()
        .route("/internal/moderation/statement", post(apply_statement))
        .route("/internal/moderation/list", post(push_list))
        .route("/internal/moderation/blob/{blob_hex}", get(read_blob))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_internal_secret,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_INTERNAL_BODY_BYTES,
        ))
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(lakko_log_hygiene::request_span),
        )
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
    let Ok(presented) = STANDARD.decode(presented.trim().as_bytes()) else {
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn refused(e: &ApplyError) -> Response {
    coded(
        StatusCode::from_u16(e.status()).unwrap_or(StatusCode::BAD_REQUEST),
        e.code(),
    )
}

/// Raise the floor to `list`'s version; the response of a list older than
/// the floor (`409 stale-list`) or of an index failure.
async fn raise_floor(state: &InternalState, list: &ModeratorList) -> Result<(), Response> {
    match state
        .app
        .index
        .raise_moderation_list_floor(list.list_version)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(refused(&ApplyError::StaleList)),
        Err(e) => {
            tracing::error!(error = %e, "moderator list floor: index failure");
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

/// `GET /internal/moderation/blob/{blob_hex}`.
async fn read_blob(State(state): State<InternalState>, Path(blob_hex): Path<String>) -> Response {
    match crate::blob::read_for_moderator(&state.app, &blob_hex).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn apply_statement(
    State(state): State<InternalState>,
    Json(req): Json<StatementBody>,
) -> Response {
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
    let checked = match req.check_against(
        &list,
        ApplyingService::Storage.actions(),
        i64::try_from(now_ms()).unwrap_or(i64::MAX),
    ) {
        Ok(c) => c,
        Err(e) => return refused(&e),
    };
    // BlobDelete is storage's only action; its target is a 32-byte blob id.
    let Ok(id) = BlobId::try_from(checked.statement.target.bytes()) else {
        return coded(StatusCode::BAD_REQUEST, "wrong-service");
    };
    let delete = ModerationDelete {
        statement_id: checked.statement.statement_id,
        list_version: checked.list_version,
        id,
        day: today(),
        now_ms: now_ms(),
    };
    let outcome = match state.app.index.apply_moderation_delete(&delete).await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "moderation statement: index failure");
            return coded(StatusCode::INTERNAL_SERVER_ERROR, "internal");
        },
    };
    let body = match outcome {
        ModerationOutcome::Removed { node } => {
            // The file lives on the row's node; with no row, a file left on
            // this node (an orphan) goes too.
            let on_this_node = match node.as_deref() {
                None => true,
                Some(n) => n == &*state.app.node,
            };
            if on_this_node {
                remove_blob_file(&blob_path(&state.app.blob_root, &id)).await;
            } else {
                tracing::warn!("moderator blob delete: the file is on another node");
            }
            serde_json::json!({ "applied": true })
        },
        ModerationOutcome::AlreadyRemoved => {
            serde_json::json!({ "applied": false, "already_removed": true })
        },
        ModerationOutcome::Repeat => serde_json::json!({ "applied": false, "repeat": true }),
        ModerationOutcome::StaleList => return coded(StatusCode::CONFLICT, "stale-list"),
    };
    tracing::info!(
        applied = body["applied"].as_bool().unwrap_or(false),
        "moderator blob delete applied"
    );
    (StatusCode::OK, Json(body)).into_response()
}

#[cfg(test)]
mod tests;
