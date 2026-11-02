// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HTTP endpoints for the channels service.
//!
//! The Phase 4a broadcast routes and the ADR-0059 directory:
//!
//! | Method | Path | Gate | Purpose |
//! |---|---|---|---|
//! | `POST` | `/v1/channels` | none | Register a channel: `{channel_id, owner_pubkey_hex, private?}` (`private` defaults to `false`, applied only when the channel is created). Re-creating a channel never changes its owner key or its flag. |
//! | `PUT`  | `/v1/channels/:id/access` | owner proof | Set the flag (ADR-0087 section 8): `{"private": bool}`. Going private withdraws the listing and deletes every public seed. |
//! | `PUT`  | `/v1/channels/:id/public-seed/:epoch` | publisher proof | File a public channel's seed envelope for the epoch (ADR-0087 section 6): the 64 bytes of `lakko_channels::seal_public_seed`, `application/octet-stream`. Immutable once stored. |
//! | `GET`  | `/v1/channels/:id/public-seed/:epoch` | none | The envelope, raw bytes, `Cache-Control: public, max-age=31536000, immutable`. |
//! | `GET`  | `/v1/channels/:id/invite-keys` | publisher proof | A private channel's invite keys with expiry and use count (ADR-0087 section 7). |
//! | `PUT`  | `/v1/channels/:id/invite-keys/:pk` | publisher proof | Register or update an invite key: `{expiry_ms, max_uses}`. |
//! | `DELETE` | `/v1/channels/:id/invite-keys/:pk` | publisher proof | Revoke it. |
//! | `GET`  | `/v1/channels/:id/subscribers/count` | publisher proof | The maintained subscriber count (ADR-0087 section 15). |
//! | `PUT`  | `/v1/channels/:id/owner` | owner proof (by the CURRENT owner key) | Hand the channel to a new owner key (ADR-0074 section 3.5). Body: the new key, 32 raw bytes, `application/octet-stream`. |
//! | `POST` | `/v1/channels/:id/posts` | publisher proof | Publish an opaque ChannelPost. |
//! | `GET`  | `/v1/channels/:id/posts?after=&limit=` | none | Cursor-paginated fetch. |
//! | `POST` | `/v1/channels/:id/subscribers` | one `channel-subscribe` token; on a private channel also an `x-lakko-channel-invite-proof` | Register a subscriber key, with its sealed admission ticket on a private channel. |
//! | `GET`  | `/v1/channels/:id/subscribers?after=<seq>&limit=<n>` | publisher proof | A page of subscriber rows in insertion order, with their tickets (the publisher admits and reseals to them). |
//! | `PUT`  | `/v1/channels/:id/listing` | owner proof | Store or replace the listing. Body: the canonical `lakko/channel-listing/v1` bytes, `application/octet-stream`. |
//! | `DELETE` | `/v1/channels/:id/listing` | owner proof | Withdraw the listing. |
//! | `GET`  | `/v1/directory/bucket/:topic/:language` | none | Top 100 listings of a bucket, identical for every caller, cacheable for an hour. |
//! | `GET`  | `/v1/directory/listing/:id` | none | One listing. |
//! | `GET`  | `/v1/directory/search?q=` | one `directory-search` token | Top 24 matches; the query is never logged. |
//! | `DELETE` | `/v1/channels/:id/subscribers/:pk` | subscriber signature, or publisher proof | Unsubscribe (ADR-0060), signed by the subscriber key with a fresh timestamp; or a publisher removes the key from a private channel (ADR-0087 section 7), after which its envelope reads answer `410 subscriber-removed`. |
//! | `PUT`  | `/v1/channels/:id/members/:pk` | subscriber signature | Upload a member card (ADR-0060), signed by the subscriber key. |
//! | `DELETE` | `/v1/channels/:id/members/:pk` | subscriber signature | Remove it, signed likewise. |
//! | `GET`  | `/v1/channels/:id/members?after=<pk_b64>&limit=<n>` | none | A page of member cards. |
//!
//! | `POST` | `/v1/channels/:id/subscribers/:pk/envelope` | publisher proof | File one seed envelope. |
//! | `POST` | `/v1/channels/:id/rotate` | publisher proof | Atomic batch delivery + epoch bump. |
//! | `POST` | `/v1/channels/:id/rotate/stage` | publisher proof | Stage a chunk of the next epoch (ADR-0030). |
//! | `POST` | `/v1/channels/:id/rotate/commit` | publisher proof | Promote the staged epoch (ADR-0030). |
//!
//! "Publisher proof" is [`require_publisher`]: an owner proof, or a proof by
//! a co-publisher key the owner appointed, over the method, the routed path,
//! the query and the exact body bytes (ADR-0059 section 9, closing G-43).
//! The gate runs before the body is parsed. The reads (posts, envelopes,
//! epoch) stay open: what they serve is sealed or public.
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
    body::{Body, Bytes},
    extract::{FromRequest, Path, Query, Request, State},
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
use crate::storage::SealedTicket;
use crate::storage::{
    ChannelId, ChannelStore, DirectoryQuery, InviteKeyRow, PublicSeedOutcome, StorageError,
    StoredListing, StoredPost, SubscriberEntry, SubscriberId, INVITE_KEY_LEN, OWNER_PUBKEY_LEN,
    PUBLIC_SEED_LEN, SEALED_TICKET_LEN,
};

/// `Cache-Control` of a directory bucket: public and identical for every
/// caller, so a cache in front removes even the per-request view (ADR-0059
/// section 5).
pub const BUCKET_CACHE_CONTROL: &str = "public, max-age=3600";
/// `Cache-Control` of a public seed envelope: stored once and never
/// replaced, so a cache in front may keep it (ADR-0087 section 6).
pub const PUBLIC_SEED_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// Most invite keys a channel may hold at once (ADR-0087 section 7). Chosen,
/// not measured: a one-use link per invited contact, with expired keys
/// pruned.
pub const MAX_INVITE_KEYS: usize = 4096;

/// How long after its expiry an invite key is kept, so a publisher admitting
/// a row that came in just before the expiry can still read the key. Chosen,
/// not measured.
pub const INVITE_KEY_GRACE_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Default page of `GET .../subscribers`.
pub const SUBSCRIBER_PAGE_DEFAULT: usize = 1000;

/// Largest page of `GET .../subscribers` (the rotation stage-chunk bound).
pub const SUBSCRIBER_PAGE_MAX: usize = 5000;

/// `Cache-Control` of a single listing (chosen: shorter than a bucket so an
/// owner's edit shows sooner on an opened card).
pub const LISTING_CACHE_CONTROL: &str = "public, max-age=300";

/// Seed-envelope wire length pinned to
/// `lakko_channels::subscribe::SEED_ENVELOPE_LEN`, mirrored here and
/// cross-checked by a `const _: () = assert!(...)` in the integration
/// suite. (The service links `lakko-channels` only for the member-card
/// frame reader, ADR-0060; see the Cargo.toml comment.)
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
    /// ADR-0087: the channel is private. Applied only when the channel is
    /// created; `false` when absent.
    #[serde(default)]
    pub private: bool,
}

/// Body for `PUT /v1/channels/:id/access`.
#[derive(Deserialize)]
pub struct AccessRequest {
    /// The flag to set.
    pub private: bool,
}

/// Body for `PUT /v1/channels/:id/invite-keys/:pk`.
#[derive(Deserialize)]
pub struct InviteKeyRequest {
    /// Unix milliseconds after which the key admits nobody; `0` = never.
    pub expiry_ms: u64,
    /// `0` = one use, `4294967295` = unlimited, else the ceiling (the
    /// discovery ledger's rule, so a link's row and key carry one number).
    pub max_uses: u32,
}

/// One invite key in `GET /v1/channels/:id/invite-keys`.
#[derive(Serialize)]
pub struct InviteKeyView {
    /// 64-char lowercase hex of the invite public key.
    pub invite_pk_hex: String,
    /// As registered.
    pub expiry_ms: u64,
    /// As registered.
    pub max_uses: u32,
    /// New subscribers it let in.
    pub used_count: u32,
}

impl InviteKeyView {
    fn from_row(r: &InviteKeyRow) -> Self {
        Self {
            invite_pk_hex: hex::encode(r.invite_pk),
            expiry_ms: r.expiry_ms,
            max_uses: r.max_uses,
            used_count: r.used_count,
        }
    }
}

/// Response of `GET /v1/channels/:id/invite-keys`.
#[derive(Serialize)]
pub struct InviteKeysResponse {
    /// Every registered key, ascending.
    pub invite_keys: Vec<InviteKeyView>,
}

/// Response of `GET /v1/channels/:id/subscribers/count`.
#[derive(Serialize)]
pub struct SubscriberCountResponse {
    /// The maintained count of subscriber rows.
    pub count: u64,
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
    /// 64-char lowercase hex of the subscriber's Ed25519
    /// public key (ADR-0060; the seed envelope is sealed to
    /// its X25519 image).
    pub subscriber_pubkey_hex: String,
    /// ADR-0087 section 7: the sealed admission ticket, base64url without
    /// padding of `lakko_channels::ADMISSION_TICKET_LEN` bytes. Required on a
    /// private channel; stored opaquely, never opened here.
    #[serde(default)]
    pub sealed_ticket_b64: Option<String>,
}

/// Response for `POST /v1/channels/:id/subscribers`.
#[derive(Serialize)]
pub struct SubscribeResponse {
    /// Echoed subscriber pubkey hex.
    pub subscriber_id: String,
}

/// Query string for `GET /v1/channels/:id/subscribers`.
#[derive(Deserialize)]
pub struct SubscribersQuery {
    /// Return rows strictly after this row number; `0` from the start.
    #[serde(default)]
    pub after: u64,
    /// Page size, at most [`SUBSCRIBER_PAGE_MAX`] (default
    /// [`SUBSCRIBER_PAGE_DEFAULT`]).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One row of `GET /v1/channels/:id/subscribers`.
#[derive(Serialize)]
pub struct SubscriberRowView {
    /// The row number; pass the last one as `after` for the next page.
    pub seq: u64,
    /// Subscriber pubkey hex.
    pub subscriber_id: String,
    /// The sealed admission ticket, base64url without padding, or `null`.
    pub sealed_ticket_b64: Option<String>,
}

impl SubscriberRowView {
    fn from_entry(e: &SubscriberEntry) -> Self {
        Self {
            seq: e.seq,
            subscriber_id: e.subscriber.to_hex(),
            sealed_ticket_b64: e.sealed_ticket.map(|t| URL_SAFE_NO_PAD.encode(t)),
        }
    }
}

/// Response for `GET /v1/channels/:id/subscribers`.
#[derive(Serialize)]
pub struct ListSubscribersResponse {
    /// The page's subscriber pubkey hex strings, in row order.
    pub subscribers: Vec<String>,
    /// The same rows with their row numbers and tickets.
    pub entries: Vec<SubscriberRowView>,
    /// The last row number in the page, or the requested `after` when the
    /// page is empty.
    pub next_after: u64,
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
    match state
        .store
        .create_with_access(channel, owner, req.private)
        .await
    {
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
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &body).await {
        return refused;
    }
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
///
/// On a private channel (ADR-0087 section 7) the request also carries a
/// sealed admission ticket in the body and an
/// `x-lakko-channel-invite-proof` by a registered invite key over the
/// channel and the subscriber key; every proof refusal is the one
/// `401 invite-proof-required`, before the token is spent. A new row takes
/// one use of the key; a re-subscribe of a row already there takes none, so
/// a retry after a lost answer works with a one-use link.
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
    // ADR-0060: the subscriber key is Ed25519. A key that is not a curve
    // point, or is of small order, is refused here - the one entry point to
    // the subscriber table - so every later path (envelope, member card)
    // holds only valid keys.
    let Some(sub) = SubscriberId::from_hex(&req.subscriber_pubkey_hex)
        .ok()
        .filter(|sub| lakko_channels::subscriber_public_key(sub.as_bytes()).is_ok())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-subscriber-id",
                "subscriber_pubkey_hex must be 64 hex chars of a valid Ed25519 key",
            )),
        )
            .into_response();
    };
    let ticket = match parse_ticket(req.sealed_ticket_b64.as_deref()) {
        Ok(t) => t,
        Err(resp) => return *resp,
    };
    let private = match state.store.is_private(channel).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(err(
                    "unknown-channel",
                    "register the channel via POST /v1/channels first",
                )),
            )
                .into_response()
        },
        Err(e) => {
            tracing::error!(error = %e, "channel flag lookup failed");
            return internal_error();
        },
    };
    let mut claimed: Option<[u8; INVITE_KEY_LEN]> = None;
    if private {
        if ticket.is_none() {
            return (
                StatusCode::BAD_REQUEST,
                Json(err(
                    "sealed-ticket-required",
                    "subscribing to a private channel needs a sealed admission ticket",
                )),
            )
                .into_response();
        }
        match check_invite_proof(&state, &headers, channel, sub).await {
            Ok(key) => claimed = key,
            Err(resp) => return resp,
        }
    }
    if let Err(refused) = state
        .anon_tokens
        .spend_header(&headers, TokenContext::ChannelSubscribe)
        .await
    {
        release_claim(&state, channel, claimed).await;
        return refused.into_response();
    }
    finish_subscribe(&state, channel, sub, ticket, claimed).await
}

/// Write the subscriber row after every gate passed, giving back a claimed
/// invite use when the row turns out not to be new or the write fails.
async fn finish_subscribe(
    state: &AppState,
    channel: ChannelId,
    sub: SubscriberId,
    ticket: Option<SealedTicket>,
    claimed: Option<[u8; INVITE_KEY_LEN]>,
) -> Response {
    match state
        .store
        .subscribe_with_ticket(channel, sub, unix_ms(), ticket)
        .await
    {
        Ok(is_new) => {
            if !is_new {
                // A concurrent request inserted the row first; this one let
                // nobody new in.
                release_claim(state, channel, claimed).await;
            }
            (
                StatusCode::CREATED,
                Json(SubscribeResponse {
                    subscriber_id: sub.to_hex(),
                }),
            )
                .into_response()
        },
        Err(StorageError::UnknownChannel) => {
            release_claim(state, channel, claimed).await;
            (
                StatusCode::NOT_FOUND,
                Json(err(
                    "unknown-channel",
                    "register the channel via POST /v1/channels first",
                )),
            )
                .into_response()
        },
        Err(e) => {
            release_claim(state, channel, claimed).await;
            tracing::error!(error = %e, "subscribe failed");
            internal_error()
        },
    }
}

/// Decode an optional sealed admission ticket: base64url without padding
/// of exactly [`SEALED_TICKET_LEN`] bytes, else `400 bad-sealed-ticket`.
fn parse_ticket(b64: Option<&str>) -> Result<Option<SealedTicket>, Box<Response>> {
    let Some(b64) = b64 else {
        return Ok(None);
    };
    URL_SAFE_NO_PAD
        .decode(b64)
        .ok()
        .and_then(|b| <[u8; SEALED_TICKET_LEN]>::try_from(b.as_slice()).ok())
        .map(Some)
        .ok_or_else(|| {
            Box::new(
                (
                    StatusCode::BAD_REQUEST,
                    Json(err(
                        "bad-sealed-ticket",
                        format!(
                            "sealed_ticket_b64 must be {SEALED_TICKET_LEN} bytes in base64url \
                         without padding"
                        ),
                    )),
                )
                    .into_response(),
            )
        })
}

/// The single refusal of every failed invite proof (ADR-0087 section 7).
fn invite_proof_required() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(err(
            "invite-proof-required",
            "subscribing to this channel needs a valid invitation",
        )),
    )
        .into_response()
}

/// The invite gate of a private channel's subscribe. In order: the header
/// parses; the window and the signature verify over this channel and
/// subscriber key; the key is registered for the channel and, for a new row,
/// live (unexpired, uses left) - for a row already there, unexpired; the
/// nonce is fresh (recorded only after the signature verified); a new row
/// claims one use of the key atomically. Returns the key whose use was
/// claimed, for the caller to give back if the request is then refused.
async fn check_invite_proof(
    state: &AppState,
    headers: &HeaderMap,
    channel: ChannelId,
    sub: SubscriberId,
) -> Result<Option<[u8; INVITE_KEY_LEN]>, Response> {
    let Some(proof) = headers
        .get(lakko_channels::INVITE_PROOF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| lakko_channels::InviteProof::parse(v).ok())
    else {
        return Err(invite_proof_required());
    };
    let now = unix_ms();
    if lakko_channels::verify_invite_proof(&proof, channel.as_bytes(), sub.as_bytes(), now).is_err()
    {
        return Err(invite_proof_required());
    }
    let key = match state.store.invite_key(channel, proof.invite_key).await {
        Ok(Some(key)) => key,
        Ok(None) => return Err(invite_proof_required()),
        Err(e) => {
            tracing::error!(error = %e, "invite key lookup failed");
            return Err(internal_error());
        },
    };
    let already = state.store.is_subscribed(channel, sub).await;
    let usable = if already {
        !key.is_expired(now)
    } else {
        key.is_live(now)
    };
    if !usable {
        return Err(invite_proof_required());
    }
    match state
        .store
        .consume_owner_nonce(proof.proof.nonce, now)
        .await
    {
        Ok(true) => {},
        Ok(false) => return Err(invite_proof_required()),
        Err(e) => {
            tracing::error!(error = %e, "invite nonce store failed; refusing");
            return Err(internal_error());
        },
    }
    if already {
        return Ok(None);
    }
    match state
        .store
        .claim_invite_use(channel, proof.invite_key, now)
        .await
    {
        Ok(true) => Ok(Some(proof.invite_key)),
        Ok(false) => Err(invite_proof_required()),
        Err(e) => {
            tracing::error!(error = %e, "invite use claim failed");
            Err(internal_error())
        },
    }
}

/// Give back a claimed invite use. A failure only leaves the key one use
/// short, so it is logged, not surfaced.
async fn release_claim(
    state: &AppState,
    channel: ChannelId,
    claimed: Option<[u8; INVITE_KEY_LEN]>,
) {
    if let Some(key) = claimed {
        if let Err(e) = state.store.release_invite_use(channel, key).await {
            tracing::warn!(error = %e, "invite use release failed");
        }
    }
}

/// `GET /v1/channels/:channel_id/subscribers?after=<seq>&limit=<n>` — a page
/// of subscriber rows in insertion order (ADR-0087 section 7), with each
/// row's sealed ticket. Owner-signed, or signed by a co-publisher key the
/// owner appointed: a publisher needs the keys to admit and reseal, nobody
/// else does, and the exact count stops being public (ADR-0059 section 4).
async fn list_subscribers(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Query(q): Query<SubscribersQuery>,
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
    let limit = q
        .limit
        .unwrap_or(SUBSCRIBER_PAGE_DEFAULT)
        .clamp(1, SUBSCRIBER_PAGE_MAX);
    match state
        .store
        .list_subscriber_page(channel, q.after, limit)
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(ListSubscribersResponse {
                subscribers: page.iter().map(|e| e.subscriber.to_hex()).collect(),
                next_after: page.last().map_or(q.after, |e| e.seq),
                entries: page.iter().map(SubscriberRowView::from_entry).collect(),
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

/// `GET /v1/channels/:channel_id/subscribers/count` — the maintained
/// subscriber count (ADR-0087 section 15), publisher-signed.
async fn subscriber_count(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &[]).await {
        return refused;
    }
    match state.store.subscriber_count(channel).await {
        Ok(count) => (StatusCode::OK, Json(SubscriberCountResponse { count })).into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "subscriber count failed");
            internal_error()
        },
    }
}

/// The answer to an envelope read for a key with no row: `410
/// subscriber-removed` when a publisher removed it from a private channel
/// (ADR-0087 section 7: the removed device ends the channel), else `404
/// unknown-subscriber`.
async fn no_subscriber_row(state: &AppState, channel: ChannelId, sub: SubscriberId) -> Response {
    match state.store.is_removed(channel, sub).await {
        Ok(true) => (
            StatusCode::GONE,
            Json(err(
                "subscriber-removed",
                "a publisher removed this subscriber from the channel",
            )),
        )
            .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(err(
                "unknown-subscriber",
                "subscriber has not registered against this channel",
            )),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "removal lookup failed");
            internal_error()
        },
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
    method: Method,
    uri: Uri,
    headers: HeaderMap,
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
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &body).await {
        return refused;
    }
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
        Err(StorageError::UnknownSubscriber) => no_subscriber_row(&state, channel, sub).await,
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
        Err(StorageError::UnknownSubscriber) => no_subscriber_row(&state, channel, sub).await,
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
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &body).await {
        return refused;
    }
    let req: RotateRequest = match parse_json_body(&headers, body).await {
        Ok(r) => r,
        Err(rejected) => return rejected,
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
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &body).await {
        return refused;
    }
    let req: StageRotateRequest = match parse_json_body(&headers, body).await {
        Ok(r) => r,
        Err(rejected) => return rejected,
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
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &body).await {
        return refused;
    }
    let req: CommitRotateRequest = match parse_json_body(&headers, body).await {
        Ok(r) => r,
        Err(rejected) => return rejected,
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

// ---------------------------------------------------------
// ADR-0060 section 3 — member cards
// ---------------------------------------------------------

/// The header carrying a subscriber's signature over a member-card write
/// or delete: 64 bytes, base64url without padding. Defined in
/// `lakko-channels` so clients and server share it.
pub use lakko_channels::MEMBER_CARD_SIG_HEADER;

/// Domain that opens the bytes a member-card signature covers
/// (`lakko/channel/member-card-sig/v1`, distinct from the card key's HKDF
/// info label).
pub use lakko_channels::MEMBER_CARD_SIG_DOMAIN_V1 as MEMBER_CARD_SIG_DOMAIN;

/// Default and maximum page size of `GET .../members`.
pub const MEMBER_PAGE_DEFAULT: usize = 100;
/// The largest page `GET .../members` returns.
pub const MEMBER_PAGE_MAX: usize = 500;

/// The bytes a subscriber signs to write (`card` = the card) or delete
/// (`card` = empty) its member card - `lakko_channels::member_card_signed_bytes`,
/// the one definition the clients sign.
#[must_use]
pub fn member_card_signed_bytes(
    channel: &ChannelId,
    subscriber: &SubscriberId,
    card: &[u8],
) -> Vec<u8> {
    lakko_channels::member_card_signed_bytes(channel.as_bytes(), subscriber.as_bytes(), card)
}

/// Whether the request carries a valid signature by `subscriber` (its
/// Ed25519 key, ADR-0060) over [`member_card_signed_bytes`].
fn member_signature_valid(
    headers: &HeaderMap,
    channel: &ChannelId,
    subscriber: &SubscriberId,
    card: &[u8],
) -> bool {
    let Some(value) = headers
        .get(MEMBER_CARD_SIG_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Ok(sig_bytes) = URL_SAFE_NO_PAD.decode(value) else {
        return false;
    };
    lakko_channels::verify_member_card_request(
        channel.as_bytes(),
        subscriber.as_bytes(),
        card,
        &sig_bytes,
    )
    .is_ok()
}

fn bad_ids(channel_id: &str, pubkey_hex: &str) -> Result<(ChannelId, SubscriberId), Box<Response>> {
    let Ok(channel) = ChannelId::from_hex(channel_id) else {
        return Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
            )
                .into_response(),
        ));
    };
    let Ok(sub) = SubscriberId::from_hex(pubkey_hex) else {
        return Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(err(
                    "bad-subscriber-id",
                    "subscriber pubkey must be 64 hex chars",
                )),
            )
                .into_response(),
        ));
    };
    Ok((channel, sub))
}

fn bad_member_signature() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(err(
            "bad-member-signature",
            "the request must be signed by the subscriber key it names",
        )),
    )
        .into_response()
}

fn member_store_error(e: StorageError) -> Response {
    match e {
        StorageError::UnknownChannel => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        StorageError::UnknownSubscriber => (
            StatusCode::NOT_FOUND,
            Json(err(
                "unknown-subscriber",
                "subscriber has not registered against this channel",
            )),
        )
            .into_response(),
        StorageError::MemberCardEpochInFuture { current, got } => (
            StatusCode::CONFLICT,
            Json(err(
                "member-card-epoch-in-future",
                format!("epoch {got} exceeds the channel's current epoch {current}"),
            )),
        )
            .into_response(),
        StorageError::MemberCardStale { stored, got } => (
            StatusCode::CONFLICT,
            Json(err(
                "member-card-stale",
                format!("epoch {got} is older than the stored card's {stored}"),
            )),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err("internal", "unexpected error")),
        )
            .into_response(),
    }
}

/// `PUT /v1/channels/:channel_id/members/:pubkey_hex` — upload the
/// subscriber's member card (ADR-0060). The body is the opaque card; the
/// server reads only its clear epoch. Signed by the subscriber key the path
/// names ([`MEMBER_CARD_SIG_HEADER`] over [`member_card_signed_bytes`]).
async fn put_member_card(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (channel, sub) = match bad_ids(&channel_id, &pubkey_hex) {
        Ok(ids) => ids,
        Err(resp) => return *resp,
    };
    // `member_card_epoch` checks the frame (length bounds and version)
    // without opening anything.
    let Ok(epoch) = lakko_channels::member_card_epoch(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-member-card",
                format!(
                    "a member card is {}..={} bytes of a known version",
                    lakko_channels::MEMBER_CARD_MIN_LEN,
                    lakko_channels::MEMBER_CARD_MAX_LEN
                ),
            )),
        )
            .into_response();
    };
    if !member_signature_valid(&headers, &channel, &sub, &body) {
        return bad_member_signature();
    }
    match state
        .store
        .put_member_card(channel, sub, epoch.as_u32(), body.to_vec())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => member_store_error(e),
    }
}

/// `DELETE /v1/channels/:channel_id/members/:pubkey_hex` — remove the
/// subscriber's member card (unsubscribe). Signed like the upload, over the
/// empty card. Idempotent.
async fn delete_member_card(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let (channel, sub) = match bad_ids(&channel_id, &pubkey_hex) {
        Ok(ids) => ids,
        Err(resp) => return *resp,
    };
    if !member_signature_valid(&headers, &channel, &sub, &[]) {
        return bad_member_signature();
    }
    match state.store.delete_member_card(channel, sub).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => member_store_error(e),
    }
}

/// The header carrying an unsubscribe signature (ADR-0060):
/// `<issued_at_ms>.<signature base64url>`. Defined in `lakko-channels`.
pub use lakko_channels::UNSUBSCRIBE_SIG_HEADER;

/// Parse the [`UNSUBSCRIBE_SIG_HEADER`] value into `(issued_at_ms,
/// signature)`. `None` for anything malformed.
fn parse_unsubscribe_header(headers: &HeaderMap) -> Option<(u64, Vec<u8>)> {
    let value = headers.get(UNSUBSCRIBE_SIG_HEADER)?.to_str().ok()?;
    let (stamp, sig) = value.split_once('.')?;
    // Digits only: `u64::from_str` would also accept a leading `+`.
    if stamp.is_empty() || !stamp.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let issued_at_ms = stamp.parse::<u64>().ok()?;
    let sig = URL_SAFE_NO_PAD.decode(sig).ok()?;
    Some((issued_at_ms, sig))
}

/// `DELETE /v1/channels/:channel_id/subscribers/:pubkey_hex` — leave the
/// channel (ADR-0060 section 3): the subscriber row, its seed envelopes, its
/// member card and any staged envelope go. Signed by the subscriber key the
/// path names over `lakko_channels::unsubscribe_signed_bytes`, stamped with
/// the signer's clock and accepted only within
/// `lakko_channels::UNSUBSCRIBE_WINDOW_MS` of the server's. The signature is
/// checked before the store is consulted, so an unsigned request learns
/// nothing about who subscribes. Idempotent `204`.
async fn unsubscribe(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let (channel, sub) = match bad_ids(&channel_id, &pubkey_hex) {
        Ok(ids) => ids,
        Err(resp) => return *resp,
    };
    if headers.contains_key(lakko_channels::owner::OWNER_PROOF_HEADER_NAME)
        || headers.contains_key(lakko_channels::copublisher::COPUBLISHER_PROOF_HEADER_NAME)
    {
        return remove_subscriber(&state, channel, sub, &method, &uri, &headers).await;
    }
    let Some((issued_at_ms, sig)) = parse_unsubscribe_header(&headers) else {
        return bad_unsubscribe_signature("bad-unsubscribe-signature");
    };
    match lakko_channels::verify_unsubscribe_request(
        channel.as_bytes(),
        sub.as_bytes(),
        issued_at_ms,
        &sig,
        unix_ms(),
    ) {
        Ok(()) => {},
        Err(lakko_channels::ChannelError::StaleRequest) => {
            return bad_unsubscribe_signature("stale-unsubscribe");
        },
        Err(_) => return bad_unsubscribe_signature("bad-unsubscribe-signature"),
    }
    match state.store.unsubscribe(channel, sub).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => member_store_error(e),
    }
}

/// A publisher removes `sub` from a private channel (ADR-0087 section 7):
/// the publisher proof over the empty body, then the removal, recorded so
/// the removed key's envelope reads answer `410 subscriber-removed`.
/// Idempotent `204`; a public channel is `409 channel-public` (anyone can
/// come back to a public channel).
async fn remove_subscriber(
    state: &AppState,
    channel: ChannelId,
    sub: SubscriberId,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Response {
    if let Err(refused) = require_publisher(state, channel, method, uri, headers, &[]).await {
        return refused;
    }
    match state.store.remove_subscriber(channel, sub, unix_ms()).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(StorageError::ChannelPublic) => (
            StatusCode::CONFLICT,
            Json(err(
                "channel-public",
                "a public channel's subscribers cannot be removed",
            )),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "remove subscriber failed");
            internal_error()
        },
    }
}

fn bad_unsubscribe_signature(code: &'static str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(err(
            code,
            "an unsubscribe must be signed by the subscriber key it names, \
             stamped within the freshness window",
        )),
    )
        .into_response()
}

/// Query string for `GET /v1/channels/:id/members`.
#[derive(Deserialize)]
pub struct MembersQuery {
    /// Base64url (no padding) of the last subscriber key of the previous
    /// page; the page starts strictly after it.
    #[serde(default)]
    pub after: Option<String>,
    /// Page size, at most [`MEMBER_PAGE_MAX`] (default
    /// [`MEMBER_PAGE_DEFAULT`]).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One entry of `GET /v1/channels/:id/members`.
#[derive(Serialize)]
pub struct MemberView {
    /// Base64url (no padding) of the subscriber key.
    pub subscriber_pk_b64: String,
    /// Base64url (no padding) of the opaque member card.
    pub card_b64: String,
}

/// Response of `GET /v1/channels/:id/members`.
#[derive(Serialize)]
pub struct MembersResponse {
    /// Cards ascending by subscriber key.
    pub members: Vec<MemberView>,
    /// The `after` for the next page; `null` when this page was the last
    /// one (shorter than the limit).
    pub next_after: Option<String>,
}

/// `GET /v1/channels/:channel_id/members` — a page of member cards
/// (ADR-0060). Unauthenticated like every read here: a card opens only
/// under the channel's epoch seed.
async fn list_member_cards(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Query(q): Query<MembersQuery>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err("bad-channel-id", "channel_id must be 32 hex chars")),
        )
            .into_response();
    };
    let after = match q.after.as_deref() {
        None | Some("") => None,
        Some(b64) => {
            match URL_SAFE_NO_PAD
                .decode(b64)
                .ok()
                .and_then(|b| SubscriberId::from_bytes(&b).ok())
            {
                Some(sub) => Some(sub),
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(err(
                            "bad-after",
                            "after must be a 32-byte key in base64url without padding",
                        )),
                    )
                        .into_response()
                },
            }
        },
    };
    let limit = q
        .limit
        .unwrap_or(MEMBER_PAGE_DEFAULT)
        .clamp(1, MEMBER_PAGE_MAX);
    match state.store.list_member_cards(channel, after, limit).await {
        Ok(page) => {
            let next_after = (page.len() == limit)
                .then(|| {
                    page.last()
                        .map(|(sub, _)| URL_SAFE_NO_PAD.encode(sub.as_bytes()))
                })
                .flatten();
            let members = page
                .into_iter()
                .map(|(sub, card)| MemberView {
                    subscriber_pk_b64: URL_SAFE_NO_PAD.encode(sub.as_bytes()),
                    card_b64: URL_SAFE_NO_PAD.encode(card),
                })
                .collect();
            (
                StatusCode::OK,
                Json(MembersResponse {
                    members,
                    next_after,
                }),
            )
                .into_response()
        },
        Err(e) => member_store_error(e),
    }
}

/// Parse a JSON body the gate has already accepted, with the same
/// `Content-Type` check and the same rejection responses as the `Json`
/// extractor the handler used before it had to hash the raw bytes (a
/// missing JSON content type is 415, a syntax error 400, a shape error
/// 422).
async fn parse_json_body<T: serde::de::DeserializeOwned>(
    headers: &HeaderMap,
    body: Bytes,
) -> Result<T, Response> {
    let mut req = Request::new(Body::from(body));
    if let Some(ct) = headers.get(header::CONTENT_TYPE) {
        req.headers_mut().insert(header::CONTENT_TYPE, ct.clone());
    }
    Json::<T>::from_request(req, &())
        .await
        .map(|Json(v)| v)
        .map_err(IntoResponse::into_response)
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
        Err(StorageError::ChannelPrivate) => channel_private(),
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

/// `PUT /v1/channels/:channel_id/owner` — hand the channel to a new owner
/// key ([ADR-0074] section 3.5). Signed by the CURRENT owner key; the body is
/// the new key, 32 raw bytes (`lakko_channels::owner_handover_body`).
///
/// When it is used: the account that owns the channel changed its account
/// keys, and the owner key is derived from the account seed. The device that
/// holds both seeds signs with the key the old seed derives and names the key
/// the new seed derives. After the `204` the old key is nobody to this
/// channel: its proofs are the uniform `401`, re-creating the channel under
/// it is `409 channel-exists`.
///
/// Order: the owner proof over the exact body (the uniform `401
/// owner-proof-required`; an unknown channel is refused the same way), then
/// the body (`400 bad-owner-pubkey`), then the swap. The swap is a
/// compare-and-swap against the key the proof was verified under, so of two
/// handovers signed by one key exactly one lands and the other is `409
/// owner-changed`.
///
/// A handover that names the key that already owns the channel is a `204`
/// that changes nothing: the new owner's way to ask "did my handover land?"
/// after a lost answer, since the old key can no longer ask anything.
///
/// The co-publisher appointments and the listing stay as they are: neither
/// carries a signature by the owner key (an appointment is a row the owner
/// asked for, a listing is checked at upload).
///
/// [ADR-0074]: ../../../docs/adr/0074-account-takeover-resistance.md
async fn replace_owner(
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
    // The key the proof is about to be verified under: the swap below is
    // conditional on it still being the owner's.
    let current = match state.store.owner_pubkey(channel).await {
        Ok(key) => key,
        Err(e) => {
            tracing::error!(error = %e, "owner key lookup failed");
            return internal_error();
        },
    };
    if let Err(refused) = require_owner(&state, channel, &method, &uri, &headers, &body).await {
        return refused;
    }
    let Some(current) = current else {
        // `require_owner` refuses an unknown channel; reaching here means the
        // channel appeared between the two reads.
        return (
            StatusCode::CONFLICT,
            Json(err(
                "owner-changed",
                "the channel's owner changed; ask again",
            )),
        )
            .into_response();
    };
    let Ok(new_owner) = lakko_channels::parse_owner_handover_body(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-owner-pubkey",
                "the body must be the 32 bytes of the new owner's Ed25519 public key",
            )),
        )
            .into_response();
    };
    let new_owner = new_owner.to_bytes();
    if new_owner == current {
        return StatusCode::NO_CONTENT.into_response();
    }
    match state.store.replace_owner(channel, current, new_owner).await {
        Ok(true) => {
            tracing::info!("channel owner key replaced");
            StatusCode::NO_CONTENT.into_response()
        },
        Ok(false) => (
            StatusCode::CONFLICT,
            Json(err(
                "owner-changed",
                "the channel's owner key changed while this request was in flight",
            )),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => (
            StatusCode::NOT_FOUND,
            Json(err("unknown-channel", "no such channel")),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "replace owner failed");
            internal_error()
        },
    }
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

fn channel_private() -> Response {
    (
        StatusCode::CONFLICT,
        Json(err(
            "channel-private",
            "this is a private channel: it is never listed and holds no public seed",
        )),
    )
        .into_response()
}

fn unknown_channel() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(err("unknown-channel", "no such channel")),
    )
        .into_response()
}

/// `PUT /v1/channels/:channel_id/access` — set the channel's flag
/// (ADR-0087 section 8), owner-signed over the exact body `{"private":
/// bool}`. Only the owner changes it. Going private withdraws the listing and
/// deletes every public seed in the same step; going public changes only the
/// flag (the client then publishes the listing and the public seed).
/// Idempotent `204`.
async fn put_access(
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
    let req: AccessRequest = match parse_json_body(&headers, body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match state.store.set_private(channel, req.private).await {
        Ok(changed) => {
            if changed {
                tracing::info!(private = req.private, "channel flag changed");
            }
            StatusCode::NO_CONTENT.into_response()
        },
        Err(StorageError::UnknownChannel) => unknown_channel(),
        Err(e) => {
            tracing::error!(error = %e, "set channel flag failed");
            internal_error()
        },
    }
}

fn parse_epoch(epoch: &str) -> Option<u32> {
    // Digits only: `u32::from_str` would also accept a leading `+`.
    if epoch.is_empty() || epoch.len() > 10 || !epoch.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    epoch.parse().ok()
}

fn bad_epoch() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(err("bad-epoch", "epoch must be a decimal u32")),
    )
        .into_response()
}

/// `PUT /v1/channels/:channel_id/public-seed/:epoch` — file a public
/// channel's seed envelope for `epoch` (ADR-0087 section 6). Publisher-signed
/// over the exact body, the 64 bytes of `lakko_channels::seal_public_seed`,
/// whose clear epoch must name the path's. The service never opens it: a
/// subscriber holds the read key, so only this route keeps one from
/// replacing it. The epoch may be at most the next one (filed before the
/// rotation commits). Immutable: the identical envelope again is a `204`,
/// another is `409 public-seed-exists`.
async fn put_public_seed(
    State(state): State<Arc<AppState>>,
    Path((channel_id, epoch)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    let Some(epoch) = parse_epoch(&epoch) else {
        return bad_epoch();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &body).await {
        return refused;
    }
    let Ok(envelope) = <[u8; PUBLIC_SEED_LEN]>::try_from(body.as_ref()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "bad-public-seed",
                format!("a public seed envelope is {PUBLIC_SEED_LEN} bytes"),
            )),
        )
            .into_response();
    };
    if lakko_channels::public_seed_epoch(&envelope).map(lakko_channels::EpochId::as_u32)
        != Some(epoch)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(err(
                "public-seed-epoch-mismatch",
                "the envelope names another epoch than the path",
            )),
        )
            .into_response();
    }
    match state
        .store
        .put_public_seed(channel, epoch, envelope, unix_ms())
        .await
    {
        Ok(PublicSeedOutcome::Stored | PublicSeedOutcome::AlreadyStored) => {
            StatusCode::NO_CONTENT.into_response()
        },
        Err(StorageError::UnknownChannel) => unknown_channel(),
        Err(StorageError::ChannelPrivate) => channel_private(),
        Err(StorageError::PublicSeedEpochInFuture { current, got }) => (
            StatusCode::CONFLICT,
            Json(err(
                "public-seed-epoch-in-future",
                format!("epoch {got} is beyond the next epoch (current epoch is {current})"),
            )),
        )
            .into_response(),
        Err(StorageError::PublicSeedConflict) => (
            StatusCode::CONFLICT,
            Json(err(
                "public-seed-exists",
                "another public seed is stored for this epoch",
            )),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "put public seed failed");
            internal_error()
        },
    }
}

/// `GET /v1/channels/:channel_id/public-seed/:epoch` — the stored public
/// seed envelope, raw bytes. Open and immutable: a cache in front serves it
/// ([`PUBLIC_SEED_CACHE_CONTROL`]). An absent seed (unknown channel, private
/// channel, epoch not filed yet) is `404 public-seed-not-found` with
/// `no-store`, so no cache pins the absence.
async fn get_public_seed(
    State(state): State<Arc<AppState>>,
    Path((channel_id, epoch)): Path<(String, String)>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    let Some(epoch) = parse_epoch(&epoch) else {
        return bad_epoch();
    };
    match state.store.public_seed(channel, epoch).await {
        Ok(Some(envelope)) => {
            let mut resp = (StatusCode::OK, envelope.to_vec()).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            with_cache_control(resp, PUBLIC_SEED_CACHE_CONTROL)
        },
        Ok(None) => with_cache_control(
            (
                StatusCode::NOT_FOUND,
                Json(err(
                    "public-seed-not-found",
                    "no public seed is stored for this channel and epoch",
                )),
            )
                .into_response(),
            "no-store",
        ),
        Err(e) => {
            tracing::error!(error = %e, "public seed read failed");
            internal_error()
        },
    }
}

/// Parse a 64-hex invite key: a curve point of full order.
fn parse_invite_key(hex_str: &str) -> Option<[u8; INVITE_KEY_LEN]> {
    let key = parse_pubkey_hex(hex_str)?;
    let vk = lakko_crypto::signing::VerifyingKey::from_bytes(&key).ok()?;
    (!vk.is_weak()).then_some(key)
}

fn bad_invite_key() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(err(
            "bad-invite-key",
            "the invite key must be 64 hex chars of an Ed25519 public key",
        )),
    )
        .into_response()
}

/// `GET /v1/channels/:channel_id/invite-keys` — a private channel's
/// registered invite keys with expiry and use count (ADR-0087 section 7),
/// publisher-signed. A publisher reads them to check an admission ticket's
/// key, including keys a co-publisher registered.
async fn list_invite_keys(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &[]).await {
        return refused;
    }
    match state.store.list_invite_keys(channel).await {
        Ok(keys) => (
            StatusCode::OK,
            Json(InviteKeysResponse {
                invite_keys: keys.iter().map(InviteKeyView::from_row).collect(),
            }),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => unknown_channel(),
        Err(e) => {
            tracing::error!(error = %e, "list invite keys failed");
            internal_error()
        },
    }
}

/// `PUT /v1/channels/:channel_id/invite-keys/:pubkey_hex` — register or
/// update a link's invite key (ADR-0087 section 7), publisher-signed over
/// the exact body `{expiry_ms, max_uses}`. Updating keeps the use count. At
/// most [`MAX_INVITE_KEYS`] per channel (`409 too-many-invite-keys`).
async fn put_invite_key(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &body).await {
        return refused;
    }
    let Some(key) = parse_invite_key(&pubkey_hex) else {
        return bad_invite_key();
    };
    let req: InviteKeyRequest = match parse_json_body(&headers, body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match state
        .store
        .put_invite_key(
            channel,
            key,
            req.expiry_ms,
            req.max_uses,
            MAX_INVITE_KEYS,
            unix_ms(),
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(StorageError::TooManyInviteKeys) => (
            StatusCode::CONFLICT,
            Json(err(
                "too-many-invite-keys",
                "revoke an invite key before registering another",
            )),
        )
            .into_response(),
        Err(StorageError::UnknownChannel) => unknown_channel(),
        Err(e) => {
            tracing::error!(error = %e, "put invite key failed");
            internal_error()
        },
    }
}

/// `DELETE /v1/channels/:channel_id/invite-keys/:pubkey_hex` — revoke a
/// link's invite key, publisher-signed. A new subscribe under it is refused
/// at once. Idempotent `204`.
async fn delete_invite_key(
    State(state): State<Arc<AppState>>,
    Path((channel_id, pubkey_hex)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else {
        return bad_channel_id();
    };
    if let Err(refused) = require_publisher(&state, channel, &method, &uri, &headers, &[]).await {
        return refused;
    }
    let Some(key) = parse_invite_key(&pubkey_hex) else {
        return bad_invite_key();
    };
    match state.store.delete_invite_key(channel, key).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "delete invite key failed");
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
        .route("/v1/channels/{channel_id}/owner", put(replace_owner))
        .route("/v1/channels/{channel_id}/access", put(put_access))
        .route(
            "/v1/channels/{channel_id}/public-seed/{epoch}",
            put(put_public_seed).get(get_public_seed),
        )
        .route(
            "/v1/channels/{channel_id}/invite-keys",
            get(list_invite_keys),
        )
        .route(
            "/v1/channels/{channel_id}/invite-keys/{pubkey_hex}",
            put(put_invite_key).delete(delete_invite_key),
        )
        .route(
            "/v1/channels/{channel_id}/subscribers/count",
            get(subscriber_count),
        )
        .route(
            "/v1/channels/{channel_id}/posts",
            post(publish_post).get(fetch_posts),
        )
        .route(
            "/v1/channels/{channel_id}/subscribers",
            post(subscribe).get(list_subscribers),
        )
        .route(
            "/v1/channels/{channel_id}/subscribers/{pubkey_hex}",
            axum::routing::delete(unsubscribe),
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
        // ADR-0060 — channel member cards.
        .route("/v1/channels/{channel_id}/members", get(list_member_cards))
        .route(
            "/v1/channels/{channel_id}/members/{pubkey_hex}",
            axum::routing::put(put_member_card).delete(delete_member_card),
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
