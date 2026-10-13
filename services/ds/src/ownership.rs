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
//! | `reset` | the root; or ANY signer carrying a key-reset attestation (`key_reset_attest_b64`, ADR-0073) AND the attested identity key's endorsement of that signer (`root_endorsement_b64`, ADR-0074 D11; `src/key_reset.rs`) | replaces the chain (and the registrar set when sent); attested: replaces the root too, drops the push binding, records the generation |
//! | `access` | root or registrar | appends or replaces an epoch, revokes lower ranks |
//! | `registrars` | the root | adds, re-ranks, removes registrars |
//! | `registrars-set` | the root | replaces the whole registrar set |
//! | `root` | the root | names a new root (naming itself: a `204` no-op, the "am I still the root?" probe) |
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
//! **The epoch rule** (`access`), with `latest` the newest registered epoch
//! and ranks member 1, admin 2, root 3 (a claim or reset registers as the
//! root). A statement says which of two things it does (the 2026-09-28
//! security pass: without it, a lagging root device's ordinary registration
//! truncated epochs other devices had legitimately advanced):
//!
//! - **Append** `e`: re-sending the key already registered at `e` changes
//!   nothing (`204`); otherwise `e` must be `latest + 1` (`409`). A
//!   **member** may append any number of epochs in a row (ADR-0057
//!   amendment of 2026-10-01, item 3: the earlier limit `e <= H + 1` blocked
//!   two device admissions in a row with no owner or admin online). What
//!   keeps a member from freezing writes is the write tier, not this rule:
//!   every epoch from `H` onward stays writable (`src/access.rs`), `H` the
//!   highest epoch registered by an admin or the root, so a member's keys
//!   past `H` cannot push the real key out. One bound remains, on storage
//!   only: a member may not append once [`MAX_RETAINED_EPOCHS`] epochs
//!   already sit at or after `H` (`403`), so the epochs the write tier needs
//!   are always among the newest [`MAX_RETAINED_EPOCHS`] the register-time
//!   bound keeps, and a member cannot grow a journal's rows without limit.
//!   An admin or root registration moves `H` and lifts it.
//! - **Replace** `e`: a member never replaces (`403`; there is no lower rank
//!   to replace). For `e <= latest`: every epoch strictly above `e` must have
//!   been registered by a strictly lower rank than the signer's, and the
//!   registration at `e`, if any, by a strictly lower rank or with the same
//!   key (`403` otherwise); the epochs above `e` are dropped and `e` is
//!   written (or kept). The root follows the same rule - there is no root
//!   override - so a lagging root device can never truncate what another
//!   root device registered; replacing a root registration is a reset. This
//!   is how an admin at epoch `N` whose append of `N + 1` met a
//!   member-registered fake `N + 1` drops it: `Replace(N, its own key)`.
//!   `e == latest + 1` has nothing to drop and is an append; a skip past
//!   `latest + 1` is the root's (`403` for a registrar).
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
    Rank, RegisterMode, RegistrarChange, RegistrarEntry, SignedStatement, StatementBody,
    StatementKind, MAX_REGISTRARS, ROOT_RANK_BYTE,
};
use serde::Deserialize;
use sqlx::mysql::{MySqlConnection, MySqlDatabaseError};

use crate::access::{
    consume_nonce, db_error, lock_group, now_ms, refused, registration_db_error, HandlerError,
    MAX_ACCESS_EPOCH, MAX_RETAINED_EPOCHS,
};
use crate::journal::{self, PSEUDONYM_LEN};
use crate::key_reset;
use crate::AppState;
use lakko_attest::KeyResetAttestation;

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
#[allow(clippy::struct_field_names)] // the names are the wire's
pub(crate) struct StatementJson {
    /// Standard base64 of the signed statement blob.
    statement_b64: String,
    /// ADR-0073 section 5, `reset` only: standard base64 of the 177-byte
    /// key-reset attestation (auth hands it out URL-safe; the client
    /// re-encodes, as it does for every other base64 field here). On any
    /// other route the field is refused (`400`).
    #[serde(default)]
    key_reset_attest_b64: Option<String>,
    /// ADR-0074 section 4.6 (decision D11), required with
    /// `key_reset_attest_b64` and refused without it: standard base64 of the
    /// 64-byte root endorsement, the attested identity key signing the key
    /// that becomes this journal's root (`src/key_reset.rs`).
    #[serde(default)]
    root_endorsement_b64: Option<String>,
}

/// The attestation fields of a statement body, as they arrived.
struct AttestFields {
    key_reset_attest_b64: String,
    root_endorsement_b64: Option<String>,
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
/// The second value is the attestation the body carries, which only the
/// `reset` and `root` routes accept.
fn parse_statement(
    body: &[u8],
    kind: StatementKind,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<(SignedStatement, Option<AttestFields>), HandlerError> {
    let json: StatementJson = serde_json::from_slice(body)
        .map_err(|_| bad_request("body must be {\"statement_b64\": ...}"))?;
    if json.key_reset_attest_b64.is_some()
        && !matches!(kind, StatementKind::Reset | StatementKind::RootTransfer)
    {
        return Err(bad_request(
            "key_reset_attest_b64 belongs to the reset and root routes",
        ));
    }
    if json.root_endorsement_b64.is_some() && json.key_reset_attest_b64.is_none() {
        return Err(bad_request(
            "root_endorsement_b64 accompanies key_reset_attest_b64",
        ));
    }
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
    let attest = json.key_reset_attest_b64.map(|field| AttestFields {
        key_reset_attest_b64: field,
        root_endorsement_b64: json.root_endorsement_b64,
    });
    Ok((statement, attest))
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
statement_route!(put_registrars_set, StatementKind::RegistrarsSet);

async fn handle(
    state: &AppState,
    group_hex: &str,
    body: &[u8],
    kind: StatementKind,
) -> Result<Response, HandlerError> {
    let group_id = journal::decode_pseudonym(group_hex)?;
    let (statement, attest_fields) = parse_statement(body, kind, &group_id)?;
    // ADR-0073: the attested arm. Shape and the pinned key first (`400` /
    // `503`), then the signature and the journal binding, then the root
    // endorsement (ADR-0074 D11: the attested identity key must name the key
    // that becomes the root), the last two the uniform `401`; all before the
    // statement's own signature is checked.
    let attested = match attest_fields {
        None => None,
        Some(fields) => {
            let Some(attest_key) = state.attest_key.as_ref() else {
                return Ok(key_reset::disabled());
            };
            let attestation =
                key_reset::verify_for_journal(&fields.key_reset_attest_b64, attest_key, &group_id)?;
            let becomes_root = match &statement.body {
                StatementBody::RootTransfer { new_root } => new_root,
                _ => &statement.signer,
            };
            key_reset::verify_endorsement(
                fields.root_endorsement_b64.as_deref(),
                &attestation,
                becomes_root,
            )?;
            Some(attestation)
        },
    };
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
    let decision = decide(
        &mut tx,
        &group_id,
        &statement,
        pepper.as_ref(),
        attested.as_ref(),
    )
    .await?;
    tx.commit()
        .await
        .map_err(registration_db_error("commit ownership statement"))?;
    decision.map(|()| StatusCode::NO_CONTENT.into_response())
}

/// Whether a signer of `standing` is heard on a statement of `kind` at
/// all. A verified key-reset attestation (`attested`, ADR-0073) lets any
/// signer be heard on a reset; the generation rule then decides. A
/// registrar sending a registrars set is heard and refused (403): it is
/// someone to this journal, only not the root.
const fn is_heard(kind: StatementKind, standing: Standing, attested: bool) -> bool {
    match kind {
        StatementKind::Claim => matches!(standing, Standing::Unclaimed | Standing::Root),
        StatementKind::Reset => matches!(standing, Standing::Root) || attested,
        StatementKind::Registrars | StatementKind::RootTransfer => {
            matches!(standing, Standing::Root)
        },
        StatementKind::RegisterEpoch
        | StatementKind::PushBinding
        | StatementKind::RegistrarsSet => {
            matches!(standing, Standing::Root | Standing::Registrar(_))
        },
    }
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

#[allow(clippy::too_many_lines)] // one arm per statement kind, kept in one place
async fn decide(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    statement: &SignedStatement,
    pepper: Option<&[u8; KEY_LEN]>,
    attested: Option<&KeyResetAttestation>,
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
    if !is_heard(statement.body.kind(), standing, attested.is_some()) {
        return Ok(Err(refused()));
    }
    // ADR-0073 section 5 step 5: the generation is checked BEFORE the nonce
    // is spent, so a replayed or overtaken attestation costs the server no
    // row (the uniform refusal order of ADR-0057 for everything else).
    if let Some(attestation) = attested {
        let stored = key_reset::stored_generation(&mut *conn, group_id).await?;
        if !key_reset::generation_is_newer(attestation.key_generation, stored) {
            return Ok(Err(conflict(
                "key-reset attestation is not newer than the journal's generation",
            )));
        }
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
        } => match attested {
            Some(attestation) => {
                key_reset::apply_attested_reset(
                    &mut *conn,
                    group_id,
                    signer,
                    attestation.key_generation,
                    *epoch,
                    epoch_key,
                    registrars.as_deref(),
                )
                .await
            },
            None => {
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
        },
        StatementBody::RegisterEpoch {
            epoch,
            epoch_key,
            mode,
            revoke,
        } => {
            let registration = Registration {
                signer,
                standing,
                epoch: *epoch,
                epoch_key,
                mode: *mode,
            };
            apply_register(&mut *conn, group_id, &registration, revoke).await
        },
        StatementBody::RegistrarsSet { registrars } => {
            if standing != Standing::Root {
                return Ok(Err(forbidden("the registrar set is the root's")));
            }
            replace_registrars(&mut *conn, group_id, registrars).await
        },
        StatementBody::Registrars { changes } => {
            manage_registrars(&mut *conn, group_id, signer, changes).await
        },
        StatementBody::RootTransfer { new_root } => {
            transfer_root(
                &mut *conn,
                group_id,
                signer,
                new_root,
                attested.map(|a| a.key_generation),
            )
            .await
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

/// The highest rank that registered any epoch strictly above `above`
/// (0 when none).
async fn highest_rank_above(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    above: u64,
) -> Result<u8, HandlerError> {
    let row: (Option<u8>,) = sqlx::query_as(
        "SELECT MAX(registered_rank) FROM ds_group_access WHERE group_id = ? AND epoch > ?",
    )
    .bind(&group_id[..])
    .bind(above)
    .fetch_one(&mut *conn)
    .await
    .map_err(registration_db_error("load suffix ranks"))?;
    Ok(row.0.unwrap_or(0))
}

/// The key and rank registered at `epoch`.
async fn registration_at(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    epoch: u64,
) -> Result<Option<([u8; KEY_LEN], u8)>, HandlerError> {
    let row: Option<(Vec<u8>, u8)> = sqlx::query_as(
        "SELECT access_pk, registered_rank FROM ds_group_access WHERE group_id = ? AND epoch = ?",
    )
    .bind(&group_id[..])
    .bind(epoch)
    .fetch_optional(&mut *conn)
    .await
    .map_err(registration_db_error("load registration"))?;
    Ok(row.and_then(|(pk, rank)| key_of(&pk).map(|k| (k, rank))))
}

/// The highest epoch registered by an admin or the root (a claim and a reset
/// register as the root): `H`, the start of the write tier
/// (`src/access.rs`) and of the member storage bound.
async fn highest_admin_epoch(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<Option<u64>, HandlerError> {
    let row: (Option<u64>,) = sqlx::query_as(
        "SELECT MAX(epoch) FROM ds_group_access WHERE group_id = ? AND registered_rank >= ?",
    )
    .bind(&group_id[..])
    .bind(Rank::Admin.to_byte())
    .fetch_one(&mut *conn)
    .await
    .map_err(registration_db_error("load admin epoch"))?;
    Ok(row.0)
}

// ============================================================================
// Writes
// ============================================================================

pub(crate) async fn insert_epoch(
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

pub(crate) async fn upsert_registrar(
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

/// One register-epoch statement, as the rules read it.
struct Registration<'a> {
    signer: &'a [u8; KEY_LEN],
    standing: Standing,
    epoch: u64,
    epoch_key: &'a [u8; KEY_LEN],
    mode: RegisterMode,
}

/// What a registration does to the chain once the rules allow it.
enum EpochWrite {
    /// The key is already there; nothing to write.
    Keep,
    /// Write `epoch`.
    Write,
    /// Drop every epoch above `epoch`, then write it.
    TruncateAndWrite,
}

/// The epoch rule of the module doc: `Ok(write)` or the refusal.
async fn epoch_rule(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    reg: &Registration<'_>,
) -> Result<Result<EpochWrite, HandlerError>, HandlerError> {
    let signer_rank = reg.standing.rank_byte();
    let epoch = reg.epoch;
    let at = registration_at(&mut *conn, group_id, epoch).await?;
    let latest = latest_epoch(&mut *conn, group_id).await?;
    let next = latest.and_then(|l| l.checked_add(1));
    match reg.mode {
        RegisterMode::Append => {
            if at.is_some_and(|(key, _)| key == *reg.epoch_key) {
                return Ok(Ok(EpochWrite::Keep));
            }
            if next != Some(epoch) {
                return Ok(Err(conflict(
                    "an append registers the next epoch only; replacing is a replace",
                )));
            }
            if reg.standing == Standing::Registrar(Rank::Member) {
                // Storage bound only (module doc): once this one is
                // written, `H..=epoch` - that is `epoch - H + 1` epochs -
                // sit at or after `H`, so the bound is `epoch - H <
                // MAX_RETAINED_EPOCHS`. No `H` at all cannot happen on a
                // claimed journal (the claim is a root registration); it is
                // refused rather than trusted.
                let within = highest_admin_epoch(&mut *conn, group_id)
                    .await?
                    .and_then(|h| epoch.checked_sub(h))
                    .is_some_and(|past| past < MAX_RETAINED_EPOCHS);
                if !within {
                    return Ok(Err(forbidden(
                        "too many member registrations past the last admin or root \
                         registration; an admin or the owner registers first",
                    )));
                }
            }
            Ok(Ok(EpochWrite::Write))
        },
        RegisterMode::Replace => {
            if reg.standing == Standing::Registrar(Rank::Member) {
                return Ok(Err(forbidden("a member does not replace a registration")));
            }
            match latest {
                Some(l) if epoch <= l => {
                    let above = highest_rank_above(&mut *conn, group_id, epoch).await?;
                    let at_ok = at.map_or(true, |(key, rank)| {
                        rank < signer_rank || key == *reg.epoch_key
                    });
                    if above >= signer_rank || !at_ok {
                        return Ok(Err(forbidden(
                            "an epoch registered by an equal or higher rank is not replaced",
                        )));
                    }
                    let unchanged = at.is_some_and(|(key, _)| key == *reg.epoch_key);
                    Ok(Ok(match (unchanged, epoch < l) {
                        (true, false) => EpochWrite::Keep,
                        (false, false) => EpochWrite::Write,
                        (_, true) => EpochWrite::TruncateAndWrite,
                    }))
                },
                _ if next == Some(epoch) => Ok(Ok(EpochWrite::Write)),
                _ => {
                    if reg.standing == Standing::Root {
                        Ok(Ok(EpochWrite::Write))
                    } else {
                        Ok(Err(forbidden("a skip past the next epoch is the root's")))
                    }
                },
            }
        },
    }
}

async fn apply_register(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    reg: &Registration<'_>,
    revoke: &[[u8; KEY_LEN]],
) -> Decision {
    let signer_rank = reg.standing.rank_byte();
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

    let write = match epoch_rule(&mut *conn, group_id, reg).await? {
        Ok(write) => write,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if !matches!(write, EpochWrite::Keep) {
        if matches!(write, EpochWrite::TruncateAndWrite) {
            sqlx::query("DELETE FROM ds_group_access WHERE group_id = ? AND epoch > ?")
                .bind(&group_id[..])
                .bind(reg.epoch)
                .execute(&mut *conn)
                .await
                .map_err(registration_db_error("drop replaced epochs"))?;
        }
        insert_epoch(
            conn,
            group_id,
            reg.epoch,
            reg.epoch_key,
            reg.signer,
            signer_rank,
        )
        .await?;
        // Bound the read tier below the newest epoch; the journal pruner
        // ages out the rest. The floor is computed here so no epoch
        // arithmetic runs in SQL. It never reaches `H` or above: the member
        // storage bound already keeps every epoch `>= H` within the newest
        // [`MAX_RETAINED_EPOCHS`], and the clamp below holds that even if
        // the bound were ever relaxed.
        let newest = latest_epoch(&mut *conn, group_id)
            .await?
            .unwrap_or(reg.epoch);
        let admin_floor = highest_admin_epoch(&mut *conn, group_id).await?;
        let floor = newest
            .checked_sub(MAX_RETAINED_EPOCHS)
            .and_then(|f| match admin_floor {
                Some(h) if f >= h => h.checked_sub(1),
                _ => Some(f),
            });
        if let Some(floor) = floor {
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

/// The root replaces the whole registrar set: every registrar not listed
/// goes, every listed one is written at its rank. Idempotent (re-sending the
/// same set is a `204` that changes nothing). The codec already refused a
/// set naming the root, a repeated key and more than [`MAX_REGISTRARS`].
async fn replace_registrars(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    registrars: &[RegistrarEntry],
) -> Decision {
    if registrars.len() > MAX_REGISTRARS {
        return Ok(Err(conflict("too many registrars on this journal")));
    }
    sqlx::query("DELETE FROM ds_group_registrars WHERE group_id = ?")
        .bind(&group_id[..])
        .execute(&mut *conn)
        .await
        .map_err(registration_db_error("replace registrar set"))?;
    for entry in registrars {
        upsert_registrar(conn, group_id, &entry.key, entry.rank).await?;
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

/// Hand the root over. With `generation` (an attested transfer, ADR-0074
/// section 4.6: a key ROTATION moving the account's DeviceGroup journal to
/// the root its new seed derives) the journal's key generation is recorded
/// too, so the attestation that rode along cannot be presented again, on
/// this route or on `reset`. The epoch chain, the registrar set and the push
/// binding are untouched: unlike an attested reset, a transfer cuts nobody
/// off.
async fn transfer_root(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    root: &[u8; KEY_LEN],
    new_root: &[u8; KEY_LEN],
    generation: Option<u64>,
) -> Decision {
    if new_root == root && generation.is_none() {
        return Ok(Ok(()));
    }
    if new_root != root {
        // The root is not a registrar; a key that was one stops being one.
        delete_registrar(conn, group_id, new_root).await?;
    }
    sqlx::query(
        "UPDATE ds_group_owner SET root_pk = ?, \
            key_generation = COALESCE(?, key_generation), \
            updated_at = CURRENT_TIMESTAMP(3) \
         WHERE group_id = ?",
    )
    .bind(&new_root[..])
    .bind(generation)
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
    use lakko_ds_proof::RegisterMode;
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

    /// Publish one message to the journal under `key` at `epoch`; the status.
    async fn publish_as(
        app: &axum::Router,
        gid: &[u8; PSEUDONYM_LEN],
        key: &SigningKey,
        epoch: u64,
    ) -> StatusCode {
        send(
            app,
            Req {
                method: "POST",
                uri: &members_uri(gid),
                body: b"ciphertext".to_vec(),
                proof: Some((key, epoch)),
            },
        )
        .await
        .0
    }

    /// Read the journal's knocks under `key` at `epoch`; the status.
    async fn read_knocks_as(
        app: &axum::Router,
        gid: &[u8; PSEUDONYM_LEN],
        key: &SigningKey,
        epoch: u64,
    ) -> StatusCode {
        send(
            app,
            Req {
                method: "GET",
                uri: &format!("/ds/group/{}/knocks?after=0", hex::encode(gid)),
                body: vec![],
                proof: Some((key, epoch)),
            },
        )
        .await
        .0
    }

    /// ADR-0057 Context, attack 1, the 2026-09-28 security pass finding 1,
    /// and the 2026-10-01 amendment (item 3) that removed the member limit.
    /// An evicted member extends the chain with keys only it holds - now as
    /// far as it likes. That no longer keeps the journal: the real epoch
    /// key, registered at or after `H`, stays in the write tier however far
    /// the fakes reach; the admin committer's replace drops the fakes and
    /// revokes the evictee in the same transaction; and once the group has
    /// moved one epoch past the removal, the epoch every member (the
    /// evictee included) held before it stops writing.
    #[tokio::test]
    async fn an_evictee_cannot_keep_the_journal() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x62).await;
        let (fake2, fake3) = (key(0xF2), key(0xF3));
        for (e, k) in [(2, &fake2), (3, &fake3)] {
            assert_eq!(
                register(&app, &j.gid, &j.member, e, k, &[]).await,
                StatusCode::NO_CONTENT,
                "a member may chain epochs past the last admin or root registration"
            );
        }
        // The fakes did not push the real key out: epoch 1 is the newest
        // root registration (`H`), so it still writes and reads knocks.
        assert_eq!(
            publish_as(&app, &j.gid, &key(1), 1).await,
            StatusCode::CREATED
        );
        assert_eq!(
            read_knocks_as(&app, &j.gid, &key(1), 1).await,
            StatusCode::OK
        );
        // The admin's ordinary append meets the fake; a replace drops it.
        let real2 = key(0x22);
        assert_eq!(
            register(&app, &j.gid, &j.admin, 2, &real2, &[&j.member]).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            replace(&app, &j.gid, &j.admin, 2, &real2, &[&j.member]).await,
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
        for (k, e) in [(&fake2, 2), (&fake3, 3)] {
            assert_eq!(read_as(&app, &j.gid, k, e).await, StatusCode::UNAUTHORIZED);
            assert_eq!(
                publish_as(&app, &j.gid, k, e).await,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(read_as(&app, &j.gid, &real2, 2).await, StatusCode::OK);
        // The group moves on: an admin registration at 3 moves `H` to 3, so
        // the pre-removal epoch 1 (< 3 - 1) leaves the write tier. It keeps
        // the read tail (ADR-0053), unchanged by the amendment.
        assert_eq!(
            register(&app, &j.gid, &j.admin, 3, &key(0x23), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            publish_as(&app, &j.gid, &key(1), 1).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            read_knocks_as(&app, &j.gid, &key(1), 1).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(read_as(&app, &j.gid, &key(1), 1).await, StatusCode::OK);
        assert_eq!(
            publish_as(&app, &j.gid, &real2, 2).await,
            StatusCode::CREATED
        );
    }

    /// The 2026-10-01 amendment (ADR-0057 item 3): two members register
    /// epochs in a row past `H` - two device admissions with no owner or
    /// admin online - and both land. A member that then registers garbage
    /// keys at `N + 1` and `N + 2` freezes no chat: the real epoch-`N` key
    /// (`N >= H`) still publishes and reads knocks, though no longer one of
    /// the two newest. A member still never replaces or skips, and an admin
    /// replace repairs the chain.
    #[tokio::test]
    async fn a_member_chain_never_pushes_the_real_key_out_of_the_write_tier() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x70).await;
        let (real2, real3) = (key(0x72), key(0x73));
        assert_eq!(
            register(&app, &j.gid, &j.member, 2, &real2, &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            register(&app, &j.gid, &j.member2, 3, &real3, &[]).await,
            StatusCode::NO_CONTENT,
            "a second member registration in a row past H is accepted"
        );
        // The malicious member appends garbage at N + 1 and N + 2 (N = 3).
        for (e, k) in [(4, 0xF4), (5, 0xF5)] {
            assert_eq!(
                register(&app, &j.gid, &j.member, e, &key(k), &[]).await,
                StatusCode::NO_CONTENT
            );
        }
        // A skip or a replace is still no member's.
        assert_eq!(
            replace(&app, &j.gid, &j.member, 5, &key(0xE5), &[]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            register(&app, &j.gid, &j.member, 7, &key(0xF7), &[]).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            epochs(&pool, &j.gid).await,
            vec![(1, 3), (2, 1), (3, 1), (4, 1), (5, 1)]
        );
        // Every epoch from H = 1 on writes, the real epoch 3 included.
        for (k, e) in [(&real3, 3), (&real2, 2), (&key(1), 1)] {
            assert_eq!(publish_as(&app, &j.gid, k, e).await, StatusCode::CREATED);
            assert_eq!(read_knocks_as(&app, &j.gid, k, e).await, StatusCode::OK);
        }
        // An admin replace at the real epoch drops the garbage above it.
        assert_eq!(
            replace(&app, &j.gid, &j.admin, 3, &real3, &[&j.member]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 1), (3, 2)]);
        // H is now 3: epoch 2 writes as one of the two newest, epoch 1 not.
        assert_eq!(
            publish_as(&app, &j.gid, &real2, 2).await,
            StatusCode::CREATED
        );
        assert_eq!(
            publish_as(&app, &j.gid, &key(1), 1).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            register(&app, &j.gid, &j.member2, 4, &key(0x74), &[]).await,
            StatusCode::NO_CONTENT
        );
    }

    /// The one bound left on member appends is storage: at most
    /// [`MAX_RETAINED_EPOCHS`] epochs at or after `H`, so the register-time
    /// bound never cuts an epoch the write tier needs. An admin or root
    /// registration lifts it, and only then does the oldest epoch (below
    /// the new `H`) go.
    #[tokio::test]
    async fn member_appends_past_h_are_bounded_by_the_retained_epochs() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x73).await;
        let last = MAX_RETAINED_EPOCHS; // H = 1, so 1..=64 is 64 epochs.
        for e in 2..=last {
            assert_eq!(
                register(&app, &j.gid, &j.member, e, &key(0x80), &[]).await,
                StatusCode::NO_CONTENT,
                "member append at {e}"
            );
        }
        assert_eq!(
            register(&app, &j.gid, &j.member, last + 1, &key(0x80), &[]).await,
            StatusCode::FORBIDDEN
        );
        let kept = epochs(&pool, &j.gid).await;
        assert_eq!(kept.len(), usize::try_from(last).unwrap());
        assert_eq!(kept.first(), Some(&(1, 3)), "H is never cut");
        assert_eq!(
            publish_as(&app, &j.gid, &key(1), 1).await,
            StatusCode::CREATED
        );
        assert_eq!(
            register(&app, &j.gid, &j.admin, last + 1, &key(0x81), &[]).await,
            StatusCode::NO_CONTENT
        );
        let kept = epochs(&pool, &j.gid).await;
        assert_eq!(kept.first().map(|(e, _)| *e), Some(2));
        assert_eq!(kept.last(), Some(&(last + 1, 2)));
        assert_eq!(
            register(&app, &j.gid, &j.member, last + 2, &key(0x80), &[]).await,
            StatusCode::NO_CONTENT
        );
    }

    /// The coordinator's amendment to the replace rule: an admin at epoch N
    /// (registered by an admin) whose append of N+1 met a member's fake N+1
    /// drops it by replacing N with the key already there; it cannot drop
    /// what another admin registered above N.
    #[tokio::test]
    async fn an_admin_drops_a_fake_next_epoch_by_replacing_its_own() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x71).await;
        let admin2 = fresh_signer(0x71);
        assert_eq!(
            set_registrars(&app, &j.gid, &j.root, &[(&admin2, Some(Rank::Admin))]).await,
            StatusCode::NO_CONTENT
        );
        let real2 = key(0x42);
        assert_eq!(
            register(&app, &j.gid, &admin2, 2, &real2, &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            register(&app, &j.gid, &j.member, 3, &key(0xF3), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            register(&app, &j.gid, &j.admin, 3, &key(0x43), &[]).await,
            StatusCode::CONFLICT
        );
        // Another key at N is an equal rank's registration: refused.
        assert_eq!(
            replace(&app, &j.gid, &j.admin, 2, &key(0x52), &[]).await,
            StatusCode::FORBIDDEN
        );
        // The same key at N: the fake above is dropped, N kept.
        assert_eq!(
            replace(&app, &j.gid, &j.admin, 2, &real2, &[&j.member]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 2)]);
        assert_eq!(read_as(&app, &j.gid, &real2, 2).await, StatusCode::OK);
        assert_eq!(
            register(&app, &j.gid, &j.admin, 3, &key(0x43), &[]).await,
            StatusCode::NO_CONTENT
        );
        // Now an admin epoch sits above 2: the other admin cannot drop it.
        assert_eq!(
            replace(&app, &j.gid, &admin2, 2, &real2, &[]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 2), (3, 2)]);
    }

    /// Security pass finding 7: a lagging root device's ordinary append of
    /// an epoch that is no longer the next one is a 409 and truncates
    /// nothing; a replace cannot drop what another root device registered.
    #[tokio::test]
    async fn a_lagging_root_does_not_truncate() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x72).await;
        for (e, k) in [(2, 0x52), (3, 0x53)] {
            assert_eq!(
                register(&app, &j.gid, &j.root, e, &key(k), &[]).await,
                StatusCode::NO_CONTENT
            );
        }
        // The lagging device still thinks the chain ends at 1.
        assert_eq!(
            register(&app, &j.gid, &j.root, 2, &key(0x62), &[]).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            replace(&app, &j.gid, &j.root, 2, &key(0x62), &[]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 3), (3, 3)]);
        // Re-sending what is there stays a no-op for either mode.
        assert_eq!(
            register(&app, &j.gid, &j.root, 2, &key(0x52), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            replace(&app, &j.gid, &j.root, 3, &key(0x53), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 3), (3, 3)]);
        // The root may still replace a lower rank's suffix.
        assert_eq!(
            register(&app, &j.gid, &j.admin, 4, &key(0x54), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            replace(&app, &j.gid, &j.root, 3, &key(0x53), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 3), (3, 3)]);
    }

    /// Security pass finding 2: the root replaces the whole registrar set; a
    /// registrar it no longer lists stops being heard at once, whatever the
    /// owner's device remembers.
    #[tokio::test]
    async fn a_registrars_set_removes_every_unlisted_registrar() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x73).await;
        let set = |signer: &StatementSigner, entries: Vec<RegistrarEntry>| {
            let app = app.clone();
            let gid = j.gid;
            let signer = signer.clone();
            async move {
                send_statement(
                    &app,
                    &gid,
                    &signer,
                    StatementBody::RegistrarsSet {
                        registrars: entries,
                    },
                )
                .await
            }
        };
        let only_admin = vec![RegistrarEntry {
            key: j.admin.public_key(),
            rank: Rank::Admin,
        }];
        // Only the root: a registrar is heard and refused, a stranger is not
        // heard.
        let only_member = vec![RegistrarEntry {
            key: j.member.public_key(),
            rank: Rank::Admin,
        }];
        assert_eq!(
            set(&j.admin, only_member.clone()).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            set(&fresh_signer(0x73), only_member).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(registrar_count(&pool, &j.gid).await, 3);
        assert_eq!(
            set(&j.root, only_admin.clone()).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(registrar_count(&pool, &j.gid).await, 1);
        for member in [&j.member, &j.member2] {
            assert_eq!(
                register(&app, &j.gid, member, 2, &key(0x82), &[]).await,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            register(&app, &j.gid, &j.admin, 2, &key(0x82), &[]).await,
            StatusCode::NO_CONTENT
        );
        // The same set again changes nothing; a new rank is written.
        assert_eq!(set(&j.root, only_admin).await, StatusCode::NO_CONTENT);
        assert_eq!(registrar_count(&pool, &j.gid).await, 1);
        assert_eq!(
            set(
                &j.root,
                vec![RegistrarEntry {
                    key: j.admin.public_key(),
                    rank: Rank::Member,
                }]
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            replace(&app, &j.gid, &j.admin, 2, &key(0x92), &[]).await,
            StatusCode::FORBIDDEN,
            "demoted to member: no replace"
        );
        // Empty removes all.
        assert_eq!(set(&j.root, vec![]).await, StatusCode::NO_CONTENT);
        assert_eq!(registrar_count(&pool, &j.gid).await, 0);
        assert_eq!(
            register(&app, &j.gid, &j.admin, 3, &key(0x83), &[]).await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// ADR-0057 Context, attack 2: a member pre-registers N+1 to block its
    /// own removal. The admin's sweep replaces it; a member never replaces,
    /// and a rank never replaces what an equal or higher one registered.
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
        // Another member: an append of a taken epoch conflicts, a replace is
        // not a member's.
        assert_eq!(
            register(&app, &j.gid, &j.member2, 2, &key(0x32), &[]).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            replace(&app, &j.gid, &j.member2, 2, &key(0x32), &[]).await,
            StatusCode::FORBIDDEN
        );
        // Higher rank: replaced.
        assert_eq!(
            replace(&app, &j.gid, &j.admin, 2, &key(0x42), &[&j.member]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 2)]);
        // The root replaces the admin's; the admin does not replace the
        // root's, at that epoch or below it.
        assert_eq!(
            replace(&app, &j.gid, &j.root, 2, &key(0x62), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            replace(&app, &j.gid, &j.admin, 2, &key(0x72), &[]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            replace(&app, &j.gid, &j.admin, 1, &key(0x73), &[]).await,
            StatusCode::FORBIDDEN,
            "a suffix containing a root registration is the root's"
        );
        assert_eq!(epochs(&pool, &j.gid).await, vec![(1, 3), (2, 3)]);
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
        // A skip past latest + 1 is never an append, and as a replace it
        // is the root's.
        for signer in [&j.admin, &j.root] {
            assert_eq!(
                register(&app, &j.gid, signer, 5, &key(5), &[]).await,
                StatusCode::CONFLICT
            );
        }
        assert_eq!(
            replace(&app, &j.gid, &j.admin, 5, &key(5), &[]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            replace(&app, &j.gid, &j.root, 5, &key(5), &[]).await,
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
            replace(&app, &j.gid, &j.root, 9, &key(9), &[]).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            set_registrars(&app, &j.gid, &new_root, &[(&j.admin, None)]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            replace(&app, &j.gid, &new_root, 9, &key(9), &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            claim(&app, &j.gid, &j.root, 1, &key(1)).await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// A root transfer naming the current root is the clients' probe "am I
    /// still the root?": 204 and nothing changes for the root, the uniform
    /// 401 for a key that is not (or no longer) the root.
    #[tokio::test]
    async fn a_transfer_to_the_current_root_is_a_probe() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let j = journal(&app, 0x74).await;
        let probe = |signer: &StatementSigner, named: &StatementSigner| {
            let app = app.clone();
            let gid = j.gid;
            let signer = signer.clone();
            let new_root = named.public_key();
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
        assert_eq!(probe(&j.root, &j.root).await, StatusCode::NO_CONTENT);
        assert_eq!(probe(&j.root, &j.root).await, StatusCode::NO_CONTENT);
        assert_eq!(probe(&j.admin, &j.admin).await, StatusCode::UNAUTHORIZED);
        assert_eq!(registrar_count(&pool, &j.gid).await, 3);
        let new_root = fresh_signer(0x74);
        assert_eq!(probe(&j.root, &new_root).await, StatusCode::NO_CONTENT);
        assert_eq!(probe(&j.root, &j.root).await, StatusCode::UNAUTHORIZED);
        assert_eq!(probe(&new_root, &new_root).await, StatusCode::NO_CONTENT);
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
                    mode: RegisterMode::Append,
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
                    mode: RegisterMode::Append,
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
                    mode: RegisterMode::Append,
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
            claim(&app, &gid, &root, MAX_ACCESS_EPOCH - 1, &key(1)).await,
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
