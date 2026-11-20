// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Routes for comments and reactions under a post ([ADR-0087] section 13a).
//!
//! | Method | Path | Gate | Purpose |
//! |---|---|---|---|
//! | `POST` | `/v1/channels/:id/posts/:cursor/reactions` | write proof by the reaction's key, then one `channel-react` token | Add the row `{writer_key_hex, label_hex}`. `201 {"added": true}`, or `200 {"added": false}` when the row was there. |
//! | `DELETE` | `/v1/channels/:id/posts/:cursor/reactions/:writer_key_hex/:label_hex` | write proof by that key | Take the reaction off. `204` whether or not it was there. |
//! | `GET` | `/v1/channels/:id/reactions?posts=<c1>,<c2>,...` | none | Label counts and the comment count of up to 50 posts, `{"posts": [{"cursor", "labels": [{"label_hex", "count"}], "comments"}]}`, at most 64 labels a post, the most counted first; `comments` is the post's maintained comment count (2026-11-05). `Cache-Control: public, max-age=15`. |
//! | `POST` | `/v1/channels/:id/posts/:cursor/comments/:comment_cursor/reactions` | write proof by the reaction's key (op `0x05`), then one `channel-react` token | Add a reaction on the comment `{writer_key_hex, label_hex}` (a `CommentReactionKey` label). `201 {"added": true}` / `200 {"added": false}`; `404 unknown-comment`. |
//! | `DELETE` | `/v1/channels/:id/posts/:cursor/comments/:comment_cursor/reactions/:writer_key_hex/:label_hex` | write proof by that key (op `0x06`) | Take it off. `204`. |
//! | `GET` | `/v1/channels/:id/posts/:cursor/comment-reactions?comments=<c1>,...` | none | Label counts of up to 50 comments under the post, `{"comments": [{"cursor", "labels": [{"label_hex", "count"}]}]}`. `Cache-Control: public, max-age=15`. |
//! | `POST` | `/v1/channels/:id/posts/:cursor/comments` | write proof by the comment's key, then one `channel-comment` token | Add `{writer_key_hex, sealed_b64}`. `201 {"cursor": n}`. |
//! | `GET` | `/v1/channels/:id/posts/:cursor/comments?after=&limit=` | none | A page of comments, cursor ascending, at most 20: `{"comments": [{"cursor", "writer_key_hex", "sealed_b64", "server_ts_ms"}], "next_after"}`. |
//! | `DELETE` | `/v1/channels/:id/posts/:cursor/comments/:comment_cursor` | write proof by the comment's key, or a publisher proof | Delete the comment. `204`; `404 unknown-comment`. |
//!
//! ## The gate, in order
//!
//! For a write by a subscriber:
//!
//! 1. the path and body parse (a malformed request costs nothing);
//! 2. the `x-lakko-channel-write-proof` header parses and
//!    `lakko_channels::verify_write_proof` accepts it over the write
//!    (window, a valid full-order key, the signature);
//! 3. its nonce has not been seen, recording it;
//! 4. only for an add: the post exists (`404 unknown-post` otherwise, so no
//!    token is spent on a post that is not there), then one anonymous token
//!    of the write's context is spent. A post pruned between the check and
//!    the insert still answers `404 unknown-post`, from the store.
//!
//! Every refusal of steps 2 and 3 is one `401 {"code":
//! "write-proof-required"}`. A store failure is a 500 and nothing is
//! honoured. A comment's delete reads the row's key first, so the proof is
//! checked against the key the row names; a publisher may instead present
//! the owner or co-publisher proof every publisher route takes.
//!
//! **Recorded residual:** step 3 records a nonce for any caller who signs
//! with a key of its own making, before a token is spent, so a caller with no
//! token can add rows to `channel_write_nonces`, one per request, each kept
//! for 240 s. The rate in front of the service bounds it.
//!
//! The service never opens a comment and holds no emoji: it stores labels,
//! sealed bytes and per-write keys, and serves them back.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_token::TokenContext;
use lakko_channels::copublisher::COPUBLISHER_PROOF_HEADER_NAME;
use lakko_channels::owner::OwnerProof;
use lakko_channels::owner::OWNER_PROOF_HEADER_NAME;
use lakko_channels::{
    verify_write_proof, SubscriberWrite, WriteStatement, WRITE_PROOF_HEADER_NAME,
};
use serde::{Deserialize, Serialize};

use crate::api::{err, unix_ms, AppState, ErrJson};
use crate::feedback::{
    Label, WriterKey, COMMENT_PAGE_LEN, COMMENT_SEALED_MAX_LEN, COMMENT_SEALED_MIN_LEN,
    MAX_COUNT_COMMENTS, MAX_COUNT_POSTS,
};
use crate::metrics::{proof_refused, ProofKind, RefusalReason};
use crate::owner_auth::require_publisher;
use crate::storage::{ChannelId, StorageError};

/// `Cache-Control` of the counts read: identical for every caller, so a
/// cache in front may hold it briefly. Chosen, not measured.
pub const REACTION_COUNTS_CACHE_CONTROL: &str = "public, max-age=15";

/// How long a write-proof nonce is remembered: twice the proof window.
pub const WRITE_NONCE_RETENTION_MS: u64 = 2 * lakko_channels::WRITE_PROOF_WINDOW_MS;

/// Body of `POST .../reactions`.
#[derive(Deserialize)]
pub struct AddReactionRequest {
    /// 64-char hex of the reaction's write key.
    pub writer_key_hex: String,
    /// 32-char hex of the label.
    pub label_hex: String,
}

/// Answer of `POST .../reactions`.
#[derive(Serialize)]
pub struct AddReactionResponse {
    /// `false` when the row was already there.
    pub added: bool,
}

/// Body of `POST .../comments`.
#[derive(Deserialize)]
pub struct AddCommentRequest {
    /// 64-char hex of the comment's write key.
    pub writer_key_hex: String,
    /// The sealed comment, base64url without padding.
    pub sealed_b64: String,
}

/// Answer of `POST .../comments`.
#[derive(Serialize)]
pub struct AddCommentResponse {
    /// The comment cursor.
    pub cursor: u64,
}

/// Query of `GET .../reactions`.
#[derive(Deserialize)]
pub struct CountsQuery {
    /// Comma-separated post cursors, 1 to 50.
    #[serde(default)]
    pub posts: String,
}

/// One label count.
#[derive(Serialize)]
pub struct LabelCountView {
    /// 32-char hex of the label.
    pub label_hex: String,
    /// Rows with it.
    pub count: u64,
}

/// The counts of one post.
#[derive(Serialize)]
pub struct PostCountsView {
    /// The post cursor.
    pub cursor: u64,
    /// Most counted first, at most 64.
    pub labels: Vec<LabelCountView>,
    /// How many comments the post has (the maintained count).
    pub comments: u64,
}

/// The reaction counts of one comment.
#[derive(Serialize)]
pub struct CommentCountsView {
    /// The comment cursor.
    pub cursor: u64,
    /// Most counted first, at most 64.
    pub labels: Vec<LabelCountView>,
}

/// Answer of `GET .../comment-reactions`.
#[derive(Serialize)]
pub struct CommentCountsResponse {
    /// One entry per requested comment, in the order asked.
    pub comments: Vec<CommentCountsView>,
}

/// Query of `GET .../comment-reactions`.
#[derive(Deserialize)]
pub struct CommentCountsQuery {
    /// Comma-separated comment cursors, 1 to 50.
    #[serde(default)]
    pub comments: String,
}

/// Answer of `GET .../reactions`.
#[derive(Serialize)]
pub struct CountsResponse {
    /// One entry per requested post, in the order asked.
    pub posts: Vec<PostCountsView>,
}

/// Query of `GET .../comments`.
#[derive(Deserialize)]
pub struct CommentsQuery {
    /// Comments with a cursor above this one; `0` from the start.
    #[serde(default)]
    pub after: u64,
    /// At most this many, capped at 20.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One comment in a page.
#[derive(Serialize)]
pub struct CommentView {
    /// The comment cursor.
    pub cursor: u64,
    /// 64-char hex of the comment's write key.
    pub writer_key_hex: String,
    /// The sealed comment, base64url without padding.
    pub sealed_b64: String,
    /// When it was stored, unix milliseconds.
    pub server_ts_ms: u64,
}

/// Answer of `GET .../comments`.
#[derive(Serialize)]
pub struct CommentsResponse {
    /// Cursor ascending.
    pub comments: Vec<CommentView>,
    /// The last cursor in the page, or `after` when it is empty.
    pub next_after: u64,
}

fn bad(code: &'static str, detail: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, ErrJson(err(code, detail))).into_response()
}

fn proof_required() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        ErrJson(err(
            "write-proof-required",
            "this write needs a valid write proof",
        )),
    )
        .into_response()
}

fn internal(e: &StorageError) -> Response {
    tracing::error!(error = %e, "comments and reactions store failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrJson(err("internal", "unexpected error")),
    )
        .into_response()
}

fn unknown_post() -> Response {
    (
        StatusCode::NOT_FOUND,
        ErrJson(err(
            "unknown-post",
            "the channel holds no post at this cursor",
        )),
    )
        .into_response()
}

fn parse_channel(hex: &str) -> Result<ChannelId, Box<Response>> {
    ChannelId::from_hex(hex)
        .map_err(|_| Box::new(bad("bad-channel-id", "channel_id must be 32 hex chars")))
}

fn parse_fixed<const N: usize>(hex_str: &str) -> Option<[u8; N]> {
    hex::decode(hex_str)
        .ok()
        .and_then(|b| <[u8; N]>::try_from(b.as_slice()).ok())
}

fn parse_writer_key(hex_str: &str) -> Result<WriterKey, Box<Response>> {
    parse_fixed(hex_str)
        .ok_or_else(|| Box::new(bad("bad-writer-key", "writer key must be 64 hex chars")))
}

fn parse_label(hex_str: &str) -> Result<Label, Box<Response>> {
    parse_fixed(hex_str).ok_or_else(|| Box::new(bad("bad-label", "label must be 32 hex chars")))
}

/// The post an add names exists, checked before a token is spent.
async fn require_post(
    state: &AppState,
    channel: ChannelId,
    post_cursor: u64,
) -> Result<(), Box<Response>> {
    match state.store.post_exists(channel, post_cursor).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(unknown_post())),
        Err(e) => Err(Box::new(internal(&e))),
    }
}

/// Steps 2 and 3 of the gate: the proof over `statement`, then its nonce.
async fn check_write_proof(
    state: &AppState,
    headers: &HeaderMap,
    statement: &WriteStatement<'_>,
) -> Result<(), Response> {
    let Some(proof) = headers
        .get(WRITE_PROOF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| OwnerProof::parse(v).ok())
    else {
        proof_refused(ProofKind::Write, RefusalReason::Malformed);
        return Err(proof_required());
    };
    let now = unix_ms();
    if verify_write_proof(&proof, statement, now).is_err() {
        proof_refused(ProofKind::Write, RefusalReason::Invalid);
        return Err(proof_required());
    }
    match state.store.consume_write_nonce(proof.nonce, now).await {
        Ok(true) => Ok(()),
        Ok(false) => {
            proof_refused(ProofKind::Write, RefusalReason::Replayed);
            Err(proof_required())
        },
        Err(e) => Err(internal(&e)),
    }
}

/// `POST /v1/channels/:id/posts/:cursor/reactions`.
async fn add_reaction(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor)): Path<(String, u64)>,
    headers: HeaderMap,
    Json(req): Json<AddReactionRequest>,
) -> Response {
    let channel = match parse_channel(&channel_id) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let writer_key = match parse_writer_key(&req.writer_key_hex) {
        Ok(k) => k,
        Err(r) => return *r,
    };
    let label = match parse_label(&req.label_hex) {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let statement = WriteStatement {
        channel_id: channel.as_bytes(),
        post_cursor,
        writer_key: &writer_key,
        write: SubscriberWrite::AddReaction { label },
    };
    if let Err(r) = check_write_proof(&state, &headers, &statement).await {
        return r;
    }
    if let Err(r) = require_post(&state, channel, post_cursor).await {
        return *r;
    }
    if let Err(refused) = state
        .anon_tokens
        .spend_header(&headers, TokenContext::ChannelReact)
        .await
    {
        return refused.into_response();
    }
    match state
        .store
        .add_reaction(channel, post_cursor, writer_key, label, unix_ms())
        .await
    {
        Ok(true) => (
            StatusCode::CREATED,
            Json(AddReactionResponse { added: true }),
        )
            .into_response(),
        Ok(false) => (StatusCode::OK, Json(AddReactionResponse { added: false })).into_response(),
        Err(StorageError::UnknownPost) => unknown_post(),
        Err(e) => internal(&e),
    }
}

/// `DELETE /v1/channels/:id/posts/:cursor/reactions/:writer_key_hex/:label_hex`.
async fn delete_reaction(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor, writer_key_hex, label_hex)): Path<(String, u64, String, String)>,
    headers: HeaderMap,
) -> Response {
    let channel = match parse_channel(&channel_id) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let writer_key = match parse_writer_key(&writer_key_hex) {
        Ok(k) => k,
        Err(r) => return *r,
    };
    let label = match parse_label(&label_hex) {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let statement = WriteStatement {
        channel_id: channel.as_bytes(),
        post_cursor,
        writer_key: &writer_key,
        write: SubscriberWrite::DeleteReaction { label },
    };
    if let Err(r) = check_write_proof(&state, &headers, &statement).await {
        return r;
    }
    match state
        .store
        .delete_reaction(channel, post_cursor, writer_key, label)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal(&e),
    }
}

/// `GET /v1/channels/:id/reactions?posts=`.
async fn reaction_counts(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Query(q): Query<CountsQuery>,
) -> Response {
    let channel = match parse_channel(&channel_id) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let cursors = match parse_cursor_list(&q.posts, MAX_COUNT_POSTS) {
        Ok(c) => c,
        Err(CursorListError::Malformed) => {
            return bad("bad-posts", "posts must be comma-separated post cursors")
        },
        Err(CursorListError::TooMany) => return bad("too-many-posts", "at most 50 posts per read"),
    };
    let comment_counts = match state.store.comment_counts(channel, &cursors).await {
        Ok(c) => c,
        Err(e) => return internal(&e),
    };
    match state.store.reaction_counts(channel, &cursors).await {
        Ok(counts) => {
            let body = CountsResponse {
                posts: counts
                    .into_iter()
                    .zip(comment_counts)
                    .map(|(p, (_, comments))| PostCountsView {
                        cursor: p.post_cursor,
                        labels: label_views(p.labels),
                        comments,
                    })
                    .collect(),
            };
            let mut resp = Json(body).into_response();
            resp.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                HeaderValue::from_static(REACTION_COUNTS_CACHE_CONTROL),
            );
            resp
        },
        Err(e) => internal(&e),
    }
}

/// Why a cursor list was refused.
enum CursorListError {
    Malformed,
    TooMany,
}

/// Parse `a,b,c` into at most `max` distinct cursors, in order.
fn parse_cursor_list(list: &str, max: usize) -> Result<Vec<u64>, CursorListError> {
    let mut cursors: Vec<u64> = Vec::new();
    for part in list.split(',') {
        let c = part
            .trim()
            .parse::<u64>()
            .map_err(|_| CursorListError::Malformed)?;
        if !cursors.contains(&c) {
            cursors.push(c);
        }
        if cursors.len() > max {
            return Err(CursorListError::TooMany);
        }
    }
    Ok(cursors)
}

fn label_views(labels: Vec<(Label, u64)>) -> Vec<LabelCountView> {
    labels
        .into_iter()
        .map(|(label, count)| LabelCountView {
            label_hex: hex::encode(label),
            count,
        })
        .collect()
}

/// `POST /v1/channels/:id/posts/:cursor/comments/:comment_cursor/reactions`.
async fn add_comment_reaction(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor, comment_cursor)): Path<(String, u64, u64)>,
    headers: HeaderMap,
    Json(req): Json<AddReactionRequest>,
) -> Response {
    let channel = match parse_channel(&channel_id) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let writer_key = match parse_writer_key(&req.writer_key_hex) {
        Ok(k) => k,
        Err(r) => return *r,
    };
    let label = match parse_label(&req.label_hex) {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let statement = WriteStatement {
        channel_id: channel.as_bytes(),
        post_cursor,
        writer_key: &writer_key,
        write: SubscriberWrite::AddCommentReaction {
            comment_cursor,
            label,
        },
    };
    if let Err(r) = check_write_proof(&state, &headers, &statement).await {
        return r;
    }
    // The comment exists before a token is spent.
    match state
        .store
        .comment_writer(channel, post_cursor, comment_cursor)
        .await
    {
        Ok(Some(_)) => {},
        Ok(None) => return unknown_comment(),
        Err(e) => return internal(&e),
    }
    if let Err(refused) = state
        .anon_tokens
        .spend_header(&headers, TokenContext::ChannelReact)
        .await
    {
        return refused.into_response();
    }
    match state
        .store
        .add_comment_reaction(
            channel,
            post_cursor,
            comment_cursor,
            writer_key,
            label,
            unix_ms(),
        )
        .await
    {
        Ok(true) => (
            StatusCode::CREATED,
            Json(AddReactionResponse { added: true }),
        )
            .into_response(),
        Ok(false) => (StatusCode::OK, Json(AddReactionResponse { added: false })).into_response(),
        Err(StorageError::UnknownComment) => unknown_comment(),
        Err(e) => internal(&e),
    }
}

/// `DELETE .../comments/:comment_cursor/reactions/:writer_key_hex/:label_hex`.
async fn delete_comment_reaction(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor, comment_cursor, writer_key_hex, label_hex)): Path<(
        String,
        u64,
        u64,
        String,
        String,
    )>,
    headers: HeaderMap,
) -> Response {
    let channel = match parse_channel(&channel_id) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let writer_key = match parse_writer_key(&writer_key_hex) {
        Ok(k) => k,
        Err(r) => return *r,
    };
    let label = match parse_label(&label_hex) {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let statement = WriteStatement {
        channel_id: channel.as_bytes(),
        post_cursor,
        writer_key: &writer_key,
        write: SubscriberWrite::DeleteCommentReaction {
            comment_cursor,
            label,
        },
    };
    if let Err(r) = check_write_proof(&state, &headers, &statement).await {
        return r;
    }
    match state
        .store
        .delete_comment_reaction(channel, post_cursor, comment_cursor, writer_key, label)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal(&e),
    }
}

/// `GET /v1/channels/:id/posts/:cursor/comment-reactions?comments=`.
async fn comment_reaction_counts(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor)): Path<(String, u64)>,
    Query(q): Query<CommentCountsQuery>,
) -> Response {
    let channel = match parse_channel(&channel_id) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let cursors = match parse_cursor_list(&q.comments, MAX_COUNT_COMMENTS) {
        Ok(c) => c,
        Err(CursorListError::Malformed) => {
            return bad(
                "bad-comments",
                "comments must be comma-separated comment cursors",
            )
        },
        Err(CursorListError::TooMany) => {
            return bad("too-many-comments", "at most 50 comments per read")
        },
    };
    match state
        .store
        .comment_reaction_counts(channel, post_cursor, &cursors)
        .await
    {
        Ok(counts) => {
            let body = CommentCountsResponse {
                comments: counts
                    .into_iter()
                    .map(|c| CommentCountsView {
                        cursor: c.comment_cursor,
                        labels: label_views(c.labels),
                    })
                    .collect(),
            };
            let mut resp = Json(body).into_response();
            resp.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                HeaderValue::from_static(REACTION_COUNTS_CACHE_CONTROL),
            );
            resp
        },
        Err(e) => internal(&e),
    }
}

/// `POST /v1/channels/:id/posts/:cursor/comments`.
async fn add_comment(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor)): Path<(String, u64)>,
    headers: HeaderMap,
    Json(req): Json<AddCommentRequest>,
) -> Response {
    let channel = match parse_channel(&channel_id) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let writer_key = match parse_writer_key(&req.writer_key_hex) {
        Ok(k) => k,
        Err(r) => return *r,
    };
    let Some(sealed) = URL_SAFE_NO_PAD
        .decode(&req.sealed_b64)
        .ok()
        .filter(|s| (COMMENT_SEALED_MIN_LEN..=COMMENT_SEALED_MAX_LEN).contains(&s.len()))
    else {
        return bad(
            "bad-sealed-comment",
            "sealed_b64 must be a sealed comment of at most 2048 bytes, base64url without padding",
        );
    };
    let statement = WriteStatement {
        channel_id: channel.as_bytes(),
        post_cursor,
        writer_key: &writer_key,
        write: SubscriberWrite::AddComment { sealed: &sealed },
    };
    if let Err(r) = check_write_proof(&state, &headers, &statement).await {
        return r;
    }
    if let Err(r) = require_post(&state, channel, post_cursor).await {
        return *r;
    }
    if let Err(refused) = state
        .anon_tokens
        .spend_header(&headers, TokenContext::ChannelComment)
        .await
    {
        return refused.into_response();
    }
    match state
        .store
        .add_comment(channel, post_cursor, writer_key, sealed, unix_ms())
        .await
    {
        Ok(cursor) => (StatusCode::CREATED, Json(AddCommentResponse { cursor })).into_response(),
        Err(StorageError::UnknownPost) => unknown_post(),
        Err(e) => internal(&e),
    }
}

/// `GET /v1/channels/:id/posts/:cursor/comments`.
async fn list_comments(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor)): Path<(String, u64)>,
    Query(q): Query<CommentsQuery>,
) -> Response {
    let channel = match parse_channel(&channel_id) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let limit = q
        .limit
        .unwrap_or(COMMENT_PAGE_LEN)
        .clamp(1, COMMENT_PAGE_LEN);
    match state
        .store
        .list_comments(channel, post_cursor, q.after, limit)
        .await
    {
        Ok(page) => {
            let next_after = page.last().map_or(q.after, |c| c.cursor);
            Json(CommentsResponse {
                comments: page
                    .into_iter()
                    .map(|c| CommentView {
                        cursor: c.cursor,
                        writer_key_hex: hex::encode(c.writer_key),
                        sealed_b64: URL_SAFE_NO_PAD.encode(&c.sealed),
                        server_ts_ms: c.server_ts_ms,
                    })
                    .collect(),
                next_after,
            })
            .into_response()
        },
        Err(e) => internal(&e),
    }
}

/// `DELETE /v1/channels/:id/posts/:cursor/comments/:comment_cursor`.
async fn delete_comment(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor, comment_cursor)): Path<(String, u64, u64)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let channel = match parse_channel(&channel_id) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let by_publisher = headers.contains_key(OWNER_PROOF_HEADER_NAME)
        || headers.contains_key(COPUBLISHER_PROOF_HEADER_NAME);
    if by_publisher {
        if let Err(r) = require_publisher(&state, channel, &method, &uri, &headers, &[]).await {
            return r;
        }
    } else {
        let writer_key = match state
            .store
            .comment_writer(channel, post_cursor, comment_cursor)
            .await
        {
            Ok(Some(k)) => k,
            Ok(None) => return unknown_comment(),
            Err(e) => return internal(&e),
        };
        let statement = WriteStatement {
            channel_id: channel.as_bytes(),
            post_cursor,
            writer_key: &writer_key,
            write: SubscriberWrite::DeleteComment { comment_cursor },
        };
        if let Err(r) = check_write_proof(&state, &headers, &statement).await {
            return r;
        }
    }
    match state
        .store
        .delete_comment(channel, post_cursor, comment_cursor)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => unknown_comment(),
        Err(e) => internal(&e),
    }
}

fn unknown_comment() -> Response {
    (
        StatusCode::NOT_FOUND,
        ErrJson(err("unknown-comment", "no such comment under this post")),
    )
        .into_response()
}

/// The routes of this module, merged into [`crate::api::router`].
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/v1/channels/{channel_id}/posts/{post_cursor}/reactions",
            post(add_reaction),
        )
        .route(
            "/v1/channels/{channel_id}/posts/{post_cursor}/reactions/{writer_key_hex}/{label_hex}",
            delete(delete_reaction),
        )
        .route("/v1/channels/{channel_id}/reactions", get(reaction_counts))
        .route(
            "/v1/channels/{channel_id}/posts/{post_cursor}/comments",
            post(add_comment).get(list_comments),
        )
        .route(
            "/v1/channels/{channel_id}/posts/{post_cursor}/comments/{comment_cursor}",
            delete(delete_comment),
        )
        .route(
            "/v1/channels/{channel_id}/posts/{post_cursor}/comments/{comment_cursor}/reactions",
            post(add_comment_reaction),
        )
        .route(
            "/v1/channels/{channel_id}/posts/{post_cursor}/comments/{comment_cursor}/reactions/{writer_key_hex}/{label_hex}",
            delete(delete_comment_reaction),
        )
        .route(
            "/v1/channels/{channel_id}/posts/{post_cursor}/comment-reactions",
            get(comment_reaction_counts),
        )
}
