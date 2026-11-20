// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group and channel `@name`s ([ADR-0087] sections 2 and 3).
//!
//! A group or channel holds its name with a handle key that names no
//! account. Every write is one signed statement
//! (`lakko_contacts::handle::HandleStatement`) in a JSON body, with no
//! session:
//!
//! ```text
//! { "statement_b64": "...", "public_record_b64": "...", "terms_b64": ["..."] }
//! ```
//!
//! | Route | Statement | Token |
//! |---|---|---|
//! | `POST /discovery/handle/claim` | claim | one `discovery-register` |
//! | `PUT /discovery/handle` | update | none |
//! | `POST /discovery/handle/replace` | replace | one `discovery-register` |
//! | `POST /discovery/handle/rekey` | rekey | none |
//! | `DELETE /discovery/handle` | retire | none |
//!
//! ## What is stored
//!
//! One `handles` row per name held or recently held: the name's lookup key,
//! a random `handle_id`, the handle key, the flag, the lease and the
//! retirement time. No account, no group or channel id. A public name also
//! stores one sealed record and its name search terms keyed on the
//! `handle_id` ([`crate::visibility::publish`]); a private one stores
//! nothing else, and `POST /discovery/lookup` answers it exactly as an
//! absent name.
//!
//! ## The checks, in order
//!
//! 1. The body parses; the statement decodes and its kind is the route's
//!    (`400`).
//! 2. The window and the signature (`401 statement-refused`).
//! 3. The content: a public name carries a record and terms, a private one
//!    neither, and the statement's content hash is over exactly what the
//!    body carries (`400`).
//! 4. The nonce is new (`401 statement-refused`): consumed only after the
//!    signature verified, so nobody can burn another key's nonce.
//! 5. The row: held by this key (`401 statement-refused` otherwise, the same
//!    answer as a bad signature), the name free (`409 name-taken`). A claim
//!    of a name this key already holds as its current row is not a conflict:
//!    it answers as the first claim did, with the statement's content and a
//!    renewed lease (a retry of a claim whose answer was lost).
//! 6. Moderation (ADR-0099 step 5, [`crate::moderation`]): a row a moderator
//!    blocked refuses every write (`403 blocked`) and is never freed by the
//!    pruner, so its name stays held; a delisted row refuses a write that
//!    publishes it or moves it to another name (`403 delisted`). A name a
//!    moderator took back holds a `name_claims` row of its own, so a claim
//!    meets `409 name-taken`.
//!
//! ## The lease
//!
//! A handle names no account, so nothing else would free a dead one. A
//! claim, an update and a replace set `lease_until` to [`LEASE_SECS`] from
//! now. A row whose lease passed answers nothing at once; the hourly
//! [`spawn_pruner`] retires it (`retired_at = lease_until`), and deletes a
//! retired row and its claim once it has been retired longer than the hold
//! (`LAKKO_DISCOVERY_RETIRED_NAME_HOLD_SECS`). During the hold the key may
//! take it back with an update.
//!
//! ## Lock order and retries
//!
//! Every transaction here takes its locks in one order: `name_claims`, then
//! `handles`, then `search_terms` and `public_records` (the route writes and
//! [`prune`] alike). Under concurrency MariaDB can still pick a transaction
//! as a deadlock victim (gap and insert-intention locks on the unique
//! indexes are outside any order a statement chooses), so a route that meets
//! a deadlock or a lock-wait timeout runs its transaction again, up to
//! [`LOCK_ATTEMPTS`] times, before answering `500`; the nonce it consumed was
//! rolled back with it.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_attest::HandleCertificate;
use lakko_contacts::handle::{
    handle_content_hash, HandleStatement, HandleStatementBody, HandleVisibility,
    HANDLE_STATEMENT_MAX_LEN, HANDLE_STATEMENT_WINDOW_MS, LOOKUP_KEY_LEN, MAX_HANDLE_TERMS,
};
use lakko_crypto::signing::SigningKey;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;

use crate::moderation::handle_write_refusal;
use crate::registry::CLAIM_HANDLE;
use crate::visibility::{decode_hash, decode_public_record, publish, TARGET_HANDLE};
use crate::AppState;

/// How long a renewal keeps a name: 180 days (ADR-0087 section 3). Chosen,
/// not measured.
pub(crate) const LEASE_SECS: u64 = 180 * 86_400;

/// How often the pruner runs.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// The unique key on a handle key's one current row (migration `0035`), as
/// the database names it in a duplicate-key error.
const CURRENT_WRITE_PK_KEY: &str = "uq_handles_current_write_pk";

const CODE_NAME_TAKEN: &str = "name-taken";
const CODE_REFUSED: &str = "statement-refused";
const CODE_ALREADY_SET: &str = "handle-already-set";
const CODE_NO_HANDLE: &str = "no-such-handle";
const CODE_KEY_IN_USE: &str = "key-in-use";

fn coded(status: StatusCode, code: &'static str) -> Response {
    crate::metrics::with_code(
        (status, Json(serde_json::json!({ "error": code }))).into_response(),
        code,
    )
}

fn bad_request(what: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, what.into()).into_response()
}

fn refused() -> Response {
    coded(StatusCode::UNAUTHORIZED, CODE_REFUSED)
}

/// How many times a transaction that met a deadlock or a lock-wait timeout
/// is run in all. Chosen, not measured.
pub(crate) const LOCK_ATTEMPTS: usize = 3;

/// Marks a `500` that came from a deadlock or a lock-wait timeout, so
/// [`retry_on_lock_conflict`] runs the transaction again.
#[derive(Clone, Copy, Debug)]
struct LockConflict;

fn internal(what: &'static str) -> impl FnOnce(sqlx::Error) -> Response {
    move |e| {
        let conflict = crate::identity_bundle::is_lock_conflict(&e);
        if conflict {
            tracing::warn!(error = %e, what, "handles lock conflict");
        } else {
            tracing::error!(error = %e, what, "handles db error");
        }
        let mut resp = (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
            .into_response();
        if conflict {
            resp.extensions_mut().insert(LockConflict);
        }
        resp
    }
}

/// Run a route's transaction, again when it met a deadlock or a lock-wait
/// timeout (MariaDB rolled it back), at most [`LOCK_ATTEMPTS`] times in all.
/// Any other outcome is returned as it is.
async fn retry_on_lock_conflict<T, F, Fut>(mut attempt: F) -> Result<T, Response>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, Response>>,
{
    let mut tries = 1;
    loop {
        match attempt().await {
            Err(resp)
                if tries < LOCK_ATTEMPTS && resp.extensions().get::<LockConflict>().is_some() =>
            {
                tries += 1;
            },
            other => return other,
        }
    }
}

/// [`retry_on_lock_conflict`] for the pruner's own transactions.
async fn retry_db<T, F, Fut>(mut attempt: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    let mut tries = 1;
    loop {
        match attempt().await {
            Err(e) if tries < LOCK_ATTEMPTS && crate::identity_bundle::is_lock_conflict(&e) => {
                tries += 1;
            },
            other => return other,
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A write's body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // the wire names say every field is base64
pub(crate) struct HandleRequest {
    /// Standard base64 of the signed statement.
    pub(crate) statement_b64: String,
    /// Standard base64 of the public record, for a public claim, update or
    /// replace only.
    #[serde(default)]
    pub(crate) public_record_b64: Option<String>,
    /// Standard base64 of each name search term's lookup key, for a public
    /// name only. At most [`MAX_HANDLE_TERMS`].
    #[serde(default)]
    pub(crate) terms_b64: Vec<String>,
}

/// The answer to a claim, an update, a replace and a rekey.
#[derive(Serialize)]
pub(crate) struct HandleResponse {
    /// Standard base64 of the 169-byte handle certificate
    /// (`lakko_attest::handle_cert`) binding the name to the handle key.
    pub(crate) handle_certificate_b64: String,
    /// When the lease ends, Unix milliseconds.
    pub(crate) lease_until_ms: u64,
}

/// Which statement a route takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Claim,
    Update,
    Replace,
    Rekey,
    Retire,
}

const fn kind_matches(expect: Expect, body: &HandleStatementBody) -> bool {
    matches!(
        (expect, body),
        (Expect::Claim, HandleStatementBody::Claim { .. })
            | (Expect::Update, HandleStatementBody::Update { .. })
            | (Expect::Replace, HandleStatementBody::Replace { .. })
            | (Expect::Rekey, HandleStatementBody::Rekey { .. })
            | (Expect::Retire, HandleStatementBody::Retire)
    )
}

/// What a claim, update or replace publishes.
struct Content {
    visibility: HandleVisibility,
    record: Option<Vec<u8>>,
    terms: Vec<[u8; LOOKUP_KEY_LEN]>,
}

/// Steps 1 to 4: decode, verify, check the content, consume the nonce.
#[allow(clippy::result_large_err)] // the error is the handler's own response
fn decode_statement(req: &HandleRequest, expect: Expect) -> Result<HandleStatement, Response> {
    if req.statement_b64.len() > HANDLE_STATEMENT_MAX_LEN.div_ceil(3) * 4 {
        return Err(bad_request("statement too long"));
    }
    let bytes = B64
        .decode(&req.statement_b64)
        .map_err(|_| bad_request("statement: bad base64"))?;
    let statement = HandleStatement::decode(&bytes)
        .map_err(|e| bad_request(format!("statement: {}", e.code())))?;
    if !kind_matches(expect, &statement.body) {
        return Err(bad_request("statement: wrong kind for this route"));
    }
    statement.verify(now_ms()).map_err(|_| refused())?;
    Ok(statement)
}

/// Step 3 for a statement that publishes.
#[allow(clippy::result_large_err)] // the error is the handler's own response
fn content_of(
    req: &HandleRequest,
    visibility: HandleVisibility,
    content_hash: &[u8; 32],
) -> Result<Content, Response> {
    if req.terms_b64.len() > MAX_HANDLE_TERMS {
        return Err(bad_request(format!("at most {MAX_HANDLE_TERMS} terms")));
    }
    let (record, terms) = match (visibility, req.public_record_b64.as_deref()) {
        (HandleVisibility::Public, Some(b64)) => {
            let record = decode_public_record(b64).map_err(|(s, m)| (s, m).into_response())?;
            let mut terms = Vec::with_capacity(req.terms_b64.len());
            for t in &req.terms_b64 {
                terms.push(decode_hash(t).map_err(|(s, m)| (s, m).into_response())?);
            }
            (Some(record), terms)
        },
        (HandleVisibility::Public, None) => {
            return Err(bad_request("a public name needs a public record"));
        },
        (HandleVisibility::Private, None) if req.terms_b64.is_empty() => (None, Vec::new()),
        (HandleVisibility::Private, _) => {
            return Err(bad_request("a private name publishes nothing"));
        },
    };
    let hash = handle_content_hash(record.as_deref(), &terms)
        .map_err(|e| bad_request(format!("content: {}", e.code())))?;
    if &hash != content_hash {
        return Err(bad_request("content does not match the statement"));
    }
    let mut sorted = terms;
    sorted.sort_unstable();
    sorted.dedup();
    Ok(Content {
        visibility,
        record,
        terms: sorted,
    })
}

/// Step 4: consume the nonce; a seen one is a replay.
async fn consume_nonce(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    statement: &HandleStatement,
) -> Result<(), Response> {
    match sqlx::query("INSERT INTO handle_statement_nonces (nonce) VALUES (?)")
        .bind(&statement.nonce[..])
        .execute(&mut **tx)
        .await
    {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(refused()),
        Err(e) => Err(internal("consume handle nonce")(e)),
    }
}

/// A handle row as the write paths read it.
struct Row {
    handle_id: Vec<u8>,
    write_pk: Vec<u8>,
    retired: bool,
    /// `handles.moderation` (ADR-0099 step 5): open, delisted or blocked.
    moderation: u8,
}

async fn lock_row(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    lookup_key: &[u8],
) -> Result<Option<Row>, Response> {
    let row: Option<(Vec<u8>, Vec<u8>, bool, u8)> = sqlx::query_as(
        "SELECT handle_id, write_pk, retired_at IS NOT NULL, moderation FROM handles \
         WHERE lookup_key = ? FOR UPDATE",
    )
    .bind(lookup_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal("lock handle"))?;
    Ok(row.map(|(handle_id, write_pk, retired, moderation)| Row {
        handle_id,
        write_pk,
        retired,
        moderation,
    }))
}

fn random_handle_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut id);
    id
}

/// Sign the certificate for `(lookup_key, write_pk)` now.
fn certificate(key: &SigningKey, lookup_key: [u8; 64], write_pk: [u8; 32]) -> String {
    let issued = i64::try_from(now_ms()).unwrap_or(i64::MAX);
    B64.encode(HandleCertificate::new(lookup_key, write_pk, issued).sign(key))
}

fn answer(
    state: &AppState,
    lookup_key: [u8; 64],
    write_pk: [u8; 32],
    lease_until_ms: u64,
) -> Json<HandleResponse> {
    Json(HandleResponse {
        handle_certificate_b64: certificate(&state.username_cert_key, lookup_key, write_pk),
        lease_until_ms,
    })
}

fn lease_from_now() -> (u64, i64) {
    let until = now_ms().saturating_add(LEASE_SECS * 1000);
    (until, i64::try_from(LEASE_SECS).unwrap_or(i64::MAX))
}

/// Insert the claim and the row for a new name. `Err` with `409
/// name-taken` when the name is held; `409 handle-already-set` when the
/// key already holds a current name.
async fn insert_name(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    lookup_key: &[u8],
    handle_id: &[u8],
    write_pk: &[u8],
    content: &Content,
    lease_secs: i64,
) -> Result<(), Response> {
    match sqlx::query("INSERT INTO name_claims (lookup_key, kind) VALUES (?, ?)")
        .bind(lookup_key)
        .bind(CLAIM_HANDLE)
        .execute(&mut **tx)
        .await
    {
        Ok(_) => {},
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(coded(StatusCode::CONFLICT, CODE_NAME_TAKEN));
        },
        Err(e) => return Err(internal("claim name")(e)),
    }
    match sqlx::query(
        "INSERT INTO handles (lookup_key, handle_id, write_pk, is_public, lease_until) \
         VALUES (?, ?, ?, ?, NOW(3) + INTERVAL ? SECOND)",
    )
    .bind(lookup_key)
    .bind(handle_id)
    .bind(write_pk)
    .bind(content.visibility.is_public())
    .bind(lease_secs)
    .execute(&mut **tx)
    .await
    {
        Ok(_) => {},
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(if e.message().contains(CURRENT_WRITE_PK_KEY) {
                coded(StatusCode::CONFLICT, CODE_ALREADY_SET)
            } else {
                coded(StatusCode::CONFLICT, CODE_NAME_TAKEN)
            });
        },
        Err(e) => return Err(internal("insert handle")(e)),
    }
    publish(
        tx,
        TARGET_HANDLE,
        handle_id,
        content.record.as_deref(),
        &content.terms,
    )
    .await
    .map_err(internal("publish handle"))
}

/// Make a held row current again with this content, renewing the lease.
async fn revive(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    lookup_key: &[u8],
    handle_id: &[u8],
    content: &Content,
    lease_secs: i64,
) -> Result<(), Response> {
    match sqlx::query(
        "UPDATE handles SET retired_at = NULL, is_public = ?, \
         lease_until = NOW(3) + INTERVAL ? SECOND WHERE lookup_key = ?",
    )
    .bind(content.visibility.is_public())
    .bind(lease_secs)
    .bind(lookup_key)
    .execute(&mut **tx)
    .await
    {
        Ok(_) => {},
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(coded(StatusCode::CONFLICT, CODE_ALREADY_SET));
        },
        Err(e) => return Err(internal("renew handle")(e)),
    }
    publish(
        tx,
        TARGET_HANDLE,
        handle_id,
        content.record.as_deref(),
        &content.terms,
    )
    .await
    .map_err(internal("publish handle"))
}

/// Retire a row: it stops answering at once and keeps its name for the hold.
async fn retire(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    lookup_key: &[u8],
    handle_id: &[u8],
) -> Result<(), Response> {
    sqlx::query(
        "UPDATE handles SET retired_at = NOW(3), is_public = FALSE \
         WHERE lookup_key = ? AND retired_at IS NULL",
    )
    .bind(lookup_key)
    .execute(&mut **tx)
    .await
    .map_err(internal("retire handle"))?;
    publish(tx, TARGET_HANDLE, handle_id, None, &[])
        .await
        .map_err(internal("unpublish handle"))
}

/// A connection whose next transaction runs READ COMMITTED, as
/// `PUT /discovery/visibility` and the username rename do: under the default
/// REPEATABLE READ the deletes and inserts on `search_terms` take gap locks
/// on the `(kind, target)` index, and two names published at once
/// deadlocked on each other's inserts. Uniqueness is held by the unique keys
/// (`name_claims`, `handles`), which do not need gap locks.
async fn read_committed(
    pool: &MySqlPool,
) -> Result<sqlx::pool::PoolConnection<sqlx::MySql>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *conn)
        .await?;
    Ok(conn)
}

async fn connection(state: &AppState) -> Result<sqlx::pool::PoolConnection<sqlx::MySql>, Response> {
    read_committed(&state.db)
        .await
        .map_err(internal("acquire handle connection"))
}

async fn begin(
    conn: &mut sqlx::pool::PoolConnection<sqlx::MySql>,
) -> Result<sqlx::Transaction<'_, sqlx::MySql>, Response> {
    sqlx::Connection::begin(&mut **conn)
        .await
        .map_err(internal("begin handle write"))
}

/// `POST /discovery/handle/claim` — take a name (one `discovery-register`
/// token, spent by the gate). `409 name-taken` when it is held by anyone
/// else, or retired; `409 handle-already-set` when this key holds another
/// current name (a move is `replace`).
///
/// A claim of a name this key already holds as its current row answers as
/// a fresh claim does: the statement's content and flag replace the stored
/// ones and the lease is renewed, exactly as an update would. That is what
/// lets a client that lost the answer to its first claim (a crash, a failed
/// step after the claim) retry with the same handle key. A retired row of
/// this key is not taken back here; that is `PUT /discovery/handle`.
pub(crate) async fn claim(
    State(state): State<AppState>,
    Json(req): Json<HandleRequest>,
) -> Result<Json<HandleResponse>, Response> {
    let statement = decode_statement(&req, Expect::Claim)?;
    let HandleStatementBody::Claim {
        visibility,
        content_hash,
    } = statement.body
    else {
        return Err(bad_request("statement: wrong kind for this route"));
    };
    let content = content_of(&req, visibility, &content_hash)?;
    let (st, stmt, body) = (&state, &statement, &content);
    let (until, reclaimed) = retry_on_lock_conflict(|| async move {
        let mut conn = connection(st).await?;
        let mut tx = begin(&mut conn).await?;
        consume_nonce(&mut tx, stmt).await?;
        let held = held_by_this_key(&mut tx, &stmt.lookup_key, &stmt.write_pk).await?;
        let (until, lease_secs) = lease_from_now();
        match &held {
            Some(handle_id) => {
                revive(&mut tx, &stmt.lookup_key, handle_id, body, lease_secs).await?;
            },
            None => {
                insert_name(
                    &mut tx,
                    &stmt.lookup_key,
                    &random_handle_id(),
                    &stmt.write_pk,
                    body,
                    lease_secs,
                )
                .await?;
            },
        }
        tx.commit().await.map_err(internal("commit claim"))?;
        Ok((until, held.is_some()))
    })
    .await?;
    tracing::info!(
        public = content.visibility.is_public(),
        reclaimed,
        "handle claimed"
    );
    Ok(answer(
        &state,
        statement.lookup_key,
        statement.write_pk,
        until,
    ))
}

/// The `handle_id` of the name at `lookup_key` when it is held by
/// `write_pk` as a current row; `None` when the name is free; `409
/// name-taken` when anyone else holds it, it is a username, or the row is
/// retired. Locks the claim, then the row (the module's lock order).
async fn held_by_this_key(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    lookup_key: &[u8],
    write_pk: &[u8],
) -> Result<Option<Vec<u8>>, Response> {
    let claimed: Option<(u8,)> =
        sqlx::query_as("SELECT kind FROM name_claims WHERE lookup_key = ? FOR UPDATE")
            .bind(lookup_key)
            .fetch_optional(&mut **tx)
            .await
            .map_err(internal("read name claim"))?;
    if claimed.is_none() {
        return Ok(None);
    }
    match (claimed, lock_row(tx, lookup_key).await?) {
        (Some((CLAIM_HANDLE,)), Some(row)) if !row.retired && row.write_pk == write_pk => {
            Ok(Some(row.handle_id))
        },
        _ => Err(coded(StatusCode::CONFLICT, CODE_NAME_TAKEN)),
    }
}

/// `PUT /discovery/handle` — replace the record and terms, set the flag,
/// renew the lease. A row retired within the hold is taken back, unless the
/// key holds another current name (`409 handle-already-set`).
pub(crate) async fn update(
    State(state): State<AppState>,
    Json(req): Json<HandleRequest>,
) -> Result<Json<HandleResponse>, Response> {
    let statement = decode_statement(&req, Expect::Update)?;
    let HandleStatementBody::Update {
        visibility,
        content_hash,
    } = statement.body
    else {
        return Err(bad_request("statement: wrong kind for this route"));
    };
    let content = content_of(&req, visibility, &content_hash)?;
    let (st, stmt, body) = (&state, &statement, &content);
    let (until, revived) = retry_on_lock_conflict(|| async move {
        let mut conn = connection(st).await?;
        let mut tx = begin(&mut conn).await?;
        consume_nonce(&mut tx, stmt).await?;
        let Some(row) = lock_row(&mut tx, &stmt.lookup_key).await? else {
            return Err(coded(StatusCode::NOT_FOUND, CODE_NO_HANDLE));
        };
        if row.write_pk != stmt.write_pk {
            return Err(refused());
        }
        if let Some(refusal) = handle_write_refusal(row.moderation, body.visibility.is_public()) {
            return Err(refusal);
        }
        let (until, lease_secs) = lease_from_now();
        revive(&mut tx, &stmt.lookup_key, &row.handle_id, body, lease_secs).await?;
        tx.commit().await.map_err(internal("commit update"))?;
        Ok((until, row.retired))
    })
    .await?;
    tracing::info!(
        public = content.visibility.is_public(),
        revived,
        "handle updated"
    );
    Ok(answer(
        &state,
        statement.lookup_key,
        statement.write_pk,
        until,
    ))
}

/// `POST /discovery/handle/replace` — move to a new name (one
/// `discovery-register` token). The named row must be this key's current
/// one; the new name must be free, or one of this key's own retired names
/// (taken back). The row left is retired and held; every earlier retired
/// row of this key is freed at once, so a key holds at most one retired
/// name (as a username does, ADR-0076).
pub(crate) async fn replace(
    State(state): State<AppState>,
    Json(req): Json<HandleRequest>,
) -> Result<Json<HandleResponse>, Response> {
    let statement = decode_statement(&req, Expect::Replace)?;
    let HandleStatementBody::Replace {
        new_lookup_key,
        visibility,
        content_hash,
    } = statement.body
    else {
        return Err(bad_request("statement: wrong kind for this route"));
    };
    if new_lookup_key == statement.lookup_key {
        return Err(coded(StatusCode::CONFLICT, CODE_NAME_TAKEN));
    }
    let content = content_of(&req, visibility, &content_hash)?;
    let (st, stmt, body, new_key) = (&state, &statement, &content, &new_lookup_key);
    let until = retry_on_lock_conflict(|| async move {
        let mut conn = connection(st).await?;
        let mut tx = begin(&mut conn).await?;
        consume_nonce(&mut tx, stmt).await?;
        let Some(old) = lock_row(&mut tx, &stmt.lookup_key).await? else {
            return Err(coded(StatusCode::NOT_FOUND, CODE_NO_HANDLE));
        };
        if old.write_pk != stmt.write_pk {
            return Err(refused());
        }
        if old.retired {
            return Err(coded(StatusCode::NOT_FOUND, CODE_NO_HANDLE));
        }
        // A handle under moderation does not move: a new name would carry
        // none of it.
        if let Some(refusal) = handle_write_refusal(old.moderation, true) {
            return Err(refusal);
        }
        // The new name: free, or this key's own retired row.
        let claimed: Option<(u8,)> =
            sqlx::query_as("SELECT kind FROM name_claims WHERE lookup_key = ? FOR UPDATE")
                .bind(&new_key[..])
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal("read new name claim"))?;
        let target = lock_row(&mut tx, new_key).await?;
        let take_back = match (&claimed, &target) {
            (None, None) => None,
            (Some((CLAIM_HANDLE,)), Some(row)) if row.retired && row.write_pk == old.write_pk => {
                if let Some(refusal) =
                    handle_write_refusal(row.moderation, body.visibility.is_public())
                {
                    return Err(refusal);
                }
                Some(row.handle_id.clone())
            },
            _ => return Err(coded(StatusCode::CONFLICT, CODE_NAME_TAKEN)),
        };
        // Free this key's other retired names.
        free_retired_except(&mut tx, &stmt.write_pk, new_key).await?;
        // Retire the row left, before the new one becomes current (one current
        // row per key).
        retire(&mut tx, &stmt.lookup_key, &old.handle_id).await?;
        let (until, lease_secs) = lease_from_now();
        match take_back {
            Some(handle_id) => {
                revive(&mut tx, new_key, &handle_id, body, lease_secs).await?;
            },
            None => {
                insert_name(
                    &mut tx,
                    new_key,
                    &random_handle_id(),
                    &stmt.write_pk,
                    body,
                    lease_secs,
                )
                .await?;
            },
        }
        tx.commit().await.map_err(internal("commit replace"))?;
        Ok(until)
    })
    .await?;
    tracing::info!(public = content.visibility.is_public(), "handle replaced");
    Ok(answer(&state, new_lookup_key, statement.write_pk, until))
}

/// Delete every retired row of `write_pk` but the one at `keep`, and their
/// claims.
async fn free_retired_except(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    write_pk: &[u8],
    keep: &[u8],
) -> Result<(), Response> {
    for sql in [
        "DELETE FROM name_claims WHERE lookup_key IN \
           (SELECT lookup_key FROM handles \
            WHERE write_pk = ? AND retired_at IS NOT NULL AND lookup_key <> ? \
              AND moderation <> 2)",
        "DELETE FROM handles WHERE write_pk = ? AND retired_at IS NOT NULL AND lookup_key <> ? \
           AND moderation <> 2",
    ] {
        sqlx::query(sql)
            .bind(write_pk)
            .bind(keep)
            .execute(&mut **tx)
            .await
            .map_err(internal("free retired handles"))?;
    }
    Ok(())
}

/// `POST /discovery/handle/rekey` — move every row of the signing key
/// (current and retired) to a new key, signed by the old (an ownership
/// transfer, a key rotation). The named row must be this key's.
/// `409 key-in-use` when the new key already holds a row.
pub(crate) async fn rekey(
    State(state): State<AppState>,
    Json(req): Json<HandleRequest>,
) -> Result<Json<HandleResponse>, Response> {
    let statement = decode_statement(&req, Expect::Rekey)?;
    let HandleStatementBody::Rekey { new_write_pk } = statement.body else {
        return Err(bad_request("statement: wrong kind for this route"));
    };
    if req.public_record_b64.is_some() || !req.terms_b64.is_empty() {
        return Err(bad_request("a rekey publishes nothing"));
    }
    let (st, stmt, new_pk) = (&state, &statement, &new_write_pk);
    let lease = retry_on_lock_conflict(|| async move {
    let mut conn = connection(st).await?;
    let mut tx = begin(&mut conn).await?;
    consume_nonce(&mut tx, stmt).await?;
    let Some(row) = lock_row(&mut tx, &stmt.lookup_key).await? else {
        return Err(coded(StatusCode::NOT_FOUND, CODE_NO_HANDLE));
    };
    if row.write_pk != stmt.write_pk {
        return Err(refused());
    }
    if let Some(refusal) = handle_write_refusal(row.moderation, false) {
        return Err(refusal);
    }
    let taken: Option<(i32,)> =
        sqlx::query_as("SELECT 1 FROM handles WHERE write_pk = ? LIMIT 1 FOR UPDATE")
            .bind(&new_pk[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal("read new key"))?;
    if taken.is_some() {
        return Err(coded(StatusCode::CONFLICT, CODE_KEY_IN_USE));
    }
    sqlx::query("UPDATE handles SET write_pk = ? WHERE write_pk = ?")
        .bind(&new_pk[..])
        .bind(&stmt.write_pk[..])
        .execute(&mut *tx)
        .await
        .map_err(internal("rekey handles"))?;
    let lease: (i64,) = sqlx::query_as(
        "SELECT CAST(UNIX_TIMESTAMP(lease_until) * 1000 AS SIGNED) FROM handles WHERE lookup_key = ?",
    )
    .bind(&stmt.lookup_key[..])
    .fetch_one(&mut *tx)
    .await
    .map_err(internal("read lease"))?;
    tx.commit().await.map_err(internal("commit rekey"))?;
    Ok(lease)
    })
    .await?;
    tracing::info!("handle rekeyed");
    Ok(answer(
        &state,
        statement.lookup_key,
        new_write_pk,
        u64::try_from(lease.0).unwrap_or(0),
    ))
}

/// `DELETE /discovery/handle` — retire the name: it stops answering at once
/// and stays held for the hold. Idempotent on a retired row.
pub(crate) async fn retire_name(
    State(state): State<AppState>,
    Json(req): Json<HandleRequest>,
) -> Result<StatusCode, Response> {
    let statement = decode_statement(&req, Expect::Retire)?;
    if req.public_record_b64.is_some() || !req.terms_b64.is_empty() {
        return Err(bad_request("a retire publishes nothing"));
    }
    let (st, stmt) = (&state, &statement);
    retry_on_lock_conflict(|| async move {
        let mut conn = connection(st).await?;
        let mut tx = begin(&mut conn).await?;
        consume_nonce(&mut tx, stmt).await?;
        let Some(row) = lock_row(&mut tx, &stmt.lookup_key).await? else {
            return Err(coded(StatusCode::NOT_FOUND, CODE_NO_HANDLE));
        };
        if row.write_pk != stmt.write_pk {
            return Err(refused());
        }
        if let Some(refusal) = handle_write_refusal(row.moderation, false) {
            return Err(refusal);
        }
        retire(&mut tx, &stmt.lookup_key, &row.handle_id).await?;
        tx.commit().await.map_err(internal("commit retire"))
    })
    .await?;
    tracing::info!("handle retired");
    Ok(StatusCode::NO_CONTENT)
}

/// Rows one pruner transaction takes. Chosen, not measured: a batch is short
/// enough that a route waiting on one of its rows waits briefly.
const PRUNE_BATCH: usize = 500;

/// One pass of the pruner: retire lapsed rows (`retired_at = lease_until`,
/// their records and terms deleted), delete rows retired longer than
/// `hold_secs` ago with their claims, and drop nonces older than twice the
/// statement window. Returns `(lapsed, freed)`.
///
/// Each batch is its own READ COMMITTED transaction and takes its locks in
/// the routes' order (`name_claims`, `handles`, then `search_terms` and
/// `public_records`), by key, with no join or subquery over another table:
/// the earlier form deleted `search_terms` through a join on `handles` and
/// so locked the two tables in the opposite order to an update or a retire,
/// which deadlocked with them (seen as MariaDB 1213 in the tests). A batch
/// that still meets a deadlock or a lock-wait timeout is run again
/// ([`LOCK_ATTEMPTS`]).
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn prune(pool: &MySqlPool, hold_secs: u64) -> Result<(u64, u64), sqlx::Error> {
    let hold = i64::try_from(hold_secs).unwrap_or(i64::MAX);
    let mut lapsed = 0u64;
    loop {
        let (seen, done) = retry_db(|| retire_lapsed_batch(pool)).await?;
        lapsed = lapsed.saturating_add(done);
        if seen < PRUNE_BATCH {
            break;
        }
    }
    let mut freed = 0u64;
    loop {
        let (seen, done) = retry_db(|| free_held_batch(pool, hold)).await?;
        freed = freed.saturating_add(done);
        if seen < PRUNE_BATCH {
            break;
        }
    }
    let window = i64::try_from(2 * HANDLE_STATEMENT_WINDOW_MS / 1000).unwrap_or(i64::MAX);
    retry_db(|| async move {
        let mut conn = read_committed(pool).await?;
        sqlx::query(
            "DELETE FROM handle_statement_nonces WHERE seen_at <= NOW(3) - INTERVAL ? SECOND",
        )
        .bind(window)
        .execute(&mut *conn)
        .await
    })
    .await?;
    Ok((lapsed, freed))
}

/// Push `keys` as a bound `IN (...)` list.
fn push_in_list(q: &mut sqlx::QueryBuilder<sqlx::MySql>, keys: &[Vec<u8>]) {
    q.push(" IN (");
    let mut sep = q.separated(", ");
    for key in keys {
        sep.push_bind(key.clone());
    }
    q.push(")");
}

/// Retire up to [`PRUNE_BATCH`] lapsed rows: lock them, delete their terms
/// and records, mark them retired at their lease end. Returns `(rows
/// locked, rows retired)`.
async fn retire_lapsed_batch(pool: &MySqlPool) -> Result<(usize, u64), sqlx::Error> {
    let mut conn = read_committed(pool).await?;
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
    let rows: Vec<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT lookup_key, handle_id FROM handles \
         WHERE retired_at IS NULL AND lease_until <= NOW(3) \
         ORDER BY lookup_key LIMIT ? FOR UPDATE",
    )
    .bind(u64::try_from(PRUNE_BATCH).unwrap_or(u64::MAX))
    .fetch_all(&mut *tx)
    .await?;
    if rows.is_empty() {
        tx.commit().await?;
        return Ok((0, 0));
    }
    let (keys, ids): (Vec<Vec<u8>>, Vec<Vec<u8>>) = rows.into_iter().unzip();
    for table in ["search_terms", "public_records"] {
        let mut q = sqlx::QueryBuilder::<sqlx::MySql>::new("DELETE FROM ");
        q.push(table);
        q.push(" WHERE kind = ");
        q.push_bind(crate::visibility::TARGET_HANDLE);
        q.push(" AND target");
        push_in_list(&mut q, &ids);
        q.build().execute(&mut *tx).await?;
    }
    let mut q = sqlx::QueryBuilder::<sqlx::MySql>::new(
        "UPDATE handles SET retired_at = lease_until, is_public = FALSE \
         WHERE retired_at IS NULL AND lease_until <= NOW(3) AND lookup_key",
    );
    push_in_list(&mut q, &keys);
    let retired = q.build().execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok((keys.len(), retired))
}

/// Free up to [`PRUNE_BATCH`] rows retired longer than `hold` seconds ago,
/// with their claims. The candidates are read without a lock; the claims
/// are locked first and the rows second (the order of a claim and a
/// replace), and a row taken back in between is left alone. Returns `(rows
/// read, rows freed)`.
async fn free_held_batch(pool: &MySqlPool, hold: i64) -> Result<(usize, u64), sqlx::Error> {
    let mut conn = read_committed(pool).await?;
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
    let candidates: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT lookup_key FROM handles \
         WHERE retired_at IS NOT NULL AND retired_at <= NOW(3) - INTERVAL ? SECOND \
           AND moderation <> 2 \
         ORDER BY lookup_key LIMIT ?",
    )
    .bind(hold)
    .bind(u64::try_from(PRUNE_BATCH).unwrap_or(u64::MAX))
    .fetch_all(&mut *tx)
    .await?;
    if candidates.is_empty() {
        tx.commit().await?;
        return Ok((0, 0));
    }
    let mut q = sqlx::QueryBuilder::<sqlx::MySql>::new(
        "SELECT lookup_key FROM name_claims WHERE lookup_key",
    );
    push_in_list(&mut q, &candidates);
    q.push(" ORDER BY lookup_key FOR UPDATE");
    q.build().fetch_all(&mut *tx).await?;
    let mut q = sqlx::QueryBuilder::<sqlx::MySql>::new(
        "SELECT lookup_key FROM handles WHERE retired_at IS NOT NULL AND moderation <> 2 \
         AND retired_at <= NOW(3) - INTERVAL ",
    );
    q.push_bind(hold);
    q.push(" SECOND AND lookup_key");
    push_in_list(&mut q, &candidates);
    q.push(" ORDER BY lookup_key FOR UPDATE");
    let still_held: Vec<Vec<u8>> = q.build_query_scalar().fetch_all(&mut *tx).await?;
    let mut freed = 0;
    if !still_held.is_empty() {
        let mut q =
            sqlx::QueryBuilder::<sqlx::MySql>::new("DELETE FROM name_claims WHERE lookup_key");
        push_in_list(&mut q, &still_held);
        q.build().execute(&mut *tx).await?;
        let mut q = sqlx::QueryBuilder::<sqlx::MySql>::new("DELETE FROM handles WHERE lookup_key");
        push_in_list(&mut q, &still_held);
        freed = q.build().execute(&mut *tx).await?.rows_affected();
    }
    tx.commit().await?;
    Ok((candidates.len(), freed))
}

/// Hourly pruner: lapsed leases, held names past the hold, old nonces.
pub(crate) fn spawn_pruner(pool: MySqlPool, hold_secs: u64) -> tokio::task::JoinHandle<()> {
    use crate::metrics::{job_run, jobs};
    lakko_metrics::job_registered(jobs::HANDLE_PRUNE);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = prune(&pool, hold_secs).await;
            job_run(jobs::HANDLE_PRUNE, &result);
            match result {
                Ok((0, 0)) => {},
                Ok((lapsed, freed)) => tracing::info!(lapsed, freed, "handles pruned"),
                Err(e) => tracing::warn!(error = %e, "handles prune failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests;
