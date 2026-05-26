// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HTTP endpoints for the channels service.
//!
//! Three Phase 4a routes:
//!
//! | Method | Path | Purpose |
//! |---|---|---|
//! | `POST` | `/v1/channels` | Register a fresh channel. Body carries the 32-hex `channel_id`. |
//! | `POST` | `/v1/channels/:channel_id/posts` | Publish an opaque ChannelPost. Body is application/octet-stream. |
//! | `GET`  | `/v1/channels/:channel_id/posts?after=<cursor>&limit=<n>` | Cursor-paginated fetch. |
//!
//! Every error path returns a JSON envelope with a stable
//! machine-readable `code` so clients can branch without
//! parsing prose. The wire shape:
//!
//! ```json
//! { "code": "unknown-channel", "detail": "..." }
//! ```

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::storage::{ChannelId, ChannelStore, StorageError, StoredPost};

/// Shared application state — config + storage handle.
#[derive(Clone)]
pub struct AppState {
    /// Runtime config (read-only after boot).
    pub cfg: Config,
    /// In-memory channel store. Multi-thread-safe; shares
    /// one `Arc<RwLock<…>>` across handlers.
    pub store: ChannelStore,
}

impl AppState {
    /// Build a fresh state container behind an `Arc`.
    #[must_use]
    pub fn new(cfg: Config, store: ChannelStore) -> Arc<Self> {
        Arc::new(Self { cfg, store })
    }
}

// ---------------------------------------------------------
// Wire types
// ---------------------------------------------------------

/// Body for `POST /v1/channels`.
#[derive(Deserialize)]
pub struct CreateChannelRequest {
    /// 32-char lowercase hex channel id.
    pub channel_id: String,
}

/// Response for `POST /v1/channels`.
#[derive(Serialize)]
pub struct CreateChannelResponse {
    /// Echoed channel id, lowercase hex.
    pub channel_id: String,
}

/// Query string for `GET /v1/channels/:id/posts`.
#[derive(Deserialize)]
pub struct FetchQuery {
    /// Return posts strictly after this cursor. `None`
    /// or `0` returns the channel from the beginning.
    #[serde(default)]
    pub after: u64,
    /// Maximum entries to return. Server clamps to
    /// `cfg.fetch_limit_max`.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One post in the fetch response.
#[derive(Serialize)]
pub struct PostView {
    /// Server-assigned monotone cursor.
    pub cursor: u64,
    /// Wall-clock at insert time, unix milliseconds.
    pub server_ts_ms: u64,
    /// Opaque payload as base64url (no padding).
    pub payload_b64: String,
}

impl PostView {
    fn from_stored(p: StoredPost) -> Self {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        Self {
            cursor: p.cursor,
            server_ts_ms: p.server_ts_ms,
            payload_b64: URL_SAFE_NO_PAD.encode(&p.payload),
        }
    }
}

/// Response for `GET /v1/channels/:id/posts`.
#[derive(Serialize)]
pub struct FetchResponse {
    /// Posts in cursor-ascending order.
    pub posts: Vec<PostView>,
    /// Highest cursor in the response, or the requested
    /// `after` if the response is empty. Lets the client
    /// thread the next request without scanning the
    /// payload.
    pub next_after: u64,
}

/// Wire-level error response.
#[derive(Serialize)]
pub struct ErrorResponse {
    /// Stable machine-readable code (kebab-case).
    pub code: &'static str,
    /// Human-readable detail. Never carries token /
    /// payload contents.
    pub detail: String,
}

fn err(code: &'static str, detail: impl Into<String>) -> ErrorResponse {
    ErrorResponse {
        code,
        detail: detail.into(),
    }
}

// ---------------------------------------------------------
// Handlers
// ---------------------------------------------------------

/// `POST /v1/channels` — register a new channel.
async fn create_channel(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateChannelRequest>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&req.channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    state.store.create(channel).await;
    (
        StatusCode::CREATED,
        Json(CreateChannelResponse {
            channel_id: channel.to_hex(),
        }),
    )
        .into_response()
}

/// `POST /v1/channels/:channel_id/posts` — publish.
async fn publish_post(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    body: Bytes,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
    match state
        .store
        .put(channel, body.to_vec(), state.cfg.max_post_bytes, now)
        .await
    {
        Ok(stored) => (
            StatusCode::CREATED,
            Json(PostView::from_stored(stored)),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err(
                "unknown-channel",
                "register the channel via POST /v1/channels first",
            )),
        )
            .into_response(),
        Err(StorageError::EmptyPost) => (
            StatusCode::BAD_REQUEST,
            Json(err("empty-post", "post body cannot be empty")),
        )
            .into_response(),
        Err(StorageError::PostTooLarge { max, got }) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(err(
                "post-too-large",
                format!("post body {got} bytes exceeds limit {max}"),
            )),
        )
            .into_response(),
        Err(StorageError::BadChannelId) => (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response(),
    }
}

/// `GET /v1/channels/:channel_id/posts` — fetch.
async fn fetch_posts(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Query(q): Query<FetchQuery>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    let limit = q
        .limit
        .unwrap_or(state.cfg.fetch_limit_default)
        .min(state.cfg.fetch_limit_max);
    match state.store.fetch(channel, q.after, limit).await {
        Ok(list) => {
            let next_after = list.last().map_or(q.after, |p| p.cursor);
            let resp = FetchResponse {
                posts: list.into_iter().map(PostView::from_stored).collect(),
                next_after,
            };
            (StatusCode::OK, Json(resp)).into_response()
        },
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(StorageError::BadChannelId) => (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("internal", "unexpected error")),
        )
            .into_response(),
    }
}

/// Assemble the axum router. Used by both the production
/// binary and integration tests.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/channels", post(create_channel))
        .route(
            "/v1/channels/{channel_id}/posts",
            post(publish_post).get(fetch_posts),
        )
        .route("/health", get(health))
        .route("/version", get(version))
        .with_state(state)
}

/// `GET /health` — liveness probe.
async fn health() -> &'static str {
    "ok"
}

/// `GET /version` — service identity as JSON.
async fn version() -> impl IntoResponse {
    Json(serde_json::json!({
        "service": "lakko-svc-channels",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}
