// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0053 - a journal is opened by a key, not by its name.
//!
//! A journal with a registration row (`ds_group_access` / `ds_mailbox_access`)
//! is **claimed**: its gated routes answer only a request that carries a valid
//! [`lakko_ds_proof`] header. A journal with no row keeps the Phase 1.3
//! posture (anyone who names it) - which is every journal until its owner
//! claims it, and the rendezvous mailboxes by design.
//!
//! What is gated: a claimed group's `GET`/`POST messages`, `GET knocks` and
//! every registration after the first; a claimed mailbox's `GET messages`.
//! What is not: a mailbox `POST` (the cold inbox strangers are meant to
//! reach), a group's `POST knock` (PoW-gated instead), and the first claim of
//! an unclaimed journal (a 256-bit name cannot be probed, and the flag day has
//! no production data).
//!
//! A failed check - no header, unknown epoch, bad signature, stale time,
//! replayed nonce - is one uniform `401` with no detail (the ADR's amendment:
//! a silent drop would hide a wrong-clock member's lost publishes, and what a
//! `401` reveals is nothing a caller does not already know).
//!
//! The server holds public keys only: one Ed25519 verifying key per registered
//! epoch of a group, and the identity key a mailbox's pseudonym is the hash of.
//!
//! **Two tiers of epoch key** (ADR-0053 open question 1, resolved in S3):
//! the two newest registered epochs may write (publish, register the next
//! epoch); every retained older epoch may still **read**. Without the read
//! tier a device offline across two commits would hold only a key the server
//! had dropped, could not fetch the commits that would bring it forward, and
//! would be stranded for good. Old keys are kept for the journal's own
//! retention window - a row older than that is pruned anyway, so a device
//! that far behind is lost regardless - and at most [`MAX_RETAINED_EPOCHS`].
//! The cost, stated: an evicted member can still read ciphertext it cannot
//! decrypt (sizes and timing) until its epoch ages out; it cannot write.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_ds_proof::{Proof, RequestParts, VerifyingKey, MAILBOX_EPOCH, PROOF_HEADER_NAME};
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;

use crate::journal::{self, Queue, DEFAULT_LIMIT, PSEUDONYM_LEN};
use crate::AppState;

/// A knock is a request to join, not a message: 16 KiB carries an external
/// join proposal plus its attribution with room to spare.
pub(crate) const MAX_KNOCK_BYTES: usize = 16 * 1024;
/// Knocks outlive the proof window by days, not the month a message does.
pub(crate) const KNOCK_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;
/// A nonce row is useless once no proof carrying it could still be inside
/// the window; two windows is a comfortable margin.
pub(crate) const NONCE_RETENTION_SECS: i64 = {
    let window_secs = lakko_ds_proof::PROOF_WINDOW_MS / 1000;
    // The window is a small compile-time constant; the cast cannot wrap.
    #[allow(clippy::cast_possible_wrap)]
    let secs = 2 * window_secs as i64;
    secs
};
/// The most epoch keys a group keeps (the two write-capable newest plus the
/// read-only tail). A bound on the rows one group can hold, not a policy
/// knob: the retention window prunes first in any group that commits less
/// than this many times a month.
pub(crate) const MAX_RETAINED_EPOCHS: u64 = 64;
/// The two newest registered epochs are the write tier.
const WRITE_EPOCHS: usize = 2;
/// At most this many payloads in one knock (the attribution and the proposal
/// fit in two; the rest is headroom).
pub(crate) const MAX_KNOCK_PAYLOADS: usize = 4;

/// What a gated request is allowed to do with the epoch key it presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    /// A read: any retained epoch key opens it.
    Read,
    /// A write: only the two newest epochs.
    Write,
}

/// Effort env var for the knock's proof of work.
const EFFORT_VAR: &str = "LAKKO_DS_POW_EFFORT_BITS";

type HandlerError = (StatusCode, String);

/// The one refusal every failed proof maps to.
fn refused() -> HandlerError {
    (StatusCode::UNAUTHORIZED, "proof required".to_string())
}

fn db_error(what: &'static str) -> impl Fn(sqlx::Error) -> HandlerError {
    move |e| {
        tracing::error!(error = %e, "{what}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    }
}

/// The server's clock in unix milliseconds. Saturating, never panicking.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ============================================================================
// Registrations
// ============================================================================

/// The registered access keys of a group, newest epoch first. Empty means
/// unclaimed. The first [`WRITE_EPOCHS`] are the write tier.
async fn group_keys(
    pool: &MySqlPool,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<Vec<(u64, VerifyingKey)>, HandlerError> {
    let rows: Vec<(u64, Vec<u8>)> = sqlx::query_as(
        "SELECT epoch, access_pk FROM ds_group_access WHERE group_id = ? ORDER BY epoch DESC LIMIT 64",
    )
    .bind(&group_id[..])
    .fetch_all(pool)
    .await
    .map_err(db_error("load group access keys"))?;
    let mut keys = Vec::with_capacity(rows.len());
    for (epoch, pk) in rows {
        // A row this server wrote from a validated key; a row that does not
        // parse is skipped rather than trusted.
        if let Ok(key) = VerifyingKey::from_bytes(&pk) {
            keys.push((epoch, key));
        }
    }
    Ok(keys)
}

/// The identity key a claimed mailbox is named after, or `None` when unclaimed.
async fn mailbox_key(
    pool: &MySqlPool,
    device_id: &[u8; PSEUDONYM_LEN],
) -> Result<Option<VerifyingKey>, HandlerError> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT identity_pk FROM ds_mailbox_access WHERE device_id = ?")
            .bind(&device_id[..])
            .fetch_optional(pool)
            .await
            .map_err(db_error("load mailbox access key"))?;
    Ok(row.and_then(|(pk,)| VerifyingKey::from_bytes(&pk).ok()))
}

// ============================================================================
// The check
// ============================================================================

/// Parse the proof header, verify it under `key_for_epoch`, and consume the
/// nonce. Every failure is [`refused`].
async fn check_proof(
    pool: &MySqlPool,
    pseudonym: &[u8; PSEUDONYM_LEN],
    headers: &HeaderMap,
    parts: &RequestParts<'_>,
    key_for_epoch: impl Fn(u64) -> Option<VerifyingKey>,
) -> Result<(), HandlerError> {
    let value = headers
        .get(PROOF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(refused)?;
    let proof = Proof::parse(value).map_err(|_| refused())?;
    let key = key_for_epoch(proof.epoch).ok_or_else(refused)?;
    lakko_ds_proof::verify(&proof, parts, &key, now_ms()).map_err(|_| refused())?;
    // Consume the nonce. The primary key makes a repeat a duplicate-key
    // error, which is the replay refusal; any other DB error is a 500.
    let inserted =
        sqlx::query("INSERT IGNORE INTO ds_proof_nonces (pseudonym, nonce) VALUES (?, ?)")
            .bind(&pseudonym[..])
            .bind(&proof.nonce[..])
            .execute(pool)
            .await
            .map_err(db_error("record proof nonce"))?;
    if inserted.rows_affected() == 0 {
        return Err(refused());
    }
    Ok(())
}

fn parts<'a>(method: &'a Method, uri: &'a Uri, body: &'a [u8]) -> RequestParts<'a> {
    RequestParts {
        method: method.as_str(),
        canonical_path: uri.path(),
        query: uri.query().unwrap_or(""),
        body,
    }
}

/// Gate a group-journal request: open when the group is unclaimed, else a
/// valid proof under a registered epoch of the tier `access` allows - any
/// retained epoch for a read, one of the two newest for a write.
pub(crate) async fn gate_group(
    state: &AppState,
    group_id: &[u8; PSEUDONYM_LEN],
    access: Access,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), HandlerError> {
    let keys = group_keys(&state.db, group_id).await?;
    if keys.is_empty() {
        return Ok(());
    }
    let tier = match access {
        Access::Read => keys.len(),
        Access::Write => WRITE_EPOCHS.min(keys.len()),
    };
    let usable = &keys[..tier];
    let req = parts(method, uri, body);
    check_proof(&state.db, group_id, headers, &req, |epoch| {
        usable.iter().find(|(e, _)| *e == epoch).map(|(_, k)| *k)
    })
    .await
}

/// Gate a mailbox read: open when the mailbox is unclaimed, else a valid
/// proof under the identity key at `MAILBOX_EPOCH`.
pub(crate) async fn gate_mailbox_read(
    state: &AppState,
    device_id: &[u8; PSEUDONYM_LEN],
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<(), HandlerError> {
    let Some(key) = mailbox_key(&state.db, device_id).await? else {
        return Ok(());
    };
    let req = parts(method, uri, &[]);
    check_proof(&state.db, device_id, headers, &req, |epoch| {
        (epoch == MAILBOX_EPOCH).then_some(key)
    })
    .await
}

// ============================================================================
// Routes: claim
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct GroupAccessBody {
    pub(crate) epoch: u64,
    /// Standard base64 of the 32-byte Ed25519 verifying key.
    pub(crate) access_pk_b64: String,
}

fn decode_pk(b64: &str) -> Result<VerifyingKey, HandlerError> {
    let bytes = B64.decode(b64).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "access_pk_b64: bad base64".to_string(),
        )
    })?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "access_pk_b64: not an Ed25519 key".to_string(),
        )
    })
}

/// `PUT /ds/group/{hex}/access` - register the access key of an epoch.
///
/// The first registration of an unclaimed journal needs no proof, and neither
/// does re-sending a key already registered for that epoch (it changes
/// nothing, and it is what a client's claim-on-open sends). Every other
/// registration must be proven under a write-tier key and must name either the
/// next epoch (`latest + 1`, proven under either of the two newest) or the
/// latest epoch again (a replacement, proven under the epoch before it - the
/// shape an abandoned staged sweep leaves behind). Older epochs stay as the
/// read tier, up to [`MAX_RETAINED_EPOCHS`] and the journal retention window.
pub(crate) async fn put_group_access(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, HandlerError> {
    let group_id = journal::decode_pseudonym(&group_hex)?;
    let parsed: GroupAccessBody = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("bad json: {e}")))?;
    let new_key = decode_pk(&parsed.access_pk_b64)?;
    let keys = group_keys(&state.db, &group_id).await?;
    if keys
        .iter()
        .any(|(e, k)| *e == parsed.epoch && k.to_bytes() == new_key.to_bytes())
    {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let write_tier = &keys[..WRITE_EPOCHS.min(keys.len())];

    if let Some((latest, _)) = keys.first() {
        let latest = *latest;
        let req = parts(&method, &uri, &body);
        // Which registered epochs may vouch for this registration.
        let allowed: Vec<u64> = if parsed.epoch == latest.saturating_add(1) {
            write_tier.iter().map(|(e, _)| *e).collect()
        } else if parsed.epoch == latest {
            write_tier
                .iter()
                .map(|(e, _)| *e)
                .filter(|e| *e < latest)
                .collect()
        } else {
            return Err((
                StatusCode::CONFLICT,
                "epoch must be the latest registered epoch or the one after it".to_string(),
            ));
        };
        check_proof(&state.db, &group_id, &headers, &req, |epoch| {
            allowed
                .contains(&epoch)
                .then(|| keys.iter().find(|(e, _)| *e == epoch).map(|(_, k)| *k))
                .flatten()
        })
        .await?;
    }

    sqlx::query(
        "INSERT INTO ds_group_access (group_id, epoch, access_pk) VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE access_pk = VALUES(access_pk), created_at = CURRENT_TIMESTAMP(3)",
    )
    .bind(&group_id[..])
    .bind(parsed.epoch)
    .bind(&new_key.to_bytes()[..])
    .execute(&state.db)
    .await
    .map_err(db_error("register group access key"))?;
    // Bound the read tier; age-based pruning is the journal pruner's.
    sqlx::query("DELETE FROM ds_group_access WHERE group_id = ? AND epoch + ? <= ?")
        .bind(&group_id[..])
        .bind(MAX_RETAINED_EPOCHS)
        .bind(parsed.epoch)
        .execute(&state.db)
        .await
        .map_err(db_error("prune group access keys"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub(crate) struct MailboxAccessBody {
    /// Standard base64 of the 32-byte identity verifying key.
    pub(crate) identity_pk_b64: String,
}

/// `PUT /ds/mailbox/{hex}/access` - claim a mailbox under the identity key
/// it is named after. No proof: the key is public, and registering it grants
/// the registrant nothing (reads then need the private half). Idempotent.
pub(crate) async fn put_mailbox_access(
    State(state): State<AppState>,
    AxumPath(device_hex): AxumPath<String>,
    Json(body): Json<MailboxAccessBody>,
) -> Result<Response, HandlerError> {
    let device_id = journal::decode_pseudonym(&device_hex)?;
    let key = decode_pk(&body.identity_pk_b64)
        .map_err(|(s, m)| (s, m.replace("access_pk_b64", "identity_pk_b64")))?;
    if !lakko_ds_proof::mailbox_owner_matches(&device_id, &key) {
        return Err((
            StatusCode::BAD_REQUEST,
            "identity_pk_b64 is not the key this mailbox is named after".to_string(),
        ));
    }
    sqlx::query("INSERT IGNORE INTO ds_mailbox_access (device_id, identity_pk) VALUES (?, ?)")
        .bind(&device_id[..])
        .bind(&key.to_bytes()[..])
        .execute(&state.db)
        .await
        .map_err(db_error("claim mailbox"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ============================================================================
// Routes: knock
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct KnockBody {
    pub(crate) pow: lakko_pow_server::PowProof,
    /// Standard base64 of each knock row - typically the joiner's attribution
    /// and its external-join proposal - at most [`MAX_KNOCK_PAYLOADS`] and
    /// `MAX_KNOCK_BYTES` decoded in total. **Empty is a probe:** nothing is
    /// stored and the response carries only the epoch, which is how a joiner
    /// learns the epoch its proposal must target before it can build one.
    #[serde(default)]
    pub(crate) payloads_b64: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct KnockResponse {
    /// The ids of the stored rows, in order (empty for a probe).
    pub(crate) ids: Vec<u64>,
    /// The group's latest registered access epoch - the epoch a joiner must
    /// target - or `null` for an unclaimed group.
    pub(crate) epoch: Option<u64>,
}

/// `POST /ds/group/{hex}/knock` - a non-member's request to join.
pub(crate) async fn post_knock(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    Json(body): Json<KnockBody>,
) -> Result<Response, HandlerError> {
    let group_id = journal::decode_pseudonym(&group_hex)?;
    if body.payloads_b64.len() > MAX_KNOCK_PAYLOADS {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("at most {MAX_KNOCK_PAYLOADS} knock payloads"),
        ));
    }
    let mut payloads = Vec::with_capacity(body.payloads_b64.len());
    let mut total = 0usize;
    for b64 in &body.payloads_b64 {
        let payload = B64.decode(b64).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "payloads_b64: bad base64".to_string(),
            )
        })?;
        total = total.saturating_add(payload.len());
        if payload.is_empty() || total > MAX_KNOCK_BYTES {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "knock payloads must be non-empty and total at most {MAX_KNOCK_BYTES} bytes"
                ),
            ));
        }
        payloads.push(payload);
    }
    lakko_pow_server::verify_proof(&state.db, &body.pow)
        .await
        .map_err(HandlerError::from)?;
    let mut ids = Vec::with_capacity(payloads.len());
    for payload in &payloads {
        let published = journal::publish(
            &state.db,
            &state.notifiers,
            Queue::Knock,
            &group_id,
            payload,
        )
        .await?;
        ids.push(published.id);
    }
    let epoch = group_keys(&state.db, &group_id)
        .await?
        .first()
        .map(|(e, _)| *e);
    Ok((StatusCode::CREATED, Json(KnockResponse { ids, epoch })).into_response())
}

/// `GET /ds/group/{hex}/knocks` - the members read what was knocked; gated
/// exactly like the group's messages.
pub(crate) async fn get_knocks(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<journal::FetchQuery>,
) -> Result<Response, HandlerError> {
    let group_id = journal::decode_pseudonym(&group_hex)?;
    gate_group(
        &state,
        &group_id,
        Access::Read,
        &method,
        &uri,
        &headers,
        &[],
    )
    .await?;
    let resp = journal::fetch_with_wait(
        &state.db,
        &state.notifiers,
        Queue::Knock,
        &group_id,
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

/// `POST /pow/challenge` - a fresh proof-of-work challenge for a knock.
pub(crate) async fn issue_pow_challenge(
    State(state): State<AppState>,
) -> Result<Json<lakko_pow_server::ChallengeResponse>, HandlerError> {
    let effort = lakko_pow_server::configured_effort_bits(EFFORT_VAR);
    lakko_pow_server::issue_challenge(&state.db, effort)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// Age out read-tier epoch keys older than the journal's retention window,
/// never touching a group's two newest epochs (a quiet group that has not
/// committed in a month must stay openable). Runs on the journal pruner's tick.
pub(crate) async fn prune_group_access(pool: &MySqlPool, retention_secs: i64) {
    // MariaDB refuses a DELETE whose subquery reads the same table unless the
    // subquery is materialised, hence the derived table.
    let sql = "DELETE a FROM ds_group_access a \
               JOIN (SELECT group_id, MAX(epoch) AS latest FROM ds_group_access GROUP BY group_id) m \
                 ON m.group_id = a.group_id \
               WHERE a.created_at < NOW(3) - INTERVAL ? SECOND AND a.epoch + 1 < m.latest";
    match sqlx::query(sql).bind(retention_secs).execute(pool).await {
        Ok(r) if r.rows_affected() > 0 => {
            tracing::debug!(rows = r.rows_affected(), "pruned read-tier epoch keys");
        },
        Ok(_) => {},
        Err(e) => tracing::warn!(error = %e, "epoch key prune failed"),
    }
}

/// Delete nonce rows past their usefulness. Runs on the journal pruner's tick.
pub(crate) async fn prune_nonces(pool: &MySqlPool) {
    match sqlx::query("DELETE FROM ds_proof_nonces WHERE seen_at < NOW(3) - INTERVAL ? SECOND")
        .bind(NONCE_RETENTION_SECS)
        .execute(pool)
        .await
    {
        Ok(r) if r.rows_affected() > 0 => {
            tracing::debug!(rows = r.rows_affected(), "pruned proof nonces");
        },
        Ok(_) => {},
        Err(e) => tracing::warn!(error = %e, "proof nonce prune failed"),
    }
}

// ============================================================================
// Tests - gated on `LAKKO_DS_TEST_DATABASE_URL`, skipped silently without it
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::Router;
    use http_body_util::BodyExt as _;
    use lakko_ds_proof::{sign, SigningKey};
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt as _;

    /// Parallel tests share one database; sqlx's migrator does not tolerate
    /// two concurrent runs on it, so migrate once per process.
    static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

    async fn app() -> Option<(Router, MySqlPool)> {
        let Ok(url) = std::env::var("LAKKO_DS_TEST_DATABASE_URL") else {
            eprintln!("skipping: LAKKO_DS_TEST_DATABASE_URL unset");
            return None;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test db");
        MIGRATED
            .get_or_init(|| async {
                crate::MIGRATIONS.run(&pool).await.expect("migrate");
            })
            .await;
        let state = AppState {
            db: pool.clone(),
            notifiers: journal::Notifiers::default(),
        };
        Some((crate::build_app(state), pool))
    }

    static COUNTER: AtomicU64 = AtomicU64::new(1);

    /// A fresh, unique 32-byte name per call so parallel tests never share
    /// a journal.
    fn fresh_name(tag: u8) -> [u8; PSEUDONYM_LEN] {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut out = [0u8; PSEUDONYM_LEN];
        out[0] = tag;
        out[1..9].copy_from_slice(&n.to_be_bytes());
        out[9..17].copy_from_slice(&now_ms().to_be_bytes());
        out
    }

    fn fresh_nonce() -> [u8; lakko_ds_proof::NONCE_LEN] {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut out = [0u8; lakko_ds_proof::NONCE_LEN];
        out[..8].copy_from_slice(&n.to_be_bytes());
        out[8..].copy_from_slice(&now_ms().to_be_bytes());
        out
    }

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; 32])
    }

    fn pk_b64(key: &SigningKey) -> String {
        B64.encode(key.verifying_key().to_bytes())
    }

    struct Req<'a> {
        method: &'a str,
        uri: &'a str,
        body: Vec<u8>,
        proof: Option<(&'a SigningKey, u64)>,
    }

    fn proof_header(req: &Req<'_>) -> Option<String> {
        let (key, epoch) = req.proof?;
        let (path, query) = req.uri.split_once('?').unwrap_or((req.uri, ""));
        let parts = RequestParts {
            method: req.method,
            canonical_path: path,
            query,
            body: &req.body,
        };
        Some(sign(key, &parts, epoch, now_ms(), fresh_nonce()).encode())
    }

    async fn send(app: &Router, req: Req<'_>) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder().method(req.method).uri(req.uri);
        if !req.body.is_empty() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        if let Some(h) = proof_header(&req) {
            builder = builder.header(PROOF_HEADER_NAME, h);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::from(req.body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, body)
    }

    fn messages_uri(kind: &str, name: &[u8; PSEUDONYM_LEN]) -> String {
        format!("/ds/{kind}/{}/messages", hex::encode(name))
    }

    fn access_body(epoch: u64, key: &SigningKey) -> Vec<u8> {
        serde_json::json!({ "epoch": epoch, "access_pk_b64": pk_b64(key) })
            .to_string()
            .into_bytes()
    }

    async fn claim_group(
        app: &Router,
        gid: &[u8; PSEUDONYM_LEN],
        epoch: u64,
        key: &SigningKey,
        proof: Option<(&SigningKey, u64)>,
    ) -> StatusCode {
        send(
            app,
            Req {
                method: "PUT",
                uri: &format!("/ds/group/{}/access", hex::encode(gid)),
                body: access_body(epoch, key),
                proof,
            },
        )
        .await
        .0
    }

    #[tokio::test]
    async fn an_unclaimed_group_stays_open() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(1);
        let uri = messages_uri("group", &gid);
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &uri,
                body: b"hello".to_vec(),
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_claimed_group_answers_only_a_valid_proof() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(2);
        let k3 = key(3);
        assert_eq!(
            claim_group(&app, &gid, 3, &k3, None).await,
            StatusCode::NO_CONTENT
        );
        let uri = messages_uri("group", &gid);

        // No header.
        let (status, body) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, b"proof required");
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &uri,
                body: b"x".to_vec(),
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Wrong key at the right epoch.
        let wrong = key(9);
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: Some((&wrong, 3)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Right key, unknown epoch.
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: Some((&k3, 4)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Valid: both directions.
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &uri,
                body: b"hello".to_vec(),
                proof: Some((&k3, 3)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, body) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: Some((&k3, 3)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["messages"].as_array().map(Vec::len), Some(1));

        // A proof over a different body does not carry a publish.
        let parts = RequestParts {
            method: "POST",
            canonical_path: &uri,
            query: "",
            body: b"hello",
        };
        let header = sign(&k3, &parts, 3, now_ms(), fresh_nonce()).encode();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&uri)
                    .header(PROOF_HEADER_NAME, header)
                    .body(Body::from("tampered"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_replayed_proof_is_refused() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(4);
        let k = key(5);
        assert_eq!(
            claim_group(&app, &gid, 1, &k, None).await,
            StatusCode::NO_CONTENT
        );
        let uri = format!("{}?after=0", messages_uri("group", &gid));
        let parts = RequestParts {
            method: "GET",
            canonical_path: uri.split('?').next().unwrap(),
            query: "after=0",
            body: b"",
        };
        let header = sign(&k, &parts, 1, now_ms(), fresh_nonce()).encode();
        for expected in [StatusCode::OK, StatusCode::UNAUTHORIZED] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(&uri)
                        .header(PROOF_HEADER_NAME, header.clone())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }

    #[tokio::test]
    async fn registration_rotates_with_a_two_epoch_write_tier() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let gid = fresh_name(6);
        let (k1, k2, k3, k3b) = (key(11), key(12), key(13), key(14));
        // First claim: open.
        assert_eq!(
            claim_group(&app, &gid, 1, &k1, None).await,
            StatusCode::NO_CONTENT
        );
        // A second unproven claim is refused.
        assert_eq!(
            claim_group(&app, &gid, 2, &k2, None).await,
            StatusCode::UNAUTHORIZED
        );
        // Re-sending the registered key is a no-op that needs no proof (the
        // client's claim-on-open).
        assert_eq!(
            claim_group(&app, &gid, 1, &k1, None).await,
            StatusCode::NO_CONTENT
        );
        // Epoch 2 proven under epoch 1.
        assert_eq!(
            claim_group(&app, &gid, 2, &k2, Some((&k1, 1))).await,
            StatusCode::NO_CONTENT
        );
        // Epoch 3 proven under the previous epoch (1) - the grace window.
        assert_eq!(
            claim_group(&app, &gid, 3, &k3, Some((&k1, 1))).await,
            StatusCode::NO_CONTENT
        );
        // Epoch 1 is kept as the read tier.
        let epochs: Vec<(u64,)> =
            sqlx::query_as("SELECT epoch FROM ds_group_access WHERE group_id = ? ORDER BY epoch")
                .bind(&gid[..])
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(epochs, vec![(1,), (2,), (3,)]);
        // A device still at epoch 1 (offline across two commits) can READ the
        // journal to catch up, but can no longer write or vouch.
        let uri = messages_uri("group", &gid);
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: Some((&k1, 1)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &uri,
                body: b"stale".to_vec(),
                proof: Some((&k1, 1)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            claim_group(&app, &gid, 4, &key(15), Some((&k1, 1))).await,
            StatusCode::UNAUTHORIZED
        );
        // A skip (epoch 5) is a conflict even with a valid proof.
        assert_eq!(
            claim_group(&app, &gid, 5, &k3b, Some((&k3, 3))).await,
            StatusCode::CONFLICT
        );
        // Replacing the latest epoch needs the epoch before it, not itself.
        assert_eq!(
            claim_group(&app, &gid, 3, &k3b, Some((&k3, 3))).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            claim_group(&app, &gid, 3, &k3b, Some((&k2, 2))).await,
            StatusCode::NO_CONTENT
        );
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: Some((&k3b, 3)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: Some((&k3, 3)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_claimed_mailbox_is_read_by_its_owner_and_written_by_anyone() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        // A fresh identity per run: the mailbox name derives from it, and a
        // fixed key would find the previous run's rows in a reused test DB.
        let identity = SigningKey::from_seed(&fresh_name(21));
        let device = lakko_ds_proof::mailbox_pseudonym(&identity.verifying_key());
        let access_uri = format!("/ds/mailbox/{}/access", hex::encode(device));
        // A key that is not the one the name is derived from is refused.
        let (status, _) = send(
            &app,
            Req {
                method: "PUT",
                uri: &access_uri,
                body: serde_json::json!({ "identity_pk_b64": pk_b64(&key(22)) })
                    .to_string()
                    .into_bytes(),
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // The right key claims it (twice: idempotent).
        for _ in 0..2 {
            let (status, _) = send(
                &app,
                Req {
                    method: "PUT",
                    uri: &access_uri,
                    body: serde_json::json!({ "identity_pk_b64": pk_b64(&identity) })
                        .to_string()
                        .into_bytes(),
                    proof: None,
                },
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT);
        }
        let uri = messages_uri("mailbox", &device);
        // Anyone still posts.
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &uri,
                body: b"hi".to_vec(),
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        // Reading needs the identity key at the mailbox epoch.
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: Some((&identity, 1)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{uri}?after=0"),
                body: vec![],
                proof: Some((&identity, MAILBOX_EPOCH)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["messages"].as_array().map(Vec::len), Some(1));
    }

    async fn fresh_pow(pool: &MySqlPool) -> serde_json::Value {
        let challenge = lakko_pow_server::issue_challenge(pool, 0).await.unwrap();
        let seed: [u8; 16] = B64.decode(&challenge.seed).unwrap().try_into().unwrap();
        let solved = lakko_pow::solve(&seed, 0).unwrap();
        serde_json::json!({
            "seed": challenge.seed,
            "nonce": B64.encode(solved.nonce),
            "solution": B64.encode(solved.solution),
        })
    }

    #[tokio::test]
    async fn an_empty_knock_is_a_probe_that_stores_nothing() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let gid = fresh_name(8);
        let k = key(41);
        assert_eq!(
            claim_group(&app, &gid, 7, &k, None).await,
            StatusCode::NO_CONTENT
        );
        let knock_uri = format!("/ds/group/{}/knock", hex::encode(gid));
        let body = serde_json::json!({ "pow": fresh_pow(&pool).await, "payloads_b64": [] })
            .to_string()
            .into_bytes();
        let (status, resp) = send(
            &app,
            Req {
                method: "POST",
                uri: &knock_uri,
                body,
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let parsed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(parsed["epoch"], 7);
        assert_eq!(parsed["ids"].as_array().map(Vec::len), Some(0));
        let read_uri = format!("/ds/group/{}/knocks?after=0", hex::encode(gid));
        let (status, body) = send(
            &app,
            Req {
                method: "GET",
                uri: &read_uri,
                body: vec![],
                proof: Some((&k, 7)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["messages"].as_array().map(Vec::len), Some(0));
        // Too many payloads is refused before any PoW is spent.
        let many: Vec<String> = (0..5).map(|_| B64.encode(b"x")).collect();
        let body = serde_json::json!({ "pow": fresh_pow(&pool).await, "payloads_b64": many })
            .to_string()
            .into_bytes();
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &knock_uri,
                body,
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn a_knock_pays_proof_of_work_and_learns_the_epoch() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let gid = fresh_name(7);
        let k = key(31);
        assert_eq!(
            claim_group(&app, &gid, 4, &k, None).await,
            StatusCode::NO_CONTENT
        );
        let knock_uri = format!("/ds/group/{}/knock", hex::encode(gid));

        // Effort 0 so the solver returns at once; the row still has to exist.
        let challenge = lakko_pow_server::issue_challenge(&pool, 0).await.unwrap();
        let seed: [u8; 16] = B64.decode(&challenge.seed).unwrap().try_into().unwrap();
        let solved = lakko_pow::solve(&seed, 0).unwrap();
        let pow = serde_json::json!({
            "seed": challenge.seed,
            "nonce": B64.encode(solved.nonce),
            "solution": B64.encode(solved.solution),
        });
        let body = serde_json::json!({ "pow": pow, "payloads_b64": [B64.encode(b"join me")] })
            .to_string()
            .into_bytes();
        let (status, resp) = send(
            &app,
            Req {
                method: "POST",
                uri: &knock_uri,
                body: body.clone(),
                proof: None,
            },
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&resp)
        );
        let parsed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(parsed["epoch"], 4);
        // The same proof of work does not pay twice.
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &knock_uri,
                body,
                proof: None,
            },
        )
        .await;
        assert_ne!(status, StatusCode::CREATED);

        // The members read it under the group key; a stranger does not.
        let read_uri = format!("/ds/group/{}/knocks?after=0", hex::encode(gid));
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &read_uri,
                body: vec![],
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = send(
            &app,
            Req {
                method: "GET",
                uri: &read_uri,
                body: vec![],
                proof: Some((&k, 4)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let msgs = parsed["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["payload_b64"], B64.encode(b"join me"));
    }
}
