// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HTTP endpoints for the channels service.
//!
//! The Phase 4a broadcast routes and the ADR-0059 directory:
//!
//! | Method | Path | Gate | Purpose |
//! |---|---|---|---|
//! | `POST` | `/v1/channels` | none | Register a channel: `{channel_id, owner_pubkey_hex}`. The owner key is fixed at creation. |
//! | `POST` | `/v1/channels/:id/posts` | none (G-43) | Publish an opaque ChannelPost. |
//! | `GET`  | `/v1/channels/:id/posts?after=&limit=` | none | Cursor-paginated fetch. |
//! | `POST` | `/v1/channels/:id/subscribers` | one `channel-subscribe` token | Register a subscriber key. |
//! | `GET`  | `/v1/channels/:id/subscribers` | owner proof | The subscriber keys (the publisher reseals to them). |
//! | `PUT`  | `/v1/channels/:id/listing` | owner proof | Store or replace the listing. Body: the canonical `lakko/channel-listing/v1` bytes, `application/octet-stream`. |
//! | `DELETE` | `/v1/channels/:id/listing` | owner proof | Withdraw the listing. |
//! | `GET`  | `/v1/directory/bucket/:topic/:language` | none | Top 100 listings of a bucket, identical for every caller, cacheable for an hour. |
//! | `GET`  | `/v1/directory/listing/:id` | none | One listing. |
//! | `GET`  | `/v1/directory/search?q=` | one `directory-search` token | Top 24 matches; the query is never logged. |
//!
//! Envelope delivery, rotation and the epoch routes are unchanged and, like
//! publishing, still unauthenticated (G-43, out of scope of ADR-0059).
//! Moderation is not here: it is served on the internal listener
//! ([`crate::internal`]).
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
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Json, Response},
    routing::{get, post, put},
    Router,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_token::TokenContext;
use lakko_anon_token_server::TokenVerifier;
use lakko_channels::listing::{decode_channel_listing, is_known_topic, TOPIC_OTHER};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::directory::{
    band, normalize_search_query, rank, DirectoryEntry, BUCKET_PAGE_LEN, SEARCH_PAGE_LEN,
};
use crate::owner_auth::{require_owner, require_publisher};
use crate::storage::{
    ChannelId, ChannelStore, DirectoryQuery, StorageError, StoredListing, StoredPost, SubscriberId,
    OWNER_PUBKEY_LEN,
};

/// `Cache-Control` of a directory bucket: public and identical for every
/// caller, so a cache in front removes even the per-request view (ADR-0059
/// section 5).
pub const BUCKET_CACHE_CONTROL: &str = "public, max-age=3600";
/// `Cache-Control` of a single listing (chosen: shorter than a bucket so an
/// owner's edit shows sooner on an opened card).
pub const LISTING_CACHE_CONTROL: &str = "public, max-age=300";

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
    /// Channel store — `Arc<dyn ChannelStore + …>` so the
    /// same handler code dispatches dynamically between
    /// the in-memory and the SQL-backed impls (G4-1-sql).
    pub store: Arc<dyn ChannelStore + Send + Sync>,
    /// The anonymous-token spend gate (subscribe, search). Its spent-set is
    /// the service database under the SQL store and process memory under the
    /// in-memory store.
    pub anon_tokens: TokenVerifier,
}

impl AppState {
    /// Build a fresh state container behind an `Arc`. Takes
    /// a boxed trait object so the caller picks the impl
    /// (production wires a `SqlChannelStore`; tests wire
    /// an `InMemoryChannelStore`).
    #[must_use]
    pub fn new(
        cfg: Config,
        store: Arc<dyn ChannelStore + Send + Sync>,
        anon_tokens: TokenVerifier,
    ) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            store,
            anon_tokens,
        })
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
    /// 64-char hex of the channel owner's Ed25519 public key (ADR-0059
    /// section 1), derived by the creating account from its seed and the
    /// channel id. Fixed at creation.
    pub owner_pubkey_hex: String,
}

/// One directory item: the owner's listing bytes and the published band of
/// its subscriber count (never the exact count).
#[derive(Serialize)]
pub struct DirectoryItem {
    /// 32-char lowercase hex channel id.
    pub channel_id: String,
    /// The canonical `lakko/channel-listing/v1` bytes, base64url without
    /// padding; clients decode them with the shared codec.
    pub listing_b64: String,
    /// One of `<10`, `10+`, `100+`, `1k+`, `10k+`, `100k+`.
    pub subscribers: &'static str,
}

impl DirectoryItem {
    fn from_entry(e: &DirectoryEntry) -> Self {
        Self {
            channel_id: e.channel.to_hex(),
            listing_b64: URL_SAFE_NO_PAD.encode(&e.listing),
            subscribers: band(e.subscribers),
        }
    }
}

/// Response for `GET /v1/directory/bucket/:topic/:language`.
#[derive(Serialize)]
pub struct BucketResponse {
    /// The bucket topic.
    pub topic: u8,
    /// The bucket language.
    pub language: String,
    /// At most 100 items, best first.
    pub items: Vec<DirectoryItem>,
}

/// Response for `GET /v1/directory/search`.
#[derive(Serialize)]
pub struct SearchResponse {
    /// At most 24 items, best first.
    pub items: Vec<DirectoryItem>,
}

/// Query string for `GET /v1/directory/search`.
#[derive(Deserialize)]
pub struct SearchQuery {
    /// The search text.
    #[serde(default)]
    pub q: String,
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

/// Query string for
/// `POST /v1/channels/:id/subscribers/:pk/envelope` — the
/// optional epoch the publisher files this envelope under
/// (ADR-0031). Omitted defaults to the channel's current
/// epoch; a value greater than the current epoch is refused
/// with `409 envelope-epoch-in-future`.
#[derive(Deserialize)]
pub struct DeliverEnvelopeQuery {
    /// Epoch to file the envelope under. `None` -> current.
    #[serde(default)]
    pub epoch: Option<u32>,
}

/// One `(epoch, envelope)` entry in the plural
/// `GET .../envelopes` response (ADR-0031).
#[derive(Serialize)]
pub struct EnvelopeAtEpoch {
    /// The epoch this seed envelope belongs to.
    pub epoch: u32,
    /// Opaque HPKE-sealed seed envelope, base64url
    /// (no padding).
    pub envelope_b64: String,
}

/// Response for
/// `GET /v1/channels/:id/subscribers/:pk/envelopes` — the
/// whole retained set, ascending by epoch (ADR-0031). A
/// catching-up subscriber opens each into its `epoch -> seed`
/// map and decrypts each post with the seed matching its
/// `epoch_id`.
#[derive(Serialize)]
pub struct FetchEnvelopesResponse {
    /// Retained envelopes, ascending by epoch.
    pub envelopes: Vec<EnvelopeAtEpoch>,
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

/// Maximum entries a single `POST /rotate/stage` chunk may
/// carry (ADR-0030 open question 1 — a conservative entry
/// cap, tune under load test). A chunk over this is refused
/// so no single request is unbounded; the publisher simply
/// splits its fanout into more chunks.
pub const MAX_STAGE_CHUNK_ENTRIES: usize = 5000;

/// Body for `POST /v1/channels/:id/rotate/stage` (ADR-0030
/// prepare phase). A bounded, resumable, idempotent chunk of
/// the next epoch's envelopes.
#[derive(Deserialize)]
pub struct StageRotateRequest {
    /// The epoch these envelopes become live at on commit.
    /// Must equal `current_epoch + 1`; the server refuses a
    /// stale / replayed target so a rotation cannot roll the
    /// epoch back or skip.
    pub target_epoch: u32,
    /// This chunk's per-subscriber envelopes. Bounded by
    /// [`MAX_STAGE_CHUNK_ENTRIES`]. Staged, never served —
    /// re-sending a chunk is an idempotent upsert.
    pub envelopes: Vec<RotateBatchEntry>,
}

/// Response for `POST /v1/channels/:id/rotate/stage`.
#[derive(Serialize)]
pub struct StageResponse {
    /// Echoed target epoch — the epoch this staged set will
    /// become live at once the publisher commits.
    pub target_epoch: u32,
    /// Number of envelopes accepted into the staging area by
    /// this chunk. Lets the publisher track fanout progress.
    pub staged: usize,
}

/// Body for `POST /v1/channels/:id/rotate/commit` (ADR-0030
/// commit phase — the single atomic point).
#[derive(Deserialize)]
pub struct CommitRotateRequest {
    /// The staged epoch to promote. Must equal
    /// `current_epoch + 1`; re-verified inside the commit
    /// transaction.
    pub target_epoch: u32,
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

pub(crate) fn err(code: &'static str, detail: impl Into<String>) -> ErrorResponse {
    ErrorResponse {
        code,
        detail: detail.into(),
    }
}

// ---------------------------------------------------------
// Handlers
// ---------------------------------------------------------

/// `POST /v1/channels` — register a new channel under its owner key.
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
    // A key that does not decode as an Ed25519 point could never verify a
    // proof; refuse it at the door rather than create a channel nobody owns.
    let owner = hex::decode(&req.owner_pubkey_hex)
        .ok()
        .and_then(|b| <[u8; OWNER_PUBKEY_LEN]>::try_from(b.as_slice()).ok())
        .filter(|k| lakko_crypto::signing::VerifyingKey::from_bytes(k).is_ok());
    let Some(owner) = owner else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-owner-pubkey",
                "owner_pubkey_hex must be 64 hex chars of an Ed25519 public key",
            )),
        )
            .into_response();
    };
    match state.store.create(channel, owner).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(CreateChannelResponse {
                channel_id: channel.to_hex(),
            }),
        )
            .into_response(),
        Err(StorageError::OwnerMismatch) => (
            StatusCode::CONFLICT,
            Json(err(
                "channel-exists",
                "a channel with this id exists under another owner key",
            )),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("storage", e.to_string())),
        )
            .into_response(),
    }
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
        Ok(stored) => (StatusCode::CREATED, Json(PostView::from_stored(stored))).into_response(),
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

/// `POST /v1/channels/:channel_id/subscribers` — register. Spends one
/// `channel-subscribe` anonymous token (ADR-0059 section 4), after the body
/// validated and the channel was found, so a malformed or misdirected
/// request costs none.
async fn subscribe(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    headers: HeaderMap,
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
    if !state.store.exists(channel).await {
        return (
            StatusCode::NOT_FOUND,
            Json(err(
                "unknown-channel",
                "register the channel via POST /v1/channels first",
            )),
        )
            .into_response();
    }
    if let Err(refused) = state
        .anon_tokens
        .spend_header(&headers, TokenContext::ChannelSubscribe)
        .await
    {
        return refused.into_response();
    }
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

/// `GET /v1/channels/:channel_id/subscribers` — list. Owner-signed, or
/// signed by a co-publisher key the owner appointed: a publisher needs the
/// keys to reseal, nobody else does, and the exact count stops being public
/// (ADR-0059 section 4).
async fn list_subscribers(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &[]).await {
        return refused;
    }
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
/// An optional `?epoch=<u32>` files the envelope under a
/// concrete epoch (ADR-0031); omitted defaults to the
/// channel's current epoch, and a future epoch is refused.
async fn deliver_envelope(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
    Query(q): Query<DeliverEnvelopeQuery>,
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
        .deliver_envelope(channel, sub, q.epoch, body.to_vec(), EXPECTED_ENVELOPE_LEN)
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
        Err(StorageError::EnvelopeEpochInFuture { current, got }) => (
            StatusCode::CONFLICT,
            Json(err(
                "envelope-epoch-in-future",
                format!("epoch {got} exceeds the channel's current epoch {current}"),
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
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
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
            // Bump `last_active_at` so the
            // stale-subscriber pruner does not GC this
            // subscriber on its next tick. The
            // subscriber's own poll loop (default 3 s on
            // the client side) is the activity signal —
            // no separate heartbeat protocol. Errors are
            // silently swallowed: a failed touch only
            // delays the inevitable GC, never breaks the
            // GET response the caller is waiting on.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            let _ = state
                .store
                .touch_subscriber_activity(channel, sub, now_ms)
                .await;
            (
                StatusCode::OK,
                Json(EnvelopeView {
                    envelope_b64: URL_SAFE_NO_PAD.encode(&env),
                }),
            )
                .into_response()
        },
        Ok(None) => {
            // Subscriber is registered but no envelope
            // yet — still counts as activity (they're
            // polling), so bump last_active_at.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            let _ = state
                .store
                .touch_subscriber_activity(channel, sub, now_ms)
                .await;
            (
                StatusCode::NOT_FOUND,
                Json(err(
                    "envelope-not-ready",
                    "no envelope delivered to this subscriber yet",
                )),
            )
                .into_response()
        },
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

/// `GET /v1/channels/:channel_id/subscribers/:pubkey_hex/envelopes` —
/// the whole retained `[(epoch, envelope)]` set for a
/// catching-up subscriber (ADR-0031), ascending by epoch.
async fn fetch_envelopes(
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
    match state.store.fetch_envelopes(channel, sub).await {
        Ok(list) => {
            // A plural fetch is the catch-up poll — count it as
            // subscriber activity so the pruner does not GC
            // them, mirroring the singular fetch. Errors are
            // swallowed: a failed touch only delays GC.
            let _ = state
                .store
                .touch_subscriber_activity(channel, sub, unix_ms())
                .await;
            let envelopes = list
                .into_iter()
                .map(|(epoch, env)| EnvelopeAtEpoch {
                    epoch,
                    envelope_b64: URL_SAFE_NO_PAD.encode(&env),
                })
                .collect();
            (StatusCode::OK, Json(FetchEnvelopesResponse { envelopes })).into_response()
        },
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
                    format!(
                        "subscriber_id {:?} is not 64 hex chars",
                        entry.subscriber_id
                    ),
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

/// Decode a rotation batch's wire entries into
/// `(SubscriberId, raw envelope bytes)` pairs, returning the
/// [`ErrorResponse`] for a 400 on the first malformed entry
/// (both failure modes are client errors). Keeps the
/// "validate every entry before the store sees anything"
/// property for the ADR-0030 stage path.
fn decode_rotate_batch(
    entries: Vec<RotateBatchEntry>,
) -> Result<Vec<(SubscriberId, Vec<u8>)>, ErrorResponse> {
    let mut batch: Vec<(SubscriberId, Vec<u8>)> = Vec::with_capacity(entries.len());
    for entry in entries {
        let Ok(sub) = SubscriberId::from_hex(&entry.subscriber_id) else {
            return Err(err(
                "bad-subscriber-id",
                format!(
                    "subscriber_id {:?} is not 64 hex chars",
                    entry.subscriber_id
                ),
            ));
        };
        let Ok(env_bytes) = URL_SAFE_NO_PAD.decode(&entry.envelope_b64) else {
            return Err(err(
                "bad-envelope-b64",
                "envelope_b64 must be valid base64url (no padding)",
            ));
        };
        batch.push((sub, env_bytes));
    }
    Ok(batch)
}

/// `POST /v1/channels/:channel_id/rotate/stage` — ADR-0030
/// prepare phase. Stages a bounded, resumable, idempotent
/// chunk of the next epoch's envelopes WITHOUT bumping the
/// epoch or serving them to subscribers.
async fn rotate_stage(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Json(req): Json<StageRotateRequest>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };

    // Bound the chunk BEFORE decoding — a single request must
    // never be unbounded (ADR-0030). The publisher splits its
    // fanout into more chunks instead.
    if req.envelopes.len() > MAX_STAGE_CHUNK_ENTRIES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(err(
                "chunk-too-large",
                format!(
                    "a stage chunk may carry at most {MAX_STAGE_CHUNK_ENTRIES} entries (got {})",
                    req.envelopes.len()
                ),
            )),
        )
            .into_response();
    }

    let target_epoch = req.target_epoch;
    let staged = req.envelopes.len();
    let batch = match decode_rotate_batch(req.envelopes) {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(e)).into_response(),
    };

    match state
        .store
        .stage_envelopes(channel, target_epoch, batch, EXPECTED_ENVELOPE_LEN)
        .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(StageResponse {
                target_epoch,
                staged,
            }),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(StorageError::StaleTargetEpoch { expected, got }) => (
            StatusCode::CONFLICT,
            Json(err(
                "stale-target-epoch",
                format!("target_epoch must be current + 1 (expected {expected}, got {got})"),
            )),
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

/// `POST /v1/channels/:channel_id/rotate/commit` — ADR-0030
/// commit phase, the single atomic point. Promotes the staged
/// envelopes for `target_epoch` into the live set + bumps the
/// epoch in one transaction.
async fn rotate_commit(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Json(req): Json<CommitRotateRequest>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    match state.store.commit_rotation(channel, req.target_epoch).await {
        Ok(epoch) => (StatusCode::OK, Json(EpochView { epoch })).into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(StorageError::StaleTargetEpoch { expected, got }) => (
            StatusCode::CONFLICT,
            Json(err(
                "stale-target-epoch",
                format!("target_epoch must be current + 1 (expected {expected}, got {got})"),
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

fn bad_channel_id() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
    )
        .into_response()
}

fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(err("internal", "unexpected error")),
    )
        .into_response()
}

fn with_cache_control(mut resp: Response, value: &'static str) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    resp
}

/// `PUT /v1/channels/:channel_id/listing` — store or replace the listing
/// (owner-signed). The body is the canonical listing bytes; the listing must
/// name this channel, decode under the shared codec and be strictly newer
/// than the stored one.
async fn put_listing(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    if let Err(refused) = require_owner(&state, channel, &method, &uri, &headers, &body).await {
        return refused;
    }
    let listing = match decode_channel_listing(&body) {
        Ok(l) => l,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(err(
                    "bad-listing",
                    format!("listing refused by the codec: {}", e.code()),
                )),
            )
                .into_response()
        },
    };
    if listing.channel_id != *channel.as_bytes() {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "listing-channel-mismatch",
                "the listing names another channel than the path",
            )),
        )
            .into_response();
    }
    let stored = StoredListing {
        topic: if is_known_topic(listing.topic) {
            listing.topic
        } else {
            TOPIC_OTHER
        },
        language: listing.language,
        handle: listing.handle,
        name: listing.name,
        description: listing.description,
        bytes: body.to_vec(),
        updated_at_ms: listing.updated_at_ms,
    };
    match state.store.put_listing(channel, stored, unix_ms()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(StorageError::StaleListing) => (
            StatusCode::CONFLICT,
            Json(err(
                "stale-listing",
                "updated_at_ms must be newer than the stored listing's",
            )),
        )
            .into_response(),
        Err(StorageError::ListingBlocked) => (
            StatusCode::FORBIDDEN,
            Json(err(
                "listing-blocked",
                "this channel cannot be listed in the directory",
            )),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "put listing failed");
            internal_error()
        },
    }
}

/// `DELETE /v1/channels/:channel_id/listing` — withdraw (owner-signed).
/// Idempotent: withdrawing an unlisted channel is also a 204.
async fn delete_listing(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    if let Err(refused) = require_owner(&state, channel, &method, &uri, &headers, &[]).await {
        return refused;
    }
    match state.store.withdraw_listing(channel).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "withdraw listing failed");
            internal_error()
        },
    }
}

/// Parse a 64-hex Ed25519 public key; `None` unless it is a curve point.
fn parse_pubkey_hex(hex_str: &str) -> Option<[u8; 32]> {
    if hex_str.len() != 64 {
        return None;
    }
    let bytes = hex::decode(hex_str).ok()?;
    let key: [u8; 32] = bytes.as_slice().try_into().ok()?;
    lakko_crypto::signing::VerifyingKey::from_bytes(&key).ok()?;
    Some(key)
}

/// `PUT /v1/channels/:channel_id/copublishers/:pubkey_hex` — appoint a
/// co-publisher key (owner-signed, empty body). Idempotent; the owner learns
/// the key end-to-end from the co-publisher's binding (ADR-0059 section 1),
/// never from this service. At most `lakko_channels::MAX_COPUBLISHERS` keys
/// per channel (`409 too-many-copublishers`).
async fn appoint_copublisher(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    if let Err(refused) = require_owner(&state, channel, &method, &uri, &headers, &[]).await {
        return refused;
    }
    let Some(key) = parse_pubkey_hex(&pubkey_hex) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-copublisher-key",
                "the co-publisher key must be 64 hex chars of an Ed25519 public key",
            )),
        )
            .into_response();
    };
    match state
        .store
        .appoint_copublisher(channel, key, lakko_channels::MAX_COPUBLISHERS, unix_ms())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(StorageError::TooManyCoPublishers) => (
            StatusCode::CONFLICT,
            Json(err(
                "too-many-copublishers",
                "revoke a co-publisher key before appointing another",
            )),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "appoint co-publisher failed");
            internal_error()
        },
    }
}

/// `DELETE /v1/channels/:channel_id/copublishers/:pubkey_hex` — revoke a
/// co-publisher key (owner-signed). Idempotent: revoking a key that is not
/// appointed is also a 204.
async fn revoke_copublisher(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    if let Err(refused) = require_owner(&state, channel, &method, &uri, &headers, &[]).await {
        return refused;
    }
    let Some(key) = parse_pubkey_hex(&pubkey_hex) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-copublisher-key",
                "the co-publisher key must be 64 hex chars of an Ed25519 public key",
            )),
        )
            .into_response();
    };
    match state.store.revoke_copublisher(channel, key).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "revoke co-publisher failed");
            internal_error()
        },
    }
}

/// `GET /v1/directory/bucket/:topic/:language` — the top listings of one
/// bucket. No session, no token, no cookie; the response depends only on the
/// bucket and the clock.
async fn directory_bucket(
    State(state): State<Arc<AppState>>,
    Path((topic, language)): Path<(String, String)>,
) -> Response {
    let topic = match topic.parse::<u8>() {
        Ok(t) if is_known_topic(t) => t,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(err("bad-topic", "topic must be a known topic id")),
            )
                .into_response()
        },
    };
    if lakko_channels::listing::check_language(&language).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-language",
                "language must be 2-3 lowercase ASCII letters",
            )),
        )
            .into_response();
    }
    let now = unix_ms();
    let query = DirectoryQuery::Bucket {
        topic,
        language: language.clone(),
    };
    match state.store.directory_candidates(&query, now).await {
        Ok(entries) => {
            let items = rank(entries, now, BUCKET_PAGE_LEN)
                .iter()
                .map(DirectoryItem::from_entry)
                .collect();
            with_cache_control(
                (
                    StatusCode::OK,
                    Json(BucketResponse {
                        topic,
                        language,
                        items,
                    }),
                )
                    .into_response(),
                BUCKET_CACHE_CONTROL,
            )
        },
        Err(e) => {
            tracing::error!(error = %e, "directory bucket read failed");
            internal_error()
        },
    }
}

/// `GET /v1/directory/listing/:channel_id` — one listing, for a card opened
/// from a group-shared list.
async fn directory_listing(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    match state.store.listing(channel, unix_ms()).await {
        Ok(Some(entry)) => with_cache_control(
            (StatusCode::OK, Json(DirectoryItem::from_entry(&entry))).into_response(),
            LISTING_CACHE_CONTROL,
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(err("not-listed", "this channel is not in the directory")),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "directory listing read failed");
            internal_error()
        },
    }
}

/// `GET /v1/directory/search?q=` — the top matches over name, handle and
/// description. Spends one `directory-search` token after the query
/// validated. The query is never logged, here or below.
async fn directory_search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> Response {
    let Ok(needle) = normalize_search_query(&q.q) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-query",
                "q must be 2-64 characters without control characters",
            )),
        )
            .into_response();
    };
    if let Err(refused) = state
        .anon_tokens
        .spend_header(&headers, TokenContext::DirectorySearch)
        .await
    {
        return refused.into_response();
    }
    let now = unix_ms();
    match state
        .store
        .directory_candidates(&DirectoryQuery::Search { needle }, now)
        .await
    {
        Ok(entries) => {
            let items = rank(entries, now, SEARCH_PAGE_LEN)
                .iter()
                .map(DirectoryItem::from_entry)
                .collect();
            with_cache_control(
                (StatusCode::OK, Json(SearchResponse { items })).into_response(),
                "no-store",
            )
        },
        Err(e) => {
            // The error names the store failure, never the query.
            tracing::error!(error = %e, "directory search failed");
            internal_error()
        },
    }
}

pub(crate) fn unix_ms() -> u64 {
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
        .route(
            "/v1/channels/{channel_id}/subscribers/{pubkey_hex}/envelopes",
            get(fetch_envelopes),
        )
        .route("/v1/channels/{channel_id}/rotate", post(rotate_channel))
        .route("/v1/channels/{channel_id}/rotate/stage", post(rotate_stage))
        .route(
            "/v1/channels/{channel_id}/rotate/commit",
            post(rotate_commit),
        )
        .route("/v1/channels/{channel_id}/epoch", get(fetch_epoch))
        .route(
            "/v1/channels/{channel_id}/listing",
            put(put_listing).delete(delete_listing),
        )
        .route(
            "/v1/channels/{channel_id}/copublishers/{pubkey_hex}",
            put(appoint_copublisher).delete(revoke_copublisher),
        )
        .route(
            "/v1/directory/bucket/{topic}/{language}",
            get(directory_bucket),
        )
        .route("/v1/directory/listing/{channel_id}", get(directory_listing))
        .route("/v1/directory/search", get(directory_search))
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
