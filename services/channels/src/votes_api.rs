// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Routes for channel poll votes ([ADR-0090] section 4.3).
//!
//! | Method | Path | Body | Answer |
//! |---|---|---|---|
//! | `PUT` | `/v1/channels/:id/posts/:cursor/votes/:tag_hex` | `{presentation_b64, labels_hex, at_ms}` | `201 {"replaced": false}` / `200 {"replaced": true}` |
//! | `DELETE` | same | `{presentation_b64, at_ms}` | `204` |
//!
//! The counts ride the reaction counts read (`GET .../reactions?posts=`,
//! [`crate::feedback_api`]): each post gains `votes: {voters, labels}`.
//!
//! ## The gate, in order
//!
//! 1. The path and body parse: the tag is 64 hex chars, the presentation
//!    base64url, a put's labels 1..=10 strictly ascending 32-hex-char labels
//!    (`400` otherwise).
//! 2. A verifier is configured (`503 anon-credential-disabled`).
//! 3. `at_ms` is within [`VOTE_WINDOW_MS`] of the service clock
//!    (`400 vote-time-out-of-window` with `server_time_ms`).
//! 4. A put names a post that exists (`404 unknown-post`).
//! 5. The presentation verifies for the scope `(channel, post)` and the
//!    message `lakko_channels::vote_message(action, at_ms, labels)` under the
//!    credential key of its period (the current or the previous period), and
//!    its tag is the path's (`401 vote-proof-required`, one answer for every
//!    failure).
//! 6. The tag has requests left in the in-memory cap (`429
//!    vote-rate-limited`).
//! 7. The store accepts `at_ms` as newer than the tag's stored one (`409
//!    stale-vote` with `stored_at_ms` otherwise), and writes the row and the
//!    counters in one transaction.
//!
//! No anonymous token is spent: the credential is the rate limit for the
//! vote itself. The service logs no tag, channel or post.
//!
//! [ADR-0090]: ../../../docs/adr/0090-attachment-kinds.md

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::put,
    Router,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_cred::{period_at, PollScope, PRESENTATION_LEN};
use lakko_channels::{vote_message, VoteAction, MAX_VOTE_LABELS};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::{err, unix_ms, AppState};
use crate::feedback::Label;
use crate::storage::{ChannelId, StorageError};
use crate::votes::{Tag, VoteOutcome, VOTE_WINDOW_MS};

/// Body of `PUT .../votes/:tag_hex`.
#[derive(Deserialize)]
pub struct PutVoteRequest {
    /// The credential presentation, base64url without padding.
    pub presentation_b64: String,
    /// 1..=10 option labels, 32 hex chars each, strictly ascending.
    pub labels_hex: Vec<String>,
    /// The client's clock, unix milliseconds, as bound into the
    /// presentation.
    pub at_ms: u64,
}

/// Body of `DELETE .../votes/:tag_hex`.
#[derive(Deserialize)]
pub struct DeleteVoteRequest {
    /// The credential presentation, base64url without padding.
    pub presentation_b64: String,
    /// The client's clock, unix milliseconds, as bound into the
    /// presentation.
    pub at_ms: u64,
}

/// Answer of a stored put.
#[derive(Serialize)]
pub struct PutVoteResponse {
    /// `true` when the tag had a counted vote before.
    pub replaced: bool,
}

fn bad(code: &'static str, detail: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, Json(err(code, detail))).into_response()
}

fn proof_required() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(err(
            "vote-proof-required",
            "the vote needs a valid credential presentation",
        )),
    )
        .into_response()
}

fn internal(e: &StorageError) -> Response {
    tracing::error!(error = %e, "poll vote store failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(err("internal", "unexpected error")),
    )
        .into_response()
}

fn unknown_post() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(err(
            "unknown-post",
            "the channel holds no post at this cursor",
        )),
    )
        .into_response()
}

fn parse_fixed<const N: usize>(hex_str: &str) -> Option<[u8; N]> {
    hex::decode(hex_str)
        .ok()
        .and_then(|b| <[u8; N]>::try_from(b.as_slice()).ok())
}

/// Parse a put's labels: 1..=10, each 16 bytes, strictly ascending.
fn parse_labels(list: &[String]) -> Option<Vec<Label>> {
    if list.is_empty() || list.len() > MAX_VOTE_LABELS {
        return None;
    }
    let labels: Vec<Label> = list.iter().map(|h| parse_fixed(h)).collect::<Option<_>>()?;
    labels.windows(2).all(|w| w[0] < w[1]).then_some(labels)
}

/// What the routes share once the body parsed.
struct Vote {
    channel: ChannelId,
    post_cursor: u64,
    tag: Tag,
    presentation: Vec<u8>,
    action: VoteAction,
    labels: Vec<Label>,
    at_ms: u64,
}

fn parse_common(
    channel_id: &str,
    tag_hex: &str,
    presentation_b64: &str,
) -> Result<(ChannelId, Tag, Vec<u8>), Box<Response>> {
    let channel = ChannelId::from_hex(channel_id)
        .map_err(|_| Box::new(bad("bad-channel-id", "channel_id must be 32 hex chars")))?;
    let tag =
        parse_fixed(tag_hex).ok_or_else(|| Box::new(bad("bad-tag", "tag must be 64 hex chars")))?;
    let presentation = URL_SAFE_NO_PAD
        .decode(presentation_b64)
        .ok()
        .filter(|p| p.len() == PRESENTATION_LEN)
        .ok_or_else(|| {
            Box::new(bad(
                "bad-presentation",
                "presentation must be base64url of the presentation layout",
            ))
        })?;
    Ok((channel, tag, presentation))
}

/// Steps 2 to 7 of the gate.
async fn run(state: &AppState, vote: Vote) -> Result<VoteOutcome, Response> {
    let Some(verifier) = state.votes.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(err(
                "anon-credential-disabled",
                "channel poll votes are not configured",
            )),
        )
            .into_response());
    };
    let now = unix_ms();
    if vote.at_ms.abs_diff(now) > VOTE_WINDOW_MS {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "code": "vote-time-out-of-window",
                "detail": "at_ms is too far from the service clock",
                "server_time_ms": now,
            })),
        )
            .into_response());
    }
    if vote.action == VoteAction::Put {
        match state
            .store
            .post_exists(vote.channel, vote.post_cursor)
            .await
        {
            Ok(true) => {},
            Ok(false) => return Err(unknown_post()),
            Err(e) => return Err(internal(&e)),
        }
    }
    let Ok(message) = vote_message(vote.action, vote.at_ms, &vote.labels) else {
        return Err(bad("bad-labels", "labels are not canonical"));
    };
    let scope = PollScope::channel_post(*vote.channel.as_bytes(), vote.post_cursor);
    let current = period_at(now / 1000);
    match verifier.verify(&vote.presentation, &scope, &message, current) {
        Ok(v) if v.tag == vote.tag => {},
        Ok(_) => return Err(proof_required()),
        Err(e) => {
            tracing::debug!(code = e.code(), "poll vote presentation refused");
            return Err(proof_required());
        },
    }
    if !state
        .vote_cap
        .admit(vote.channel, vote.post_cursor, vote.tag)
    {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(err(
                "vote-rate-limited",
                "too many vote changes for this poll; try later",
            )),
        )
            .into_response());
    }
    match state
        .store
        .apply_vote(
            vote.channel,
            vote.post_cursor,
            vote.tag,
            vote.labels,
            vote.at_ms,
        )
        .await
    {
        Ok(VoteOutcome::Stale { stored_at_ms }) => Err((
            StatusCode::CONFLICT,
            Json(json!({
                "code": "stale-vote",
                "detail": "a newer vote request for this tag was already accepted",
                "stored_at_ms": stored_at_ms,
            })),
        )
            .into_response()),
        Ok(outcome) => Ok(outcome),
        Err(StorageError::UnknownPost) => Err(unknown_post()),
        Err(e) => Err(internal(&e)),
    }
}

/// `PUT /v1/channels/:id/posts/:cursor/votes/:tag_hex`.
async fn put_vote(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor, tag_hex)): Path<(String, u64, String)>,
    Json(req): Json<PutVoteRequest>,
) -> Response {
    let (channel, tag, presentation) =
        match parse_common(&channel_id, &tag_hex, &req.presentation_b64) {
            Ok(v) => v,
            Err(r) => return *r,
        };
    let Some(labels) = parse_labels(&req.labels_hex) else {
        return bad(
            "bad-labels",
            "labels must be 1 to 10 distinct 32-hex-char labels in ascending order",
        );
    };
    let vote = Vote {
        channel,
        post_cursor,
        tag,
        presentation,
        action: VoteAction::Put,
        labels,
        at_ms: req.at_ms,
    };
    match run(&state, vote).await {
        Ok(VoteOutcome::Stored { replaced: true }) => {
            (StatusCode::OK, Json(PutVoteResponse { replaced: true })).into_response()
        },
        Ok(_) => (
            StatusCode::CREATED,
            Json(PutVoteResponse { replaced: false }),
        )
            .into_response(),
        Err(r) => r,
    }
}

/// `DELETE /v1/channels/:id/posts/:cursor/votes/:tag_hex`.
async fn delete_vote(
    State(state): State<Arc<AppState>>,
    Path((channel_id, post_cursor, tag_hex)): Path<(String, u64, String)>,
    Json(req): Json<DeleteVoteRequest>,
) -> Response {
    let (channel, tag, presentation) =
        match parse_common(&channel_id, &tag_hex, &req.presentation_b64) {
            Ok(v) => v,
            Err(r) => return *r,
        };
    let vote = Vote {
        channel,
        post_cursor,
        tag,
        presentation,
        action: VoteAction::Delete,
        labels: Vec::new(),
        at_ms: req.at_ms,
    };
    match run(&state, vote).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(r) => r,
    }
}

/// The routes of this module, merged into [`crate::api::router`].
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/v1/channels/{channel_id}/posts/{post_cursor}/votes/{tag_hex}",
        put(put_vote).delete(delete_vote),
    )
}
