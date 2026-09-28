// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0058 section 2 - knowing a person's identifier is not permission to
//! reach them.
//!
//! A claimed identity mailbox is **closed** unless its owner opened it (the
//! Everyone setting). A closed mailbox stores a write only when the write
//! carries an `x-lakko-ds-proof` at [`REACH_EPOCH`] that verifies under a
//! **reach key** the owner registered here, or a one-write delegation signed
//! by one of them (`x-lakko-ds-delegation`, the five-step contract written in
//! `lakko_ds_proof::delegation`). Reach keys are handed out by the owner - to
//! contacts, link holders, the members of groups and channels it lets find it
//! - and never looked up; the server holds only their public halves.
//!
//! What the answers reveal, and what they must not:
//!
//! - A write with **no proof** to a closed mailbox is answered exactly as a
//!   stored write to an unclaimed one (`201` with a real id and timestamp,
//!   see `journal::publish_dropped`) and nothing is stored, so a stranger
//!   cannot tell a closed mailbox from an unused pseudonym by writing to it.
//! - A write with a proof that does not verify is the one ADR-0053 `401`,
//!   whatever the mailbox's state (claimed or not, open or closed). Only a
//!   writer who once held a valid key can learn that it stopped working.
//! - An **open** mailbox takes a proofless write; a write that does carry a
//!   proof is judged as anywhere else.
//!
//! Registration (`PUT /ds/mailbox/{hex}/reach`) is signed by the identity
//! key the mailbox is named after, at `MAILBOX_EPOCH` (ADR-0053's mailbox
//! proof), and applied in one transaction behind the mailbox row's lock.
//!
//! The reach check (`POST /ds/mailbox/{hex}/reach/check`) answers `204` when
//! the proof's key is registered and unexpired, `401` otherwise; a searcher
//! calls it before showing a result so a narrowed setting takes effect at the
//! next search. Its per-address rate limit is nginx's
//! (`infra/nginx/.../lakko.conf`), the layer the DS relies on for every
//! anonymous route.
//!
//! **Finding the key.** A reach proof names the key that signed it (the
//! proof header's sixth field, inside the signed bytes - see
//! `lakko_ds_proof`), so a direct reach write costs one indexed lookup of
//! exactly that key (registered for this mailbox, unexpired) and one
//! signature verification; a proof that names an unregistered key is refused
//! without verifying anything. A delegated write costs one lookup (its
//! issuer) and two verifications, and its proof must name the delegate key.

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_ds_proof::{
    verify_delegated_proof, verify_delegation, Delegation, Proof, RequestParts, VerifyingKey,
    DELEGATION_HEADER_NAME, MAILBOX_EPOCH, NONCE_LEN, PROOF_HEADER_NAME, REACH_EPOCH,
};
use serde::Deserialize;
use sqlx::mysql::{MySql, MySqlPool};
use sqlx::QueryBuilder;

use crate::access::{consume_nonce, db_error, now_ms, parts, refused, verify_header, HandlerError};
use crate::journal::{self, PSEUDONYM_LEN};
use crate::AppState;

/// The most reach keys one mailbox may hold. Mirrors
/// `lakko_contacts::visibility::MAX_REACH_STATE_ENTRIES` (4096), the bound on
/// the reach state a client keeps, so an honest client never meets it. Chosen,
/// not measured. (A direct reach write costs one lookup and one verification
/// whatever the count, since the proof names its key.)
pub(crate) const MAX_REACH_KEYS: usize = 4096;

/// What the mailbox gate decided for a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MailboxWrite {
    /// Store it.
    Store,
    /// Answer as if stored, store nothing (a proofless write to a closed
    /// mailbox).
    Drop,
}

/// A claimed mailbox's `is_open` flag, or `None` when unclaimed.
async fn mailbox_open(
    pool: &MySqlPool,
    mailbox: &[u8; PSEUDONYM_LEN],
) -> Result<Option<bool>, HandlerError> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT is_open FROM ds_mailbox_access WHERE device_id = ?")
            .bind(&mailbox[..])
            .fetch_optional(pool)
            .await
            .map_err(db_error("load mailbox state"))?;
    Ok(row.map(|(open,)| open))
}

/// Whether `issuer_pk` is an unexpired reach key of the mailbox (the direct
/// writer's named key, or a delegation's issuer).
async fn reach_key_registered(
    pool: &MySqlPool,
    mailbox: &[u8; PSEUDONYM_LEN],
    issuer_pk: &[u8; 32],
    now: u64,
) -> Result<bool, HandlerError> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM ds_mailbox_reach \
         WHERE mailbox = ? AND reach_pk = ? \
           AND (expires_at_ms IS NULL OR expires_at_ms > ?)",
    )
    .bind(&mailbox[..])
    .bind(&issuer_pk[..])
    .bind(now)
    .fetch_optional(pool)
    .await
    .map_err(db_error("look up reach key"))?;
    Ok(row.is_some())
}

/// Parse the proof header and require the reach epoch. Every failure is the
/// uniform refusal.
fn parse_reach_proof(headers: &HeaderMap) -> Result<Proof, HandlerError> {
    let value = headers
        .get(PROOF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(refused)?;
    let proof = Proof::parse(value).map_err(|_| refused())?;
    if proof.epoch != REACH_EPOCH {
        return Err(refused());
    }
    Ok(proof)
}

/// The `lakko_ds_proof::delegation` contract, steps 1 to 5, for a write to
/// `mailbox` carrying both headers. `proof` is already parsed at the reach
/// epoch.
async fn check_delegated(
    pool: &MySqlPool,
    mailbox: &[u8; PSEUDONYM_LEN],
    headers: &HeaderMap,
    proof: &Proof,
    req: &RequestParts<'_>,
    now: u64,
) -> Result<(), HandlerError> {
    // 1. Parse, bounded and fail-closed.
    let value = headers
        .get(DELEGATION_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(refused)?;
    let delegation = Delegation::parse_header_value(value).map_err(|_| refused())?;
    // 2. Mailbox, expiry, lifetime bound, issuer signature.
    verify_delegation(&delegation, mailbox, now).map_err(|_| refused())?;
    // 3. The issuer is a registered, unexpired reach key of this mailbox.
    if !reach_key_registered(pool, mailbox, &delegation.issuer_pk, now).await? {
        return Err(refused());
    }
    // 4. The request proof names the delegate's key and verifies under it.
    verify_delegated_proof(&delegation, proof, req, now).map_err(|_| refused())?;
    // 5. Both nonces, in one transaction: a replay of either consumes neither.
    let mut tx = pool.begin().await.map_err(db_error("begin delegation"))?;
    let fresh_delegation = sqlx::query(
        "INSERT IGNORE INTO ds_delegation_nonces (mailbox, nonce, expires_at_ms) VALUES (?, ?, ?)",
    )
    .bind(&mailbox[..])
    .bind(&delegation.nonce[..])
    .bind(delegation.expires_at_ms)
    .execute(&mut *tx)
    .await
    .map_err(db_error("record delegation nonce"))?
    .rows_affected()
        > 0;
    if !fresh_delegation {
        return Err(refused());
    }
    if !consume_nonce(&mut *tx, mailbox, &proof.nonce)
        .await
        .map_err(db_error("record proof nonce"))?
    {
        return Err(refused());
    }
    tx.commit().await.map_err(db_error("commit delegation"))?;
    Ok(())
}

/// Gate a mailbox write (ADR-0058 section 2). See the module documentation
/// for the table of answers.
pub(crate) async fn gate_mailbox_write(
    state: &AppState,
    mailbox: &[u8; PSEUDONYM_LEN],
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<MailboxWrite, HandlerError> {
    let has_proof = headers.contains_key(PROOF_HEADER_NAME);
    let has_delegation = headers.contains_key(DELEGATION_HEADER_NAME);
    let open = mailbox_open(&state.db, mailbox).await?;
    if !has_proof {
        // A delegation is only ever presented with the delegate's proof.
        if has_delegation {
            return Err(refused());
        }
        return Ok(match open {
            None | Some(true) => MailboxWrite::Store,
            Some(false) => MailboxWrite::Drop,
        });
    }
    // A proof is judged the same way whatever the mailbox's state. An
    // unclaimed mailbox has no reach keys, so nothing can verify.
    let proof = parse_reach_proof(headers)?;
    if open.is_none() {
        return Err(refused());
    }
    let req = parts(method, uri, body);
    let now = now_ms();
    if has_delegation {
        check_delegated(&state.db, mailbox, headers, &proof, &req, now).await?;
    } else {
        check_direct(&state.db, mailbox, &proof, &req, now).await?;
    }
    Ok(MailboxWrite::Store)
}

/// A direct reach proof: the key it names is an unexpired reach key of the
/// mailbox, the proof verifies under that key, and its nonce is fresh.
async fn check_direct(
    pool: &MySqlPool,
    mailbox: &[u8; PSEUDONYM_LEN],
    proof: &Proof,
    req: &RequestParts<'_>,
    now: u64,
) -> Result<(), HandlerError> {
    // `parse_reach_proof` only returns reach proofs, which always name a key.
    let named = proof.reach_key.ok_or_else(refused)?;
    if !reach_key_registered(pool, mailbox, &named, now).await? {
        return Err(refused());
    }
    let key = VerifyingKey::from_bytes(&named).map_err(|_| refused())?;
    lakko_ds_proof::verify(proof, req, &key, now).map_err(|_| refused())?;
    if consume_nonce(pool, mailbox, &proof.nonce)
        .await
        .map_err(db_error("record proof nonce"))?
    {
        Ok(())
    } else {
        Err(refused())
    }
}

// ============================================================================
// Routes
// ============================================================================

/// One key to add in `PUT /ds/mailbox/{hex}/reach`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReachAdd {
    /// Standard base64 of the 32-byte Ed25519 verifying key.
    pub(crate) reach_pk_b64: String,
    /// Unix milliseconds after which the key stops being honoured; absent
    /// for a key that lives until removed.
    #[serde(default)]
    pub(crate) expires_at_ms: Option<u64>,
}

/// Body of `PUT /ds/mailbox/{hex}/reach`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReachBody {
    /// Keys to register (an existing key has its expiry replaced).
    #[serde(default)]
    pub(crate) add: Vec<ReachAdd>,
    /// Standard base64 of keys to unregister (an absent key is ignored).
    #[serde(default)]
    pub(crate) remove: Vec<String>,
    /// Set the Everyone flag; absent leaves it as it is.
    #[serde(default)]
    pub(crate) open: Option<bool>,
}

fn bad_request(msg: impl Into<String>) -> HandlerError {
    (StatusCode::BAD_REQUEST, msg.into())
}

fn decode_reach_pk(b64: &str) -> Result<[u8; 32], HandlerError> {
    let bytes = B64
        .decode(b64)
        .map_err(|_| bad_request("reach_pk_b64: bad base64"))?;
    let key = VerifyingKey::from_bytes(&bytes)
        .map_err(|_| bad_request("reach_pk_b64: not an Ed25519 key"))?;
    Ok(key.to_bytes())
}

/// A validated reach update: keys decoded, no key twice, no key both added
/// and removed.
struct ReachUpdate {
    add: Vec<([u8; 32], Option<u64>)>,
    remove: Vec<[u8; 32]>,
    open: Option<bool>,
}

fn validate(body: ReachBody) -> Result<ReachUpdate, HandlerError> {
    if body.add.len() > MAX_REACH_KEYS || body.remove.len() > MAX_REACH_KEYS {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("at most {MAX_REACH_KEYS} keys to add and {MAX_REACH_KEYS} to remove"),
        ));
    }
    let mut seen = std::collections::HashSet::with_capacity(body.add.len() + body.remove.len());
    let mut add = Vec::with_capacity(body.add.len());
    for entry in &body.add {
        let pk = decode_reach_pk(&entry.reach_pk_b64)?;
        if !seen.insert(pk) {
            return Err(bad_request("a reach key appears twice"));
        }
        add.push((pk, entry.expires_at_ms));
    }
    let mut remove = Vec::with_capacity(body.remove.len());
    for b64 in &body.remove {
        let pk = decode_reach_pk(b64)?;
        if !seen.insert(pk) {
            return Err(bad_request("a reach key appears twice"));
        }
        remove.push(pk);
    }
    Ok(ReachUpdate {
        add,
        remove,
        open: body.open,
    })
}

/// `PUT /ds/mailbox/{hex}/reach` - add and remove reach keys, and set the
/// Everyone flag, in one transaction.
///
/// Signed by the identity key the mailbox is named after (`MAILBOX_EPOCH`);
/// an unclaimed mailbox has no such key and answers the uniform `401`.
/// Idempotent: re-adding a key replaces its expiry, removing an absent key
/// changes nothing. More than [`MAX_REACH_KEYS`] unexpired keys after the
/// update is a `409` with nothing applied.
pub(crate) async fn put_mailbox_reach(
    State(state): State<AppState>,
    AxumPath(device_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, HandlerError> {
    let mailbox = journal::decode_pseudonym(&device_hex)?;
    let parsed: ReachBody =
        serde_json::from_slice(&body).map_err(|e| bad_request(format!("bad json: {e}")))?;
    let update = validate(parsed)?;
    let req = parts(&method, &uri, &body);
    let now = now_ms();

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(db_error("acquire reach connection"))?;
    // READ COMMITTED for the next transaction on this connection only, as the
    // group registration does: no statement in it takes gap locks, so updates
    // of different mailboxes never wait on each other (the mailbox row lock
    // below is what serializes updates of one mailbox).
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *conn)
        .await
        .map_err(db_error("set reach isolation"))?;
    // A DB failure or any refusal drops `tx`, which rolls everything back -
    // the nonce included, so a refused update leaves no trace.
    let mut tx = sqlx::Connection::begin(&mut *conn)
        .await
        .map_err(db_error("begin reach update"))?;
    // The row lock serializes updates of one mailbox.
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT identity_pk FROM ds_mailbox_access WHERE device_id = ? FOR UPDATE")
            .bind(&mailbox[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error("lock mailbox"))?;
    let key = row
        .and_then(|(pk,)| VerifyingKey::from_bytes(&pk).ok())
        .ok_or_else(refused)?;
    let proof = verify_header(&headers, &req, |epoch| {
        (epoch == MAILBOX_EPOCH).then_some(key)
    })?;
    if !consume_nonce(&mut *tx, &mailbox, &proof.nonce)
        .await
        .map_err(db_error("record proof nonce"))?
    {
        return Err(refused());
    }

    // Expired keys first, so they neither count against the cap nor linger.
    sqlx::query(
        "DELETE FROM ds_mailbox_reach \
         WHERE mailbox = ? AND expires_at_ms IS NOT NULL AND expires_at_ms <= ?",
    )
    .bind(&mailbox[..])
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(db_error("drop expired reach keys"))?;

    if !update.remove.is_empty() {
        let mut q: QueryBuilder<MySql> =
            QueryBuilder::new("DELETE FROM ds_mailbox_reach WHERE mailbox = ");
        q.push_bind(&mailbox[..]);
        q.push(" AND reach_pk IN (");
        let mut list = q.separated(", ");
        for pk in &update.remove {
            list.push_bind(&pk[..]);
        }
        list.push_unseparated(")");
        q.build()
            .execute(&mut *tx)
            .await
            .map_err(db_error("remove reach keys"))?;
    }

    if !update.add.is_empty() {
        let mut q: QueryBuilder<MySql> =
            QueryBuilder::new("INSERT INTO ds_mailbox_reach (mailbox, reach_pk, expires_at_ms) ");
        q.push_values(&update.add, |mut row, (pk, expires)| {
            row.push_bind(&mailbox[..])
                .push_bind(&pk[..])
                .push_bind(*expires);
        });
        q.push(" ON DUPLICATE KEY UPDATE expires_at_ms = VALUES(expires_at_ms)");
        q.build()
            .execute(&mut *tx)
            .await
            .map_err(db_error("add reach keys"))?;
    }

    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM ds_mailbox_reach WHERE mailbox = ?")
            .bind(&mailbox[..])
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error("count reach keys"))?;
    if usize::try_from(count).map_or(true, |n| n > MAX_REACH_KEYS) {
        return Err((
            StatusCode::CONFLICT,
            format!("a mailbox holds at most {MAX_REACH_KEYS} reach keys"),
        ));
    }

    if let Some(open) = update.open {
        sqlx::query("UPDATE ds_mailbox_access SET is_open = ? WHERE device_id = ?")
            .bind(open)
            .bind(&mailbox[..])
            .execute(&mut *tx)
            .await
            .map_err(db_error("set mailbox open flag"))?;
    }

    tx.commit().await.map_err(db_error("commit reach update"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /ds/mailbox/{hex}/reach/check` - `204` when the key the reach proof
/// names is registered for this mailbox, unexpired, and signed the proof; the
/// uniform `401` otherwise
/// (an unclaimed mailbox included). The proof's nonce is consumed like any
/// other. No token: the proof is the gate; the per-address rate limit is
/// nginx's.
pub(crate) async fn post_reach_check(
    State(state): State<AppState>,
    AxumPath(device_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, HandlerError> {
    let mailbox = journal::decode_pseudonym(&device_hex)?;
    let proof = parse_reach_proof(&headers)?;
    if mailbox_open(&state.db, &mailbox).await?.is_none() {
        return Err(refused());
    }
    let req = parts(&method, &uri, &body);
    check_direct(&state.db, &mailbox, &proof, &req, now_ms()).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Delete expired reach keys and the nonces of expired delegations. Runs on
/// the journal pruner's tick.
pub(crate) async fn prune_expired(pool: &MySqlPool) {
    let now = now_ms();
    for (what, sql) in [
        (
            "reach keys",
            "DELETE FROM ds_mailbox_reach WHERE expires_at_ms IS NOT NULL AND expires_at_ms <= ?",
        ),
        (
            "delegation nonces",
            "DELETE FROM ds_delegation_nonces WHERE expires_at_ms <= ?",
        ),
    ] {
        match sqlx::query(sql).bind(now).execute(pool).await {
            Ok(r) if r.rows_affected() > 0 => {
                tracing::debug!(rows = r.rows_affected(), what, "pruned expired rows");
            },
            Ok(_) => {},
            Err(e) => tracing::warn!(error = %e, what, "reach prune failed"),
        }
    }
}

// `ds_delegation_nonces.nonce` is BINARY(16).
const _: () = assert!(NONCE_LEN == 16);

// ============================================================================
// Tests - gated on `LAKKO_DS_TEST_DATABASE_URL`, skipped silently without it
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::tests::{
        app, fresh_name, fresh_nonce, key, messages_uri, pk_b64, send, send_with, Req,
    };
    use axum::Router;
    use lakko_ds_proof::{sign, sign_delegation, SigningKey};

    /// A fresh claimed mailbox: its identity key and pseudonym.
    async fn claimed_mailbox(app: &Router, tag: u8) -> (SigningKey, [u8; PSEUDONYM_LEN]) {
        let identity = SigningKey::from_seed(&fresh_name(tag));
        let mailbox = lakko_ds_proof::mailbox_pseudonym(&identity.verifying_key());
        let (status, _) = send(
            app,
            Req {
                method: "PUT",
                uri: &format!("/ds/mailbox/{}/access", hex::encode(mailbox)),
                body: serde_json::json!({ "identity_pk_b64": pk_b64(&identity) })
                    .to_string()
                    .into_bytes(),
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        (identity, mailbox)
    }

    fn reach_uri(mailbox: &[u8; PSEUDONYM_LEN]) -> String {
        format!("/ds/mailbox/{}/reach", hex::encode(mailbox))
    }

    async fn put_reach(
        app: &Router,
        mailbox: &[u8; PSEUDONYM_LEN],
        signer: Option<(&SigningKey, u64)>,
        body: &serde_json::Value,
    ) -> StatusCode {
        send(
            app,
            Req {
                method: "PUT",
                uri: &reach_uri(mailbox),
                body: body.to_string().into_bytes(),
                proof: signer,
            },
        )
        .await
        .0
    }

    async fn add_reach(
        app: &Router,
        identity: &SigningKey,
        mailbox: &[u8; PSEUDONYM_LEN],
        reach: &SigningKey,
        expires_at_ms: Option<u64>,
    ) {
        let body = serde_json::json!({
            "add": [{ "reach_pk_b64": pk_b64(reach), "expires_at_ms": expires_at_ms }],
        });
        assert_eq!(
            put_reach(app, mailbox, Some((identity, MAILBOX_EPOCH)), &body).await,
            StatusCode::NO_CONTENT
        );
    }

    async fn write(
        app: &Router,
        mailbox: &[u8; PSEUDONYM_LEN],
        body: &[u8],
        proof: Option<(&SigningKey, u64)>,
        extra: &[(&str, &str)],
    ) -> (StatusCode, serde_json::Value) {
        let (status, bytes) = send_with(
            app,
            Req {
                method: "POST",
                uri: &messages_uri("mailbox", mailbox),
                body: body.to_vec(),
                proof,
            },
            extra,
        )
        .await;
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    /// The payloads the owner reads back from its mailbox.
    async fn stored(
        app: &Router,
        identity: &SigningKey,
        mailbox: &[u8; PSEUDONYM_LEN],
    ) -> Vec<Vec<u8>> {
        let (status, body) = send(
            app,
            Req {
                method: "GET",
                uri: &format!("{}?after=0", messages_uri("mailbox", mailbox)),
                body: vec![],
                proof: Some((identity, MAILBOX_EPOCH)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| B64.decode(m["payload_b64"].as_str().unwrap()).unwrap())
            .collect()
    }

    async fn check(app: &Router, mailbox: &[u8; PSEUDONYM_LEN], signer: &SigningKey) -> StatusCode {
        send(
            app,
            Req {
                method: "POST",
                uri: &format!("{}/check", reach_uri(mailbox)),
                body: vec![],
                proof: Some((signer, REACH_EPOCH)),
            },
        )
        .await
        .0
    }

    /// The fields of a publish response, which must be the same for a stored
    /// and a dropped write.
    fn response_shape(v: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    #[tokio::test]
    async fn a_closed_mailbox_answers_a_proofless_write_as_if_stored_and_drops_it() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let unclaimed = fresh_name(40);
        let (identity, closed) = claimed_mailbox(&app, 41).await;

        let (s1, first) = write(&app, &unclaimed, b"to nobody", None, &[]).await;
        let (s2, dropped) = write(&app, &closed, b"to a stranger", None, &[]).await;
        let (s3, last) = write(&app, &unclaimed, b"to nobody again", None, &[]).await;
        assert_eq!(
            (s1, s2, s3),
            (
                StatusCode::CREATED,
                StatusCode::CREATED,
                StatusCode::CREATED
            )
        );
        assert_eq!(response_shape(&first), response_shape(&dropped));
        // The dropped write drew a real id from the same sequence.
        let id = |v: &serde_json::Value| v["id"].as_u64().unwrap();
        assert!(id(&first) < id(&dropped) && id(&dropped) < id(&last));
        assert!(dropped["created_at"].is_string());

        assert!(stored(&app, &identity, &closed).await.is_empty());
    }

    #[tokio::test]
    async fn a_registered_reach_key_writes_and_others_are_refused() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (identity, mailbox) = claimed_mailbox(&app, 42).await;
        let reach = key(43);
        add_reach(&app, &identity, &mailbox, &reach, None).await;

        let (status, _) = write(&app, &mailbox, b"hello", Some((&reach, REACH_EPOCH)), &[]).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            stored(&app, &identity, &mailbox).await,
            vec![b"hello".to_vec()]
        );

        // A key that was never registered.
        let (status, _) = write(&app, &mailbox, b"x", Some((&key(44), REACH_EPOCH)), &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The registered key at another epoch label.
        let (status, _) = write(&app, &mailbox, b"x", Some((&reach, MAILBOX_EPOCH)), &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The identity key does not write to its own closed mailbox.
        let (status, _) = write(&app, &mailbox, b"x", Some((&identity, REACH_EPOCH)), &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // A replayed proof.
        let uri = messages_uri("mailbox", &mailbox);
        let header = sign(
            &reach,
            &RequestParts {
                method: "POST",
                canonical_path: &uri,
                query: "",
                body: b"once",
            },
            REACH_EPOCH,
            now_ms(),
            fresh_nonce(),
        )
        .encode();
        let (first, _) = write(
            &app,
            &mailbox,
            b"once",
            None,
            &[(PROOF_HEADER_NAME, &header)],
        )
        .await;
        let (second, _) = write(
            &app,
            &mailbox,
            b"once",
            None,
            &[(PROOF_HEADER_NAME, &header)],
        )
        .await;
        assert_eq!(
            (first, second),
            (StatusCode::CREATED, StatusCode::UNAUTHORIZED)
        );
        // A proof to an unclaimed mailbox is refused like any failing proof.
        let (status, _) = write(
            &app,
            &fresh_name(45),
            b"x",
            Some((&reach, REACH_EPOCH)),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Removed, the key stops working at once.
        let body = serde_json::json!({ "remove": [pk_b64(&reach)] });
        assert_eq!(
            put_reach(&app, &mailbox, Some((&identity, MAILBOX_EPOCH)), &body).await,
            StatusCode::NO_CONTENT
        );
        let (status, _) = write(&app, &mailbox, b"x", Some((&reach, REACH_EPOCH)), &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(stored(&app, &identity, &mailbox).await.len(), 2);
    }

    #[tokio::test]
    async fn an_expired_reach_key_is_refused() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (identity, mailbox) = claimed_mailbox(&app, 46).await;
        let expired = key(47);
        let live = key(48);
        add_reach(&app, &identity, &mailbox, &expired, Some(now_ms() - 1)).await;
        add_reach(&app, &identity, &mailbox, &live, Some(now_ms() + 3_600_000)).await;
        let (status, _) = write(&app, &mailbox, b"x", Some((&expired, REACH_EPOCH)), &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            check(&app, &mailbox, &expired).await,
            StatusCode::UNAUTHORIZED
        );
        let (status, _) = write(&app, &mailbox, b"y", Some((&live, REACH_EPOCH)), &[]).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn a_delegation_is_good_for_exactly_one_write() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (identity, mailbox) = claimed_mailbox(&app, 50).await;
        let issuer_seed = [51u8; 32];
        let issuer = SigningKey::from_seed(&issuer_seed);
        add_reach(&app, &identity, &mailbox, &issuer, None).await;
        let delegate = key(52);
        let delegate_pk = delegate.verifying_key().to_bytes();
        let delegation = |mailbox: &[u8; 32], expires: u64| {
            sign_delegation(&issuer_seed, mailbox, &delegate_pk, expires, fresh_nonce())
                .to_header_value()
        };
        let hour = now_ms() + 3_600_000;

        let good = delegation(&mailbox, hour);
        let (status, _) = write(
            &app,
            &mailbox,
            b"welcome",
            Some((&delegate, REACH_EPOCH)),
            &[(DELEGATION_HEADER_NAME, &good)],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        // The same delegation with a fresh proof: spent.
        let (status, _) = write(
            &app,
            &mailbox,
            b"again",
            Some((&delegate, REACH_EPOCH)),
            &[(DELEGATION_HEADER_NAME, &good)],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            stored(&app, &identity, &mailbox).await,
            vec![b"welcome".to_vec()]
        );

        let refused_with = |header: String, signer: SigningKey| {
            let app = app.clone();
            async move {
                write(
                    &app,
                    &mailbox,
                    b"x",
                    Some((&signer, REACH_EPOCH)),
                    &[(DELEGATION_HEADER_NAME, &header)],
                )
                .await
                .0
            }
        };
        // For another mailbox.
        assert_eq!(
            refused_with(delegation(&fresh_name(53), hour), delegate.clone()).await,
            StatusCode::UNAUTHORIZED
        );
        // Expired.
        assert_eq!(
            refused_with(delegation(&mailbox, now_ms() - 1), delegate.clone()).await,
            StatusCode::UNAUTHORIZED
        );
        // Lives longer than the 24-hour bound.
        assert_eq!(
            refused_with(
                delegation(
                    &mailbox,
                    now_ms() + lakko_ds_proof::MAX_DELEGATION_TTL_MS + 60_000
                ),
                delegate.clone()
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        // Signed by a key the mailbox never registered.
        let stranger = sign_delegation(&[54u8; 32], &mailbox, &delegate_pk, hour, fresh_nonce())
            .to_header_value();
        assert_eq!(
            refused_with(stranger, delegate.clone()).await,
            StatusCode::UNAUTHORIZED
        );
        // The request signed by someone other than the delegate.
        assert_eq!(
            refused_with(delegation(&mailbox, hour), key(55)).await,
            StatusCode::UNAUTHORIZED
        );
        // A delegation with no proof at all.
        let (status, _) = write(
            &app,
            &mailbox,
            b"x",
            None,
            &[(DELEGATION_HEADER_NAME, &delegation(&mailbox, hour))],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(stored(&app, &identity, &mailbox).await.len(), 1);
    }

    #[tokio::test]
    async fn an_open_mailbox_takes_proofless_writes_and_still_judges_proofs() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (identity, mailbox) = claimed_mailbox(&app, 60).await;
        let set_open = |open: bool| {
            let app = app.clone();
            let identity = identity.clone();
            async move {
                put_reach(
                    &app,
                    &mailbox,
                    Some((&identity, MAILBOX_EPOCH)),
                    &serde_json::json!({ "open": open }),
                )
                .await
            }
        };
        assert_eq!(set_open(true).await, StatusCode::NO_CONTENT);
        let (status, _) = write(&app, &mailbox, b"from anyone", None, &[]).await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = write(&app, &mailbox, b"x", Some((&key(61), REACH_EPOCH)), &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(set_open(false).await, StatusCode::NO_CONTENT);
        let (status, _) = write(&app, &mailbox, b"dropped", None, &[]).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            stored(&app, &identity, &mailbox).await,
            vec![b"from anyone".to_vec()]
        );
    }

    #[tokio::test]
    async fn the_reach_check_answers_whether_a_key_is_registered() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (identity, mailbox) = claimed_mailbox(&app, 62).await;
        let reach = key(63);
        add_reach(&app, &identity, &mailbox, &reach, None).await;
        assert_eq!(check(&app, &mailbox, &reach).await, StatusCode::NO_CONTENT);
        assert_eq!(
            check(&app, &mailbox, &key(64)).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            check(&app, &fresh_name(65), &reach).await,
            StatusCode::UNAUTHORIZED
        );
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &format!("{}/check", reach_uri(&mailbox)),
                body: vec![],
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_reach_proof_is_checked_under_the_key_it_names_only() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (identity, mailbox) = claimed_mailbox(&app, 80).await;
        let reach = key(81);
        let other = key(82);
        add_reach(&app, &identity, &mailbox, &reach, None).await;
        add_reach(&app, &identity, &mailbox, &other, None).await;
        let uri = messages_uri("mailbox", &mailbox);
        let signed = |signer: &SigningKey, body: &'static [u8]| {
            sign(
                signer,
                &RequestParts {
                    method: "POST",
                    canonical_path: &uri,
                    query: "",
                    body,
                },
                REACH_EPOCH,
                now_ms(),
                fresh_nonce(),
            )
        };
        // Reach proof whose named key was swapped for another registered key:
        // the signature no longer covers the bytes, refused.
        let mut swapped = signed(&reach, b"x");
        swapped.reach_key = Some(other.verifying_key().to_bytes());
        let (status, _) = write(
            &app,
            &mailbox,
            b"x",
            None,
            &[(PROOF_HEADER_NAME, &swapped.encode())],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // A reach proof with the key field stripped does not parse.
        let full = signed(&reach, b"y").encode();
        let (stripped, _) = full.rsplit_once('.').unwrap();
        let (status, _) = write(&app, &mailbox, b"y", None, &[(PROOF_HEADER_NAME, stripped)]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The named key's proof over another body than it signed: refused;
        // over the signed body: stored.
        let (status, _) = write(&app, &mailbox, b"z", None, &[(PROOF_HEADER_NAME, &full)]).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "the body differs from the signed one"
        );
        let (status, _) = write(
            &app,
            &mailbox,
            b"y",
            None,
            &[(PROOF_HEADER_NAME, &signed(&reach, b"y").encode())],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(stored(&app, &identity, &mailbox).await, vec![b"y".to_vec()]);
    }

    #[tokio::test]
    async fn a_mailbox_read_does_not_say_whether_the_mailbox_is_claimed() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (identity, claimed) = claimed_mailbox(&app, 83).await;
        // An identity whose mailbox nobody claimed.
        let stranger_identity = SigningKey::from_seed(&fresh_name(84));
        let unclaimed = lakko_ds_proof::mailbox_pseudonym(&stranger_identity.verifying_key());
        let read = |mailbox: [u8; PSEUDONYM_LEN], proof: Option<(&SigningKey, u64)>| {
            let app = app.clone();
            let proof = proof.map(|(k, e)| (k.clone(), e));
            async move {
                send(
                    &app,
                    Req {
                        method: "GET",
                        uri: &format!("{}?after=0", messages_uri("mailbox", &mailbox)),
                        body: vec![],
                        proof: proof.as_ref().map(|(k, e)| (k, *e)),
                    },
                )
                .await
            }
        };
        let answers = [
            read(unclaimed, None).await,
            read(claimed, None).await,
            // The key the unclaimed pseudonym is named after, at the mailbox
            // epoch: still no owner on record, the same refusal.
            read(unclaimed, Some((&stranger_identity, MAILBOX_EPOCH))).await,
            read(claimed, Some((&key(85), MAILBOX_EPOCH))).await,
        ];
        for (status, body) in &answers {
            assert_eq!(*status, StatusCode::UNAUTHORIZED);
            assert_eq!(body, &answers[0].1, "every refusal is byte-identical");
        }
        // The owner still reads.
        assert_eq!(
            read(claimed, Some((&identity, MAILBOX_EPOCH))).await.0,
            StatusCode::OK
        );
        // And writes are indistinguishable too: a proofless post to either is
        // answered as stored.
        let (s1, _) = write(&app, &unclaimed, b"a", None, &[]).await;
        let (s2, _) = write(&app, &claimed, b"b", None, &[]).await;
        assert_eq!((s1, s2), (StatusCode::CREATED, StatusCode::CREATED));
    }

    fn rendezvous_uri(id: &[u8; PSEUDONYM_LEN]) -> String {
        format!("/ds/rendezvous/{}/messages", hex::encode(id))
    }

    async fn rendezvous_payloads(app: &Router, id: &[u8; PSEUDONYM_LEN]) -> Vec<Vec<u8>> {
        let (status, body) = send(
            app,
            Req {
                method: "GET",
                uri: &format!("{}?after=0", rendezvous_uri(id)),
                body: vec![],
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| B64.decode(m["payload_b64"].as_str().unwrap()).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn a_rendezvous_round_trips_without_a_proof_and_refuses_one() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let id = fresh_name(86);
        let (status, bytes) = send(
            &app,
            Req {
                method: "POST",
                uri: &rendezvous_uri(&id),
                body: b"pairing payload".to_vec(),
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["id"].as_u64().is_some());
        assert_eq!(
            rendezvous_payloads(&app, &id).await,
            vec![b"pairing payload".to_vec()]
        );
        // A proof, a delegation or a wake header is refused on either verb.
        for (name, value) in [
            (PROOF_HEADER_NAME, "v1.0.0.AAAA.AAAA"),
            (DELEGATION_HEADER_NAME, "AAAA"),
            (crate::wake::WAKE_HEADER, "x;message"),
        ] {
            for method in ["POST", "GET"] {
                let (status, _) = send_with(
                    &app,
                    Req {
                        method,
                        uri: &rendezvous_uri(&id),
                        body: if method == "POST" {
                            b"x".to_vec()
                        } else {
                            vec![]
                        },
                        proof: None,
                    },
                    &[(name, value)],
                )
                .await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{method} with {name}");
            }
        }
        // A signed request (what a client confusing the namespaces would send).
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &format!("{}?after=0", rendezvous_uri(&id)),
                body: vec![],
                proof: Some((&key(87), MAILBOX_EPOCH)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(rendezvous_payloads(&app, &id).await.len(), 1);
    }

    #[tokio::test]
    async fn a_rendezvous_and_a_mailbox_of_the_same_name_never_share_rows() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (identity, mailbox) = claimed_mailbox(&app, 88).await;
        let reach = key(89);
        add_reach(&app, &identity, &mailbox, &reach, None).await;
        let (status, _) = write(
            &app,
            &mailbox,
            b"personal",
            Some((&reach, REACH_EPOCH)),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &rendezvous_uri(&mailbox),
                body: b"meeting".to_vec(),
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            rendezvous_payloads(&app, &mailbox).await,
            vec![b"meeting".to_vec()]
        );
        assert_eq!(
            stored(&app, &identity, &mailbox).await,
            vec![b"personal".to_vec()]
        );
    }

    #[tokio::test]
    async fn only_the_identity_key_updates_reach_and_updates_are_bounded() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (identity, mailbox) = claimed_mailbox(&app, 70).await;
        let one = serde_json::json!({ "add": [{ "reach_pk_b64": pk_b64(&key(71)) }] });
        assert_eq!(
            put_reach(&app, &mailbox, None, &one).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            put_reach(&app, &mailbox, Some((&key(72), MAILBOX_EPOCH)), &one).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            put_reach(&app, &mailbox, Some((&identity, REACH_EPOCH)), &one).await,
            StatusCode::UNAUTHORIZED
        );
        // An unclaimed mailbox has no identity key to sign with.
        let unclaimed = fresh_name(73);
        assert_eq!(
            put_reach(&app, &unclaimed, Some((&identity, MAILBOX_EPOCH)), &one).await,
            StatusCode::UNAUTHORIZED
        );
        // A key both added and removed.
        let both = serde_json::json!({
            "add": [{ "reach_pk_b64": pk_b64(&key(71)) }],
            "remove": [pk_b64(&key(71))],
        });
        assert_eq!(
            put_reach(&app, &mailbox, Some((&identity, MAILBOX_EPOCH)), &both).await,
            StatusCode::BAD_REQUEST
        );
        // Idempotent.
        for _ in 0..2 {
            assert_eq!(
                put_reach(&app, &mailbox, Some((&identity, MAILBOX_EPOCH)), &one).await,
                StatusCode::NO_CONTENT
            );
        }
        // The cap: the mailbox already holds one key, so 4096 more is one
        // too many and nothing of the update lands.
        let many: Vec<serde_json::Value> = (0..u32::try_from(MAX_REACH_KEYS).unwrap())
            .map(|i| {
                let mut seed = [0u8; 32];
                seed[..4].copy_from_slice(&i.to_be_bytes());
                seed[4] = 0xEE;
                serde_json::json!({ "reach_pk_b64": pk_b64(&SigningKey::from_seed(&seed)) })
            })
            .collect();
        assert_eq!(
            put_reach(
                &app,
                &mailbox,
                Some((&identity, MAILBOX_EPOCH)),
                &serde_json::json!({ "add": many }),
            )
            .await,
            StatusCode::CONFLICT
        );
        let (status, _) = write(&app, &mailbox, b"x", Some((&key(71), REACH_EPOCH)), &[]).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "the refused update must not remove anything"
        );
        let too_many: Vec<String> = (0..=MAX_REACH_KEYS).map(|_| pk_b64(&key(74))).collect();
        assert_eq!(
            put_reach(
                &app,
                &mailbox,
                Some((&identity, MAILBOX_EPOCH)),
                &serde_json::json!({ "remove": too_many }),
            )
            .await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
