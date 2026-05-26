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
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::storage::{ChannelId, ChannelStore, StorageError, StoredPost, SubscriberId};

/// Seed-envelope wire length pinned to
/// `lakko_channels::subscribe::SEED_ENVELOPE_LEN`. We don't
/// pull that crate in here (the opacity invariant — see
/// Cargo.toml comment) so the constant is mirrored as a
/// `const _: () = assert!(...)` test in the integration
/// suite where both crates are visible.
pub const EXPECTED_ENVELOPE_LEN: usize = 80;

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

/// Body for `POST /v1/channels/:id/subscribers`.
#[derive(Deserialize)]
pub struct SubscribeRequest {
    /// 64-char lowercase hex of the subscriber's X25519
    /// identity public key.
    pub subscriber_pubkey_hex: String,
}

/// Response for `POST /v1/channels/:id/subscribers`.
#[derive(Serialize)]
pub struct SubscribeResponse {
    /// Echoed subscriber pubkey hex.
    pub subscriber_id: String,
}

/// Response for `GET /v1/channels/:id/subscribers`.
#[derive(Serialize)]
pub struct ListSubscribersResponse {
    /// Subscriber pubkey hex strings, unordered.
    pub subscribers: Vec<String>,
}

/// Response for `GET /v1/channels/:id/subscribers/:pubkey/envelope`.
#[derive(Serialize)]
pub struct EnvelopeView {
    /// Opaque HPKE-sealed seed envelope, base64url
    /// (no padding). Exactly `EXPECTED_ENVELOPE_LEN`
    /// bytes pre-encode.
    pub envelope_b64: String,
}

/// One entry in a rotation batch.
#[derive(Deserialize)]
pub struct RotateBatchEntry {
    /// 64-char lowercase hex of the subscriber's X25519
    /// public key. Must already be registered against the
    /// channel.
    pub subscriber_id: String,
    /// HPKE-sealed seed envelope for this subscriber,
    /// base64url-encoded. Decodes to exactly
    /// `EXPECTED_ENVELOPE_LEN` opaque bytes.
    pub envelope_b64: String,
}

/// Body for `POST /v1/channels/:id/rotate`.
#[derive(Deserialize)]
pub struct RotateRequest {
    /// One entry per subscriber the publisher wants to
    /// reach. Server validates every entry's length /
    /// registration before committing — either the whole
    /// batch lands or none does.
    pub envelopes: Vec<RotateBatchEntry>,
}

/// Response for `POST /v1/channels/:id/rotate` +
/// `GET /v1/channels/:id/epoch`.
#[derive(Serialize)]
pub struct EpochView {
    /// Current epoch id after the operation. Genesis is 0;
    /// first rotation bumps to 1.
    pub epoch: u32,
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
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("internal", "unexpected error")),
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

/// `POST /v1/channels/:channel_id/subscribers` — register.
async fn subscribe(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Json(req): Json<SubscribeRequest>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    let Ok(sub) = SubscriberId::from_hex(&req.subscriber_pubkey_hex) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-subscriber-id",
                "subscriber_pubkey_hex must be 64 hex chars",
            )),
        )
            .into_response();
    };
    let now = unix_ms();
    match state.store.subscribe(channel, sub, now).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(SubscribeResponse {
                subscriber_id: sub.to_hex(),
            }),
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
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("internal", "unexpected error")),
        )
            .into_response(),
    }
}

/// `GET /v1/channels/:channel_id/subscribers` — list.
async fn list_subscribers(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    match state.store.list_subscribers(channel).await {
        Ok(list) => (
            StatusCode::OK,
            Json(ListSubscribersResponse {
                subscribers: list.into_iter().map(SubscriberId::to_hex).collect(),
            }),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("internal", "unexpected error")),
        )
            .into_response(),
    }
}

/// `POST /v1/channels/:channel_id/subscribers/:pubkey_hex/envelope` — publisher delivers.
async fn deliver_envelope(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    let Ok(sub) = SubscriberId::from_hex(&pubkey_hex) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-subscriber-id",
                "subscriber pubkey must be 64 hex chars",
            )),
        )
            .into_response();
    };
    match state
        .store
        .deliver_envelope(channel, sub, body.to_vec(), EXPECTED_ENVELOPE_LEN)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(StorageError::UnknownSubscriber) => (
            StatusCode::NOT_FOUND,
            Json(err(
                "unknown-subscriber",
                "subscriber has not registered against this channel",
            )),
        )
            .into_response(),
        Err(StorageError::BadEnvelopeLen { expected, got }) => (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-envelope-len",
                format!("envelope must be {expected} bytes (got {got})"),
            )),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("internal", "unexpected error")),
        )
            .into_response(),
    }
}

/// `GET /v1/channels/:channel_id/subscribers/:pubkey_hex/envelope` — subscriber pulls.
async fn fetch_envelope(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    let Ok(sub) = SubscriberId::from_hex(&pubkey_hex) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-subscriber-id",
                "subscriber pubkey must be 64 hex chars",
            )),
        )
            .into_response();
    };
    match state.store.fetch_envelope(channel, sub).await {
        Ok(Some(env)) => {
            use base64::engine::general_purpose::URL_SAFE_NO_PAD;
            use base64::Engine as _;
            (
                StatusCode::OK,
                Json(EnvelopeView {
                    envelope_b64: URL_SAFE_NO_PAD.encode(&env),
                }),
            )
                .into_response()
        },
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(err(
                "envelope-not-ready",
                "no envelope delivered to this subscriber yet",
            )),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(StorageError::UnknownSubscriber) => (
            StatusCode::NOT_FOUND,
            Json(err(
                "unknown-subscriber",
                "subscriber has not registered against this channel",
            )),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("internal", "unexpected error")),
        )
            .into_response(),
    }
}

/// `GET /v1/channels/:channel_id/epoch` — current epoch id.
async fn fetch_epoch(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    match state.store.current_epoch(channel).await {
        Ok(epoch) => (StatusCode::OK, Json(EpochView { epoch })).into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("internal", "unexpected error")),
        )
            .into_response(),
    }
}

/// `POST /v1/channels/:channel_id/rotate` — atomic batch
/// envelope delivery + epoch bump.
async fn rotate_channel(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Json(req): Json<RotateRequest>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };

    // Validate + decode every entry BEFORE asking the
    // store to commit — keeps the "either-all-or-none"
    // property even on malformed input.
    let mut batch: Vec<(SubscriberId, Vec<u8>)> = Vec::with_capacity(req.envelopes.len());
    for entry in req.envelopes {
        let Ok(sub) = SubscriberId::from_hex(&entry.subscriber_id) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(err(
                    "bad-subscriber-id",
                    format!("subscriber_id {:?} is not 64 hex chars", entry.subscriber_id),
                )),
            )
                .into_response();
        };
        let Ok(env_bytes) = URL_SAFE_NO_PAD.decode(&entry.envelope_b64) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(err(
                    "bad-envelope-b64",
                    "envelope_b64 must be valid base64url (no padding)",
                )),
            )
                .into_response();
        };
        batch.push((sub, env_bytes));
    }

    match state
        .store
        .rotate(channel, batch, EXPECTED_ENVELOPE_LEN)
        .await
    {
        Ok(epoch) => (StatusCode::OK, Json(EpochView { epoch })).into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(StorageError::UnknownSubscriber) => (
            StatusCode::CONFLICT,
            Json(err(
                "unknown-subscriber",
                "one or more entries reference a subscriber that is not registered against this channel",
            )),
        )
            .into_response(),
        Err(StorageError::BadEnvelopeLen { expected, got }) => (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-envelope-len",
                format!("each envelope must decode to exactly {expected} bytes (got {got})"),
            )),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("internal", "unexpected error")),
        )
            .into_response(),
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
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
        .route(
            "/v1/channels/{channel_id}/subscribers",
            post(subscribe).get(list_subscribers),
        )
        .route(
            "/v1/channels/{channel_id}/subscribers/{pubkey_hex}/envelope",
            post(deliver_envelope).get(fetch_envelope),
        )
        .route("/v1/channels/{channel_id}/rotate", post(rotate_channel))
        .route("/v1/channels/{channel_id}/epoch", get(fetch_epoch))
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
