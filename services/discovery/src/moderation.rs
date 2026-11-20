// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The moderator actions discovery applies ([ADR-0099] section 4, step 5).
//!
//! A moderator signs a statement with a security key; the moderation service
//! logs it and forwards it to discovery's internal listener
//! (`LAKKO_DISCOVERY_MODERATION_BIND`, default [`DEFAULT_INTERNAL_BIND`],
//! loopback, never proxied by nginx). Every request presents
//! `x-lakko-internal-secret` = `LAKKO_DISCOVERY_INTERNAL_SECRET` (standard
//! base64 of at least [`MIN_SECRET_LEN`] bytes, compared in constant time).
//! Discovery checks the statement itself with `lakko_moderation::apply`
//! against the moderator list under `LAKKO_DISCOVERY_MODERATION_ROOT_KEY`, so
//! a compromised moderation service cannot forge an action. With the secret
//! or the root key unset the listener does not start.
//!
//! # Routes
//!
//! - `POST /internal/moderation/statement` — the body is
//!   `lakko_moderation::apply::StatementBody`. Applies:
//!   - **TakeBackName** (a username's 64-byte lookup key): the `usernames`
//!     row goes (current or retired), and with a current one the owner's
//!     public record and name terms; the name is held by a moderator claim
//!     (`name_claims.kind` [`CLAIM_MODERATION_HOLD`]) until
//!     `issued_at + 180 days` ([`HOLD_MS`]), so register, rename and a
//!     handle claim answer `409 name-taken`. A name held by a group or
//!     channel is `409 not-a-username` (a moderator uses GroupBlockHandle);
//!   - **ReleaseName**: the hold and its claim go;
//!   - **HideFromSearch** (an account target): every account whose identity
//!     bundle has the target leaves Everyone (public record and terms
//!     deleted, `is_public` cleared) and is recorded in `moderation_hidden`;
//!     `PUT /discovery/visibility` then refuses Everyone with
//!     `403 hidden-from-search`. No such account is `404 unknown-account`;
//!   - **ShowInSearch**: the record goes;
//!   - **GroupDelist** / **GroupBlockHandle** / **GroupRestore** (a handle's
//!     64-byte lookup key): `handles.moderation` is set to 1 / 2 / 0; a
//!     delist or block also clears `is_public` and deletes the public record
//!     and terms. No such handle row is `404 unknown-handle`.
//!
//!   Refusals in order: the list must verify under the root (`400
//!   bad-list`); a list older than the floor (the newest list version
//!   discovery has seen) is `409 stale-list`, and a newer one raises the
//!   floor in a transaction of its own, kept even when the statement is then
//!   refused; then the statement checks of [`lakko_moderation::apply`]
//!   (`403 not-authorised`, `400 bad-statement`, `400 stale-statement` /
//!   `400 future-statement` for a statement issued more than 24 h before or
//!   5 min after now, `400 wrong-service`, `400 reason-mismatch`); then, in
//!   one transaction, the floor again (`409 stale-list`: a newer list
//!   arrived meanwhile), a statement already applied is `200 {"applied":
//!   false, "repeat": true}`,
//!   the target refusals above, and a statement older by `issued_at_ms` than
//!   the newest one applied to the same target is recorded and answered
//!   `200 {"applied": false, "superseded": true}`. Otherwise
//!   `200 {"applied": true}`.
//! - `POST /internal/moderation/list` — the body is
//!   `lakko_moderation::apply::ListBody` (`{"moderator_list_b64"}`), pushed
//!   by the moderation service at its startup: raises the floor to the
//!   list's version. `200 {"list_version": n}`, `400 bad-list`, `409
//!   stale-list` (older than the floor).
//! - `POST /internal/moderation/account` `{"target_b64"}` — the panel's read
//!   of an account target: `{"has_username", "in_everyone",
//!   "hidden_from_search", "name_lookup_keys_b64": [...]}`, the lookup keys
//!   of the current usernames of every account with that target (standard
//!   base64), so the panel can sign a TakeBackName.
//!
//! An hourly pruner deletes holds past their end with their claims.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_moderation::apply::{
    list_floor, ApplyError, ApplyingService, CheckedStatement, FloorStep, ListBody, StatementBody,
};
use lakko_moderation::statement::Action;
use lakko_moderation::ModeratorList;
use serde::Deserialize;
use sqlx::MySqlPool;
use subtle::ConstantTimeEq as _;

use crate::registry::{CLAIM_HANDLE, CLAIM_PERSON};
use crate::visibility::{publish, TARGET_HANDLE, TARGET_PERSON};
use crate::AppState;

/// `name_claims.kind` of a name a moderator took back.
pub(crate) const CLAIM_MODERATION_HOLD: u8 = 3;

/// How long a taken-back name is held: 180 days from the statement's issue
/// time (ADR-0099 section 4). Chosen, not measured.
pub(crate) const HOLD_MS: u64 = 180 * 86_400 * 1000;

/// `handles.moderation`: nothing.
pub(crate) const HANDLE_OPEN: u8 = 0;
/// `handles.moderation`: out of search, publishing refused.
pub(crate) const HANDLE_DELISTED: u8 = 1;
/// `handles.moderation`: also every handle write refused; the row is kept.
pub(crate) const HANDLE_BLOCKED: u8 = 2;

/// Body code of a refused publish of a delisted handle.
pub(crate) const CODE_DELISTED: &str = "delisted";
/// Body code of a refused write of a blocked handle.
pub(crate) const CODE_BLOCKED: &str = "blocked";
/// Body code of a refused Everyone for an account hidden from search.
pub(crate) const CODE_HIDDEN: &str = "hidden-from-search";

/// Default bind of the internal listener: loopback, not proxied by nginx.
pub(crate) const DEFAULT_INTERNAL_BIND: &str = "127.0.0.1:8097";
/// The header the shared secret is presented in.
pub(crate) const SECRET_HEADER: &str = "x-lakko-internal-secret";
/// Minimum decoded length of the shared secret.
pub(crate) const MIN_SECRET_LEN: usize = 16;
/// Body limit of the internal routes: a signed statement, a moderator list
/// of up to 64 entries and a few short fields, in base64.
const MAX_INTERNAL_BODY_BYTES: usize = 256 * 1024;

const BIND_VAR: &str = "LAKKO_DISCOVERY_MODERATION_BIND";
const SECRET_VAR: &str = "LAKKO_DISCOVERY_INTERNAL_SECRET";
const ROOT_KEY_VAR: &str = "LAKKO_DISCOVERY_MODERATION_ROOT_KEY";

/// How often expired holds are deleted.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// How many times an apply that met a deadlock or a lock-wait timeout runs.
const LOCK_ATTEMPTS: usize = crate::handles::LOCK_ATTEMPTS;

/// The handle-write refusal for a handle under moderation, if any: a blocked
/// handle refuses every write, a delisted one refuses a write that would
/// publish it (or move it to another name, which would escape the delist).
pub(crate) fn handle_write_refusal(moderation: u8, publishes_or_moves: bool) -> Option<Response> {
    let code = match moderation {
        HANDLE_OPEN => return None,
        HANDLE_DELISTED if !publishes_or_moves => return None,
        HANDLE_DELISTED => CODE_DELISTED,
        _ => CODE_BLOCKED,
    };
    Some(coded(StatusCode::FORBIDDEN, code))
}

fn coded(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Decode base64 in any of the four common spellings.
fn decode_b64(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    STANDARD
        .decode(text)
        .or_else(|_| STANDARD_NO_PAD.decode(text))
        .or_else(|_| URL_SAFE.decode(text))
        .or_else(|_| URL_SAFE_NO_PAD.decode(text))
        .ok()
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// What the internal listener needs.
#[derive(Clone)]
pub(crate) struct InternalConfig {
    /// Where it listens.
    pub(crate) bind: SocketAddr,
    /// The shared secret the moderation service presents.
    pub(crate) secret: Arc<Vec<u8>>,
    /// The pinned moderation root public key.
    pub(crate) root_key: [u8; 32],
}

impl std::fmt::Debug for InternalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalConfig")
            .field("bind", &self.bind)
            .finish_non_exhaustive()
    }
}

/// Read the internal listener's configuration from the environment.
/// `Ok(None)` when the secret or the root key is unset.
///
/// # Errors
///
/// A message naming the variable when one is set but malformed.
pub(crate) fn load_internal() -> Result<Option<InternalConfig>, String> {
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    parse_internal(
        var(BIND_VAR).as_deref(),
        var(SECRET_VAR).as_deref(),
        var(ROOT_KEY_VAR).as_deref(),
    )
}

/// [`load_internal`] over explicit values.
///
/// # Errors
///
/// See [`load_internal`].
pub(crate) fn parse_internal(
    bind: Option<&str>,
    secret: Option<&str>,
    root_key: Option<&str>,
) -> Result<Option<InternalConfig>, String> {
    let bind: SocketAddr = bind
        .unwrap_or(DEFAULT_INTERNAL_BIND)
        .parse()
        .map_err(|_| format!("{BIND_VAR} is not host:port"))?;
    let secret = match secret {
        None => None,
        Some(raw) => {
            let bytes = STANDARD
                .decode(raw)
                .map_err(|_| format!("{SECRET_VAR} is not valid base64"))?;
            if bytes.len() < MIN_SECRET_LEN {
                return Err(format!(
                    "{SECRET_VAR} decodes to {} bytes; need >= {MIN_SECRET_LEN}",
                    bytes.len()
                ));
            }
            Some(bytes)
        },
    };
    let root_key = match root_key {
        None => None,
        Some(raw) => Some(
            URL_SAFE_NO_PAD
                .decode(raw)
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                .ok_or_else(|| format!("{ROOT_KEY_VAR} is not a base64url 32-byte key"))?,
        ),
    };
    match (secret, root_key) {
        (Some(secret), Some(root_key)) => Ok(Some(InternalConfig {
            bind,
            secret: Arc::new(secret),
            root_key,
        })),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// The router
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct InternalState {
    app: AppState,
    config: InternalConfig,
}

/// The internal router, behind the secret gate.
pub(crate) fn internal_router(app: AppState, config: InternalConfig) -> Router {
    let state = InternalState { app, config };
    Router::new()
        .route("/internal/moderation/statement", post(apply_statement))
        .route("/internal/moderation/list", post(push_list))
        .route("/internal/moderation/account", post(account_read))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_internal_secret,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_INTERNAL_BODY_BYTES,
        ))
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(lakko_log_hygiene::request_span),
        )
        .with_state(state)
}

async fn require_internal_secret(
    State(state): State<InternalState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(presented) = request
        .headers()
        .get(SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        tracing::warn!("internal request refused: missing secret header");
        return Err(StatusCode::UNAUTHORIZED);
    };
    let Ok(presented) = STANDARD.decode(presented.trim().as_bytes()) else {
        tracing::warn!("internal request refused: secret header not base64");
        return Err(StatusCode::UNAUTHORIZED);
    };
    // Constant time over the bytes; a length mismatch returns early, and the
    // length of a secret is not the secret.
    let matches: bool = state
        .config
        .secret
        .as_slice()
        .ct_eq(presented.as_slice())
        .into();
    if !matches {
        tracing::warn!("internal request refused: secret mismatch");
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

// ---------------------------------------------------------------------------
// The statement
// ---------------------------------------------------------------------------

async fn apply_statement(
    State(state): State<InternalState>,
    Json(req): Json<StatementBody>,
) -> Response {
    match apply(&state.app.db, &state.config.root_key, &req, now_ms()).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome.json())).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// What applying a statement did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Applied {
    /// It changed what it names.
    Changed,
    /// Its id was applied before; nothing changed.
    Repeat,
    /// A newer statement was already applied to its target; its id is
    /// recorded and nothing else changed.
    Superseded,
}

impl Applied {
    fn json(self) -> serde_json::Value {
        match self {
            Self::Changed => serde_json::json!({ "applied": true }),
            Self::Repeat => serde_json::json!({ "applied": false, "repeat": true }),
            Self::Superseded => serde_json::json!({ "applied": false, "superseded": true }),
        }
    }
}

/// Why a statement was refused.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// A status and a body code.
    Coded(StatusCode, &'static str),
    /// A store error.
    Db(sqlx::Error),
}

impl From<ApplyError> for Refusal {
    fn from(e: ApplyError) -> Self {
        Self::Coded(
            StatusCode::from_u16(e.status()).unwrap_or(StatusCode::BAD_REQUEST),
            e.code(),
        )
    }
}

impl From<sqlx::Error> for Refusal {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::Coded(status, code) => coded(status, code),
            Self::Db(e) => {
                tracing::error!(error = %e, "moderation statement: db error");
                coded(StatusCode::INTERNAL_SERVER_ERROR, "db-error")
            },
        }
    }
}

type Tx<'c> = sqlx::Transaction<'c, sqlx::MySql>;

/// Check `req` and apply it: the checks of [`lakko_moderation::apply`], then
/// one READ COMMITTED transaction, run again when it meets a deadlock or a
/// lock-wait timeout (at most [`LOCK_ATTEMPTS`] times in all).
///
/// # Errors
///
/// [`Refusal`].
pub(crate) async fn apply(
    pool: &MySqlPool,
    root_key: &[u8; 32],
    req: &StatementBody,
    now: i64,
) -> Result<Applied, Refusal> {
    // The list first, and the floor raised by it before the statement is
    // looked at: a newer list stays the floor even when the statement beside
    // it is refused.
    let list = req.verified_list(root_key)?;
    raise_floor(pool, &list).await?;
    let checked = req.check_against(&list, ApplyingService::Discovery.actions(), now)?;
    let mut tries = 1;
    let result = loop {
        match apply_once(pool, &checked, req.log_index, now).await {
            Err(Refusal::Db(e))
                if tries < LOCK_ATTEMPTS && crate::identity_bundle::is_lock_conflict(&e) =>
            {
                tries += 1;
            },
            other => break other,
        }
    };
    if let Ok(outcome) = &result {
        tracing::info!(
            action = checked.statement.action.name(),
            changed = *outcome == Applied::Changed,
            "moderation statement applied",
        );
    }
    result
}

/// Compare `list` with the stored floor and raise the floor to it when it is
/// newer, in a transaction of its own that commits whatever happens next.
/// `409 stale-list` for a list older than the floor.
async fn raise_floor(pool: &MySqlPool, list: &ModeratorList) -> Result<(), Refusal> {
    let mut tx = pool.begin().await?;
    let stored: Option<u64> = sqlx::query_scalar(
        "SELECT list_version FROM moderation_list_state WHERE id = 1 FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;
    if list_floor(stored, list.list_version)? == FloorStep::Raise {
        sqlx::query(
            "INSERT INTO moderation_list_state (id, list_version) VALUES (1, ?) \
             ON DUPLICATE KEY UPDATE list_version = GREATEST(list_version, VALUES(list_version))",
        )
        .bind(list.list_version)
        .execute(&mut *tx)
        .await?;
        tracing::info!(
            list_version = list.list_version,
            "moderator list floor raised"
        );
    }
    tx.commit().await?;
    Ok(())
}

/// `POST /internal/moderation/list` — raise the floor without a statement.
async fn push_list(State(state): State<InternalState>, Json(req): Json<ListBody>) -> Response {
    match push(&state.app.db, &state.config.root_key, &req).await {
        Ok(version) => Json(serde_json::json!({ "list_version": version })).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// The work of [`push_list`]: the pushed list's version, now at or below
/// the floor.
///
/// # Errors
///
/// [`Refusal`].
pub(crate) async fn push(
    pool: &MySqlPool,
    root_key: &[u8; 32],
    req: &ListBody,
) -> Result<u64, Refusal> {
    let list = req.verify(root_key)?;
    raise_floor(pool, &list).await?;
    Ok(list.list_version)
}

/// What a statement acts on, resolved under its locks before the order is
/// judged: a refusal here records nothing.
enum Resolved {
    /// A username lookup key (TakeBackName, ReleaseName).
    Name,
    /// The accounts with an account target (HideFromSearch, ShowInSearch).
    Accounts(Vec<Vec<u8>>),
    /// A handle row's id (the group actions).
    Handle(Vec<u8>),
}

async fn apply_once(
    pool: &MySqlPool,
    checked: &CheckedStatement,
    log_index: u64,
    now: i64,
) -> Result<Applied, Refusal> {
    let statement = &checked.statement;
    let mut conn = pool.acquire().await?;
    // READ COMMITTED, as the routes that write the same tables: no gap locks
    // on the search-term and username indexes.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *conn)
        .await?;
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;

    // The stored list version, under its row lock: one statement applies at
    // a time.
    let stored: Option<u64> = sqlx::query_scalar(
        "SELECT list_version FROM moderation_list_state WHERE id = 1 FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;
    list_floor(stored, checked.list_version)?;
    let repeat: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM moderation_applied_statements WHERE statement_id = ? FOR UPDATE",
    )
    .bind(&statement.statement_id[..])
    .fetch_optional(&mut *tx)
    .await?;
    if repeat.is_some() {
        return Ok(Applied::Repeat);
    }

    let target = statement.target.bytes();
    let resolved = resolve(&mut tx, statement.action, target).await?;
    let outcome = if take_decision(
        &mut tx,
        statement.target.kind().as_u8(),
        target,
        statement.issued_at_ms,
    )
    .await?
    {
        act(&mut tx, checked, target, resolved, log_index, now).await?;
        Applied::Changed
    } else {
        Applied::Superseded
    };

    sqlx::query(
        "INSERT INTO moderation_applied_statements (statement_id, applied_at_ms) VALUES (?, ?)",
    )
    .bind(&statement.statement_id[..])
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO moderation_list_state (id, list_version) VALUES (1, ?) \
         ON DUPLICATE KEY UPDATE list_version = GREATEST(list_version, VALUES(list_version))",
    )
    .bind(checked.list_version)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(outcome)
}

/// Lock and find what the statement names; the refusals that record nothing.
async fn resolve(tx: &mut Tx<'_>, action: Action, target: &[u8]) -> Result<Resolved, Refusal> {
    match action {
        Action::TakeBackName | Action::ReleaseName => {
            let kind: Option<u8> =
                sqlx::query_scalar("SELECT kind FROM name_claims WHERE lookup_key = ? FOR UPDATE")
                    .bind(target)
                    .fetch_optional(&mut **tx)
                    .await?;
            if action == Action::TakeBackName && kind == Some(CLAIM_HANDLE) {
                return Err(Refusal::Coded(StatusCode::CONFLICT, "not-a-username"));
            }
            Ok(Resolved::Name)
        },
        Action::HideFromSearch | Action::ShowInSearch => {
            let ids: Vec<Vec<u8>> = sqlx::query_scalar(
                "SELECT account_id FROM identity_bundles WHERE account_target = ? FOR UPDATE",
            )
            .bind(target)
            .fetch_all(&mut **tx)
            .await?;
            if ids.is_empty() {
                return Err(Refusal::Coded(StatusCode::NOT_FOUND, "unknown-account"));
            }
            Ok(Resolved::Accounts(ids))
        },
        Action::GroupDelist | Action::GroupBlockHandle | Action::GroupRestore => {
            let handle_id: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT handle_id FROM handles WHERE lookup_key = ? FOR UPDATE")
                    .bind(target)
                    .fetch_optional(&mut **tx)
                    .await?;
            handle_id
                .map(Resolved::Handle)
                .ok_or(Refusal::Coded(StatusCode::NOT_FOUND, "unknown-handle"))
        },
        // `ApplyingService::Discovery.actions()` admitted none of the others.
        _ => Err(Refusal::Coded(StatusCode::BAD_REQUEST, "wrong-service")),
    }
}

/// Record `issued_at_ms` as the newest decision applied to the target,
/// unless a newer one already was; `false` when it was.
async fn take_decision(
    tx: &mut Tx<'_>,
    kind: u8,
    target: &[u8],
    issued_at_ms: u64,
) -> Result<bool, Refusal> {
    let newest: Option<u64> = sqlx::query_scalar(
        "SELECT issued_at_ms FROM moderation_target_decisions \
         WHERE target_kind = ? AND target = ? FOR UPDATE",
    )
    .bind(kind)
    .bind(target)
    .fetch_optional(&mut **tx)
    .await?;
    if newest.is_some_and(|n| issued_at_ms < n) {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO moderation_target_decisions (target_kind, target, issued_at_ms) \
         VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE issued_at_ms = VALUES(issued_at_ms)",
    )
    .bind(kind)
    .bind(target)
    .bind(issued_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

/// The effect of a statement that won its target's order.
async fn act(
    tx: &mut Tx<'_>,
    checked: &CheckedStatement,
    target: &[u8],
    resolved: Resolved,
    log_index: u64,
    now: i64,
) -> Result<(), Refusal> {
    let statement = &checked.statement;
    match (statement.action, resolved) {
        (Action::TakeBackName, Resolved::Name) => {
            take_back_name(tx, checked, target, log_index, now).await
        },
        (Action::ReleaseName, Resolved::Name) => {
            sqlx::query("DELETE FROM name_claims WHERE lookup_key = ? AND kind = ?")
                .bind(target)
                .bind(CLAIM_MODERATION_HOLD)
                .execute(&mut **tx)
                .await?;
            sqlx::query("DELETE FROM moderation_name_holds WHERE lookup_key = ?")
                .bind(target)
                .execute(&mut **tx)
                .await?;
            Ok(())
        },
        (Action::HideFromSearch, Resolved::Accounts(ids)) => {
            for id in &ids {
                sqlx::query(
                    "UPDATE usernames SET is_public = FALSE \
                     WHERE account_id = ? AND retired_at IS NULL",
                )
                .bind(id.as_slice())
                .execute(&mut **tx)
                .await?;
                publish(tx, TARGET_PERSON, id, None, &[]).await?;
                sqlx::query(
                    "INSERT INTO moderation_hidden (account_id, log_index, hidden_at_ms) \
                     VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE \
                     log_index = VALUES(log_index), hidden_at_ms = VALUES(hidden_at_ms)",
                )
                .bind(id.as_slice())
                .bind(log_index)
                .bind(now)
                .execute(&mut **tx)
                .await?;
            }
            Ok(())
        },
        (Action::ShowInSearch, Resolved::Accounts(ids)) => {
            for id in &ids {
                sqlx::query("DELETE FROM moderation_hidden WHERE account_id = ?")
                    .bind(id.as_slice())
                    .execute(&mut **tx)
                    .await?;
            }
            Ok(())
        },
        (action, Resolved::Handle(handle_id)) => {
            let state = match action {
                Action::GroupDelist => HANDLE_DELISTED,
                Action::GroupBlockHandle => HANDLE_BLOCKED,
                _ => HANDLE_OPEN,
            };
            sqlx::query(
                "UPDATE handles SET moderation = ?, \
                 is_public = IF(? > 0, FALSE, is_public) WHERE lookup_key = ?",
            )
            .bind(state)
            .bind(state)
            .bind(target)
            .execute(&mut **tx)
            .await?;
            if state != HANDLE_OPEN {
                publish(tx, TARGET_HANDLE, &handle_id, None, &[]).await?;
            }
            Ok(())
        },
        _ => Err(Refusal::Coded(StatusCode::BAD_REQUEST, "wrong-service")),
    }
}

/// TakeBackName: the username row goes (current or retired) and, with a
/// current one, the owner's public record and terms; the claim becomes a
/// moderator hold until `issued_at + HOLD_MS`.
async fn take_back_name(
    tx: &mut Tx<'_>,
    checked: &CheckedStatement,
    target: &[u8],
    log_index: u64,
    now: i64,
) -> Result<(), Refusal> {
    let statement = &checked.statement;
    let row: Option<(Vec<u8>, i64)> = sqlx::query_as(
        "SELECT account_id, CAST(retired_at IS NULL AS SIGNED) FROM usernames \
         WHERE oprf_hash = ? FOR UPDATE",
    )
    .bind(target)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((owner, current)) = row {
        sqlx::query("DELETE FROM usernames WHERE oprf_hash = ?")
            .bind(target)
            .execute(&mut **tx)
            .await?;
        if current != 0 {
            publish(tx, TARGET_PERSON, &owner, None, &[]).await?;
        }
    }
    // The claim stays, now the moderator's: every path that takes a name
    // inserts into `name_claims` and meets it.
    sqlx::query(
        "INSERT INTO name_claims (lookup_key, kind) VALUES (?, ?) \
         ON DUPLICATE KEY UPDATE kind = IF(kind = ?, VALUES(kind), kind)",
    )
    .bind(target)
    .bind(CLAIM_MODERATION_HOLD)
    .bind(CLAIM_PERSON)
    .execute(&mut **tx)
    .await?;
    let until = statement.issued_at_ms.saturating_add(HOLD_MS);
    sqlx::query(
        "INSERT INTO moderation_name_holds \
            (lookup_key, until_ms, statement_id, log_index, held_at_ms) \
         VALUES (?, ?, ?, ?, ?) ON DUPLICATE KEY UPDATE until_ms = VALUES(until_ms), \
            statement_id = VALUES(statement_id), log_index = VALUES(log_index), \
            held_at_ms = VALUES(held_at_ms)",
    )
    .bind(target)
    .bind(until)
    .bind(&statement.statement_id[..])
    .bind(log_index)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The panel read
// ---------------------------------------------------------------------------

/// Body of `POST /internal/moderation/account`.
#[derive(Deserialize)]
pub(crate) struct AccountReadReq {
    /// Base64 of the 32-byte account target.
    pub(crate) target_b64: String,
}

async fn account_read(
    State(state): State<InternalState>,
    Json(req): Json<AccountReadReq>,
) -> Response {
    let Some(target) =
        decode_b64(&req.target_b64).and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
    else {
        return coded(StatusCode::BAD_REQUEST, "bad-target");
    };
    match read_account(&state.app.db, &target).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => Refusal::Db(e).into_response(),
    }
}

/// The read of `POST /internal/moderation/account`: `{"has_username",
/// "in_everyone", "hidden_from_search", "name_lookup_keys_b64"}` over every
/// account whose identity bundle has `target`.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn read_account(
    pool: &MySqlPool,
    target: &[u8; 32],
) -> Result<serde_json::Value, sqlx::Error> {
    let names: Vec<(Vec<u8>, i64)> = sqlx::query_as(
        "SELECT u.oprf_hash, CAST(u.is_public AND EXISTS( \
             SELECT 1 FROM public_records p WHERE p.kind = 1 AND p.target = u.account_id) \
             AS SIGNED) \
         FROM identity_bundles b \
         JOIN usernames u ON u.account_id = b.account_id AND u.retired_at IS NULL \
         WHERE b.account_target = ? ORDER BY u.oprf_hash",
    )
    .bind(&target[..])
    .fetch_all(pool)
    .await?;
    let hidden: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM moderation_hidden h \
         JOIN identity_bundles b ON b.account_id = h.account_id \
         WHERE b.account_target = ? LIMIT 1",
    )
    .bind(&target[..])
    .fetch_optional(pool)
    .await?;
    let keys: Vec<String> = names.iter().map(|(k, _)| STANDARD.encode(k)).collect();
    Ok(serde_json::json!({
        "has_username": !names.is_empty(),
        "in_everyone": names.iter().any(|(_, public)| *public != 0),
        "hidden_from_search": hidden.is_some(),
        "name_lookup_keys_b64": keys,
    }))
}

// ---------------------------------------------------------------------------
// Holds past their end
// ---------------------------------------------------------------------------

/// Delete the holds that ended by `now` and their moderator claims; returns
/// how many.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn prune_holds(pool: &MySqlPool, now: i64) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // The claims first, in the routes' lock order.
    sqlx::query(
        "DELETE FROM name_claims WHERE kind = ? AND lookup_key IN \
           (SELECT lookup_key FROM moderation_name_holds \
            WHERE until_ms IS NOT NULL AND until_ms <= ?)",
    )
    .bind(CLAIM_MODERATION_HOLD)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let rows = sqlx::query(
        "DELETE FROM moderation_name_holds WHERE until_ms IS NOT NULL AND until_ms <= ?",
    )
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(rows)
}

/// Hourly pruner of [`prune_holds`].
pub(crate) fn spawn_hold_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            match prune_holds(&pool, now_ms()).await {
                Ok(0) => {},
                Ok(rows) => tracing::info!(rows, "moderator name holds ended"),
                Err(e) => tracing::warn!(error = %e, "moderator name hold prune failed"),
            }
        }
    })
}

/// Whether `account_id` was hidden from search by a moderator.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn is_hidden<'e, E>(executor: E, account_id: &[u8]) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::MySql>,
{
    let row: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM moderation_hidden WHERE account_id = ?")
            .bind(account_id)
            .fetch_optional(executor)
            .await?;
    Ok(row.is_some())
}

#[cfg(test)]
mod tests;
