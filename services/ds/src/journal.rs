// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Append-only journal handlers, shared by the per-group and per-device
//! flows.
//!
//! The two queues (`ds_group_messages`, `ds_mailbox_messages`) have
//! identical shape — `(id BIGINT AUTO_INCREMENT PK, owner_id BINARY(32),
//! payload LONGBLOB, created_at TIMESTAMP(3))` — so this module
//! parameterises the SQL by table name and column name. Every handler
//! is just a thin wrapper that picks the right `Queue`.
//!
//! The server does not interpret payloads. It treats them as opaque
//! bytes, assigns a global-monotonic `id` via AUTO_INCREMENT, and
//! returns it as the cursor for the client's next fetch.

use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use http_body_util::BodyExt as _;
use serde::{Deserialize, Serialize};
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::MySqlPool;

use crate::AppState;

/// Pseudonym length: 32 raw bytes = 64 lowercase hex chars.
const PSEUDONYM_LEN: usize = 32;
const HEX_LEN: usize = PSEUDONYM_LEN * 2;

/// Per-message body ceiling. Bigger than typical MLS app messages
/// (KB-range) and large enough for Welcome messages of small/medium
/// groups; large attachments live in `services/storage` and are
/// referenced from MLS application messages by SHA-256 hash.
pub(crate) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Default GET page size if `?limit=` is omitted.
const DEFAULT_LIMIT: u32 = 100;

/// Hard cap on a single GET response.
const MAX_LIMIT: u32 = 1000;

/// Retention window — messages older than this are deleted by the
/// pruner. A device offline longer than this loses old messages.
const RETENTION_SECS: i64 = 30 * 24 * 60 * 60;

/// Period between pruner runs. Hourly is plenty given the 30-day TTL.
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

/// One of the two journals. Pinned to a `Queue` so the SQL templates
/// stay parameterised on the *exact* identifier names — avoids any
/// chance of bind-string SQL injection (we never substitute the
/// `owner_col` from user input).
#[derive(Copy, Clone, Debug)]
pub(crate) enum Queue {
    /// `ds_group_messages`, `(group_id, ...)`.
    Group,
    /// `ds_mailbox_messages`, `(device_id, ...)`.
    Mailbox,
}

impl Queue {
    fn table(self) -> &'static str {
        match self {
            Queue::Group => "ds_group_messages",
            Queue::Mailbox => "ds_mailbox_messages",
        }
    }

    fn owner_col(self) -> &'static str {
        match self {
            Queue::Group => "group_id",
            Queue::Mailbox => "device_id",
        }
    }
}

// ============================================================================
// Path validation
// ============================================================================

/// Validate that the path parameter is exactly 64 lowercase hex chars
/// and decode to 32 raw bytes.
fn decode_pseudonym(hex_str: &str) -> Result<[u8; PSEUDONYM_LEN], (StatusCode, String)> {
    if hex_str.len() != HEX_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("pseudonym must be {HEX_LEN} hex chars"),
        ));
    }
    if !hex_str
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "pseudonym must be lowercase hex [0-9a-f]".to_string(),
        ));
    }
    let mut out = [0u8; PSEUDONYM_LEN];
    hex::decode_to_slice(hex_str, &mut out)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("hex decode failed: {e}")))?;
    Ok(out)
}

// ============================================================================
// POST — publish
// ============================================================================

#[derive(Serialize)]
pub(crate) struct PublishResponse {
    /// Server-assigned message id; clients use this as the `after`
    /// cursor on subsequent GETs.
    pub(crate) id: u64,
    pub(crate) created_at: DateTime<Utc>,
}

/// Handler shared by `POST /ds/group/{...}/messages` and `POST
/// /ds/mailbox/{...}/messages`. Reads the request body (capped by the
/// `RequestBodyLimit` layer), inserts into the right table, returns
/// the assigned id.
async fn publish(
    pool: &MySqlPool,
    queue: Queue,
    owner_id: &[u8; PSEUDONYM_LEN],
    body: Body,
) -> Result<Json<PublishResponse>, (StatusCode, String)> {
    // Collect the body. The DefaultBodyLimit + RequestBodyLimit layer
    // already enforces MAX_MESSAGE_BYTES, so a client trying to sneak
    // a 4 MB blob in here gets 413 before reaching this code.
    let bytes = body
        .collect()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "client body collect error");
            (StatusCode::BAD_REQUEST, format!("body error: {e}"))
        })?
        .to_bytes();

    // Build SQL with the table + column literally substituted —
    // these are compile-time `&'static str`, never user input, so no
    // injection vector.
    let sql = format!(
        "INSERT INTO {} ({}, payload) VALUES (?, ?)",
        queue.table(),
        queue.owner_col(),
    );

    let result = sqlx::query(&sql)
        .bind(&owner_id[..])
        .bind(bytes.as_ref())
        .execute(pool)
        .await
        .map_err(|e| {
            tracing::error!(queue = ?queue, error = %e, "insert message");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;
    let id = result.last_insert_id();

    // Fetch created_at — cheap, lets the client display "delivered at".
    let sql = format!("SELECT created_at FROM {} WHERE id = ?", queue.table());
    let row: (DateTime<Utc>,) = sqlx::query_as(&sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|e| {
            tracing::error!(queue = ?queue, error = %e, "fetch created_at");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    Ok(Json(PublishResponse {
        id,
        created_at: row.0,
    }))
}

// ============================================================================
// GET — fetch with cursor
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct FetchQuery {
    /// Inclusive lower bound is `after + 1` — i.e. only return rows
    /// strictly newer than this id. Default 0 = "from the beginning".
    #[serde(default)]
    pub(crate) after: u64,
    /// Page size; clamped to `MAX_LIMIT`.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
}

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    pub(crate) messages: Vec<FetchedMessage>,
    /// `true` if there might be more rows past the last returned id —
    /// client should call again with `after = last_id`.
    pub(crate) has_more: bool,
}

#[derive(Serialize)]
pub(crate) struct FetchedMessage {
    pub(crate) id: u64,
    /// Base64-encoded raw payload. JSON does not transport binary
    /// natively; base64 costs ~33% overhead, which is acceptable for
    /// MLS messages (KB-scale). Phase 1.4 may switch to streaming a
    /// length-prefixed binary format for hot fan-out.
    pub(crate) payload_b64: String,
    pub(crate) created_at: DateTime<Utc>,
}

async fn fetch(
    pool: &MySqlPool,
    queue: Queue,
    owner_id: &[u8; PSEUDONYM_LEN],
    after: u64,
    limit: u32,
) -> Result<Json<FetchResponse>, (StatusCode, String)> {
    let limit = limit.min(MAX_LIMIT);
    let limit = if limit == 0 { DEFAULT_LIMIT } else { limit };

    // Fetch one extra row so we can tell the client whether there are
    // more rows past this page. We don't return the extra row.
    let probe_limit = u64::from(limit) + 1;

    let sql = format!(
        "SELECT id, payload, created_at FROM {} \
         WHERE {} = ? AND id > ? \
         ORDER BY id ASC \
         LIMIT ?",
        queue.table(),
        queue.owner_col(),
    );

    let rows: Vec<(u64, Vec<u8>, DateTime<Utc>)> = sqlx::query_as(&sql)
        .bind(&owner_id[..])
        .bind(after)
        .bind(probe_limit)
        .fetch_all(pool)
        .await
        .map_err(|e| {
            tracing::error!(queue = ?queue, error = %e, "fetch messages");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    let has_more = rows.len() as u64 > u64::from(limit);
    let messages = rows
        .into_iter()
        .take(limit as usize)
        .map(|(id, payload, created_at)| FetchedMessage {
            id,
            payload_b64: B64.encode(&payload),
            created_at,
        })
        .collect();

    Ok(Json(FetchResponse { messages, has_more }))
}

// ============================================================================
// Public route handlers
// ============================================================================

pub(crate) async fn post_group(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    body: Body,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&group_hex)?;
    let resp = publish(&state.db, Queue::Group, &owner, body).await?;
    Ok((StatusCode::CREATED, resp).into_response())
}

pub(crate) async fn get_group(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    Query(q): Query<FetchQuery>,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&group_hex)?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    let resp = fetch(&state.db, Queue::Group, &owner, q.after, limit).await?;
    let mut response = resp.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn post_mailbox(
    State(state): State<AppState>,
    AxumPath(device_hex): AxumPath<String>,
    body: Body,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&device_hex)?;
    let resp = publish(&state.db, Queue::Mailbox, &owner, body).await?;
    Ok((StatusCode::CREATED, resp).into_response())
}

pub(crate) async fn get_mailbox(
    State(state): State<AppState>,
    AxumPath(device_hex): AxumPath<String>,
    Query(q): Query<FetchQuery>,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&device_hex)?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    let resp = fetch(&state.db, Queue::Mailbox, &owner, q.after, limit).await?;
    let mut response = resp.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

// ============================================================================
// Pruner
// ============================================================================

/// Spawn a background task that deletes messages older than
/// `RETENTION_SECS` from both journals. Aborted when the service stops.
pub(crate) fn spawn_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        // Skip the immediate first tick.
        interval.tick().await;
        loop {
            interval.tick().await;
            for queue in [Queue::Group, Queue::Mailbox] {
                let sql = format!(
                    "DELETE FROM {} WHERE created_at < NOW(3) - INTERVAL ? SECOND",
                    queue.table(),
                );
                match sqlx::query(&sql).bind(RETENTION_SECS).execute(&pool).await {
                    Ok(r) if r.rows_affected() > 0 => {
                        tracing::debug!(
                            queue = ?queue,
                            rows = r.rows_affected(),
                            "pruned expired messages"
                        );
                    },
                    Ok(_) => {},
                    Err(e) => tracing::warn!(queue = ?queue, error = %e, "prune failed"),
                }
            }
        }
    })
}

// ============================================================================
// Tests (pure logic — DB-backed paths covered by smoke tests)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_pseudonym_accepts_canonical_lowercase() {
        let s = "0".repeat(64);
        let bytes = decode_pseudonym(&s).unwrap();
        assert_eq!(bytes, [0u8; 32]);

        let s = "ab".repeat(32);
        let bytes = decode_pseudonym(&s).unwrap();
        assert_eq!(bytes, [0xABu8; 32]);
    }

    #[test]
    fn decode_pseudonym_rejects_wrong_length_and_case() {
        assert!(decode_pseudonym("").is_err());
        assert!(decode_pseudonym(&"0".repeat(63)).is_err());
        assert!(decode_pseudonym(&"0".repeat(65)).is_err());
        // Uppercase rejected — single canonical form.
        assert!(decode_pseudonym(&"A".repeat(64)).is_err());
        assert!(decode_pseudonym(&"g".repeat(64)).is_err());
    }

    #[test]
    fn queue_sql_identifiers_are_static() {
        // Belt-and-suspenders: the table / column lookup must return
        // the exact strings the migration expects, since they're
        // substituted into SQL without parameter binding.
        assert_eq!(Queue::Group.table(), "ds_group_messages");
        assert_eq!(Queue::Group.owner_col(), "group_id");
        assert_eq!(Queue::Mailbox.table(), "ds_mailbox_messages");
        assert_eq!(Queue::Mailbox.owner_col(), "device_id");
    }
}
