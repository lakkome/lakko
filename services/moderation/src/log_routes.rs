// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The public routes of the moderation log (ADR-0089 section 5).
//!
//! No session; behind the per-network limit. Base64 is standard, padded.
//!
//! - `GET /moderation/log/operator-key` -> `{"operator_pubkey_b64"}`
//! - `GET /moderation/log/root` -> `{tree_root_b64, log_size, timestamp_unix,
//!   signature_b64, witness_cosigs_b64: [..]}`, the transparency `RootRes`
//!   shape; the co-signatures are witness v2 under the `moderation` log.
//! - `GET /moderation/log/entry/{index}?size=` -> `{index, leaf_b64,
//!   audit_path_b64: [..], log_size}`, the inclusion of a leaf in the tree of
//!   `size` leaves (default: the current size). Unknown: 404 `no-such-entry`.
//! - `GET /moderation/log/consistency?from=&to=` -> `{proof_b64: [..]}`.
//!   A range outside the log: 400 `bad-range`.
//! - `GET /moderation/snapshot/{period}/bucket/{prefix}` -> the bucket
//!   answer of `lakko_moderation::snapshot::BucketAnswer`: one bucket of the
//!   suspension snapshot of `{period}` (a decimal period, or `latest`), its
//!   path in the bucket tree, the snapshot leaf's path in the current
//!   witnessed head, and that head. `{prefix}` is exactly four hex
//!   characters. Errors: 400 `bad-period`, 400 `bad-prefix`, 404
//!   `no-snapshot` when this instance holds no snapshot of that period.
//!   `Cache-Control: public, max-age=300`.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_moderation::snapshot::{parse_prefix, BucketAnswer};
use serde::Deserialize;
use serde_json::json;

use crate::log::LogError;
use crate::{error, AppState};

/// `GET /moderation/log/operator-key`.
pub async fn operator_key(State(state): State<AppState>) -> Response {
    Json(json!({ "operator_pubkey_b64": B64.encode(state.log.operator_public()) })).into_response()
}

/// `GET /moderation/log/root`.
pub async fn root(State(state): State<AppState>) -> Response {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (root, cosigs) = state.log.root(now).await;
    Json(json!({
        "tree_root_b64": B64.encode(root.tree_root),
        "log_size": root.log_size,
        "timestamp_unix": root.timestamp_unix,
        "signature_b64": B64.encode(root.signature.to_bytes()),
        "witness_cosigs_b64": cosigs.iter().map(|c| B64.encode(c)).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// Query of the entry route.
#[derive(Debug, Deserialize)]
pub struct EntryQuery {
    size: Option<u64>,
}

/// `GET /moderation/log/entry/{index}`.
pub async fn entry(
    State(state): State<AppState>,
    Path(index): Path<u64>,
    Query(q): Query<EntryQuery>,
) -> Response {
    match state.log.entry(index, q.size).await {
        Ok(e) => Json(json!({
            "index": index,
            "leaf_b64": B64.encode(&e.leaf),
            "audit_path_b64": e.audit_path.iter().map(|h| B64.encode(h)).collect::<Vec<_>>(),
            "log_size": e.log_size,
        }))
        .into_response(),
        Err(LogError::OutOfRange) => error(StatusCode::NOT_FOUND, "no-such-entry"),
        Err(e) => {
            tracing::error!(error = %e, "moderation log entry failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        },
    }
}

/// Query of the consistency route.
#[derive(Debug, Deserialize)]
pub struct ConsistencyQuery {
    from: u64,
    to: u64,
}

/// `GET /moderation/log/consistency`.
pub async fn consistency(
    State(state): State<AppState>,
    Query(q): Query<ConsistencyQuery>,
) -> Response {
    match state.log.consistency(q.from, q.to).await {
        Ok(proof) => Json(json!({
            "proof_b64": proof.iter().map(|h| B64.encode(h)).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(LogError::OutOfRange) => error(StatusCode::BAD_REQUEST, "bad-range"),
        Err(e) => {
            tracing::error!(error = %e, "moderation log consistency failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        },
    }
}

/// How long a snapshot answer may be cached. Chosen, not measured.
pub const SNAPSHOT_CACHE_CONTROL: &str = "public, max-age=300";

/// The `{period}` of the snapshot route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeriodQuery {
    Latest,
    Period(u64),
}

fn parse_period(text: &str) -> Option<PeriodQuery> {
    if text == "latest" {
        return Some(PeriodQuery::Latest);
    }
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().map(PeriodQuery::Period)
}

/// `GET /moderation/snapshot/{period}/bucket/{prefix}`.
pub async fn snapshot_bucket(
    State(state): State<AppState>,
    Path((period, prefix)): Path<(String, String)>,
) -> Response {
    let Some(period) = parse_period(&period) else {
        return error(StatusCode::BAD_REQUEST, "bad-period");
    };
    let Some(prefix) = parse_prefix(&prefix) else {
        return error(StatusCode::BAD_REQUEST, "bad-prefix");
    };
    let wanted = match period {
        PeriodQuery::Latest => None,
        PeriodQuery::Period(p) => Some(p),
    };
    let Some(found) = state.snapshots.get(wanted).await else {
        return error(StatusCode::NOT_FOUND, "no-snapshot");
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (head, cosigs) = state.log.root(now).await;
    let entry = match state.log.entry(found.index, Some(head.log_size)).await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "moderation snapshot answer failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "internal");
        },
    };
    let answer = BucketAnswer::new(
        &found.snapshot,
        prefix,
        found.index,
        &entry.audit_path,
        &head,
        &cosigs,
    );
    (
        [(header::CACHE_CONTROL, SNAPSHOT_CACHE_CONTROL)],
        Json(answer),
    )
        .into_response()
}
