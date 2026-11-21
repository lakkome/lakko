// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Append-only journal handlers, shared by the per-group, per-device and
//! rendezvous flows.
//!
//! The queues (`ds_group_messages`, `ds_mailbox_messages`,
//! `ds_group_knocks`, `ds_rendezvous_messages`) have identical shape — `(id BIGINT AUTO_INCREMENT PK, owner_id BINARY(32),
//! payload LONGBLOB, created_at TIMESTAMP(3))` — so this module
//! parameterises the SQL by table name and column name. Every handler
//! is just a thin wrapper that picks the right `Queue`.
//!
//! The server does not interpret payloads. It treats them as opaque
//! bytes, assigns a global-monotonic `id` via AUTO_INCREMENT, and
//! returns it as the cursor for the client's next fetch.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::MySqlPool;
use tokio::sync::Notify;

use crate::metrics::{self, Pruned};
use crate::AppState;

/// Pseudonym length: 32 raw bytes = 64 lowercase hex chars.
pub(crate) const PSEUDONYM_LEN: usize = 32;
const HEX_LEN: usize = PSEUDONYM_LEN * 2;

/// Per-message body ceiling. Bigger than typical MLS app messages
/// (KB-range) and large enough for Welcome messages of small/medium
/// groups; large attachments live in `services/storage` and are
/// referenced from MLS application messages by SHA-256 hash.
pub(crate) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Default GET page size if `?limit=` is omitted.
pub(crate) const DEFAULT_LIMIT: u32 = 100;

/// Hard cap on a single GET response.
const MAX_LIMIT: u32 = 1000;

/// Hard cap on `?wait=<secs>` long-poll. Reverse proxies (nginx default,
/// Cloudflare Free) cut idle HTTP at ~100s; 60s is a safe upper bound
/// that also keeps fd-resource use bounded.
const MAX_WAIT_SECS: u32 = 60;

/// While long-polling we periodically re-fetch as a belt-and-suspenders
/// against a missed notify (in-process Notify can lose a race against a
/// reader that's currently in `fetch`). 5s is fine — under load the
/// notify path takes over and this rarely fires.
const LONG_POLL_REFETCH_INTERVAL: Duration = Duration::from_secs(5);

/// Retention window — messages older than this are deleted by the
/// pruner. A device offline longer than this loses old messages.
const RETENTION_SECS: i64 = 30 * 24 * 60 * 60;

/// Period between pruner runs. Hourly is plenty given the 30-day TTL.
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

/// One of the two journals. Pinned to a `Queue` so the SQL templates
/// stay parameterised on the *exact* identifier names — avoids any
/// chance of bind-string SQL injection (we never substitute the
/// `owner_col` from user input).
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub(crate) enum Queue {
    /// `ds_group_messages`, `(group_id, ...)`.
    Group,
    /// `ds_mailbox_messages`, `(device_id, ...)`.
    Mailbox,
    /// `ds_group_knocks`, `(group_id, ...)` - ADR-0053 section 4, the
    /// non-member's requests to join, kept apart from the members' rows.
    Knock,
    /// `ds_rendezvous_messages`, `(rendezvous_id, ...)` - ADR-0060: the
    /// pairing / recovery / courier meeting points, read and written without
    /// a proof, in their own table so a rendezvous id can never name a
    /// personal mailbox's rows.
    Rendezvous,
}

impl Queue {
    fn table(self) -> &'static str {
        match self {
            Queue::Group => "ds_group_messages",
            Queue::Mailbox => "ds_mailbox_messages",
            Queue::Knock => "ds_group_knocks",
            Queue::Rendezvous => "ds_rendezvous_messages",
        }
    }

    fn owner_col(self) -> &'static str {
        match self {
            Queue::Group | Queue::Knock => "group_id",
            Queue::Mailbox => "device_id",
            Queue::Rendezvous => "rendezvous_id",
        }
    }
}

// ============================================================================
// Long-poll fanout: in-process Notify per (queue, owner_id)
// ============================================================================

type NotifyKey = (Queue, [u8; PSEUDONYM_LEN]);

/// Concurrent map of per-pseudonym notifiers. Cheap clone: it's just an
/// `Arc<Mutex<HashMap<...>>>`. Insert-on-first-access; entries persist
/// for the process lifetime — bounded by the count of active groups
/// times two journals. TTL-based eviction is Phase 2 work.
#[derive(Clone, Default)]
pub(crate) struct Notifiers {
    inner: Arc<Mutex<HashMap<NotifyKey, Arc<Notify>>>>,
}

impl Notifiers {
    /// Get-or-insert the `Notify` handle for a key. The `Mutex` guard
    /// is held only for the HashMap lookup — we clone the `Arc<Notify>`
    /// out before any awaiting.
    fn get_or_create(&self, key: NotifyKey) -> Arc<Notify> {
        // A poisoned lock means a panic while holding it; the map holds only
        // wake-up handles, so carry on with it.
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .entry(key)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    /// Wake every task currently registered on the matching key. New
    /// tasks that subscribe after this call do **not** see the event —
    /// they must double-check via a fresh fetch first (see
    /// `fetch_with_wait`'s loop).
    pub(crate) fn wake(&self, key: NotifyKey) {
        let arc = self.get_or_create(key);
        arc.notify_waiters();
    }
}

// ============================================================================
// Path validation
// ============================================================================

/// Validate that the path parameter is exactly 64 lowercase hex chars
/// and decode to 32 raw bytes.
pub(crate) fn decode_pseudonym(hex_str: &str) -> Result<[u8; PSEUDONYM_LEN], (StatusCode, String)> {
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
/// `RequestBodyLimit` layer), inserts into the right table, wakes up
/// any in-flight long-poll GETs on the same `(queue, owner_id)` key,
/// and returns the assigned id.
pub(crate) async fn publish(
    pool: &MySqlPool,
    notifiers: &Notifiers,
    queue: Queue,
    owner_id: &[u8; PSEUDONYM_LEN],
    bytes: &[u8],
) -> Result<Json<PublishResponse>, (StatusCode, String)> {
    publish_admitted(pool, notifiers, queue, owner_id, bytes, None).await
}

/// [`publish`], recording on a mailbox row the reach key that admitted it
/// (ADR-0060 section 2, amended 2026-11-06). `reach_pk` must be `None` for
/// any other queue.
pub(crate) async fn publish_admitted(
    pool: &MySqlPool,
    notifiers: &Notifiers,
    queue: Queue,
    owner_id: &[u8; PSEUDONYM_LEN],
    bytes: &[u8],
    reach_pk: Option<&[u8; 32]>,
) -> Result<Json<PublishResponse>, (StatusCode, String)> {
    // The body arrives collected: the DefaultBodyLimit layer already
    // enforces MAX_MESSAGE_BYTES, so a client trying to sneak a 4 MB blob
    // in gets 413 before reaching this code, and the ADR-0053 gate has to
    // see the bytes before they are stored.

    // Build SQL with the table + column literally substituted —
    // these are compile-time `&'static str`, never user input, so no
    // injection vector.
    let result = if let (Queue::Mailbox, Some(pk)) = (queue, reach_pk) {
        sqlx::query(
            "INSERT INTO ds_mailbox_messages (device_id, payload, reach_pk) VALUES (?, ?, ?)",
        )
        .bind(&owner_id[..])
        .bind(bytes)
        .bind(&pk[..])
        .execute(pool)
        .await
    } else {
        let sql = format!(
            "INSERT INTO {} ({}, payload) VALUES (?, ?)",
            queue.table(),
            queue.owner_col(),
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(&owner_id[..])
            .bind(bytes)
            .execute(pool)
            .await
    };
    let result = result.map_err(|e| {
        tracing::error!(queue = ?queue, error = %e, "insert message");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;
    let id = result.last_insert_id();
    metrics::appended(queue);

    // Fetch created_at — cheap, lets the client display "delivered at".
    let sql = format!("SELECT created_at FROM {} WHERE id = ?", queue.table());
    let row: (DateTime<Utc>,) = sqlx::query_as(sqlx::AssertSqlSafe(sql))
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

    // Wake any long-poll waiters on this (queue, owner_id) before
    // returning to the publisher. The wake happens AFTER the row is
    // visible (committed by execute() above), so a notified reader
    // doing a fresh fetch will see the new row.
    notifiers.wake((queue, *owner_id));

    Ok(Json(PublishResponse {
        id,
        created_at: row.0,
    }))
}

/// Insert one row into `queue` on `conn` (inside a caller's transaction) and
/// return its id. The caller wakes the long-polls once its transaction has
/// committed (ADR-0081: the commit route appends inside the registration's
/// transaction).
pub(crate) async fn insert_row(
    conn: &mut sqlx::MySqlConnection,
    queue: Queue,
    owner_id: &[u8; PSEUDONYM_LEN],
    bytes: &[u8],
) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "INSERT INTO {} ({}, payload) VALUES (?, ?)",
        queue.table(),
        queue.owner_col(),
    );
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&owner_id[..])
        .bind(bytes)
        .execute(&mut *conn)
        .await?;
    Ok(result.last_insert_id())
}

/// Answer a mailbox publish exactly as [`publish`] would, and store nothing
/// (ADR-0060 section 2: a proofless write to a closed mailbox).
///
/// The response must not tell the writer whether an account is behind the
/// pseudonym, so it is not a made-up value: the row is inserted inside a
/// transaction that is then rolled back. InnoDB does not hand an
/// `AUTO_INCREMENT` value back on rollback, so the id is a real one from the
/// same sequence a stored write draws from, and `created_at` is the same
/// server clock. The payload is not written (an empty one is), so a dropped
/// megabyte costs no undo log.
pub(crate) async fn publish_dropped(
    pool: &MySqlPool,
    owner_id: &[u8; PSEUDONYM_LEN],
) -> Result<Json<PublishResponse>, (StatusCode, String)> {
    let db = |e: sqlx::Error| {
        tracing::error!(error = %e, "dropped mailbox publish");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    };
    let mut tx = pool.begin().await.map_err(db)?;
    let result = sqlx::query("INSERT INTO ds_mailbox_messages (device_id, payload) VALUES (?, ?)")
        .bind(&owner_id[..])
        .bind(&[][..])
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    let id = result.last_insert_id();
    let row: (DateTime<Utc>,) =
        sqlx::query_as("SELECT created_at FROM ds_mailbox_messages WHERE id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
    tx.rollback().await.map_err(db)?;
    metrics::mailbox_write_dropped();
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
    /// Long-poll: if zero or absent, return whatever's there
    /// immediately. If non-zero, hold the connection for up to that
    /// many seconds (clamped to `MAX_WAIT_SECS`) waiting for new
    /// messages. Returns as soon as there's at least one row past
    /// `after`, or empty on timeout.
    #[serde(default)]
    pub(crate) wait: Option<u32>,
}

#[derive(Serialize)]
pub(crate) struct FetchResponse {
    pub(crate) messages: Vec<FetchedMessage>,
    /// `true` if there might be more rows past the last returned id —
    /// client should call again with `after = last_id`.
    pub(crate) has_more: bool,
    /// For a signed read of a claimed group journal: the newest epoch
    /// registered for it. Absent for an unclaimed group and a mailbox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latest_epoch: Option<u64>,
    /// For a signed read of a claimed group journal (ADR-0091): every
    /// registration above the epoch the read was signed at that carries a
    /// commit row (ADR-0081), as `(epoch, id)`: row `id` is THE commit that
    /// moves the journal from `epoch - 1` to `epoch`. Ascending by epoch, at
    /// most [`crate::access::MAX_RETAINED_EPOCHS`] entries. Read after the
    /// rows, so a commit row in `messages` always has its entry (a
    /// registration and its row commit in one transaction). Absent where
    /// `latest_epoch` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) commits: Option<Vec<RegisteredCommit>>,
}

/// One entry of [`FetchResponse::commits`].
#[derive(Serialize)]
pub(crate) struct RegisteredCommit {
    /// The epoch the row registers (the epoch after the commit).
    pub(crate) epoch: u64,
    /// The journal row id of the commit.
    pub(crate) id: u64,
}

/// The registrations of `owner_id` above `proof_epoch` that carry a commit
/// row (ADR-0091), ascending by epoch.
async fn registered_commits_above(
    pool: &MySqlPool,
    owner_id: &[u8; PSEUDONYM_LEN],
    proof_epoch: u64,
) -> Result<Vec<RegisteredCommit>, (StatusCode, String)> {
    let rows: Vec<(u64, u64)> = sqlx::query_as(
        "SELECT epoch, commit_message_id FROM ds_group_access \
         WHERE group_id = ? AND epoch > ? AND commit_message_id IS NOT NULL \
         ORDER BY epoch ASC LIMIT ?",
    )
    .bind(&owner_id[..])
    .bind(proof_epoch)
    .bind(crate::access::MAX_RETAINED_EPOCHS)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "fetch registered commits");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;
    Ok(rows
        .into_iter()
        .map(|(epoch, id)| RegisteredCommit { epoch, id })
        .collect())
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
    /// Mailbox rows only: standard base64 of the reach key that admitted the
    /// row (the key a direct proof named, or a delegation's issuer), so the
    /// owner can tell a row under a retired `contact` key (ADR-0060 section
    /// 2, amended 2026-11-06). Absent for a proofless write and for every
    /// other queue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reach_pk_b64: Option<String>,
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

    // The admitting key is a mailbox column; other queues select NULL so the
    // row shape is one.
    let reach_col = if queue == Queue::Mailbox {
        "reach_pk"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT id, payload, created_at, {} FROM {} \
         WHERE {} = ? AND id > ? \
         ORDER BY id ASC \
         LIMIT ?",
        reach_col,
        queue.table(),
        queue.owner_col(),
    );

    #[allow(clippy::type_complexity)]
    let rows: Vec<(u64, Vec<u8>, DateTime<Utc>, Option<Vec<u8>>)> =
        sqlx::query_as(sqlx::AssertSqlSafe(sql))
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
        .map(|(id, payload, created_at, reach_pk)| FetchedMessage {
            id,
            payload_b64: B64.encode(&payload),
            created_at,
            reach_pk_b64: reach_pk.map(|pk| B64.encode(pk)),
        })
        .collect();

    Ok(Json(FetchResponse {
        messages,
        has_more,
        latest_epoch: None,
        commits: None,
    }))
}

/// Fetch with optional long-poll. If `wait_secs` is zero we behave like
/// `fetch` directly. Otherwise:
///
///   1. Try `fetch` once — if it returns rows, ship them.
///   2. Subscribe to the per-`(queue, owner_id)` `Notify` BEFORE the
///      next fetch — this is the standard double-check pattern that
///      avoids missing a notify that fires between the SELECT and the
///      `notified()` registration.
///   3. `select!` between (a) the notify firing, (b) a periodic
///      re-fetch tick (defence-in-depth), and (c) the deadline.
///   4. On any wakeup do a fresh `fetch`; ship rows if any. On
///      deadline, ship the empty response.
pub(crate) async fn fetch_with_wait(
    pool: &MySqlPool,
    notifiers: &Notifiers,
    queue: Queue,
    owner_id: &[u8; PSEUDONYM_LEN],
    after: u64,
    limit: u32,
    wait_secs: u32,
) -> Result<Json<FetchResponse>, (StatusCode, String)> {
    // Fast path — no long-poll requested, or rows are already there.
    let initial = fetch(pool, queue, owner_id, after, limit).await?;
    if wait_secs == 0 || !initial.messages.is_empty() {
        return Ok(initial);
    }

    let wait_secs = wait_secs.min(MAX_WAIT_SECS);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(u64::from(wait_secs));
    let notify = notifiers.get_or_create((queue, *owner_id));
    // Counted as open until this function returns, or the request is
    // dropped mid-wait (ADR-0096).
    let _open = metrics::long_poll_open(queue);

    loop {
        // Register on the notifier BEFORE we re-check — `notified()` is
        // edge-triggered, so this guarantees no notification fires
        // between the check below and our subscription.
        let notified = notify.notified();
        tokio::pin!(notified);

        // Double-check: a publish could have landed between the
        // top-of-function fetch and our notify subscription. Cheap
        // re-query covers that race.
        let resp = fetch(pool, queue, owner_id, after, limit).await?;
        if !resp.messages.is_empty() {
            return Ok(resp);
        }

        // Wait for one of: notify fires, periodic tick (belt-and-
        // suspenders against a missed notify), or deadline.
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(resp);
        }
        let until_deadline = deadline - now;
        let sleep_for = until_deadline.min(LONG_POLL_REFETCH_INTERVAL);

        tokio::select! {
            () = &mut notified => {},
            () = tokio::time::sleep(sleep_for) => {},
        }
        // Loop iterates: another fetch + register-then-check.
    }
}

// ============================================================================
// Public route handlers
// ============================================================================

pub(crate) async fn post_group(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&group_hex)?;
    // ADR-0054: a malformed wake header refuses the publish before anything
    // is checked or stored.
    let wake = crate::wake::parse_wake_header(&headers)?;
    // ADR-0053: a claimed group takes a publish only from a member.
    let gated = crate::access::gate_group(
        &state,
        &owner,
        crate::access::Access::Write,
        &method,
        &uri,
        &headers,
        &body,
    )
    .await?;
    let resp = publish(&state.db, &state.notifiers, Queue::Group, &owner, &body).await?;
    // ADR-0054: the wake follows the append, and only a proven writer of a
    // claimed journal gets one. Fire-and-forget: it cannot delay or fail
    // this response. ADR-0078: the wake is sealed to push-gw; the DS forwards
    // it unread and binds it to nothing (push-gw checks the wake-key
    // signature only members can make).
    if let Some(wake) = wake {
        if matches!(gated, crate::access::Gated::Proven { .. }) {
            state.waker.fire(&owner, wake, resp.id);
        } else {
            metrics::wake(metrics::WakeOutcome::Unclaimed);
            tracing::debug!(outcome = "unclaimed", "wake dropped");
        }
    }
    Ok((StatusCode::CREATED, resp).into_response())
}

pub(crate) async fn get_group(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<FetchQuery>,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&group_hex)?;
    // ADR-0053: a claimed group's journal is read by members only.
    let gated = crate::access::gate_group(
        &state,
        &owner,
        crate::access::Access::Read,
        &method,
        &uri,
        &headers,
        &[],
    )
    .await?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    let wait = q.wait.unwrap_or(0);
    let Json(mut resp) = fetch_with_wait(
        &state.db,
        &state.notifiers,
        Queue::Group,
        &owner,
        q.after,
        limit,
        wait,
    )
    .await?;
    if let crate::access::Gated::Proven {
        latest_epoch,
        proof_epoch,
    } = gated
    {
        resp.latest_epoch = Some(latest_epoch);
        // ADR-0091: after the rows, so every commit row returned has its
        // entry.
        resp.commits = Some(registered_commits_above(&state.db, &owner, proof_epoch).await?);
    }
    let mut response = Json(resp).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn post_mailbox(
    State(state): State<AppState>,
    AxumPath(device_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&device_hex)?;
    // ADR-0054: only a group journal wakes; a mailbox publish that asks for
    // one is refused rather than silently ignored.
    if headers.contains_key(crate::wake::WAKE_HEADER) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "{} is accepted on a group journal only",
                crate::wake::WAKE_HEADER
            ),
        ));
    }
    // ADR-0078 section 1: every mailbox row is a sealed envelope padded to a
    // size bucket. Checked first, before any state is read, so a malformed
    // row is refused alike whatever the mailbox's state and never costs a
    // token.
    if !lakko_ds_proof::is_well_formed_mailbox_row(&body) {
        return Err((
            StatusCode::BAD_REQUEST,
            "a mailbox row must be a sealed, bucket-padded envelope".to_string(),
        ));
    }
    // ADR-0060 section 2: a claimed, closed mailbox takes a write only under
    // a reach key (or a delegation from one). An open mailbox (Everyone) and
    // an unclaimed one take a proofless write that spends one `mailbox-write`
    // anonymous token (ADR-0078). A proofless write to a closed mailbox
    // spends one too and is answered as if stored and dropped; a proof that
    // fails is the uniform 401 whatever the mailbox's state.
    let resp =
        match crate::reach::gate_mailbox_write(&state, &owner, &method, &uri, &headers, &body)
            .await?
        {
            crate::reach::MailboxWrite::Store { reach_pk } => {
                publish_admitted(
                    &state.db,
                    &state.notifiers,
                    Queue::Mailbox,
                    &owner,
                    &body,
                    Some(&reach_pk),
                )
                .await?
            },
            decision @ (crate::reach::MailboxWrite::StoreAfterToken
            | crate::reach::MailboxWrite::DropAfterToken) => {
                // Spent for a closed mailbox too, so whether the token came
                // back unspent never tells a closed mailbox from another.
                if let Err(refused) = state
                    .anon_tokens
                    .spend_header(&headers, lakko_anon_token::TokenContext::MailboxWrite)
                    .await
                {
                    return Ok(refused.into_response());
                }
                if decision == crate::reach::MailboxWrite::StoreAfterToken {
                    publish(&state.db, &state.notifiers, Queue::Mailbox, &owner, &body).await?
                } else {
                    publish_dropped(&state.db, &owner).await?
                }
            },
        };
    Ok((StatusCode::CREATED, resp).into_response())
}

pub(crate) async fn get_mailbox(
    State(state): State<AppState>,
    AxumPath(device_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<FetchQuery>,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&device_hex)?;
    // ADR-0053 + ADR-0060: a mailbox is personal and read by its owner only.
    // An unclaimed mailbox has no owner key, so it answers the same 401 as a
    // claimed one read without (or with a failing) proof: the answer never
    // says whether a mailbox is claimed. Proofless meeting points are the
    // rendezvous routes below.
    crate::access::gate_mailbox_read(&state, &owner, &method, &uri, &headers).await?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    let wait = q.wait.unwrap_or(0);
    let resp = fetch_with_wait(
        &state.db,
        &state.notifiers,
        Queue::Mailbox,
        &owner,
        q.after,
        limit,
        wait,
    )
    .await?;
    let mut response = resp.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

// ============================================================================
// Rendezvous (ADR-0060)
// ============================================================================

/// The headers a rendezvous request must not carry: it is never claimable,
/// so a proof or a delegation would gate nothing, and a client sending one
/// has confused a rendezvous with a personal mailbox. Refused (400) rather
/// than ignored, as the wake header is on a mailbox.
fn refuse_rendezvous_headers(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    for name in [
        lakko_ds_proof::PROOF_HEADER_NAME,
        lakko_ds_proof::DELEGATION_HEADER_NAME,
        crate::wake::WAKE_HEADER,
    ] {
        if headers.contains_key(name) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("{name} is not accepted on a rendezvous"),
            ));
        }
    }
    Ok(())
}

/// `POST /ds/rendezvous/{hex}/messages` - append to a rendezvous (pairing,
/// recovery, the welcome courier, the state bundle, a rejoin note, a gap
/// history). No proof and no claim: whoever knows the random id may write,
/// but every write spends one `rendezvous-write` anonymous token
/// (security review 2026-11-21), so a name alone no longer writes without
/// limit and the DS still learns no account. Checked after the id and the
/// headers, so a malformed request costs no token.
pub(crate) async fn post_rendezvous(
    State(state): State<AppState>,
    AxumPath(rendezvous_hex): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&rendezvous_hex)?;
    refuse_rendezvous_headers(&headers)?;
    if let Err(refused) = state
        .anon_tokens
        .spend_header(&headers, lakko_anon_token::TokenContext::RendezvousWrite)
        .await
    {
        return Ok(refused.into_response());
    }
    let resp = publish(
        &state.db,
        &state.notifiers,
        Queue::Rendezvous,
        &owner,
        &body,
    )
    .await?;
    Ok((StatusCode::CREATED, resp).into_response())
}

/// `GET /ds/rendezvous/{hex}/messages` - `?after=<id>&limit=<n>&wait=<secs>`,
/// the mailbox semantics, no proof.
pub(crate) async fn get_rendezvous(
    State(state): State<AppState>,
    AxumPath(rendezvous_hex): AxumPath<String>,
    headers: HeaderMap,
    Query(q): Query<FetchQuery>,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&rendezvous_hex)?;
    refuse_rendezvous_headers(&headers)?;
    let resp = fetch_with_wait(
        &state.db,
        &state.notifiers,
        Queue::Rendezvous,
        &owner,
        q.after,
        q.limit.unwrap_or(DEFAULT_LIMIT),
        q.wait.unwrap_or(0),
    )
    .await?;
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
/// `RETENTION_SECS` from the group, mailbox and rendezvous journals, knocks older than
/// `KNOCK_RETENTION_SECS`, and spent proof nonces. Aborted when the service
/// stops.
///
/// Each pass is one run of the job [`metrics::JOB_PRUNER`] (ADR-0096): a
/// success when every statement in it succeeded, a failure otherwise (the
/// statements that worked still deleted their rows).
pub(crate) fn spawn_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    lakko_metrics::job_registered(metrics::JOB_PRUNER);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        // Skip the immediate first tick.
        interval.tick().await;
        loop {
            interval.tick().await;
            if prune_once(&pool).await {
                lakko_metrics::job_succeeded(metrics::JOB_PRUNER);
            } else {
                lakko_metrics::job_failed(metrics::JOB_PRUNER);
            }
        }
    })
}

/// One pruner pass. `true` when every statement succeeded.
pub(crate) async fn prune_once(pool: &MySqlPool) -> bool {
    let mut ok = crate::access::prune_nonces(pool).await;
    ok &= crate::reach::prune_expired(pool).await;
    ok &= crate::access::prune_group_access(pool, RETENTION_SECS).await;
    for queue in [
        Queue::Group,
        Queue::Mailbox,
        Queue::Knock,
        Queue::Rendezvous,
    ] {
        let retention = match queue {
            Queue::Knock => crate::access::KNOCK_RETENTION_SECS,
            Queue::Group | Queue::Mailbox | Queue::Rendezvous => RETENTION_SECS,
        };
        let sql = format!(
            "DELETE FROM {} WHERE created_at < NOW(3) - INTERVAL ? SECOND",
            queue.table(),
        );
        match sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(retention)
            .execute(pool)
            .await
        {
            Ok(r) => {
                metrics::pruned(Pruned::of_queue(queue), r.rows_affected());
                if r.rows_affected() > 0 {
                    tracing::debug!(
                        queue = ?queue,
                        rows = r.rows_affected(),
                        "pruned expired messages"
                    );
                }
            },
            Err(e) => {
                ok = false;
                tracing::warn!(queue = ?queue, error = %e, "prune failed");
            },
        }
    }
    ok
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
        assert_eq!(Queue::Knock.table(), "ds_group_knocks");
        assert_eq!(Queue::Knock.owner_col(), "group_id");
        assert_eq!(Queue::Rendezvous.table(), "ds_rendezvous_messages");
        assert_eq!(Queue::Rendezvous.owner_col(), "rendezvous_id");
    }

    #[test]
    fn notifiers_share_handle_for_same_key() {
        let n = Notifiers::default();
        let key: NotifyKey = (Queue::Group, [0xAB; PSEUDONYM_LEN]);
        let a = n.get_or_create(key);
        let b = n.get_or_create(key);
        assert!(Arc::ptr_eq(&a, &b), "same key must yield same Notify");
    }

    #[test]
    fn notifiers_separate_handles_per_queue() {
        let n = Notifiers::default();
        let owner = [0u8; PSEUDONYM_LEN];
        let g = n.get_or_create((Queue::Group, owner));
        let m = n.get_or_create((Queue::Mailbox, owner));
        assert!(
            !Arc::ptr_eq(&g, &m),
            "Group and Mailbox with same pseudonym must NOT share a notifier",
        );
    }

    #[tokio::test]
    async fn wake_unblocks_a_waiting_task() {
        let n = Notifiers::default();
        let key: NotifyKey = (Queue::Group, [1u8; PSEUDONYM_LEN]);
        let arc = n.get_or_create(key);

        // Spawn a task that registers on `notified()` and waits.
        let h = tokio::spawn(async move {
            arc.notified().await;
        });

        // Give the spawned task a tick to actually subscribe.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Wake — the spawned task should complete promptly.
        n.wake(key);

        tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .expect("wake must unblock notified() within 1 s")
            .expect("spawned task panicked");
    }

    #[tokio::test]
    async fn wake_with_no_waiters_is_noop() {
        let n = Notifiers::default();
        let key: NotifyKey = (Queue::Mailbox, [2u8; PSEUDONYM_LEN]);
        // Should not panic even though nobody is waiting.
        n.wake(key);
    }
}
