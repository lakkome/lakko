// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0086 - a person's circle directory.
//!
//! A directory is ciphertext the owner writes and contacts read. This module
//! stores it, gates it and reads none of it: the head, the edges, the chain,
//! the filter and the rows are opaque bytes of a checked length. The only
//! fields the server understands are the ones the JSON carries for it: the
//! version, the epoch and the read key.
//!
//! | Route | Proof | Effect |
//! |---|---|---|
//! | `PUT /ds/directory/{id}/claim` | owner, plus a `circle-claim` token | creates the directory at version 1, epoch 1 |
//! | `POST /ds/directory/{id}/write` | owner | applies one write in one transaction |
//! | `GET /ds/directory/{id}/header` | none | the head; `?nodes=` edges; `?chain_from=&chain_to=` chain |
//! | `GET /ds/directory/{id}/filter` | current read key | the sealed filter |
//! | `GET /ds/directory/{id}/rows/{index}` | current read key | one sealed row |
//! | `DELETE /ds/directory/{id}` | owner | removes the directory and everything in it |
//!
//! **The id proves the owner.** It is the hash of the owner key, so a claim
//! needs no prior registration and nobody can squat a person's directory
//! ([`lakko_circle::directory_owner_matches`]). The owner's proof is an
//! ADR-0053 proof at [`OWNER_PROOF_EPOCH`], read proofs at the directory's
//! epoch, which starts at 1, so neither passes as the other.
//!
//! **Why the claim spends a token** (not in ADR-0086; a key is free to make,
//! so without one a directory is megabytes of free storage per key). As with
//! the knock, the token is spent last, so a malformed claim costs none.
//!
//! **One read key.** A directory holds the read key of its current epoch and
//! no other, unlike a group journal's retained tiers: ADR-0086 wants a read
//! under the previous epoch refused at once. Registering the next key is one
//! update of one row, inside the write that carries the edges, so the key and
//! the header a member needs to reach it land together.
//!
//! **Compare and swap.** Every write names the version it creates, and an epoch
//! either the current one or the next. So two devices of one owner cannot
//! overwrite each other, and a device that missed a removal cannot put a filter
//! under the old key back. The same bytes sent again answer as the first time:
//! the digest of the last write is kept for that.
//!
//! **A failed check** is the uniform `401` of `src/access.rs`, also for a
//! directory that does not exist, on every gated route. The public header route
//! answers `404` for one: anyone holding the id may read a header, and the id is
//! not guessable.

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_anon_token::TokenContext;
use lakko_circle::{
    directory_owner_matches, write_digest, HEAD_LEN, MAX_EPOCH, MAX_FILTER_BYTES, MAX_NODE,
    MAX_ROWS, MAX_ROWS_PER_WRITE, OWNER_PROOF_EPOCH, ROW_LEN, TREE_HEIGHT, WRAP_LEN,
};
use lakko_ds_proof::VerifyingKey;
use serde::{Deserialize, Serialize};
use sqlx::mysql::{MySql, MySqlConnection, MySqlPool};
use sqlx::QueryBuilder;

use crate::access::{
    check_proof, consume_nonce, db_error, parts, refused, verify_header, HandlerError,
};
use crate::journal::{self, PSEUDONYM_LEN};
use crate::AppState;

/// Largest body of a claim or a write: the largest growth of the tree, the
/// largest filter and a full batch of rows, in base64 inside JSON.
pub(crate) const MAX_WRITE_BODY: usize = 12 * 1024 * 1024;
/// Most edges one write touches: the largest growth of the tree.
const MAX_EDGES_PER_WRITE: usize = 1 << 16;
/// Most edges one header read returns: a whole path.
const MAX_NODES_PER_READ: usize = TREE_HEIGHT as usize + 1;
/// Most chain entries one header read returns. A reader further behind reads
/// again, naming `chain_to`.
const MAX_CHAIN_PER_READ: u32 = 256;
/// Rows of one batched statement.
const STATEMENT_CHUNK: usize = 1000;

const KEY_LEN: usize = 32;

fn bad_request(msg: &str) -> HandlerError {
    (StatusCode::BAD_REQUEST, msg.to_string())
}

fn conflict(msg: &str) -> HandlerError {
    (StatusCode::CONFLICT, msg.to_string())
}

/// The body of a claim or a write.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteJson {
    version: u64,
    epoch: u64,
    head_b64: String,
    #[serde(default)]
    read_pk_b64: Option<String>,
    #[serde(default)]
    owner_pk_b64: Option<String>,
    #[serde(default)]
    chain_b64: Option<String>,
    #[serde(default)]
    puts: Vec<(u32, String)>,
    #[serde(default)]
    deletes: Vec<u32>,
    #[serde(default)]
    filter_b64: Option<String>,
    #[serde(default)]
    rows: Vec<(u32, String)>,
    #[serde(default)]
    row_deletes: Vec<u32>,
}

/// A decoded body, every length checked. Bytes stay opaque.
struct Write {
    version: u64,
    epoch: u64,
    head: Vec<u8>,
    read_pk: Option<[u8; KEY_LEN]>,
    owner_pk: Option<[u8; KEY_LEN]>,
    chain: Option<Vec<u8>>,
    puts: Vec<(u32, Vec<u8>)>,
    deletes: Vec<u32>,
    filter: Option<Vec<u8>>,
    rows: Vec<(u32, Vec<u8>)>,
    row_deletes: Vec<u32>,
}

fn decode_exact(b64: &str, len: usize, what: &str) -> Result<Vec<u8>, HandlerError> {
    let bytes = B64
        .decode(b64.as_bytes())
        .map_err(|_| bad_request(&format!("{what}: bad base64")))?;
    if bytes.len() == len {
        Ok(bytes)
    } else {
        Err(bad_request(&format!("{what}: must be {len} bytes")))
    }
}

fn decode_key(b64: &str, what: &str) -> Result<[u8; KEY_LEN], HandlerError> {
    let bytes = decode_exact(b64, KEY_LEN, what)?;
    let key = VerifyingKey::from_bytes(&bytes)
        .map_err(|_| bad_request(&format!("{what}: not an Ed25519 key")))?;
    if key.is_weak() {
        return Err(bad_request(&format!("{what}: not an Ed25519 key")));
    }
    Ok(key.to_bytes())
}

const fn valid_node(node: u32) -> bool {
    node >= 2 && node <= MAX_NODE
}

fn decode_write(body: &[u8]) -> Result<Write, HandlerError> {
    let json: WriteJson =
        serde_json::from_slice(body).map_err(|_| bad_request("body does not parse"))?;
    // The epoch bound is what bounds the chain, which is never pruned.
    if json.version == 0 || json.epoch == 0 || json.epoch > MAX_EPOCH {
        return Err(bad_request("version or epoch out of range"));
    }
    if json.puts.len() > MAX_EDGES_PER_WRITE || json.deletes.len() > MAX_EDGES_PER_WRITE {
        return Err(bad_request("too many edges"));
    }
    if json.rows.len() > MAX_ROWS_PER_WRITE || json.row_deletes.len() > MAX_ROWS_PER_WRITE {
        return Err(bad_request("too many rows"));
    }
    let mut puts = Vec::with_capacity(json.puts.len());
    for (node, wrap) in &json.puts {
        if !valid_node(*node) {
            return Err(bad_request("node out of range"));
        }
        puts.push((*node, decode_exact(wrap, WRAP_LEN, "puts")?));
    }
    if !json.deletes.iter().all(|node| valid_node(*node)) {
        return Err(bad_request("node out of range"));
    }
    let mut rows = Vec::with_capacity(json.rows.len());
    for (index, row) in &json.rows {
        if *index >= MAX_ROWS {
            return Err(bad_request("row out of range"));
        }
        rows.push((*index, decode_exact(row, ROW_LEN, "rows")?));
    }
    if json.row_deletes.iter().any(|index| *index >= MAX_ROWS) {
        return Err(bad_request("row out of range"));
    }
    let filter = match &json.filter_b64 {
        Some(b64) => {
            let bytes = B64
                .decode(b64.as_bytes())
                .map_err(|_| bad_request("filter_b64: bad base64"))?;
            if bytes.is_empty() || bytes.len() > MAX_FILTER_BYTES {
                return Err(bad_request("filter_b64: out of range"));
            }
            Some(bytes)
        },
        None => None,
    };
    Ok(Write {
        version: json.version,
        epoch: json.epoch,
        head: decode_exact(&json.head_b64, HEAD_LEN, "head_b64")?,
        read_pk: json
            .read_pk_b64
            .as_deref()
            .map(|b64| decode_key(b64, "read_pk_b64"))
            .transpose()?,
        owner_pk: json
            .owner_pk_b64
            .as_deref()
            .map(|b64| decode_key(b64, "owner_pk_b64"))
            .transpose()?,
        chain: json
            .chain_b64
            .as_deref()
            .map(|b64| decode_exact(b64, WRAP_LEN, "chain_b64"))
            .transpose()?,
        puts,
        deletes: json.deletes,
        filter,
        rows,
        row_deletes: json.row_deletes,
    })
}

/// The row a directory is decided on.
struct Current {
    owner_pk: Vec<u8>,
    epoch: u64,
    version: u64,
    write_hash: Vec<u8>,
}

type CurrentRow = (Vec<u8>, u64, u64, Vec<u8>);

fn current_of(row: CurrentRow) -> Current {
    let (owner_pk, epoch, version, write_hash) = row;
    Current {
        owner_pk,
        epoch,
        version,
        write_hash,
    }
}

async fn load_current(
    pool: &MySqlPool,
    id: &[u8; PSEUDONYM_LEN],
) -> Result<Option<Current>, HandlerError> {
    let row: Option<CurrentRow> = sqlx::query_as(
        "SELECT owner_pk, epoch, version, write_hash FROM ds_directory WHERE directory_id = ?",
    )
    .bind(&id[..])
    .fetch_optional(pool)
    .await
    .map_err(db_error("load directory"))?;
    Ok(row.map(current_of))
}

/// The writes every claim and write shares, on one transaction.
async fn apply_contents(
    conn: &mut MySqlConnection,
    id: &[u8; PSEUDONYM_LEN],
    write: &Write,
) -> Result<(), HandlerError> {
    for chunk in write.deletes.chunks(STATEMENT_CHUNK) {
        let mut q: QueryBuilder<MySql> =
            QueryBuilder::new("DELETE FROM ds_directory_edges WHERE directory_id = ");
        q.push_bind(&id[..]);
        q.push(" AND node IN (");
        let mut separated = q.separated(", ");
        for node in chunk {
            separated.push_bind(*node);
        }
        q.push(")");
        q.build()
            .execute(&mut *conn)
            .await
            .map_err(db_error("delete directory edges"))?;
    }
    for chunk in write.puts.chunks(STATEMENT_CHUNK) {
        let mut q: QueryBuilder<MySql> =
            QueryBuilder::new("INSERT INTO ds_directory_edges (directory_id, node, wrap) ");
        q.push_values(chunk, |mut row, (node, wrap)| {
            row.push_bind(&id[..])
                .push_bind(*node)
                .push_bind(wrap.as_slice());
        });
        q.push(" ON DUPLICATE KEY UPDATE wrap = VALUES(wrap)");
        q.build()
            .execute(&mut *conn)
            .await
            .map_err(db_error("write directory edges"))?;
    }
    for chunk in write.row_deletes.chunks(STATEMENT_CHUNK) {
        let mut q: QueryBuilder<MySql> =
            QueryBuilder::new("DELETE FROM ds_directory_rows WHERE directory_id = ");
        q.push_bind(&id[..]);
        q.push(" AND row_index IN (");
        let mut separated = q.separated(", ");
        for index in chunk {
            separated.push_bind(*index);
        }
        q.push(")");
        q.build()
            .execute(&mut *conn)
            .await
            .map_err(db_error("delete directory rows"))?;
    }
    for chunk in write.rows.chunks(STATEMENT_CHUNK) {
        let mut q: QueryBuilder<MySql> = QueryBuilder::new(
            "INSERT INTO ds_directory_rows (directory_id, row_index, row_bytes) ",
        );
        q.push_values(chunk, |mut row, (index, bytes)| {
            row.push_bind(&id[..])
                .push_bind(*index)
                .push_bind(bytes.as_slice());
        });
        q.push(" ON DUPLICATE KEY UPDATE row_bytes = VALUES(row_bytes)");
        q.build()
            .execute(&mut *conn)
            .await
            .map_err(db_error("write directory rows"))?;
    }
    if let Some(filter) = &write.filter {
        sqlx::query(
            "INSERT INTO ds_directory_filter (directory_id, filter_bytes) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE filter_bytes = VALUES(filter_bytes)",
        )
        .bind(&id[..])
        .bind(filter.as_slice())
        .execute(&mut *conn)
        .await
        .map_err(db_error("write directory filter"))?;
    }
    Ok(())
}

/// `PUT /ds/directory/{hex}/claim`.
pub(crate) async fn put_claim(
    State(state): State<AppState>,
    AxumPath(directory_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, HandlerError> {
    let id = journal::decode_pseudonym(&directory_hex)?;
    let write = decode_write(&body)?;
    let (Some(owner_pk), Some(read_pk)) = (write.owner_pk, write.read_pk) else {
        return Err(bad_request("a claim names the owner and the read key"));
    };
    if write.version != 1 || write.epoch != 1 || write.chain.is_some() {
        return Err(bad_request("a claim starts at version 1, epoch 1"));
    }
    if !directory_owner_matches(&id, &owner_pk) {
        return Err(bad_request(
            "owner_pk_b64 is not the key this directory is named after",
        ));
    }
    let owner = VerifyingKey::from_bytes(&owner_pk).map_err(|_| refused())?;
    let proof = verify_header(&headers, &parts(&method, &uri, &body), |epoch| {
        (epoch == OWNER_PROOF_EPOCH).then_some(owner)
    })?;
    let digest = write_digest(&body);
    if let Some(current) = load_current(&state.db, &id).await? {
        return if current.version == 1 && current.write_hash == digest {
            Ok(StatusCode::NO_CONTENT.into_response())
        } else {
            Err(conflict("already claimed"))
        };
    }
    if !consume_nonce(&state.db, &id, &proof.nonce)
        .await
        .map_err(db_error("record claim nonce"))?
    {
        return Err(refused());
    }
    if let Err(refusal) = state
        .anon_tokens
        .spend_header(&headers, TokenContext::CircleClaim)
        .await
    {
        return Ok(refusal.into_response());
    }
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(db_error("begin directory claim"))?;
    let inserted = sqlx::query(
        "INSERT INTO ds_directory \
           (directory_id, owner_pk, epoch, read_pk, version, head, write_hash) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id[..])
    .bind(&owner_pk[..])
    .bind(write.epoch)
    .bind(&read_pk[..])
    .bind(write.version)
    .bind(write.head.as_slice())
    .bind(&digest[..])
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        if e.as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
        {
            return Err(conflict("already claimed"));
        }
        return Err(db_error("insert directory")(e));
    }
    apply_contents(&mut tx, &id, &write).await?;
    tx.commit()
        .await
        .map_err(db_error("commit directory claim"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /ds/directory/{hex}/write`.
pub(crate) async fn post_write(
    State(state): State<AppState>,
    AxumPath(directory_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, HandlerError> {
    let id = journal::decode_pseudonym(&directory_hex)?;
    let write = decode_write(&body)?;
    if write.owner_pk.is_some() {
        return Err(bad_request("only a claim names the owner"));
    }
    let Some(current) = load_current(&state.db, &id).await? else {
        return Err(refused());
    };
    let owner = VerifyingKey::from_bytes(&current.owner_pk).map_err(|_| refused())?;
    let proof = verify_header(&headers, &parts(&method, &uri, &body), |epoch| {
        (epoch == OWNER_PROOF_EPOCH).then_some(owner)
    })?;

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(db_error("begin directory write"))?;
    let row: Option<CurrentRow> = sqlx::query_as(
        "SELECT owner_pk, epoch, version, write_hash FROM ds_directory \
         WHERE directory_id = ? FOR UPDATE",
    )
    .bind(&id[..])
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error("lock directory"))?;
    let Some(current) = row.map(current_of) else {
        return Err(refused());
    };
    // A refusal below commits: the nonce stays spent.
    if !consume_nonce(&mut *tx, &id, &proof.nonce)
        .await
        .map_err(db_error("record write nonce"))?
    {
        tx.commit()
            .await
            .map_err(db_error("commit refused write"))?;
        return Err(refused());
    }
    let digest = write_digest(&body);
    let decision = decide(&current, &write, &digest);
    if let Ok(Decision::Apply) = decision {
        let read_pk = write.read_pk;
        let updated = sqlx::query(
            "UPDATE ds_directory SET epoch = ?, version = ?, head = ?, write_hash = ?, \
               read_pk = COALESCE(?, read_pk) \
             WHERE directory_id = ?",
        )
        .bind(write.epoch)
        .bind(write.version)
        .bind(write.head.as_slice())
        .bind(&digest[..])
        .bind(read_pk.as_ref().map(<[u8; KEY_LEN]>::as_slice))
        .bind(&id[..])
        .execute(&mut *tx)
        .await
        .map_err(db_error("update directory"))?;
        if updated.rows_affected() != 1 {
            return Err(refused());
        }
        if let Some(chain) = &write.chain {
            sqlx::query(
                "INSERT INTO ds_directory_chain (directory_id, epoch, wrap) VALUES (?, ?, ?)",
            )
            .bind(&id[..])
            .bind(write.epoch)
            .bind(chain.as_slice())
            .execute(&mut *tx)
            .await
            .map_err(db_error("write directory chain"))?;
        }
        apply_contents(&mut tx, &id, &write).await?;
    }
    tx.commit()
        .await
        .map_err(db_error("commit directory write"))?;
    decision.map(|_| StatusCode::NO_CONTENT.into_response())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    /// Apply the write.
    Apply,
    /// The last write sent again: answered, not applied.
    AlreadyApplied,
}

/// The rule of a write against the row it was locked on.
fn decide(current: &Current, write: &Write, digest: &[u8; 32]) -> Result<Decision, HandlerError> {
    if write.version == current.version && current.write_hash == digest {
        return Ok(Decision::AlreadyApplied);
    }
    if Some(write.version) != current.version.checked_add(1) {
        return Err(conflict("not the next version"));
    }
    let next_epoch = current.epoch.checked_add(1);
    if write.epoch == current.epoch {
        if write.read_pk.is_some() || write.chain.is_some() {
            return Err(bad_request(
                "a read key and a chain entry come with a new epoch",
            ));
        }
        Ok(Decision::Apply)
    } else if Some(write.epoch) == next_epoch {
        if write.read_pk.is_none() || write.chain.is_none() {
            return Err(bad_request(
                "a new epoch needs a read key and a chain entry",
            ));
        }
        Ok(Decision::Apply)
    } else {
        Err(conflict("not the current or the next epoch"))
    }
}

/// Gate a read: a proof by the current epoch's read key. Any other epoch, and
/// a directory that does not exist, is the uniform refusal.
async fn gate_read(
    state: &AppState,
    id: &[u8; PSEUDONYM_LEN],
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<(), HandlerError> {
    let row: Option<(u64, Vec<u8>)> =
        sqlx::query_as("SELECT epoch, read_pk FROM ds_directory WHERE directory_id = ?")
            .bind(&id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(db_error("load directory read key"))?;
    let Some((current_epoch, read_pk)) = row else {
        return Err(refused());
    };
    let key = VerifyingKey::from_bytes(&read_pk).map_err(|_| refused())?;
    check_proof(&state.db, id, headers, &parts(method, uri, &[]), |epoch| {
        (epoch == current_epoch).then_some(key)
    })
    .await
    .map(|_| ())
}

fn binary(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
}

/// `GET /ds/directory/{hex}/filter`.
pub(crate) async fn get_filter(
    State(state): State<AppState>,
    AxumPath(directory_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, HandlerError> {
    let id = journal::decode_pseudonym(&directory_hex)?;
    gate_read(&state, &id, &method, &uri, &headers).await?;
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT filter_bytes FROM ds_directory_filter WHERE directory_id = ?")
            .bind(&id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(db_error("load directory filter"))?;
    row.map_or_else(
        || Err((StatusCode::NOT_FOUND, "no filter".to_string())),
        |(bytes,)| Ok(binary(bytes)),
    )
}

/// `GET /ds/directory/{hex}/rows/{index}`.
pub(crate) async fn get_row(
    State(state): State<AppState>,
    AxumPath((directory_hex, index)): AxumPath<(String, u32)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, HandlerError> {
    let id = journal::decode_pseudonym(&directory_hex)?;
    gate_read(&state, &id, &method, &uri, &headers).await?;
    let row: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT row_bytes FROM ds_directory_rows WHERE directory_id = ? AND row_index = ?",
    )
    .bind(&id[..])
    .bind(index)
    .fetch_optional(&state.db)
    .await
    .map_err(db_error("load directory row"))?;
    row.map_or_else(
        || Err((StatusCode::NOT_FOUND, "no such row".to_string())),
        |(bytes,)| Ok(binary(bytes)),
    )
}

#[derive(Deserialize)]
pub(crate) struct HeaderQuery {
    /// Comma-separated node numbers: the member's path.
    #[serde(default)]
    nodes: Option<String>,
    /// Chain entries of the epochs after this one.
    #[serde(default)]
    chain_from: Option<u64>,
    /// ... up to and including this one (default: the current epoch).
    #[serde(default)]
    chain_to: Option<u64>,
}

#[derive(Serialize)]
struct HeaderResponse {
    head_b64: String,
    nodes: Vec<(u32, String)>,
    chain: Vec<(u64, String)>,
}

fn parse_nodes(list: &str) -> Result<Vec<u32>, HandlerError> {
    let mut out = Vec::new();
    for field in list.split(',').filter(|f| !f.is_empty()) {
        if out.len() == MAX_NODES_PER_READ {
            return Err(bad_request("too many nodes"));
        }
        let node: u32 = field.parse().map_err(|_| bad_request("bad node"))?;
        if !valid_node(node) {
            return Err(bad_request("node out of range"));
        }
        out.push(node);
    }
    Ok(out)
}

/// `GET /ds/directory/{hex}/header`. Public: all of it is ciphertext, or the
/// head, which carries no secret.
pub(crate) async fn get_header(
    State(state): State<AppState>,
    AxumPath(directory_hex): AxumPath<String>,
    Query(query): Query<HeaderQuery>,
) -> Result<Response, HandlerError> {
    let id = journal::decode_pseudonym(&directory_hex)?;
    let nodes = parse_nodes(query.nodes.as_deref().unwrap_or(""))?;
    let row: Option<(Vec<u8>, u64)> =
        sqlx::query_as("SELECT head, epoch FROM ds_directory WHERE directory_id = ?")
            .bind(&id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(db_error("load directory head"))?;
    let Some((head, epoch)) = row else {
        return Err((StatusCode::NOT_FOUND, "no such directory".to_string()));
    };

    let mut edges = Vec::new();
    if !nodes.is_empty() {
        let mut q: QueryBuilder<MySql> =
            QueryBuilder::new("SELECT node, wrap FROM ds_directory_edges WHERE directory_id = ");
        q.push_bind(&id[..]);
        q.push(" AND node IN (");
        let mut separated = q.separated(", ");
        for node in &nodes {
            separated.push_bind(*node);
        }
        q.push(")");
        let found: Vec<(u32, Vec<u8>)> = q
            .build_query_as()
            .fetch_all(&state.db)
            .await
            .map_err(db_error("load directory edges"))?;
        edges = found
            .into_iter()
            .map(|(node, wrap)| (node, B64.encode(wrap)))
            .collect();
    }

    let mut chain = Vec::new();
    if let Some(from) = query.chain_from {
        let to = query.chain_to.unwrap_or(epoch);
        let found: Vec<(u64, Vec<u8>)> = sqlx::query_as(
            "SELECT epoch, wrap FROM ds_directory_chain \
             WHERE directory_id = ? AND epoch > ? AND epoch <= ? \
             ORDER BY epoch DESC LIMIT ?",
        )
        .bind(&id[..])
        .bind(from)
        .bind(to)
        .bind(MAX_CHAIN_PER_READ)
        .fetch_all(&state.db)
        .await
        .map_err(db_error("load directory chain"))?;
        chain = found
            .into_iter()
            .map(|(at, wrap)| (at, B64.encode(wrap)))
            .collect();
    }
    Ok(Json(HeaderResponse {
        head_b64: B64.encode(head),
        nodes: edges,
        chain,
    })
    .into_response())
}

/// `DELETE /ds/directory/{hex}`: the owner removes everything, on account
/// deletion or to start a tree over.
pub(crate) async fn delete_directory(
    State(state): State<AppState>,
    AxumPath(directory_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, HandlerError> {
    let id = journal::decode_pseudonym(&directory_hex)?;
    let Some(current) = load_current(&state.db, &id).await? else {
        return Err(refused());
    };
    let owner = VerifyingKey::from_bytes(&current.owner_pk).map_err(|_| refused())?;
    check_proof(
        &state.db,
        &id,
        &headers,
        &parts(&method, &uri, &[]),
        |epoch| (epoch == OWNER_PROOF_EPOCH).then_some(owner),
    )
    .await?;
    sqlx::query("DELETE FROM ds_directory WHERE directory_id = ?")
        .bind(&id[..])
        .execute(&state.db)
        .await
        .map_err(db_error("delete directory"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests;
