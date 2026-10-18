// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0081 - an epoch is registered together with the commit row that
//! reaches it.
//!
//! `POST /ds/group/{hex}/commit` takes, as `{"statement_b64", "row_b64"}`
//! ([`lakko_ds_proof::commit`]):
//!
//! - an append-mode register-epoch statement for `N + 1`, signed by the
//!   journal's root or a registrar (the ADR-0057 rules of `PUT .../access`
//!   apply unchanged: who may register, the member storage bound, the
//!   revocations);
//! - the commit row, opaque (the DS never parses it, invariant #2);
//!
//! with the request proof (ADR-0053) signed over that body under the key of
//! epoch `N`, which must be in the write tier.
//!
//! In ONE transaction behind the journal's lock row it verifies both
//! signatures, accepts the append at `latest + 1`, appends the row, and
//! keeps `BLAKE3(row)` and the row's id with the registration. So a key
//! registered without its commit row cannot exist: a crash, a lost answer or
//! a refused write leaves neither.
//!
//! **Re-send.** The same row under the key already registered at that epoch
//! through this route appends nothing and answers the original id (`200`,
//! where a new append answers `201`). A re-send is recognised before the
//! write tier and the nonces are looked at: it changes nothing, so a proof
//! under any retained epoch key identifies it, and a device that made its
//! `N + 1` state current before the answer reached it (or whose siblings
//! moved on since) still learns the id. Any other append at an existing
//! epoch is `409`.
//!
//! **Order of checks.** Shape (`400`: the body, the statement kind and
//! journal, append mode, the epoch bound, an empty row, a malformed wake
//! header), then the statement's signature and window and the proof header's
//! parse (`401`), then inside the transaction: the proof's key (`401` unless
//! it names a retained epoch of this journal), the signer's standing (`401`
//! for a stranger, as on the statement routes), the re-send, an existing
//! registration at the epoch or an epoch other than `latest + 1` (`409`), the
//! write tier (`401`), the two nonces (`401` on a replay), the proof epoch
//! being the one before the registered epoch (`400`), and the rest of the
//! ADR-0057 rule (`403`: the member storage bound, a revocation of an equal
//! or higher rank). A refusal after the nonces commits, so they stay spent.
//!
//! **The only route for a new epoch** (ADR-0081 open question 1, decided
//! 2026-10-18): `PUT .../access` refuses an append-mode statement with `400
//! {"error":"append-needs-commit"}` for every journal, so an epoch above a
//! claim or a reset is registered only here, with its row.

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_ds_proof::statement::KEY_LEN;
use lakko_ds_proof::{
    commit_row_hash, RegisterMode, SignedStatement, StatementBody, StatementKind,
};
use serde::Deserialize;
use sqlx::mysql::MySqlConnection;
use sqlx::types::chrono::{DateTime, Utc};

use crate::access::{
    admin_floor, consume_nonce, group_keys_on, in_write_tier, lock_group, now_ms, parts, refused,
    registration_db_error, verify_header, HandlerError, MAX_ACCESS_EPOCH,
};
use crate::journal::{self, PublishResponse, Queue, PSEUDONYM_LEN};
use crate::ownership::{apply_register, is_heard, registration_at, standing_of, Registration};
use crate::AppState;

/// The route's body. Field names are [`lakko_ds_proof::COMMIT_STATEMENT_FIELD`]
/// and [`lakko_ds_proof::COMMIT_ROW_FIELD`] (pinned by a test). An unknown
/// field is refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitJson {
    statement_b64: String,
    row_b64: String,
}

fn bad_request(msg: &str) -> HandlerError {
    (StatusCode::BAD_REQUEST, msg.to_string())
}

/// A parsed, shape-checked commit request.
struct CommitRequest {
    statement: SignedStatement,
    row: Vec<u8>,
    epoch: u64,
    epoch_key: [u8; KEY_LEN],
}

fn parse_body(body: &[u8], group_id: &[u8; PSEUDONYM_LEN]) -> Result<CommitRequest, HandlerError> {
    let json: CommitJson = serde_json::from_slice(body)
        .map_err(|_| bad_request("body must be {\"statement_b64\": ..., \"row_b64\": ...}"))?;
    let blob = B64
        .decode(json.statement_b64.as_bytes())
        .map_err(|_| bad_request("statement_b64: bad base64"))?;
    let row = B64
        .decode(json.row_b64.as_bytes())
        .map_err(|_| bad_request("row_b64: bad base64"))?;
    if row.is_empty() {
        return Err(bad_request("a commit row must be non-empty"));
    }
    let statement =
        SignedStatement::decode(&blob).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    if statement.body.kind() != StatementKind::RegisterEpoch {
        return Err(bad_request("a commit carries a register-epoch statement"));
    }
    if statement.journal_id != *group_id {
        return Err(bad_request("statement names another journal"));
    }
    let StatementBody::RegisterEpoch {
        epoch,
        epoch_key,
        mode,
        ..
    } = &statement.body
    else {
        return Err(bad_request("a commit carries a register-epoch statement"));
    };
    if *mode != RegisterMode::Append {
        return Err(bad_request("a commit registers its epoch by append"));
    }
    if *epoch > MAX_ACCESS_EPOCH {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("epoch must be at most {MAX_ACCESS_EPOCH}"),
        ));
    }
    let (epoch, epoch_key) = (*epoch, *epoch_key);
    Ok(CommitRequest {
        statement,
        row,
        epoch,
        epoch_key,
    })
}

/// What the transaction decided.
enum Outcome {
    /// The row was appended under the new registration.
    Appended(PublishResponse),
    /// An identical re-send: the original row.
    Resent(PublishResponse),
}

/// `POST /ds/group/{hex}/commit`.
pub(crate) async fn post_commit(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, HandlerError> {
    let group_id = journal::decode_pseudonym(&group_hex)?;
    let wake = crate::wake::parse_wake_header(&headers)?;
    let request = parse_body(&body, &group_id)?;
    request.statement.verify(now_ms()).map_err(|_| refused())?;

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(crate::access::db_error("acquire commit connection"))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *conn)
        .await
        .map_err(crate::access::db_error("set commit isolation"))?;
    let mut tx = sqlx::Connection::begin(&mut *conn)
        .await
        .map_err(crate::access::db_error("begin commit"))?;
    let req_parts = parts(&method, &uri, &body);
    // A DB failure drops `tx` (rollback: neither the key nor the row). A
    // decision commits, so a refusal after the nonces keeps them spent.
    let decision = decide_commit(&mut tx, &group_id, &headers, &req_parts, &request).await?;
    tx.commit()
        .await
        .map_err(registration_db_error("commit the commit"))?;
    match decision? {
        Outcome::Appended(resp) => {
            state.notifiers.wake((Queue::Group, group_id));
            if let Some(wake) = wake {
                state.waker.fire(&group_id, wake, resp.id);
            }
            Ok((StatusCode::CREATED, Json(resp)).into_response())
        },
        Outcome::Resent(resp) => Ok((StatusCode::OK, Json(resp)).into_response()),
    }
}

/// The decision, on a connection inside a transaction. Split from the handler
/// so a test can run it and roll the transaction back.
async fn decide_commit(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    headers: &HeaderMap,
    req_parts: &lakko_ds_proof::RequestParts<'_>,
    request: &CommitRequest,
) -> Result<Result<Outcome, HandlerError>, HandlerError> {
    lock_group(&mut *conn, group_id).await?;
    let keys = group_keys_on(&mut *conn, group_id).await?;
    // Any retained epoch identifies the caller; the write tier is asked below,
    // after a re-send (which writes nothing) has been recognised.
    let Ok(proof) = verify_header(headers, req_parts, |epoch| {
        keys.iter().find(|k| k.epoch == epoch).map(|k| k.key)
    }) else {
        return Ok(Err(refused()));
    };
    let signer = &request.statement.signer;
    let standing = standing_of(&mut *conn, group_id, signer).await?;
    if !is_heard(StatementKind::RegisterEpoch, standing) {
        return Ok(Err(refused()));
    }
    let row_hash = commit_row_hash(&request.row);
    if let Some(resp) = resend_of(&mut *conn, group_id, request, &row_hash).await? {
        return Ok(Ok(Outcome::Resent(resp)));
    }
    // A conflict is told before the write tier and the nonces: it writes
    // nothing, and a device whose epoch has left the write tier learns that
    // its commit lost (and stops retrying) rather than a bare 401. A reader
    // of the journal learns the latest epoch on every read anyway.
    if registration_at(&mut *conn, group_id, request.epoch)
        .await?
        .is_some()
    {
        return Ok(Err((
            StatusCode::CONFLICT,
            "that epoch is already registered".to_string(),
        )));
    }
    // `keys` is newest first; an append lands only at `latest + 1`.
    let next = keys.first().and_then(|k| k.epoch.checked_add(1));
    if next != Some(request.epoch) {
        return Ok(Err((
            StatusCode::CONFLICT,
            "an append registers the next epoch only".to_string(),
        )));
    }
    let floor = admin_floor(&keys);
    let writable = keys
        .iter()
        .enumerate()
        .any(|(index, k)| k.epoch == proof.epoch && in_write_tier(index, k.epoch, floor));
    if !writable {
        return Ok(Err(refused()));
    }
    if !consume_nonce(&mut *conn, group_id, &proof.nonce)
        .await
        .map_err(registration_db_error("record commit proof nonce"))?
    {
        return Ok(Err(refused()));
    }
    if !consume_nonce(&mut *conn, group_id, &request.statement.nonce)
        .await
        .map_err(registration_db_error("record commit statement nonce"))?
    {
        return Ok(Err(refused()));
    }
    if proof.epoch.checked_add(1) != Some(request.epoch) {
        return Ok(Err(bad_request(
            "the proof is signed under the epoch before the one registered",
        )));
    }
    let StatementBody::RegisterEpoch { revoke, .. } = &request.statement.body else {
        return Ok(Err(bad_request(
            "a commit carries a register-epoch statement",
        )));
    };
    let registration = Registration {
        signer,
        standing,
        epoch: request.epoch,
        epoch_key: &request.epoch_key,
        mode: RegisterMode::Append,
    };
    if let Err(refusal) = apply_register(&mut *conn, group_id, &registration, revoke).await? {
        return Ok(Err(refusal));
    }
    let resp = append_row(&mut *conn, group_id, request, &row_hash).await?;
    Ok(Ok(Outcome::Appended(resp)))
}

/// `access_pk, commit_row_hash, commit_message_id, created_at` of one
/// registration.
type RegistrationRow = (Vec<u8>, Option<Vec<u8>>, Option<u64>, DateTime<Utc>);

/// The original row when `request` re-sends a commit this route already
/// took: the same key registered at the epoch, with the same row digest.
async fn resend_of(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    request: &CommitRequest,
    row_hash: &[u8; 32],
) -> Result<Option<PublishResponse>, HandlerError> {
    let row: Option<RegistrationRow> = sqlx::query_as(
        "SELECT access_pk, commit_row_hash, commit_message_id, created_at FROM ds_group_access \
         WHERE group_id = ? AND epoch = ?",
    )
    .bind(&group_id[..])
    .bind(request.epoch)
    .fetch_optional(&mut *conn)
    .await
    .map_err(registration_db_error("load commit registration"))?;
    let Some((key, Some(hash), Some(id), registered_at)) = row else {
        return Ok(None);
    };
    if key.as_slice() != request.epoch_key || hash.as_slice() != row_hash {
        return Ok(None);
    }
    // The row may have aged out of the journal; the id is still the answer.
    let created: Option<(DateTime<Utc>,)> =
        sqlx::query_as("SELECT created_at FROM ds_group_messages WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(registration_db_error("load committed row"))?;
    Ok(Some(PublishResponse {
        id,
        created_at: created.map_or(registered_at, |(at,)| at),
    }))
}

/// Append the row and bind it to the registration just written.
async fn append_row(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    request: &CommitRequest,
    row_hash: &[u8; 32],
) -> Result<PublishResponse, HandlerError> {
    let id = journal::insert_row(&mut *conn, Queue::Group, group_id, &request.row)
        .await
        .map_err(registration_db_error("append commit row"))?;
    sqlx::query(
        "UPDATE ds_group_access SET commit_row_hash = ?, commit_message_id = ? \
         WHERE group_id = ? AND epoch = ?",
    )
    .bind(&row_hash[..])
    .bind(id)
    .bind(&group_id[..])
    .bind(request.epoch)
    .execute(&mut *conn)
    .await
    .map_err(registration_db_error("bind commit row"))?;
    let (created_at,): (DateTime<Utc>,) =
        sqlx::query_as("SELECT created_at FROM ds_group_messages WHERE id = ?")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .map_err(registration_db_error("load commit row time"))?;
    Ok(PublishResponse { id, created_at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use axum::body::Body;
    use axum::http::{header, Request};
    use http_body_util::BodyExt as _;
    use lakko_ds_proof::statement::StatementSigner;
    use lakko_ds_proof::{
        commit_body_json, sign, Rank, RegistrarChange, SigningKey, PROOF_HEADER_NAME,
    };
    use sqlx::MySqlPool;
    use tower::ServiceExt as _;

    fn commit_uri(gid: &[u8; PSEUDONYM_LEN]) -> String {
        format!("/ds/group/{}/commit", hex::encode(gid))
    }

    /// The JSON body for a commit of `row` registering `epoch_key` at `epoch`.
    fn body_for(
        gid: &[u8; PSEUDONYM_LEN],
        signer: &StatementSigner,
        epoch: u64,
        epoch_key: &SigningKey,
        revoke: &[&StatementSigner],
        row: &[u8],
    ) -> Vec<u8> {
        let statement = signer
            .sign_commit_at(
                gid,
                epoch,
                epoch_key.verifying_key().to_bytes(),
                revoke.iter().map(|s| s.public_key()).collect(),
                now_ms(),
                fresh_nonce(),
            )
            .unwrap();
        commit_body_json(&statement, row).unwrap().into_bytes()
    }

    /// POST a commit body proven under `proof_key` at `proof_epoch`.
    async fn post(
        app: &axum::Router,
        gid: &[u8; PSEUDONYM_LEN],
        body: Vec<u8>,
        proof_key: &SigningKey,
        proof_epoch: u64,
    ) -> (StatusCode, serde_json::Value) {
        let (status, bytes) = send(
            app,
            Req {
                method: "POST",
                uri: &commit_uri(gid),
                body,
                proof: Some((proof_key, proof_epoch)),
            },
        )
        .await;
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    async fn rows(pool: &MySqlPool, gid: &[u8; PSEUDONYM_LEN]) -> Vec<(u64, Vec<u8>)> {
        sqlx::query_as("SELECT id, payload FROM ds_group_messages WHERE group_id = ? ORDER BY id")
            .bind(&gid[..])
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn epochs(pool: &MySqlPool, gid: &[u8; PSEUDONYM_LEN]) -> Vec<u64> {
        sqlx::query_as::<_, (u64,)>(
            "SELECT epoch FROM ds_group_access WHERE group_id = ? ORDER BY epoch",
        )
        .bind(&gid[..])
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(e,)| e)
        .collect()
    }

    /// A journal claimed by a fresh root at epoch 1 under `key(1)`.
    async fn claimed(app: &axum::Router, tag: u8) -> ([u8; PSEUDONYM_LEN], StatementSigner) {
        let gid = fresh_name(tag);
        let root = fresh_signer(tag);
        assert_eq!(
            claim(app, &gid, &root, 1, &key(1)).await,
            StatusCode::NO_CONTENT
        );
        (gid, root)
    }

    #[test]
    fn the_route_reads_the_fields_lakko_ds_proof_writes() {
        let gid = [0x10; PSEUDONYM_LEN];
        let signer = StatementSigner::from_seed(&[0x11; 32]);
        let body = body_for(&gid, &signer, 2, &key(2), &[], b"commit row");
        let parsed = parse_body(&body, &gid).unwrap();
        assert_eq!(parsed.row, b"commit row");
        assert_eq!(parsed.epoch, 2);
        assert_eq!(parsed.epoch_key, key(2).verifying_key().to_bytes());
        assert_eq!(lakko_ds_proof::COMMIT_STATEMENT_FIELD, "statement_b64");
        assert_eq!(lakko_ds_proof::COMMIT_ROW_FIELD, "row_b64");
        // Another journal, an unknown field: refused before any state.
        assert!(parse_body(&body, &[0x12; PSEUDONYM_LEN]).is_err());
        let mut extra: serde_json::Value = serde_json::from_slice(&body).unwrap();
        extra["wake"] = serde_json::json!("x");
        assert!(parse_body(extra.to_string().as_bytes(), &gid).is_err());
    }

    /// Decision 1: the key and the row land together, in order, and the row
    /// is readable under the new epoch's key.
    #[tokio::test]
    async fn a_commit_registers_the_epoch_and_appends_its_row_together() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let (gid, root) = claimed(&app, 0x70).await;
        let body = body_for(&gid, &root, 2, &key(2), &[], b"commit 1->2");
        let (status, json) = post(&app, &gid, body, &key(1), 1).await;
        assert_eq!(status, StatusCode::CREATED);
        let id = json["id"].as_u64().unwrap();
        assert_eq!(rows(&pool, &gid).await, vec![(id, b"commit 1->2".to_vec())]);
        assert_eq!(epochs(&pool, &gid).await, vec![1, 2]);
        let hash: (Option<Vec<u8>>, Option<u64>) = sqlx::query_as(
            "SELECT commit_row_hash, commit_message_id FROM ds_group_access \
             WHERE group_id = ? AND epoch = 2",
        )
        .bind(&gid[..])
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(hash.0.unwrap(), commit_row_hash(b"commit 1->2").to_vec());
        assert_eq!(hash.1, Some(id));
    }

    /// Atomicity: a commit whose transaction does not commit leaves neither
    /// the registration nor the row (the decision runs inside the handler's
    /// one transaction; this one is rolled back after it).
    #[tokio::test]
    async fn a_rolled_back_commit_leaves_neither_key_nor_row() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let (gid, root) = claimed(&app, 0x71).await;
        let body = body_for(&gid, &root, 2, &key(2), &[], b"lost row");
        let uri: Uri = commit_uri(&gid).parse().unwrap();
        let req = Req {
            method: "POST",
            uri: &commit_uri(&gid),
            body: body.clone(),
            proof: Some((&key(1), 1)),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            PROOF_HEADER_NAME,
            proof_header(&req).unwrap().parse().unwrap(),
        );
        let request = parse_body(&body, &gid).unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let mut tx = sqlx::Connection::begin(&mut *conn).await.unwrap();
        let outcome = decide_commit(
            &mut tx,
            &gid,
            &headers,
            &parts(&Method::POST, &uri, &body),
            &request,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Ok(Outcome::Appended(_))));
        tx.rollback().await.unwrap();
        drop(conn);
        assert!(rows(&pool, &gid).await.is_empty());
        assert_eq!(epochs(&pool, &gid).await, vec![1]);
        // Refused by the ADR-0057 rule (a member revoking an admin): the
        // handler's transaction commits the spent nonces and nothing else.
        let admin = fresh_signer(0x71);
        let member = fresh_signer(0x71);
        assert_eq!(
            send_statement(
                &app,
                &gid,
                &root,
                StatementBody::Registrars {
                    changes: vec![
                        RegistrarChange {
                            key: admin.public_key(),
                            rank: Some(Rank::Admin),
                        },
                        RegistrarChange {
                            key: member.public_key(),
                            rank: Some(Rank::Member),
                        },
                    ],
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &member, 2, &key(2), &[&admin], b"refused row"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(rows(&pool, &gid).await.is_empty());
        assert_eq!(epochs(&pool, &gid).await, vec![1]);
    }

    /// Decision 1: an identical re-send appends nothing and answers the
    /// original id - also under the NEW epoch's key (a device that made its
    /// next state current before the answer arrived) and after siblings
    /// moved on.
    #[tokio::test]
    async fn an_identical_resend_answers_the_original_id() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let (gid, root) = claimed(&app, 0x72).await;
        let (status, first) = post(
            &app,
            &gid,
            body_for(&gid, &root, 2, &key(2), &[], b"row two"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = first["id"].as_u64().unwrap();
        for (proof_key, proof_epoch) in [(key(1), 1), (key(2), 2)] {
            let (status, again) = post(
                &app,
                &gid,
                body_for(&gid, &root, 2, &key(2), &[], b"row two"),
                &proof_key,
                proof_epoch,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(again["id"].as_u64(), Some(id));
        }
        // A sibling moves the journal on; the re-send still answers.
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &root, 3, &key(3), &[], b"row three"),
            &key(2),
            2,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, again) = post(
            &app,
            &gid,
            body_for(&gid, &root, 2, &key(2), &[], b"row two"),
            &key(3),
            3,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again["id"].as_u64(), Some(id));
        assert_eq!(rows(&pool, &gid).await.len(), 2);
    }

    /// Decision 1: any other append at an existing epoch is 409, and leaves
    /// nothing behind - another key, or the same key with another row.
    #[tokio::test]
    async fn another_commit_at_a_registered_epoch_is_a_conflict() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let (gid, root) = claimed(&app, 0x73).await;
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &root, 2, &key(2), &[], b"winner"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &root, 2, &key(0x22), &[], b"loser"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &root, 2, &key(2), &[], b"same key, other row"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        // An epoch registered with another row is not a re-send.
        assert_eq!(
            register(&app, &gid, &root, 3, &key(3), &[]).await,
            StatusCode::NO_CONTENT
        );
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &root, 3, &key(3), &[], b"late row"),
            &key(2),
            2,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let payloads: Vec<Vec<u8>> = rows(&pool, &gid)
            .await
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        // The winner, then the test registration's own row at 3; no loser.
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0], b"winner".to_vec());
        assert!(!payloads.iter().any(|p| p == b"loser" || p == b"late row"));
    }

    /// Open question 2: a member, an admin and the root each commit through
    /// the route; a stranger, an epoch key and a proof outside the write tier
    /// or under the wrong epoch are refused.
    #[tokio::test]
    async fn every_registrar_rank_commits_and_nobody_else_does() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (gid, root) = claimed(&app, 0x74).await;
        let admin = fresh_signer(0x74);
        let member = fresh_signer(0x74);
        assert_eq!(
            send_statement(
                &app,
                &gid,
                &root,
                StatementBody::Registrars {
                    changes: vec![
                        RegistrarChange {
                            key: admin.public_key(),
                            rank: Some(Rank::Admin),
                        },
                        RegistrarChange {
                            key: member.public_key(),
                            rank: Some(Rank::Member),
                        },
                    ],
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        // A stranger, and a statement signed by an epoch key: the uniform 401.
        for signer in [fresh_signer(0x75), StatementSigner::from_seed(&[1; 32])] {
            let (status, _) = post(
                &app,
                &gid,
                body_for(&gid, &signer, 2, &key(2), &[], b"x"),
                &key(1),
                1,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        // A proof under an unknown epoch: 401. Under the right key but a
        // mismatched epoch label: refused as a bad proof.
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &member, 2, &key(2), &[], b"x"),
            &key(9),
            9,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        for (signer, epoch) in [(&member, 2), (&admin, 3), (&root, 4)] {
            let (status, _) = post(
                &app,
                &gid,
                body_for(
                    &gid,
                    signer,
                    epoch,
                    &key(u8::try_from(epoch).unwrap()),
                    &[],
                    b"c",
                ),
                &key(u8::try_from(epoch - 1).unwrap()),
                epoch - 1,
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "rank committing epoch {epoch}");
        }
        // A proof two epochs behind is still in the write tier but not the
        // epoch before the registered one.
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &root, 5, &key(5), &[], b"c"),
            &key(3),
            3,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// ADR-0057's Replace from a higher rank still works next to the route:
    /// an admin drops a member-registered epoch (and its row binding) and the
    /// next commit through the route lands on top.
    #[tokio::test]
    async fn a_higher_rank_replace_still_repairs_a_lower_rank_epoch() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let (gid, root) = claimed(&app, 0x76).await;
        let admin = fresh_signer(0x76);
        let member = fresh_signer(0x76);
        assert_eq!(
            send_statement(
                &app,
                &gid,
                &root,
                StatementBody::Registrars {
                    changes: vec![
                        RegistrarChange {
                            key: admin.public_key(),
                            rank: Some(Rank::Admin),
                        },
                        RegistrarChange {
                            key: member.public_key(),
                            rank: Some(Rank::Member),
                        },
                    ],
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        // The member commits epoch 2 with a row nobody can apply.
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &member, 2, &key(0xF2), &[], b"unapplicable"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        // The admin's commit at 2 meets it; a replace at 1 drops it.
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &admin, 2, &key(2), &[&member], b"real 1->2"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            replace(&app, &gid, &admin, 1, &key(1), &[&member]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &gid).await, vec![1]);
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &admin, 2, &key(2), &[], b"real 1->2"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        // A replace over an epoch that carried a row clears its binding: the
        // old row can no longer be "re-sent".
        assert_eq!(
            replace(&app, &gid, &root, 2, &key(0x32), &[]).await,
            StatusCode::NO_CONTENT
        );
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &admin, 2, &key(2), &[], b"real 1->2"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    /// ADR-0081 open question 1 (decided 2026-10-18): `PUT .../access`
    /// refuses an append for every journal with `400 append-needs-commit`,
    /// whoever signs it; the claim still registers the first epoch and a
    /// higher rank's Replace still works there.
    #[tokio::test]
    async fn access_refuses_a_plain_append_and_keeps_claim_and_replace() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let (gid, root) = claimed(&app, 0x78).await;
        assert_eq!(
            epochs(&pool, &gid).await,
            vec![1],
            "the claim registered epoch 1"
        );
        for signer in [&root, &fresh_signer(0x78)] {
            let statement = signer
                .sign_commit_at(
                    &gid,
                    2,
                    key(2).verifying_key().to_bytes(),
                    vec![],
                    now_ms(),
                    fresh_nonce(),
                )
                .unwrap();
            let (status, body) = send(
                &app,
                Req {
                    method: "PUT",
                    uri: &format!("/ds/group/{}/access", hex::encode(gid)),
                    body: statement.body_json().into_bytes(),
                    proof: None,
                },
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error_code(&body), crate::ownership::APPEND_NEEDS_COMMIT);
        }
        assert_eq!(epochs(&pool, &gid).await, vec![1]);
        // The same epoch through the commit route lands.
        let (status, _) = post(
            &app,
            &gid,
            body_for(&gid, &root, 2, &key(2), &[], b"1->2"),
            &key(1),
            1,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        // A Replace on `/access` (the root over its own epoch, same key) works.
        assert_eq!(
            replace(&app, &gid, &root, 2, &key(2), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &gid).await, vec![1, 2]);
    }

    /// The nonces are spent by an append: replaying the exact request is the
    /// uniform 401 unless it is a re-send, which needs no fresh nonce.
    #[tokio::test]
    async fn a_replayed_request_is_answered_as_a_resend_or_refused() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let (gid, root) = claimed(&app, 0x77).await;
        let body = body_for(&gid, &root, 2, &key(2), &[], b"once");
        let req = Req {
            method: "POST",
            uri: &commit_uri(&gid),
            body: body.clone(),
            proof: Some((&key(1), 1)),
        };
        let proof = proof_header(&req).unwrap();
        let send_raw = |b: Vec<u8>, p: String| {
            let app = app.clone();
            let uri = commit_uri(&gid);
            async move {
                let response = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(uri)
                            .header(header::CONTENT_TYPE, "application/json")
                            .header(PROOF_HEADER_NAME, p)
                            .body(Body::from(b))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = response.status();
                let _ = response.into_body().collect().await;
                status
            }
        };
        assert_eq!(
            send_raw(body.clone(), proof.clone()).await,
            StatusCode::CREATED
        );
        assert_eq!(send_raw(body, proof).await, StatusCode::OK);
        // A different row signed with a fresh statement but the SAME proof
        // nonce cannot reuse the proof: the proof covers the body.
        let other = body_for(&gid, &root, 3, &key(3), &[], b"twice");
        let header = sign(
            &key(1),
            &lakko_ds_proof::RequestParts {
                method: "POST",
                canonical_path: &commit_uri(&gid),
                query: "",
                body: b"not this body",
            },
            1,
            now_ms(),
            fresh_nonce(),
        )
        .encode();
        assert_eq!(send_raw(other, header).await, StatusCode::UNAUTHORIZED);
    }
}
