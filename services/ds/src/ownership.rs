// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0057 - a journal has an owner; only those who change a group register
//! its epochs.
//!
//! Every group journal is claimed under a **root** key (derived by the
//! owner's account per group). The root appoints **registrars** ranked
//! `member` (1) or `admin` (2); the root itself counts as rank 3. Only the
//! root and registrars register epoch keys - a plain epoch-key holder, which
//! every member and every evictee of that epoch is, no longer can. Each route
//! here takes one signed statement ([`lakko_ds_proof::statement`]) as
//! `{"statement_b64": ...}`:
//!
//! | Route (`PUT /ds/group/{hex}/...`) | Signer | Effect |
//! |---|---|---|
//! | `claim` | anyone, first time | the signer becomes the root, one epoch |
//! | `reset` | the root | replaces the chain (and the registrar set when sent) |
//! | `access` | root or registrar | registers an epoch, revokes lower ranks |
//! | `registrars` | the root | adds, re-ranks, removes registrars |
//! | `root` | the root | names a new root |
//! | `push-binding` | root or admin | binds the journal to one `push_gid` |
//!
//! **Order of checks.** Shape first (`400`: a body that does not decode, a
//! statement of another kind or about another journal, an epoch above
//! [`MAX_ACCESS_EPOCH`]), then the signature and time window (`401`), then -
//! inside one transaction behind the group's lock row - whether the signer is
//! anyone to this journal (`401` when not, the same uniform refusal a bad
//! signature gets, so a stranger learns nothing about the journal's state),
//! then the nonce (`401` on a replay), then the rule (`403` when the signer's
//! rank does not reach, `409` when the statement conflicts with the state).
//! A refusal after the nonce commits, so the nonce stays spent.
//!
//! **The epoch rule** (`access`), with `latest` the newest registered epoch:
//!
//! - re-sending the key already registered at `e` changes nothing;
//! - `e == latest + 1`: any registrar or the root;
//! - `e <= latest`: allowed when every epoch registered at `e` or above was
//!   registered by a strictly lower rank than the signer's, or the signer is
//!   the root; the epochs above `e` are dropped. This is the ADR's "replace
//!   `latest`" widened to a suffix, so an evictee that raced **two** epochs
//!   ahead is still replaced by the admin or root committer's `N+1` in one
//!   step (with `e == latest` it is exactly the ADR's rule);
//! - anything else (a skip past `latest + 1`): the root only.
//!
//! Revocations ride the same statement and the same transaction: each named
//! key that is a registrar must hold a strictly lower rank than the signer,
//! or the whole statement is refused (`403`) and nothing is written.
//!
//! **Push binding.** The DS stores `keyed-BLAKE3(pepper, push_gid)` per
//! journal under a unique key, so one `push_gid` belongs to at most one
//! journal; a wake whose `push_gid` does not hash to the publishing journal's
//! binding is dropped ([`wake_is_bound`]). With `LAKKO_DS_BINDING_PEPPER`
//! unset the binding route answers `503 push-binding-disabled` and every wake
//! is dropped. Residual, stated: a current member who learns the `push_gid`
//! before the owner binds it can bind it to a journal of its own first; the
//! group then re-mints (ADR-0043 rotation) to recover its wakes.

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_ds_proof::statement::{push_binding_key, KEY_LEN};
use lakko_ds_proof::{
    Rank, RegistrarChange, RegistrarEntry, SignedStatement, StatementBody, StatementKind,
    ROOT_RANK_BYTE,
};
use serde::Deserialize;
use sqlx::mysql::{MySqlConnection, MySqlDatabaseError};

use crate::access::{
    consume_nonce, db_error, lock_group, now_ms, refused, registration_db_error, HandlerError,
    MAX_ACCESS_EPOCH, MAX_RETAINED_EPOCHS,
};
use crate::journal::{self, PSEUDONYM_LEN};
use crate::AppState;

/// Most registrars one journal holds. A bound on the rows a root can make
/// the server keep, not a group-size policy: chosen, not measured.
pub(crate) const MAX_REGISTRARS: usize = 4096;

/// MariaDB's duplicate-key error number.
const ER_DUP_ENTRY: u16 = 1062;

/// The environment variable the push-binding pepper is read from.
pub(crate) const PEPPER_ENV: &str = "LAKKO_DS_BINDING_PEPPER";

/// Read `LAKKO_DS_BINDING_PEPPER`: 64 hex characters, or unset.
///
/// # Errors
///
/// A message naming the variable when it is set but is not 64 hex
/// characters: a malformed pepper refuses startup rather than silently
/// disabling every wake.
pub(crate) fn pepper_from_lookup(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<[u8; KEY_LEN]>, String> {
    let Some(raw) = lookup(PEPPER_ENV).filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    let mut out = [0u8; KEY_LEN];
    hex::decode_to_slice(raw.trim(), &mut out)
        .map_err(|_| format!("{PEPPER_ENV} must be 64 hex characters (32 bytes)"))?;
    Ok(Some(out))
}

#[derive(Deserialize)]
pub(crate) struct StatementJson {
    /// Standard base64 of the signed statement blob.
    statement_b64: String,
}

fn bad_request(msg: &str) -> HandlerError {
    (StatusCode::BAD_REQUEST, msg.to_string())
}

fn forbidden(msg: &str) -> HandlerError {
    (StatusCode::FORBIDDEN, msg.to_string())
}

fn conflict(msg: &str) -> HandlerError {
    (StatusCode::CONFLICT, msg.to_string())
}

/// Parse and shape-check a statement for the route `kind` on `group_id`.
fn parse_statement(
    body: &[u8],
    kind: StatementKind,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<SignedStatement, HandlerError> {
    let json: StatementJson = serde_json::from_slice(body)
        .map_err(|_| bad_request("body must be {\"statement_b64\": ...}"))?;
    let blob = B64
        .decode(json.statement_b64.as_bytes())
        .map_err(|_| bad_request("statement_b64: bad base64"))?;
    let statement =
        SignedStatement::decode(&blob).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    if statement.body.kind() != kind {
        return Err(bad_request("statement kind does not match the route"));
    }
    if statement.journal_id != *group_id {
        return Err(bad_request("statement names another journal"));
    }
    let epoch = match &statement.body {
        StatementBody::Claim { epoch, .. }
        | StatementBody::Reset { epoch, .. }
        | StatementBody::RegisterEpoch { epoch, .. } => Some(*epoch),
        _ => None,
    };
    if epoch.is_some_and(|e| e > MAX_ACCESS_EPOCH) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("epoch must be at most {MAX_ACCESS_EPOCH}"),
        ));
    }
    Ok(statement)
}

/// The route handlers: one statement kind each.
macro_rules! statement_route {
    ($name:ident, $kind:expr) => {
        pub(crate) async fn $name(
            State(state): State<AppState>,
            AxumPath(group_hex): AxumPath<String>,
            body: Bytes,
        ) -> Result<Response, HandlerError> {
            handle(&state, &group_hex, &body, $kind).await
        }
    };
}

statement_route!(put_claim, StatementKind::Claim);
statement_route!(put_reset, StatementKind::Reset);
statement_route!(put_register, StatementKind::RegisterEpoch);
statement_route!(put_registrars, StatementKind::Registrars);
statement_route!(put_root, StatementKind::RootTransfer);
statement_route!(put_push_binding, StatementKind::PushBinding);

async fn handle(
    state: &AppState,
    group_hex: &str,
    body: &[u8],
    kind: StatementKind,
) -> Result<Response, HandlerError> {
    let group_id = journal::decode_pseudonym(group_hex)?;
    let statement = parse_statement(body, kind, &group_id)?;
    let pepper = if kind == StatementKind::PushBinding {
        match state.binding_pepper {
            Some(p) => Some(p),
            None => {
                return Ok((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({ "error": "push-binding-disabled" })),
                )
                    .into_response());
            },
        }
    } else {
        None
    };
    statement.verify(now_ms()).map_err(|_| refused())?;

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(db_error("acquire ownership connection"))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *conn)
        .await
        .map_err(db_error("set ownership isolation"))?;
    let mut tx = sqlx::Connection::begin(&mut *conn)
        .await
        .map_err(db_error("begin ownership statement"))?;
    // A DB failure drops `tx` (rollback). A decision commits: a refusal after
    // the nonce was taken keeps it taken.
    let decision = decide(&mut tx, &group_id, &statement, pepper.as_ref()).await?;
    tx.commit()
        .await
        .map_err(registration_db_error("commit ownership statement"))?;
    decision.map(|()| StatusCode::NO_CONTENT.into_response())
}

/// Who the signer is to this journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    /// The journal is unclaimed.
    Unclaimed,
    /// The journal's root (rank 3).
    Root,
    /// A registrar of this rank.
    Registrar(Rank),
    /// Nobody: neither the root nor a registrar.
    Stranger,
}

impl Standing {
    const fn rank_byte(self) -> u8 {
        match self {
            Self::Root => ROOT_RANK_BYTE,
            Self::Registrar(rank) => rank.to_byte(),
            Self::Unclaimed | Self::Stranger => 0,
        }
    }
}

type Decision = Result<Result<(), HandlerError>, HandlerError>;

async fn decide(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    statement: &SignedStatement,
    pepper: Option<&[u8; KEY_LEN]>,
) -> Decision {
    lock_group(&mut *conn, group_id).await?;
    let root = load_root(&mut *conn, group_id).await?;
    let standing = match root {
        None => Standing::Unclaimed,
        Some(r) if r == statement.signer => Standing::Root,
        Some(_) => match registrar_rank(&mut *conn, group_id, &statement.signer).await? {
            Some(rank) => Standing::Registrar(rank),
            None => Standing::Stranger,
        },
    };
    // Who may be heard at all, before the nonce is spent: a stranger costs
    // the server no row and learns the uniform 401.
    let heard = match statement.body.kind() {
        StatementKind::Claim => matches!(standing, Standing::Unclaimed | Standing::Root),
        StatementKind::Reset | StatementKind::Registrars | StatementKind::RootTransfer => {
            standing == Standing::Root
        },
        StatementKind::RegisterEpoch | StatementKind::PushBinding => {
            matches!(standing, Standing::Root | Standing::Registrar(_))
        },
    };
    if !heard {
        return Ok(Err(refused()));
    }
    if !consume_nonce(&mut *conn, group_id, &statement.nonce)
        .await
        .map_err(registration_db_error("record statement nonce"))?
    {
        return Ok(Err(refused()));
    }
    let signer = &statement.signer;
    match &statement.body {
        StatementBody::Claim { epoch, epoch_key } => {
            apply_claim(&mut *conn, group_id, signer, standing, *epoch, epoch_key).await
        },
        StatementBody::Reset {
            epoch,
            epoch_key,
            registrars,
        } => {
            apply_reset(
                &mut *conn,
                group_id,
                signer,
                *epoch,
                epoch_key,
                registrars.as_deref(),
            )
            .await
        },
        StatementBody::RegisterEpoch {
            epoch,
            epoch_key,
            revoke,
        } => {
            apply_register(
                &mut *conn, group_id, signer, standing, *epoch, epoch_key, revoke,
            )
            .await
        },
        StatementBody::Registrars { changes } => {
            manage_registrars(&mut *conn, group_id, signer, changes).await
        },
        StatementBody::RootTransfer { new_root } => {
            transfer_root(&mut *conn, group_id, signer, new_root).await
        },
        StatementBody::PushBinding { push_gid } => {
            if standing == Standing::Registrar(Rank::Member) {
                return Ok(Err(forbidden("a push binding needs the root or an admin")));
            }
            // `handle` refuses the route before this point when unset.
            let Some(pepper) = pepper else {
                return Ok(Err(refused()));
            };
            bind_push(&mut *conn, group_id, &push_binding_key(pepper, push_gid)).await
        },
    }
}

// ============================================================================
// State reads
// ============================================================================

fn key_of(bytes: &[u8]) -> Option<[u8; KEY_LEN]> {
    bytes.try_into().ok()
}

async fn load_root(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<Option<[u8; KEY_LEN]>, HandlerError> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT root_pk FROM ds_group_owner WHERE group_id = ?")
            .bind(&group_id[..])
            .fetch_optional(&mut *conn)
            .await
            .map_err(registration_db_error("load journal root"))?;
    Ok(row.and_then(|(pk,)| key_of(&pk)))
}

async fn registrar_rank(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    key: &[u8; KEY_LEN],
) -> Result<Option<Rank>, HandlerError> {
    let row: Option<(u8,)> = sqlx::query_as(
        "SELECT registrar_rank FROM ds_group_registrars WHERE group_id = ? AND registrar_pk = ?",
    )
    .bind(&group_id[..])
    .bind(&key[..])
    .fetch_optional(&mut *conn)
    .await
    .map_err(registration_db_error("load registrar rank"))?;
    Ok(row.and_then(|(r,)| Rank::from_byte(r)))
}

async fn access_key_at(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    epoch: u64,
) -> Result<Option<[u8; KEY_LEN]>, HandlerError> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT access_pk FROM ds_group_access WHERE group_id = ? AND epoch = ?")
            .bind(&group_id[..])
            .bind(epoch)
            .fetch_optional(&mut *conn)
            .await
            .map_err(registration_db_error("load access key"))?;
    Ok(row.and_then(|(pk,)| key_of(&pk)))
}

async fn latest_epoch(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<Option<u64>, HandlerError> {
    let row: (Option<u64>,) =
        sqlx::query_as("SELECT MAX(epoch) FROM ds_group_access WHERE group_id = ?")
            .bind(&group_id[..])
            .fetch_one(&mut *conn)
            .await
            .map_err(registration_db_error("load latest epoch"))?;
    Ok(row.0)
}

/// The highest rank that registered any epoch at or above `from`.
async fn highest_rank_from(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    from: u64,
) -> Result<u8, HandlerError> {
    let row: (Option<u8>,) = sqlx::query_as(
        "SELECT MAX(registered_rank) FROM ds_group_access WHERE group_id = ? AND epoch >= ?",
    )
    .bind(&group_id[..])
    .bind(from)
    .fetch_one(&mut *conn)
    .await
    .map_err(registration_db_error("load suffix ranks"))?;
    Ok(row.0.unwrap_or(0))
}

// ============================================================================
// Writes
// ============================================================================

async fn insert_epoch(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    epoch: u64,
    key: &[u8; KEY_LEN],
    signer: &[u8; KEY_LEN],
    rank: u8,
) -> Result<(), HandlerError> {
    sqlx::query(
        "INSERT INTO ds_group_access \
           (group_id, epoch, access_pk, registered_by, registered_rank) VALUES (?, ?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE access_pk = VALUES(access_pk), \
           registered_by = VALUES(registered_by), registered_rank = VALUES(registered_rank), \
           created_at = CURRENT_TIMESTAMP(3)",
    )
    .bind(&group_id[..])
    .bind(epoch)
    .bind(&key[..])
    .bind(&signer[..])
    .bind(rank)
    .execute(&mut *conn)
    .await
    .map_err(registration_db_error("register epoch key"))?;
    Ok(())
}

async fn upsert_registrar(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    key: &[u8; KEY_LEN],
    rank: Rank,
) -> Result<(), HandlerError> {
    sqlx::query(
        "INSERT INTO ds_group_registrars (group_id, registrar_pk, registrar_rank) \
         VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE registrar_rank = VALUES(registrar_rank)",
    )
    .bind(&group_id[..])
    .bind(&key[..])
    .bind(rank.to_byte())
    .execute(&mut *conn)
    .await
    .map_err(registration_db_error("write registrar"))?;
    Ok(())
}

async fn delete_registrar(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    key: &[u8; KEY_LEN],
) -> Result<(), HandlerError> {
    sqlx::query("DELETE FROM ds_group_registrars WHERE group_id = ? AND registrar_pk = ?")
        .bind(&group_id[..])
        .bind(&key[..])
        .execute(&mut *conn)
        .await
        .map_err(registration_db_error("remove registrar"))?;
    Ok(())
}

// ============================================================================
// The rules
// ============================================================================

async fn apply_claim(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    signer: &[u8; KEY_LEN],
    standing: Standing,
    epoch: u64,
    epoch_key: &[u8; KEY_LEN],
) -> Decision {
    if standing == Standing::Root {
        // The owner's claim-on-open: idempotent when it names what is there;
        // replacing the chain is a reset, said explicitly.
        return Ok(
            if access_key_at(&mut *conn, group_id, epoch).await? == Some(*epoch_key) {
                Ok(())
            } else {
                Err(conflict(
                    "journal already claimed; a reset replaces its chain",
                ))
            },
        );
    }
    sqlx::query("INSERT INTO ds_group_owner (group_id, root_pk) VALUES (?, ?)")
        .bind(&group_id[..])
        .bind(&signer[..])
        .execute(&mut *conn)
        .await
        .map_err(registration_db_error("claim journal"))?;
    insert_epoch(conn, group_id, epoch, epoch_key, signer, ROOT_RANK_BYTE).await?;
    Ok(Ok(()))
}

async fn apply_reset(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    signer: &[u8; KEY_LEN],
    epoch: u64,
    epoch_key: &[u8; KEY_LEN],
    registrars: Option<&[RegistrarEntry]>,
) -> Decision {
    if registrars.is_some_and(|set| set.iter().any(|e| e.key == *signer)) {
        return Ok(Err(bad_request("the root cannot be its own registrar")));
    }
    sqlx::query("DELETE FROM ds_group_access WHERE group_id = ?")
        .bind(&group_id[..])
        .execute(&mut *conn)
        .await
        .map_err(registration_db_error("reset epoch chain"))?;
    insert_epoch(conn, group_id, epoch, epoch_key, signer, ROOT_RANK_BYTE).await?;
    if let Some(set) = registrars {
        sqlx::query("DELETE FROM ds_group_registrars WHERE group_id = ?")
            .bind(&group_id[..])
            .execute(&mut *conn)
            .await
            .map_err(registration_db_error("reset registrars"))?;
        for entry in set {
            upsert_registrar(conn, group_id, &entry.key, entry.rank).await?;
        }
    }
    Ok(Ok(()))
}

#[allow(clippy::too_many_arguments)]
async fn apply_register(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    signer: &[u8; KEY_LEN],
    standing: Standing,
    epoch: u64,
    epoch_key: &[u8; KEY_LEN],
    revoke: &[[u8; KEY_LEN]],
) -> Decision {
    let signer_rank = standing.rank_byte();
    // Revocations first: all or nothing, decided before any write.
    let mut revoked = Vec::with_capacity(revoke.len());
    for key in revoke {
        if let Some(rank) = registrar_rank(&mut *conn, group_id, key).await? {
            if rank.to_byte() >= signer_rank {
                return Ok(Err(forbidden(
                    "a registrar may revoke only strictly lower ranks",
                )));
            }
            revoked.push(*key);
        }
    }

    let unchanged = access_key_at(&mut *conn, group_id, epoch).await? == Some(*epoch_key);
    if !unchanged {
        let latest = latest_epoch(&mut *conn, group_id).await?;
        let is_root = standing == Standing::Root;
        let truncate = match latest {
            Some(l) if l.checked_add(1) == Some(epoch) => false,
            Some(l) if epoch <= l => {
                let suffix_rank = highest_rank_from(&mut *conn, group_id, epoch).await?;
                if !is_root && suffix_rank >= signer_rank {
                    return Ok(Err(forbidden(
                        "an epoch registered by an equal or higher rank is not replaced",
                    )));
                }
                true
            },
            _ => {
                if !is_root {
                    return Ok(Err(conflict(
                        "epoch must be the next one, or replace what a lower rank registered",
                    )));
                }
                false
            },
        };
        if truncate {
            sqlx::query("DELETE FROM ds_group_access WHERE group_id = ? AND epoch > ?")
                .bind(&group_id[..])
                .bind(epoch)
                .execute(&mut *conn)
                .await
                .map_err(registration_db_error("drop replaced epochs"))?;
        }
        insert_epoch(conn, group_id, epoch, epoch_key, signer, signer_rank).await?;
        // Bound the read tier below the newest epoch; the journal pruner
        // ages out the rest. The floor is computed here so no epoch
        // arithmetic runs in SQL.
        let newest = latest_epoch(&mut *conn, group_id).await?.unwrap_or(epoch);
        if let Some(floor) = newest.checked_sub(MAX_RETAINED_EPOCHS) {
            sqlx::query("DELETE FROM ds_group_access WHERE group_id = ? AND epoch <= ?")
                .bind(&group_id[..])
                .bind(floor)
                .execute(&mut *conn)
                .await
                .map_err(registration_db_error("prune group access keys"))?;
        }
    }
    for key in &revoked {
        delete_registrar(conn, group_id, key).await?;
    }
    Ok(Ok(()))
}

async fn manage_registrars(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    root: &[u8; KEY_LEN],
    changes: &[RegistrarChange],
) -> Decision {
    if changes.iter().any(|c| c.key == *root) {
        return Ok(Err(bad_request("the root cannot be its own registrar")));
    }
    let current: Vec<(Vec<u8>,)> =
        sqlx::query_as("SELECT registrar_pk FROM ds_group_registrars WHERE group_id = ?")
            .bind(&group_id[..])
            .fetch_all(&mut *conn)
            .await
            .map_err(registration_db_error("load registrars"))?;
    let mut set: Vec<[u8; KEY_LEN]> = current.iter().filter_map(|(k,)| key_of(k)).collect();
    for c in changes {
        let present = set.iter().position(|k| *k == c.key);
        match (c.rank, present) {
            (Some(_), None) => set.push(c.key),
            (None, Some(i)) => {
                set.swap_remove(i);
            },
            _ => {},
        }
    }
    if set.len() > MAX_REGISTRARS {
        return Ok(Err(conflict("too many registrars on this journal")));
    }
    for c in changes {
        match c.rank {
            Some(rank) => upsert_registrar(conn, group_id, &c.key, rank).await?,
            None => delete_registrar(conn, group_id, &c.key).await?,
        }
    }
    Ok(Ok(()))
}

async fn transfer_root(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    root: &[u8; KEY_LEN],
    new_root: &[u8; KEY_LEN],
) -> Decision {
    if new_root == root {
        return Ok(Ok(()));
    }
    // The root is not a registrar; a key that was one stops being one.
    delete_registrar(conn, group_id, new_root).await?;
    sqlx::query(
        "UPDATE ds_group_owner SET root_pk = ?, updated_at = CURRENT_TIMESTAMP(3) \
         WHERE group_id = ?",
    )
    .bind(&new_root[..])
    .bind(&group_id[..])
    .execute(&mut *conn)
    .await
    .map_err(registration_db_error("transfer root"))?;
    Ok(Ok(()))
}

async fn bind_push(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    binding_key: &[u8; KEY_LEN],
) -> Decision {
    let holder: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT group_id FROM ds_push_bindings WHERE binding_key = ?")
            .bind(&binding_key[..])
            .fetch_optional(&mut *conn)
            .await
            .map_err(registration_db_error("load push binding"))?;
    match holder {
        Some((g,)) if g.as_slice() == group_id.as_slice() => return Ok(Ok(())),
        Some(_) => return Ok(Err(conflict("push_gid is bound to another journal"))),
        None => {},
    }
    // Replace this journal's binding; the previous push_gid is freed.
    sqlx::query("DELETE FROM ds_push_bindings WHERE group_id = ?")
        .bind(&group_id[..])
        .execute(&mut *conn)
        .await
        .map_err(registration_db_error("replace push binding"))?;
    let inserted =
        sqlx::query("INSERT INTO ds_push_bindings (group_id, binding_key) VALUES (?, ?)")
            .bind(&group_id[..])
            .bind(&binding_key[..])
            .execute(&mut *conn)
            .await;
    match inserted {
        Ok(_) => Ok(Ok(())),
        Err(e) => {
            let duplicate = e
                .as_database_error()
                .and_then(|d| d.try_downcast_ref::<MySqlDatabaseError>())
                .is_some_and(|m| m.number() == ER_DUP_ENTRY);
            if duplicate {
                // Another journal bound the same push_gid concurrently.
                Ok(Err(conflict("push_gid is bound to another journal")))
            } else {
                Err(registration_db_error("write push binding")(e))
            }
        },
    }
}

/// Whether a wake naming `push_gid` may go out for a publish to `group_id`:
/// the journal's binding must be that `push_gid`. `false` when the pepper is
/// unset (every wake dropped) and on any database error (the publish has
/// already succeeded; only the wake is lost, and it is logged).
pub(crate) async fn wake_is_bound(
    state: &AppState,
    group_id: &[u8; PSEUDONYM_LEN],
    push_gid: &[u8; PSEUDONYM_LEN],
) -> bool {
    let Some(pepper) = state.binding_pepper.as_ref() else {
        return false;
    };
    let key = push_binding_key(pepper, push_gid);
    match sqlx::query_as::<_, (i32,)>(
        "SELECT 1 FROM ds_push_bindings WHERE group_id = ? AND binding_key = ?",
    )
    .bind(&group_id[..])
    .bind(&key[..])
    .fetch_optional(&state.db)
    .await
    {
        Ok(row) => row.is_some(),
        Err(e) => {
            tracing::warn!(error = %e, "push binding lookup failed; wake dropped");
            false
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use lakko_ds_proof::statement::{StatementSigner, MAX_LIST_ENTRIES};
    use lakko_ds_proof::SigningKey;
    use sqlx::MySqlPool;

    #[test]
    fn the_pepper_is_64_hex_or_unset() {
        let env = |v: Option<String>| move |_: &str| v.clone();
        assert_eq!(pepper_from_lookup(env(None)), Ok(None));
        assert_eq!(pepper_from_lookup(env(Some("  ".into()))), Ok(None));
        assert_eq!(
            pepper_from_lookup(env(Some("ab".repeat(32)))).unwrap(),
            Some([0xab; 32])
        );
        assert!(pepper_from_lookup(env(Some("abcd".into()))).is_err());
        assert!(pepper_from_lookup(env(Some("zz".repeat(32)))).is_err());
    }

    fn members_uri(gid: &[u8; PSEUDONYM_LEN]) -> String {
        format!("/ds/group/{}/messages", hex::encode(gid))
    }

    /// Read the journal under `key` at `epoch`; the status.
    async fn read_as(
        app: &axum::Router,
        gid: &[u8; PSEUDONYM_LEN],
        key: &SigningKey,
        epoch: u64,
    ) -> StatusCode {
        send(
            app,
            Req {
                method: "GET",
                uri: &format!("{}?after=0", members_uri(gid)),
                body: vec![],
                proof: Some((key, epoch)),
            },
        )
        .await
        .0
    }

    async fn set_registrars(
        app: &axum::Router,
        gid: &[u8; PSEUDONYM_LEN],
        root: &StatementSigner,
        changes: &[(&StatementSigner, Option<Rank>)],
    ) -> StatusCode {
        send_statement(
            app,
            gid,
            root,
            StatementBody::Registrars {
                changes: changes
                    .iter()
                    .map(|(s, rank)| RegistrarChange {
                        key: s.public_key(),
                        rank: *rank,
                    })
                    .collect(),
            },
        )
        .await
    }

    async fn epochs(pool: &MySqlPool, gid: &[u8; PSEUDONYM_LEN]) -> Vec<(u64, u8)> {
        sqlx::query_as(
            "SELECT epoch, registered_rank FROM ds_group_access WHERE group_id = ? ORDER BY epoch",
        )
        .bind(&gid[..])
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn registrar_count(pool: &MySqlPool, gid: &[u8; PSEUDONYM_LEN]) -> i64 {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM ds_group_registrars WHERE group_id = ?")
                .bind(&gid[..])
                .fetch_one(pool)
                .await
                .unwrap();
        row.0
    }

    /// A claimed journal with a root, one admin and two members, at epoch 1.
    struct Journal {
        gid: [u8; PSEUDONYM_LEN],
        root: StatementSigner,
        admin: StatementSigner,
        member: StatementSigner,
        member2: StatementSigner,
    }

    async fn journal(app: &axum::Router, tag: u8) -> Journal {
        let j = Journal {
            gid: fresh_name(tag),
            root: fresh_signer(tag),
            admin: fresh_signer(tag),
            member: fresh_signer(tag),
            member2: fresh_signer(tag),
        };
        assert_eq!(
            claim(app, &j.gid, &j.root, 1, &key(1)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            set_registrars(
                app,
                &j.gid,
                &j.root,
                &[
                    (&j.admin, Some(Rank::Admin)),
                    (&j.member, Some(Rank::Member)),
                    (&j.member2, Some(Rank::Member)),
                ],
            )
            .await,
            StatusCode::NO_CONTENT
        );
        j
    }

    /// The last clause of ADR-0057's decision: an epoch key opens the
    /// journal, it no longer extends it. A statement signed by an epoch key
    /// is a stranger's; the ADR-0053 body is not a statement at all.
    #[tokio::test]
    async fn plain_epoch_key_holders_can_no_longer_register() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x60).await;
        let epoch_key_signer = StatementSigner::from_seed(&[1; 32]);
        assert_eq!(
            key(1).verifying_key().to_bytes(),
            epoch_key_signer.public_key()
        );
        assert_eq!(
            register(&app, &j.gid, &epoch_key_signer, 2, &key(2), &[]).await,
            StatusCode::UNAUTHORIZED
        );
        // The ADR-0053 registration body, proven under the epoch key.
        let (status, _) = send(
            &app,
            Req {
                method: "PUT",
                uri: &format!("/ds/group/{}/access", hex::encode(j.gid)),
                body: serde_json::json!({ "epoch": 2, "access_pk_b64": pk_b64(&key(2)) })
                    .to_string()
                    .into_bytes(),
                proof: Some((&key(1), 1)),
            },
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // A stranger with a fresh key: the same 401.
        assert_eq!(
            register(&app, &j.gid, &fresh_signer(0x61), 2, &key(2), &[]).await,
            StatusCode::UNAUTHORIZED
        );
        // A member registrar may register the next epoch.
        assert_eq!(
            register(&app, &j.gid, &j.member, 2, &key(2), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(read_as(&app, &j.gid, &key(2), 2).await, StatusCode::OK);
    }

    /// ADR-0057 Context, attack 1: an evicted member extends the chain with
    /// keys only it holds (here two epochs ahead). The admin committer's
    /// registration of the real next epoch replaces the whole fake suffix and
    /// revokes the evictee in the same transaction; the evictee can then
    /// neither register nor read under its fake keys.
    #[tokio::test]
    async fn an_evictee_cannot_keep_the_journal() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x62).await;
        let (fake2, fake3) = (key(0xF2), key(0xF3));
        assert_eq!(
            register(&app, &j.gid, &j.member, 2, &fake2, &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            register(&app, &j.gid, &j.member, 3, &fake3, &[]).await,
            StatusCode::NO_CONTENT
        );
        let real2 = key(0x22);
        assert_eq!(
            register(&app, &j.gid, &j.admin, 2, &real2, &[&j.member]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 2)]);
        assert_eq!(
            registrar_count(&pool, &j.gid).await,
            2,
            "the evictee was revoked"
        );
        assert_eq!(
            register(&app, &j.gid, &j.member, 3, &fake3, &[]).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            read_as(&app, &j.gid, &fake3, 3).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            read_as(&app, &j.gid, &fake2, 2).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(read_as(&app, &j.gid, &real2, 2).await, StatusCode::OK);
        // The group moves on.
        assert_eq!(
            register(&app, &j.gid, &j.admin, 3, &key(0x23), &[]).await,
            StatusCode::NO_CONTENT
        );
    }

    /// ADR-0057 Context, attack 2: a member pre-registers N+1 to block its
    /// own removal. The admin's sweep registration replaces it; an equal
    /// rank cannot, and a rank never replaces what a higher one registered.
    #[tokio::test]
    async fn a_pre_registration_does_not_block_a_sweep() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x63).await;
        assert_eq!(
            register(&app, &j.gid, &j.member, 2, &key(0xF2), &[]).await,
            StatusCode::NO_CONTENT
        );
        // Equal rank: refused.
        assert_eq!(
            register(&app, &j.gid, &j.member2, 2, &key(0x32), &[]).await,
            StatusCode::FORBIDDEN
        );
        // Higher rank: replaced.
        assert_eq!(
            register(&app, &j.gid, &j.admin, 2, &key(0x42), &[&j.member]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 2)]);
        // An admin does not replace another admin's (or its own rank's)
        // registration, nor the root's; the root replaces anything.
        assert_eq!(
            register(&app, &j.gid, &j.member2, 2, &key(0x52), &[]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            register(&app, &j.gid, &j.root, 2, &key(0x62), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            register(&app, &j.gid, &j.admin, 2, &key(0x72), &[]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            register(&app, &j.gid, &j.admin, 1, &key(0x73), &[]).await,
            StatusCode::FORBIDDEN,
            "a suffix containing a root registration is the root's"
        );
    }

    /// ADR-0057 Context, attack 3: a group with a deterministic id is
    /// re-founded after lost state. Its root (same account seed, same id) is
    /// the same, so a reset re-claims the journal; the old chain stops
    /// opening it. A claim does not silently replace a chain, and nobody else
    /// resets.
    #[tokio::test]
    async fn a_refounded_group_reclaims_by_root_reset() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let account_seed = fresh_name(0x64);
        let gid = fresh_name(0x64);
        let root = StatementSigner::root(&account_seed, &gid);
        let old = key(0x81);
        assert_eq!(
            claim(&app, &gid, &root, 40, &old).await,
            StatusCode::NO_CONTENT
        );
        let old_registrar = fresh_signer(0x64);
        assert_eq!(
            set_registrars(&app, &gid, &root, &[(&old_registrar, Some(Rank::Admin))]).await,
            StatusCode::NO_CONTENT
        );
        // The re-founded group derives the same root.
        let again = StatementSigner::root(&account_seed, &gid);
        assert_eq!(again.public_key(), root.public_key());
        // Re-sending the registered claim is idempotent; a different one is not.
        assert_eq!(
            claim(&app, &gid, &again, 40, &old).await,
            StatusCode::NO_CONTENT
        );
        let fresh = key(0x82);
        assert_eq!(
            claim(&app, &gid, &again, 0, &fresh).await,
            StatusCode::CONFLICT
        );
        // Another root neither claims nor resets.
        let other = fresh_signer(0x64);
        assert_eq!(
            claim(&app, &gid, &other, 0, &fresh).await,
            StatusCode::UNAUTHORIZED
        );
        let new_admin = fresh_signer(0x64);
        let reset = |signer: StatementSigner, registrars| {
            let app = app.clone();
            let epoch_key = fresh.verifying_key().to_bytes();
            async move {
                send_statement(
                    &app,
                    &gid,
                    &signer,
                    StatementBody::Reset {
                        epoch: 0,
                        epoch_key,
                        registrars,
                    },
                )
                .await
            }
        };
        assert_eq!(reset(other.clone(), None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(
            reset(
                again.clone(),
                Some(vec![RegistrarEntry {
                    key: new_admin.public_key(),
                    rank: Rank::Admin,
                }])
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &gid).await, vec![(0, 3)]);
        assert_eq!(
            read_as(&app, &gid, &old, 40).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(read_as(&app, &gid, &fresh, 0).await, StatusCode::OK);
        // The registrar set sent with the reset replaced the old one.
        assert_eq!(
            register(&app, &gid, &old_registrar, 1, &key(0x83), &[]).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            register(&app, &gid, &new_admin, 1, &key(0x83), &[]).await,
            StatusCode::NO_CONTENT
        );
        // A reset without a set keeps the registrars.
        assert_eq!(reset(again.clone(), None).await, StatusCode::NO_CONTENT);
        assert_eq!(registrar_count(&pool, &gid).await, 1);
    }

    #[tokio::test]
    async fn rank_rules_for_skips_and_revocations() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x65).await;
        // A skip past latest + 1 is the root's.
        assert_eq!(
            register(&app, &j.gid, &j.admin, 5, &key(5), &[]).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            register(&app, &j.gid, &j.root, 5, &key(5), &[]).await,
            StatusCode::NO_CONTENT
        );
        // Revoking an equal rank refuses the whole statement: no epoch, no
        // revocation.
        assert_eq!(
            register(&app, &j.gid, &j.member, 6, &key(6), &[&j.member2]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (5, 3)]);
        assert_eq!(registrar_count(&pool, &j.gid).await, 3);
        assert_eq!(
            register(&app, &j.gid, &j.member, 6, &key(6), &[&j.admin]).await,
            StatusCode::FORBIDDEN
        );
        // An admin revokes a member; revoking a key that is no registrar is
        // a no-op.
        let nobody = fresh_signer(0x65);
        assert_eq!(
            register(&app, &j.gid, &j.admin, 6, &key(6), &[&j.member2, &nobody]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(registrar_count(&pool, &j.gid).await, 2);
        // The root revokes an admin while registering.
        assert_eq!(
            register(&app, &j.gid, &j.root, 7, &key(7), &[&j.admin]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(registrar_count(&pool, &j.gid).await, 1);
        assert_eq!(
            register(&app, &j.gid, &j.admin, 8, &key(8), &[]).await,
            StatusCode::UNAUTHORIZED
        );
        // Re-sending a registered key changes nothing and needs no rank over
        // it.
        assert_eq!(
            register(&app, &j.gid, &j.member, 7, &key(7), &[]).await,
            StatusCode::NO_CONTENT
        );
        // Registrar management is the root's alone.
        let extra = fresh_signer(0x65);
        assert_eq!(
            set_registrars(&app, &j.gid, &j.member, &[(&extra, Some(Rank::Admin))]).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            set_registrars(&app, &j.gid, &j.root, &[(&j.root, Some(Rank::Admin))]).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn the_root_is_transferred_only_by_the_root() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x66).await;
        let new_root = fresh_signer(0x66);
        let transfer = |signer: StatementSigner| {
            let app = app.clone();
            let gid = j.gid;
            let new_root = new_root.public_key();
            async move {
                send_statement(
                    &app,
                    &gid,
                    &signer,
                    StatementBody::RootTransfer { new_root },
                )
                .await
            }
        };
        assert_eq!(transfer(j.admin.clone()).await, StatusCode::UNAUTHORIZED);
        assert_eq!(transfer(new_root.clone()).await, StatusCode::UNAUTHORIZED);
        assert_eq!(transfer(j.root.clone()).await, StatusCode::NO_CONTENT);
        // The old root is a stranger now; the new one holds every power.
        assert_eq!(
            register(&app, &j.gid, &j.root, 9, &key(9), &[]).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            set_registrars(&app, &j.gid, &new_root, &[(&j.admin, None)]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            register(&app, &j.gid, &new_root, 9, &key(9), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            claim(&app, &j.gid, &j.root, 1, &key(1)).await,
            StatusCode::UNAUTHORIZED
        );
    }

    fn wake_value(push_gid: [u8; 32]) -> String {
        format!("{};message", URL_SAFE_NO_PAD.encode(push_gid))
    }

    async fn publish_waking(
        app: &axum::Router,
        gid: &[u8; PSEUDONYM_LEN],
        key: &SigningKey,
        epoch: u64,
        push_gid: [u8; 32],
    ) -> StatusCode {
        send_with(
            app,
            Req {
                method: "POST",
                uri: &members_uri(gid),
                body: b"ciphertext".to_vec(),
                proof: Some((key, epoch)),
            },
            &[(crate::wake::WAKE_HEADER, &wake_value(push_gid))],
        )
        .await
        .0
    }

    async fn bind(
        app: &axum::Router,
        gid: &[u8; PSEUDONYM_LEN],
        signer: &StatementSigner,
        push_gid: [u8; 32],
    ) -> StatusCode {
        send_statement(app, gid, signer, StatementBody::PushBinding { push_gid }).await
    }

    /// ADR-0057 section 2: a push_gid belongs to at most one journal, bound
    /// by its root or an admin, and a wake naming any other push_gid is
    /// dropped while the publish still lands.
    #[tokio::test]
    async fn a_journal_wakes_only_its_bound_push_gid() {
        let Some((app, _pool, waker)) = app_with_waker().await else {
            return;
        };
        let a = journal(&app, 0x67).await;
        let b = journal(&app, 0x68).await;
        let gid_one = fresh_name(0x69);
        let gid_two = fresh_name(0x69);
        assert_eq!(
            bind(&app, &a.gid, &a.member, gid_one).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            bind(&app, &a.gid, &fresh_signer(0x69), gid_one).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            bind(&app, &a.gid, &a.admin, gid_one).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            bind(&app, &a.gid, &a.root, gid_one).await,
            StatusCode::NO_CONTENT
        );
        // Another journal cannot take it.
        assert_eq!(
            bind(&app, &b.gid, &b.root, gid_one).await,
            StatusCode::CONFLICT
        );

        // A's own push_gid wakes; a foreign one is dropped, the publish is not.
        assert_eq!(
            publish_waking(&app, &a.gid, &key(1), 1, gid_one).await,
            StatusCode::CREATED
        );
        assert_eq!(
            publish_waking(&app, &a.gid, &key(1), 1, gid_two).await,
            StatusCode::CREATED
        );
        // B has no binding: nothing it names wakes.
        assert_eq!(
            publish_waking(&app, &b.gid, &key(1), 1, gid_one).await,
            StatusCode::CREATED
        );
        let fired = waker.fired_targets();
        assert!(fired.contains(&gid_one));
        assert!(!fired.contains(&gid_two));
        assert_eq!(fired.iter().filter(|g| **g == gid_one).count(), 1);

        // A rotation re-binds A; the old push_gid is freed for B.
        assert_eq!(
            bind(&app, &a.gid, &a.admin, gid_two).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            bind(&app, &b.gid, &b.admin, gid_one).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            publish_waking(&app, &a.gid, &key(1), 1, gid_one).await,
            StatusCode::CREATED
        );
        let fired = waker.fired_targets();
        assert_eq!(fired.iter().filter(|g| **g == gid_one).count(), 1);
    }

    #[tokio::test]
    async fn without_a_pepper_binding_is_503_and_every_wake_drops() {
        let Some((app, _pool, waker)) = app_configured(None).await else {
            return;
        };
        let j = journal(&app, 0x6A).await;
        let push_gid = fresh_name(0x6A);
        let statement = j
            .root
            .sign_at(
                &j.gid,
                StatementBody::PushBinding { push_gid },
                now_ms(),
                fresh_nonce(),
            )
            .unwrap();
        let (status, body) = send(
            &app,
            Req {
                method: "PUT",
                uri: &format!("/ds/group/{}/push-binding", hex::encode(j.gid)),
                body: statement.body_json().into_bytes(),
                proof: None,
            },
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error_code(&body), "push-binding-disabled");
        assert_eq!(
            publish_waking(&app, &j.gid, &key(1), 1, push_gid).await,
            StatusCode::CREATED
        );
        assert!(waker.fired_targets().is_empty());
    }

    /// Shape before signature before standing before rule: a malformed body
    /// is a 400, a stranger learns only the uniform 401 whatever it names,
    /// and a replayed statement is refused.
    #[tokio::test]
    async fn refusals_come_in_order_and_replays_are_refused() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x6B).await;
        let stranger = fresh_signer(0x6B);
        for epoch in [0, 1, 2, 9, 1_000] {
            assert_eq!(
                register(&app, &j.gid, &stranger, epoch, &key(9), &[]).await,
                StatusCode::UNAUTHORIZED,
                "epoch {epoch}"
            );
        }
        // A statement about another journal, or of another kind, is a 400.
        let other = fresh_name(0x6B);
        let about_other = j
            .root
            .sign_at(
                &other,
                StatementBody::RegisterEpoch {
                    epoch: 2,
                    epoch_key: key(2).verifying_key().to_bytes(),
                    revoke: vec![],
                },
                now_ms(),
                fresh_nonce(),
            )
            .unwrap();
        let put = |segment: &'static str, body: Vec<u8>| {
            let app = app.clone();
            let uri = format!("/ds/group/{}/{segment}", hex::encode(j.gid));
            async move {
                send(
                    &app,
                    Req {
                        method: "PUT",
                        uri: &uri,
                        body,
                        proof: None,
                    },
                )
                .await
                .0
            }
        };
        assert_eq!(
            put("access", about_other.body_json().into_bytes()).await,
            StatusCode::BAD_REQUEST
        );
        let register_two = j
            .root
            .sign_at(
                &j.gid,
                StatementBody::RegisterEpoch {
                    epoch: 2,
                    epoch_key: key(2).verifying_key().to_bytes(),
                    revoke: vec![],
                },
                now_ms(),
                fresh_nonce(),
            )
            .unwrap();
        assert_eq!(
            put("registrars", register_two.body_json().into_bytes()).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(put("access", b"{}".to_vec()).await, StatusCode::BAD_REQUEST);
        assert_eq!(
            put("access", br#"{"statement_b64":"AAAA"}"#.to_vec()).await,
            StatusCode::BAD_REQUEST
        );
        // A stale statement is a 401.
        let stale = j
            .root
            .sign_at(
                &j.gid,
                StatementBody::RegisterEpoch {
                    epoch: 2,
                    epoch_key: key(2).verifying_key().to_bytes(),
                    revoke: vec![],
                },
                now_ms() - lakko_ds_proof::PROOF_WINDOW_MS - 1_000,
                fresh_nonce(),
            )
            .unwrap();
        assert_eq!(
            put("access", stale.body_json().into_bytes()).await,
            StatusCode::UNAUTHORIZED
        );
        // Accepted once, refused on replay.
        let body = register_two.body_json().into_bytes();
        assert_eq!(put("access", body.clone()).await, StatusCode::NO_CONTENT);
        assert_eq!(put("access", body).await, StatusCode::UNAUTHORIZED);
    }

    /// ADR-0053 security pass, finding 1, carried over: an epoch above the
    /// cap is a 400 on a claim and a registration alike, before anything is
    /// read, and a group at the cap prunes without overflowing.
    #[tokio::test]
    async fn an_epoch_above_the_cap_is_refused() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let gid = fresh_name(0x6C);
        let root = fresh_signer(0x6C);
        for epoch in [MAX_ACCESS_EPOCH + 1, u64::MAX] {
            assert_eq!(
                claim(&app, &gid, &root, epoch, &key(1)).await,
                StatusCode::BAD_REQUEST,
                "epoch {epoch}"
            );
        }
        assert!(
            epochs(&pool, &gid).await.is_empty(),
            "a refused claim left a row"
        );
        assert_eq!(
            claim(&app, &gid, &root, MAX_ACCESS_EPOCH, &key(1)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            register(&app, &gid, &root, MAX_ACCESS_EPOCH + 1, &key(2), &[]).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            register(&app, &gid, &root, MAX_ACCESS_EPOCH, &key(3), &[]).await,
            StatusCode::NO_CONTENT
        );
        crate::access::prune_group_access(&pool, 1).await;
    }

    /// ADR-0053 security pass, finding 4, carried over: concurrent first
    /// claims of one journal serialize behind the group's lock row. Exactly
    /// one root wins; every other finds the journal claimed (401) or loses
    /// the lock race (409), and the stored root and key are the winner's.
    #[tokio::test]
    async fn concurrent_first_claims_leave_exactly_one_root() {
        let Some((app, pool)) = app().await else {
            return;
        };
        for round in 0..4u8 {
            let gid = fresh_name(0x6D);
            let tasks: Vec<_> = (0..6u8)
                .map(|i| {
                    let app = app.clone();
                    let root = fresh_signer(0x6D);
                    let epoch_key = key(100 + round * 10 + i);
                    tokio::spawn(async move {
                        let status = claim(&app, &gid, &root, 0, &epoch_key).await;
                        (
                            status,
                            root.public_key(),
                            epoch_key.verifying_key().to_bytes(),
                        )
                    })
                })
                .collect();
            let mut winners = Vec::new();
            for t in tasks {
                let (status, root, epoch_key) = t.await.unwrap();
                match status {
                    StatusCode::NO_CONTENT => winners.push((root, epoch_key)),
                    StatusCode::UNAUTHORIZED | StatusCode::CONFLICT => {},
                    other => panic!("unexpected {other}"),
                }
            }
            assert_eq!(winners.len(), 1, "round {round}");
            let stored: (Vec<u8>,) =
                sqlx::query_as("SELECT root_pk FROM ds_group_owner WHERE group_id = ?")
                    .bind(&gid[..])
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(stored.0, winners[0].0.to_vec(), "round {round}");
            let keys: Vec<(Vec<u8>,)> =
                sqlx::query_as("SELECT access_pk FROM ds_group_access WHERE group_id = ?")
                    .bind(&gid[..])
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert_eq!(keys, vec![(winners[0].1.to_vec(),)], "round {round}");
        }
    }

    #[tokio::test]
    async fn the_registrar_set_is_bounded() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let gid = fresh_name(0x6E);
        let root = fresh_signer(0x6E);
        assert_eq!(
            claim(&app, &gid, &root, 1, &key(1)).await,
            StatusCode::NO_CONTENT
        );
        let batch = MAX_LIST_ENTRIES;
        let batches = MAX_REGISTRARS / batch;
        for _ in 0..batches {
            let signers: Vec<StatementSigner> = (0..batch).map(|_| fresh_signer(0x6E)).collect();
            let changes: Vec<_> = signers.iter().map(|s| (s, Some(Rank::Member))).collect();
            assert_eq!(
                set_registrars(&app, &gid, &root, &changes).await,
                StatusCode::NO_CONTENT
            );
        }
        let one_more = fresh_signer(0x6E);
        assert_eq!(
            set_registrars(&app, &gid, &root, &[(&one_more, Some(Rank::Member))]).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            registrar_count(&pool, &gid).await,
            i64::try_from(MAX_REGISTRARS).unwrap()
        );
    }
}
