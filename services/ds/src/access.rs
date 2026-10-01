// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0053 - a journal is opened by a key, not by its name.
//!
//! A journal with a registration row (`ds_group_access` / `ds_mailbox_access`)
//! is **claimed**: its gated routes answer only a request that carries a valid
//! [`lakko_ds_proof`] header. A journal with no row keeps the Phase 1.3
//! posture (anyone who names it) - which is every group journal until its
//! owner claims it. A mailbox is different since ADR-0060: it is personal, so
//! its `GET messages` needs the identity proof whether or not it is claimed
//! (an unclaimed one has no key and always answers the uniform `401`); the
//! proofless meeting points moved to their own `/ds/rendezvous/` namespace.
//!
//! What is gated: a claimed group's `GET`/`POST messages` and `GET knocks`
//! (write tier); every mailbox's `GET messages` (the identity proof, whether
//! or not the mailbox is claimed, ADR-0060).
//! A mailbox `POST` is gated by reach keys, not here (ADR-0060,
//! `src/reach.rs`): ADR-0053 left it the open cold inbox, and a claimed
//! mailbox is now closed to a stranger.
//! What is not gated: a group's `POST knock` (it spends an anonymous token
//! instead, ADR-0054 S3).
//!
//! **Who registers epoch keys is ADR-0057's, not this module's**
//! (`src/ownership.rs`): a journal is claimed under a root key, and only the
//! root and the registrars it appoints register epochs. An epoch key opens
//! the journal; it no longer extends it.
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
//! the two newest registered epochs may write (publish, read knocks); every
//! retained older epoch may still **read** the
//! messages. Without the read
//! tier a device offline across two commits would hold only a key the server
//! had dropped, could not fetch the commits that would bring it forward, and
//! would be stranded for good. Old keys are kept for the journal's own
//! retention window - a row older than that is pruned anyway, so a device
//! that far behind is lost regardless - and at most [`MAX_RETAINED_EPOCHS`].
//! The cost, stated: an evicted member can still read ciphertext it cannot
//! decrypt (sizes and timing) until its epoch ages out; it cannot write.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_anon_token::TokenContext;
use lakko_ds_proof::{Proof, RequestParts, VerifyingKey, MAILBOX_EPOCH, PROOF_HEADER_NAME};
use serde::{Deserialize, Serialize};
use sqlx::mysql::{MySql, MySqlConnection, MySqlDatabaseError, MySqlPool};

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
/// The highest epoch a group may register. An MLS epoch advances by one per
/// commit, so a real group stays many orders of magnitude below 2^48; the cap
/// keeps every epoch the server stores far from `u64::MAX`, where epoch
/// arithmetic would overflow. Refused with `400` (ADR-0053, *What the
/// security pass found*).
pub(crate) const MAX_ACCESS_EPOCH: u64 = (1 << 48) - 1;
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

pub(crate) type HandlerError = (StatusCode, String);

/// The one refusal every failed proof maps to.
pub(crate) fn refused() -> HandlerError {
    (StatusCode::UNAUTHORIZED, "proof required".to_string())
}

pub(crate) fn db_error(what: &'static str) -> impl Fn(sqlx::Error) -> HandlerError {
    move |e| {
        tracing::error!(error = %e, "{what}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    }
}

/// The server's clock in unix milliseconds. Saturating, never panicking.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ============================================================================
// Registrations
// ============================================================================

/// The rows of a group's registered access keys, newest epoch first, as the
/// query returns them. A row that does not parse is skipped rather than
/// trusted (this server wrote every row from a validated key).
fn parse_key_rows(rows: Vec<(u64, Vec<u8>)>) -> Vec<(u64, VerifyingKey)> {
    rows.into_iter()
        .filter_map(|(epoch, pk)| VerifyingKey::from_bytes(&pk).ok().map(|key| (epoch, key)))
        .collect()
}

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
    Ok(parse_key_rows(rows))
}

/// Take the group's ownership lock inside a statement transaction. The
/// upsert holds an exclusive record lock on the group's
/// `ds_group_access_lock` row until the transaction ends (a primary-key
/// record lock: no gap, so other groups never wait), and the transaction runs
/// at READ COMMITTED, so every read that follows sees every statement
/// committed before the lock was granted. Two statements about one group
/// therefore serialize instead of both deciding against the same read
/// (ADR-0053 security pass, finding 4; ADR-0057 reuses it for every kind).
pub(crate) async fn lock_group(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<(), HandlerError> {
    sqlx::query(
        "INSERT INTO ds_group_access_lock (group_id) VALUES (?) \
         ON DUPLICATE KEY UPDATE group_id = VALUES(group_id)",
    )
    .bind(&group_id[..])
    .execute(&mut *conn)
    .await
    .map_err(registration_db_error("lock group"))?;
    Ok(())
}

/// Whether the journal has an owner row (ADR-0057).
async fn journal_owned(
    pool: &MySqlPool,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<bool, HandlerError> {
    let row: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM ds_group_owner WHERE group_id = ?")
        .bind(&group_id[..])
        .fetch_optional(pool)
        .await
        .map_err(db_error("load journal owner"))?;
    Ok(row.is_some())
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

/// Parse the proof header and verify it under `key_for_epoch`, touching no
/// store. Every failure is [`refused`]. The nonce is not yet consumed: the
/// caller does that with [`consume_nonce`] on the connection it decides on.
pub(crate) fn verify_header(
    headers: &HeaderMap,
    parts: &RequestParts<'_>,
    key_for_epoch: impl Fn(u64) -> Option<VerifyingKey>,
) -> Result<Proof, HandlerError> {
    let value = headers
        .get(PROOF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(refused)?;
    let proof = Proof::parse(value).map_err(|_| refused())?;
    let key = key_for_epoch(proof.epoch).ok_or_else(refused)?;
    lakko_ds_proof::verify(&proof, parts, &key, now_ms()).map_err(|_| refused())?;
    Ok(proof)
}

/// Record a verified proof's nonce. `false` is a replay. The primary key
/// makes a repeat a duplicate (zero rows affected); any other DB error is an
/// error.
pub(crate) async fn consume_nonce<'e, E>(
    exec: E,
    pseudonym: &[u8; PSEUDONYM_LEN],
    nonce: &[u8; lakko_ds_proof::NONCE_LEN],
) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let inserted =
        sqlx::query("INSERT IGNORE INTO ds_proof_nonces (pseudonym, nonce) VALUES (?, ?)")
            .bind(&pseudonym[..])
            .bind(&nonce[..])
            .execute(exec)
            .await?;
    Ok(inserted.rows_affected() > 0)
}

/// Parse the proof header, verify it under `key_for_epoch`, and consume the
/// nonce. Every failure is [`refused`].
async fn check_proof(
    pool: &MySqlPool,
    pseudonym: &[u8; PSEUDONYM_LEN],
    headers: &HeaderMap,
    parts: &RequestParts<'_>,
    key_for_epoch: impl Fn(u64) -> Option<VerifyingKey>,
) -> Result<(), HandlerError> {
    let proof = verify_header(headers, parts, key_for_epoch)?;
    if consume_nonce(pool, pseudonym, &proof.nonce)
        .await
        .map_err(db_error("record proof nonce"))?
    {
        Ok(())
    } else {
        Err(refused())
    }
}

pub(crate) fn parts<'a>(method: &'a Method, uri: &'a Uri, body: &'a [u8]) -> RequestParts<'a> {
    RequestParts {
        method: method.as_str(),
        canonical_path: uri.path(),
        query: uri.query().unwrap_or(""),
        body,
    }
}

/// Which way a group-journal request passed [`gate_group`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gated {
    /// The group is unclaimed; nothing was checked.
    Unclaimed,
    /// The group is claimed and the request carried a valid proof - the
    /// caller holds an epoch key of the tier it asked for.
    Proven {
        /// The newest epoch registered for the group. A read hands it to the
        /// member, so a device that has consumed the whole journal and is
        /// still behind it can tell it will never catch up (its state forked
        /// from the chain) rather than wait forever.
        latest_epoch: u64,
    },
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
) -> Result<Gated, HandlerError> {
    let keys = group_keys(&state.db, group_id).await?;
    if keys.is_empty() {
        // No epoch keys: unclaimed - unless an owner row exists, which a
        // claim never leaves without an epoch; then nothing opens it.
        return if journal_owned(&state.db, group_id).await? {
            Err(refused())
        } else {
            Ok(Gated::Unclaimed)
        };
    }
    let tier = match access {
        Access::Read => keys.len(),
        Access::Write => WRITE_EPOCHS.min(keys.len()),
    };
    // `group_keys` orders newest first.
    let latest_epoch = keys[0].0;
    let usable = &keys[..tier];
    let req = parts(method, uri, body);
    check_proof(&state.db, group_id, headers, &req, |epoch| {
        usable.iter().find(|(e, _)| *e == epoch).map(|(_, k)| *k)
    })
    .await?;
    Ok(Gated::Proven { latest_epoch })
}

/// Gate a mailbox read: a valid proof under the identity key at
/// `MAILBOX_EPOCH`. An unclaimed mailbox has no key, so every read of it is
/// the uniform [`refused`] - the same answer a claimed mailbox gives a read
/// without a proof - and the answer never tells whether a mailbox is claimed
/// (ADR-0060; proofless meeting points use `/ds/rendezvous/`).
pub(crate) async fn gate_mailbox_read(
    state: &AppState,
    device_id: &[u8; PSEUDONYM_LEN],
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<(), HandlerError> {
    let Some(key) = mailbox_key(&state.db, device_id).await? else {
        return Err(refused());
    };
    let req = parts(method, uri, &[]);
    check_proof(&state.db, device_id, headers, &req, |epoch| {
        (epoch == MAILBOX_EPOCH).then_some(key)
    })
    .await
}

// ============================================================================
// Routes: mailbox claim
// ============================================================================

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

/// MariaDB's "deadlock found" and "lock wait timeout" error numbers.
const ER_LOCK_DEADLOCK: u16 = 1213;
const ER_LOCK_WAIT_TIMEOUT: u16 = 1205;

/// [`db_error`] for the registration transaction: a deadlock or lock-wait
/// timeout against a concurrent registration is a `409` the client retries
/// (that transaction committed nothing), every other failure a `500`.
pub(crate) fn registration_db_error(what: &'static str) -> impl Fn(sqlx::Error) -> HandlerError {
    move |e| {
        let lost_race = e
            .as_database_error()
            .and_then(|d| d.try_downcast_ref::<MySqlDatabaseError>())
            .is_some_and(|m| matches!(m.number(), ER_LOCK_DEADLOCK | ER_LOCK_WAIT_TIMEOUT));
        if lost_race {
            tracing::debug!("{what}: lost a lock race to a concurrent registration");
            return (
                StatusCode::CONFLICT,
                "concurrent registration; retry".to_string(),
            );
        }
        db_error(what)(e)
    }
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
///
/// Spends one `ds-knock` anonymous token (ADR-0054 S3, header
/// `x-lakko-anon-token`), and only after the body has been validated, so a
/// malformed knock costs the caller no token. A refused token answers the
/// gate's stable code (`401 anon-token-*`, or `503 anon-tokens-disabled`).
pub(crate) async fn post_knock(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    headers: HeaderMap,
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
    if let Err(refused) = state
        .anon_tokens
        .spend_header(&headers, TokenContext::DsKnock)
        .await
    {
        return Ok(refused.into_response());
    }
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

/// `GET /ds/group/{hex}/knocks` - the members read what was knocked. Gated
/// to the **write** tier, unlike the messages: a knock row is a cleartext
/// external-join proposal (the joiner's KeyPackage and credential), so the
/// read-only tail an evicted member keeps must not let it go on watching who
/// asks to join.
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
        Access::Write,
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

/// Age out read-tier epoch keys older than the journal's retention window,
/// never touching a group's two newest epochs (a quiet group that has not
/// committed in a month must stay openable). Runs on the journal pruner's tick.
pub(crate) async fn prune_group_access(pool: &MySqlPool, retention_secs: i64) {
    match prune_group_access_rows(pool, retention_secs).await {
        Ok(rows) if rows > 0 => {
            tracing::debug!(rows, "pruned read-tier epoch keys");
        },
        Ok(_) => {},
        Err(e) => tracing::warn!(error = %e, "epoch key prune failed"),
    }
}

/// The statement behind [`prune_group_access`], returning the rows deleted.
///
/// It is one DELETE across every group, so one row must never be able to
/// abort it: the predicate does **no epoch arithmetic** (an earlier
/// `a.epoch + 1 < m.latest` overflowed `BIGINT UNSIGNED` on a row at
/// `u64::MAX` and failed the whole statement, for every group, on every
/// tick). "Not one of the two newest" is expressed as "below the group's
/// second-newest epoch", computed by comparison alone. A group with a single
/// registered epoch has no second-newest row and loses nothing.
async fn prune_group_access_rows(
    pool: &MySqlPool,
    retention_secs: i64,
) -> Result<u64, sqlx::Error> {
    // MariaDB refuses a DELETE whose subquery reads the same table unless the
    // subquery is materialised; a derived table with GROUP BY always is.
    let sql = "DELETE a FROM ds_group_access a \
               JOIN (SELECT b.group_id, MAX(b.epoch) AS second_newest \
                       FROM ds_group_access b \
                       JOIN (SELECT group_id, MAX(epoch) AS newest \
                               FROM ds_group_access GROUP BY group_id) n \
                         ON n.group_id = b.group_id \
                      WHERE b.epoch < n.newest \
                      GROUP BY b.group_id) s \
                 ON s.group_id = a.group_id \
               WHERE a.created_at < NOW(3) - INTERVAL ? SECOND AND a.epoch < s.second_newest";
    Ok(sqlx::query(sql)
        .bind(retention_secs)
        .execute(pool)
        .await?
        .rows_affected())
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
pub(crate) mod tests {
    use super::*;
    use crate::test_support::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::Router;
    use lakko_ds_proof::{sign, SigningKey};
    use sqlx::mysql::MySqlPoolOptions;
    use tower::ServiceExt as _;

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
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // An unclaimed journal has no chain to report.
        assert!(parsed.get("latest_epoch").is_none());
    }

    /// ADR-0054: the wake header is parsed before the append, so a malformed
    /// one stores nothing; a valid one on a claimed publish is accepted (the
    /// test waker is dormant, so nothing leaves the process); a mailbox never
    /// takes one; an unclaimed group validates it and drops the wake.
    #[tokio::test]
    async fn a_wake_header_is_checked_before_the_append() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(7);
        let k = key(7);
        assert_eq!(
            claim(&app, &gid, &fresh_signer(0x52), 1, &k).await,
            StatusCode::NO_CONTENT
        );
        let uri = messages_uri("group", &gid);
        let good = format!(
            "{};message",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([5u8; 32])
        );
        let wake = crate::wake::WAKE_HEADER;

        for bad in [
            "",
            "x;message",
            &format!("{good}x"),
            &good.replace("message", "ring"),
        ] {
            let (status, _) = send_with(
                &app,
                Req {
                    method: "POST",
                    uri: &uri,
                    body: b"never stored".to_vec(),
                    proof: Some((&k, 1)),
                },
                &[(wake, bad)],
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "accepted {bad:?}");
        }
        let fetch = |app: Router| {
            let uri = format!("{uri}?after=0");
            let k = k.clone();
            async move {
                let (status, body) = send(
                    &app,
                    Req {
                        method: "GET",
                        uri: &uri,
                        body: vec![],
                        proof: Some((&k, 1)),
                    },
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                v["messages"].as_array().unwrap().len()
            }
        };
        assert_eq!(fetch(app.clone()).await, 0, "a refused publish left a row");

        let (status, _) = send_with(
            &app,
            Req {
                method: "POST",
                uri: &uri,
                body: b"hello".to_vec(),
                proof: Some((&k, 1)),
            },
            &[(wake, &good)],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(fetch(app.clone()).await, 1);

        let (status, _) = send_with(
            &app,
            Req {
                method: "POST",
                uri: &messages_uri("mailbox", &fresh_name(8)),
                body: b"hi".to_vec(),
                proof: None,
            },
            &[(wake, &good)],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = send_with(
            &app,
            Req {
                method: "POST",
                uri: &messages_uri("group", &fresh_name(9)),
                body: b"hi".to_vec(),
                proof: None,
            },
            &[(wake, &good)],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn a_claimed_group_answers_only_a_valid_proof() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(2);
        let k3 = key(3);
        assert_eq!(
            claim(&app, &gid, &fresh_signer(0x52), 3, &k3).await,
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

    /// A signed read names the newest registered epoch, so a device that has
    /// read the whole journal and is still behind it knows its state forked
    /// from the chain and will never catch up by waiting.
    #[tokio::test]
    async fn a_signed_read_names_the_latest_epoch() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(12);
        let k3 = key(3);
        assert_eq!(
            claim(&app, &gid, &fresh_signer(0x5c), 3, &k3).await,
            StatusCode::NO_CONTENT
        );
        let uri = messages_uri("group", &gid);
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
        assert_eq!(parsed["messages"].as_array().map(Vec::len), Some(0));
        assert_eq!(parsed["latest_epoch"], 3);
    }

    #[tokio::test]
    async fn a_replayed_proof_is_refused() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(4);
        let k = key(5);
        assert_eq!(
            claim(&app, &gid, &fresh_signer(0x52), 1, &k).await,
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
    async fn a_claimed_mailbox_is_read_by_its_owner_only() {
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
        // A proofless post is answered as stored; since ADR-0060 a claimed,
        // closed mailbox drops it (`src/reach.rs` covers the reach keys).
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
        assert_eq!(parsed["messages"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn an_empty_knock_is_a_probe_that_stores_nothing() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(8);
        let k = key(41);
        assert_eq!(
            claim(&app, &gid, &fresh_signer(0x52), 7, &k).await,
            StatusCode::NO_CONTENT
        );
        let knock_uri = format!("/ds/group/{}/knock", hex::encode(gid));
        let (status, resp) = knock(&app, &knock_uri, &[], Some(&knock_token())).await;
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
        // Too many payloads is refused before the token is spent: the same
        // token then pays for a well-formed knock.
        let t = knock_token();
        let many: Vec<&[u8]> = (0..5).map(|_| &b"x"[..]).collect();
        let (status, _) = knock(&app, &knock_uri, &many, Some(&t)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let (status, _) = knock(&app, &knock_uri, &[], Some(&t)).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn a_knock_spends_a_token_once_and_learns_the_epoch() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(7);
        let k = key(31);
        assert_eq!(
            claim(&app, &gid, &fresh_signer(0x52), 4, &k).await,
            StatusCode::NO_CONTENT
        );
        let knock_uri = format!("/ds/group/{}/knock", hex::encode(gid));

        let t = knock_token();
        let (status, resp) = knock(&app, &knock_uri, &[b"join me"], Some(&t)).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&resp)
        );
        let parsed: serde_json::Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(parsed["epoch"], 4);
        // The same token does not pay twice.
        let (status, resp) = knock(&app, &knock_uri, &[b"join me"], Some(&t)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(error_code(&resp), "anon-token-spent");

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

    #[tokio::test]
    async fn a_knock_refuses_a_missing_foreign_or_stale_token() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(9);
        let k = key(51);
        assert_eq!(
            claim(&app, &gid, &fresh_signer(0x52), 2, &k).await,
            StatusCode::NO_CONTENT
        );
        let knock_uri = format!("/ds/group/{}/knock", hex::encode(gid));

        let (status, resp) = knock(&app, &knock_uri, &[], None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(error_code(&resp), "anon-token-required");

        let wrong = token_at(TokenContext::PushRegister, 0);
        let (status, resp) = knock(&app, &knock_uri, &[], Some(&wrong)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(error_code(&resp), "anon-token-wrong-context");

        let previous = token_at(TokenContext::DsKnock, 1);
        let (status, _) = knock(&app, &knock_uri, &[], Some(&previous)).await;
        assert_eq!(status, StatusCode::CREATED);

        let older = token_at(TokenContext::DsKnock, 2);
        let (status, resp) = knock(&app, &knock_uri, &[], Some(&older)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(error_code(&resp), "anon-token-expired");
    }

    /// No database: the refusal happens before any store is touched.
    #[tokio::test]
    async fn a_knock_answers_503_when_tokens_are_disabled() {
        let db = MySqlPoolOptions::new()
            .connect_lazy("mysql://nobody@127.0.0.1:1/none")
            .unwrap();
        let state = AppState {
            db: db.clone(),
            notifiers: journal::Notifiers::default(),
            waker: std::sync::Arc::new(crate::wake::Waker::dormant()),
            anon_tokens: lakko_anon_token_server::TokenVerifier::new(db, None),
            binding_pepper: None,
        };
        let app = crate::build_app(state);
        let uri = format!("/ds/group/{}/knock", hex::encode([3u8; PSEUDONYM_LEN]));
        let (status, resp) = knock(&app, &uri, &[b"x"], Some(&knock_token())).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error_code(&resp), "anon-tokens-disabled");
    }

    /// Security pass, finding 1: rows at and beyond the cap (a row at
    /// `u64::MAX` stands for one written before the cap existed) never abort
    /// the global prune, and the prune still keeps each group's two newest
    /// epochs and nothing else of an aged group.
    #[tokio::test]
    async fn the_pruner_survives_rows_at_and_beyond_the_cap() {
        let Some((_app, pool)) = app().await else {
            return;
        };
        let near = fresh_name(31);
        let beyond = fresh_name(32);
        let fresh = fresh_name(33);
        let pk = key(63).verifying_key().to_bytes();
        let insert = |gid: [u8; PSEUDONYM_LEN], epoch: u64, old: bool| {
            let pool = pool.clone();
            async move {
                let sql = if old {
                    "INSERT INTO ds_group_access \
                       (group_id, epoch, access_pk, registered_by, registered_rank, created_at) \
                     VALUES (?, ?, ?, ?, 3, '2000-01-01 00:00:00')"
                } else {
                    "INSERT INTO ds_group_access \
                       (group_id, epoch, access_pk, registered_by, registered_rank) \
                     VALUES (?, ?, ?, ?, 3)"
                };
                sqlx::query(sql)
                    .bind(&gid[..])
                    .bind(epoch)
                    .bind(&pk[..])
                    .bind(&pk[..])
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        for e in MAX_ACCESS_EPOCH - 3..=MAX_ACCESS_EPOCH {
            insert(near, e, true).await;
        }
        for e in [u64::MAX - 5, u64::MAX - 1, u64::MAX] {
            insert(beyond, e, true).await;
        }
        for e in 1..=4 {
            insert(fresh, e, false).await;
        }
        // Ten years: only the rows back-dated to 2000 are old enough, so the
        // groups of tests running in parallel are never touched.
        prune_group_access_rows(&pool, 10 * 365 * 24 * 60 * 60)
            .await
            .expect("the global prune must not fail on any row");
        let epochs = |gid: [u8; PSEUDONYM_LEN]| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (u64,)>(
                    "SELECT epoch FROM ds_group_access WHERE group_id = ? ORDER BY epoch",
                )
                .bind(&gid[..])
                .fetch_all(&pool)
                .await
                .unwrap()
                .into_iter()
                .map(|(e,)| e)
                .collect::<Vec<_>>()
            }
        };
        assert_eq!(
            epochs(near).await,
            vec![MAX_ACCESS_EPOCH - 1, MAX_ACCESS_EPOCH]
        );
        assert_eq!(epochs(beyond).await, vec![u64::MAX - 1, u64::MAX]);
        assert_eq!(epochs(fresh).await, vec![1, 2, 3, 4]);
    }

    /// Security pass, finding 2: an evicted member keeps the read-only tail
    /// for the messages, but reading knocks takes a write-tier key.
    #[tokio::test]
    async fn knocks_are_read_only_by_the_write_tier() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let gid = fresh_name(34);
        let (k1, k2, k3) = (key(64), key(65), key(66));
        let root = fresh_signer(0x53);
        assert_eq!(
            claim(&app, &gid, &root, 1, &k1).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            register(&app, &gid, &root, 2, &k2, &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            register(&app, &gid, &root, 3, &k3, &[]).await,
            StatusCode::NO_CONTENT
        );
        let read = |uri: String, signer: SigningKey, epoch: u64| {
            let app = app.clone();
            async move {
                send(
                    &app,
                    Req {
                        method: "GET",
                        uri: &uri,
                        body: vec![],
                        proof: Some((&signer, epoch)),
                    },
                )
                .await
                .0
            }
        };
        let messages = format!("{}?after=0", messages_uri("group", &gid));
        let knocks = format!("/ds/group/{}/knocks?after=0", hex::encode(gid));
        assert_eq!(read(messages, k1.clone(), 1).await, StatusCode::OK);
        assert_eq!(read(knocks.clone(), k1, 1).await, StatusCode::UNAUTHORIZED);
        assert_eq!(read(knocks.clone(), k2, 2).await, StatusCode::OK);
        assert_eq!(read(knocks, k3, 3).await, StatusCode::OK);
    }

    /// Security pass, finding 3: on a claimed journal the epoch rule is
    /// answered only to a caller with a write-tier proof; everyone else gets
    /// the same 401 whatever epoch they name.
    /// Security pass, finding 4: concurrent first claims of one journal
    /// serialize. Exactly one lands; every other waits on the group's lock,
    /// then finds the journal claimed and gets its 401 (a lock-wait timeout
    /// would be a 409), and the stored key is the winner's. Before the
    /// transaction every one of them upserted and the last writer won.
    #[test]
    fn the_spent_set_migration_is_the_shared_crate_schema_byte_for_byte() {
        assert_eq!(
            include_str!("../migrations/0003_anon_token_spent.sql"),
            lakko_anon_token_server::SPENT_SET_MIGRATION_SQL
        );
    }
}
