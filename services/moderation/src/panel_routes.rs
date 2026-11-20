// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The routes of the moderators' panel ([`crate::panel`]).
//!
//! The wire contract the panel app is built against is written out in the
//! step 5 notes (`panel-api.md`); in short: JSON in and out, base64
//! standard padded in `*_b64` fields (any base64 accepted on input), hex in
//! paths, errors `{"error": code}`, every answer `Cache-Control: no-store`.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::Extension;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_moderation::statement::{Action, SignedStatement, Statement};
use lakko_moderation::webauthn::{verify_assertion, Assertion};
use lakko_moderation::{ModeratorKey, Reason, TargetKind};
use rand::RngCore as _;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error;
use crate::internal::{b64, submit_statement, ForwardOutcome, Forwarder, Forwarders};
use crate::panel::{now_ms, Moderator, PanelState, LOCK_IDLE_MS, SESSION_TTL_MS};
use crate::panel_view::{
    case_json, conversation_kind_name, date, day_of_ms, fold_cases, hex, iso_monday, iso_week,
    kind_from_name, leaf_json, lock_json, moderator_name, reason_byte_name, reason_from_name,
    statement_json, statement_of_leaf, summary_json, unhex, CaseFilter, LoggedStatement, DAY_MS,
};
use crate::store::{
    AppealSeen, Decision, LockOutcome, ReportChecks, ReportStatus, StoreError, APPEAL_DECLINED,
    APPEAL_LIFTED, OUTCOME_NO_ACTION,
};

/// Most cases one queue page lists (default [`DEFAULT_CASES`]).
pub const MAX_CASES: usize = 500;
/// Cases listed when no limit is asked.
pub const DEFAULT_CASES: usize = 200;
/// Log entries per page.
pub const LOG_PAGE: usize = 50;
/// Longest internal note, in characters.
pub const MAX_NOTE_CHARS: usize = 2000;
/// Most messages a report's checks may count (a report holds at most 20
/// evidence items).
pub const MAX_CHECKED: u16 = 255;
/// Weeks of numbers by default, and at most.
pub const DEFAULT_WEEKS: u32 = 12;
/// Most weeks of numbers asked at once.
pub const MAX_WEEKS: u32 = 104;

/// The auth read of an account target.
const AUTH_ACCOUNT_PATH: &str = "/internal/moderation/account";
/// The discovery read of an account target.
const DISCOVERY_ACCOUNT_PATH: &str = "/internal/moderation/account";
/// auth's list of appeals.
const AUTH_APPEALS_PATH: &str = "/internal/moderation/appeals";
/// storage's read of a blob for the panel.
const STORAGE_BLOB_PATH: &str = "/internal/moderation/blob/";
/// Largest evidence blob the panel passes on: storage's own upload cap
/// (`services/storage` `MAX_BLOB_BYTES`, 50 MiB).
pub const MAX_EVIDENCE_BLOB_BYTES: usize = 50 * 1024 * 1024;
/// How long the panel waits for storage to send a blob.
const BLOB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

fn store_error(e: &StoreError) -> Response {
    tracing::error!(error = %e, "panel store failed");
    error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
}

/// A case path: its kind name and hex target, checked against the kind's
/// length.
/// The error is the code of a 400.
fn parse_target(kind: &str, target_hex: &str) -> Result<(TargetKind, Vec<u8>), &'static str> {
    let kind = kind_from_name(kind).ok_or("bad-kind")?;
    let target = unhex(target_hex)
        .filter(|t| t.len() == kind.target_len())
        .ok_or("bad-target")?;
    Ok((kind, target))
}

// --- sessions -------------------------------------------------------------

/// `POST /panel/api/session/start`.
pub async fn session_start(State(state): State<PanelState>) -> Response {
    let now = now_ms();
    let Some((challenge, expires_at_ms)) = state.sessions.new_challenge(now) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "busy");
    };
    let list = &state.list.list;
    let credentials: Vec<Value> = list
        .entries
        .iter()
        .filter(|e| e.revoked_at_ms == 0)
        .map(|e| {
            json!({
                "moderator_id_b64": B64.encode(e.moderator_id),
                "credential_id_b64": B64.encode(&e.credential_id),
            })
        })
        .collect();
    Json(json!({
        "challenge_b64": B64.encode(challenge),
        "expires_at_ms": expires_at_ms,
        "rp_id": list.rp_id,
        "origin": list.origin,
        "credentials": credentials,
    }))
    .into_response()
}

/// Body of `POST /panel/api/session/finish`.
#[derive(Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct FinishReq {
    moderator_id_b64: String,
    authenticator_data_b64: String,
    client_data_json_b64: String,
    signature_b64: String,
}

/// `POST /panel/api/session/finish`.
pub async fn session_finish(
    State(state): State<PanelState>,
    Json(req): Json<FinishReq>,
) -> Response {
    let now = now_ms();
    let Some(id) = b64(&req.moderator_id_b64).and_then(|b| <[u8; 16]>::try_from(b.as_slice()).ok())
    else {
        return error(StatusCode::BAD_REQUEST, "bad-request");
    };
    let (Some(authenticator_data), Some(client_data_json), Some(signature)) = (
        b64(&req.authenticator_data_b64),
        b64(&req.client_data_json_b64),
        b64(&req.signature_b64),
    ) else {
        return error(StatusCode::BAD_REQUEST, "bad-request");
    };
    // The challenge is read from the client data; the assertion check then
    // binds the two.
    let challenge = serde_json::from_slice::<Value>(&client_data_json)
        .ok()
        .and_then(|v| {
            v.get("challenge")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .and_then(|c| b64(&c))
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
    let Some(challenge) = challenge else {
        return error(StatusCode::BAD_REQUEST, "bad-request");
    };
    if !state.sessions.take_challenge(&challenge, now) {
        return error(StatusCode::UNAUTHORIZED, "unknown-challenge");
    }
    let list = &state.list.list;
    let Some(entry) = list.authorised_at(&id, now) else {
        tracing::warn!("panel sign-in refused: not a current moderator");
        return error(StatusCode::UNAUTHORIZED, "not-a-moderator");
    };
    let assertion = Assertion {
        authenticator_data,
        client_data_json,
        signature,
    };
    if verify_assertion(list, &entry.key, &challenge, &assertion).is_err() {
        tracing::warn!("panel sign-in refused: the assertion does not verify");
        return error(StatusCode::UNAUTHORIZED, "bad-assertion");
    }
    let expires_at_ms = now.saturating_add(SESSION_TTL_MS);
    tracing::info!("panel sign-in");
    Json(json!({
        "token": state.sessions.issue(&id, expires_at_ms),
        "expires_at_ms": expires_at_ms,
        "moderator_id_b64": B64.encode(id),
        "name": entry.name,
    }))
    .into_response()
}

/// `GET /panel/api/me`.
pub async fn me(Extension(me): Extension<Moderator>) -> Response {
    Json(json!({
        "moderator_id_b64": B64.encode(me.id),
        "name": me.name,
        "expires_at_ms": me.expires_at_ms,
    }))
    .into_response()
}

/// `GET /panel/api/moderators`.
pub async fn moderators(State(state): State<PanelState>) -> Response {
    let list = &state.list.list;
    let entries: Vec<Value> = list
        .entries
        .iter()
        .map(|e| {
            json!({
                "moderator_id_b64": B64.encode(e.moderator_id),
                "name": e.name,
                "key_kind": match e.key {
                    ModeratorKey::Ed25519(_) => "ed25519",
                    ModeratorKey::P256(_) => "p256",
                },
                "credential_id_b64": B64.encode(&e.credential_id),
                "x25519_public_b64": B64.encode(e.x25519_public),
                "added_at_ms": e.added_at_ms,
                "revoked_at_ms": (e.revoked_at_ms != 0).then_some(e.revoked_at_ms),
            })
        })
        .collect();
    Json(json!({
        "list_version": list.list_version,
        "issued_at_ms": list.issued_at_ms,
        "rp_id": list.rp_id,
        "origin": list.origin,
        "list_b64": B64.encode(&state.list.bytes),
        "moderators": entries,
    }))
    .into_response()
}

// --- the queue and a case -------------------------------------------------

/// Query of `GET /panel/api/cases`.
#[derive(Debug, Deserialize)]
pub struct CasesQuery {
    status: Option<String>,
    reason: Option<String>,
    kind: Option<String>,
    checked: Option<bool>,
    limit: Option<usize>,
}

/// `GET /panel/api/cases`.
pub async fn cases(State(state): State<PanelState>, Query(q): Query<CasesQuery>) -> Response {
    let mut filter = CaseFilter {
        checked: q.checked,
        ..CaseFilter::default()
    };
    if let Some(s) = q.status.as_deref().filter(|s| !s.is_empty()) {
        let Some(status) = crate::store::CaseStatus::from_name(s) else {
            return error(StatusCode::BAD_REQUEST, "bad-status");
        };
        filter.status = Some(status);
    }
    if let Some(r) = q.reason.as_deref().filter(|s| !s.is_empty()) {
        let Some(reason) = reason_from_name(r) else {
            return error(StatusCode::BAD_REQUEST, "bad-reason");
        };
        filter.reason = Some(reason.as_u8());
    }
    if let Some(k) = q.kind.as_deref().filter(|s| !s.is_empty()) {
        let Some(kind) = kind_from_name(k) else {
            return error(StatusCode::BAD_REQUEST, "bad-kind");
        };
        filter.kind = Some(kind.as_u8());
    }
    let limit = q.limit.unwrap_or(DEFAULT_CASES).clamp(1, MAX_CASES);
    let now = now_ms();
    if let Err(e) = state
        .store
        .expire_locks(now.saturating_sub(LOCK_IDLE_MS))
        .await
    {
        return store_error(&e);
    }
    let aggs = match state.store.case_aggregates().await {
        Ok(a) => a,
        Err(e) => return store_error(&e),
    };
    let rows = fold_cases(aggs, &filter);
    let total = rows.len();
    let list = &state.list.list;
    let cases: Vec<Value> = rows
        .iter()
        .take(limit)
        .map(|r| summary_json(list, r))
        .collect();
    Json(json!({ "cases": cases, "total": total })).into_response()
}

/// What auth and discovery say about an account target; a service that is
/// not configured, unreachable or refusing gives `null`.
async fn account_reads(forwarders: &Forwarders, target: &[u8]) -> Value {
    let body = json!({ "target_b64": B64.encode(target) });
    let (auth, discovery) = futures::join!(
        read_one(forwarders.auth.as_ref(), AUTH_ACCOUNT_PATH, &body),
        read_one(forwarders.discovery.as_ref(), DISCOVERY_ACCOUNT_PATH, &body)
    );
    json!({ "auth": auth, "discovery": discovery })
}

/// One service's read; `null` unless it answered 200.
async fn read_one(forwarder: Option<&Forwarder>, path: &str, body: &Value) -> Value {
    match forwarder {
        Some(f) => match f.call(path, Some(body)).await {
            Some((200, v)) => v,
            _ => Value::Null,
        },
        None => Value::Null,
    }
}

/// `GET /panel/api/cases/{kind}/{target_hex}`.
pub async fn case(
    State(state): State<PanelState>,
    Extension(me): Extension<Moderator>,
    Path((kind, target_hex)): Path<(String, String)>,
) -> Response {
    let (kind, target) = match parse_target(&kind, &target_hex) {
        Ok(t) => t,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    let now = now_ms();
    let store = &state.store;
    if let Err(e) = store.expire_locks(now.saturating_sub(LOCK_IDLE_MS)).await {
        return store_error(&e);
    }
    if let Err(e) = store.touch_lock(kind.as_u8(), &target, &me.id, now).await {
        return store_error(&e);
    }
    let case = match store.case(kind.as_u8(), &target).await {
        Ok(Some(c)) => c,
        Ok(None) => return error(StatusCode::NOT_FOUND, "unknown-case"),
        Err(e) => return store_error(&e),
    };
    let (reports, decisions) = match (
        store.case_reports(kind.as_u8(), &target).await,
        store.decisions(kind.as_u8(), &target).await,
    ) {
        (Ok(r), Ok(d)) => (r, d),
        (Err(e), _) | (_, Err(e)) => return store_error(&e),
    };
    let list = &state.list.list;
    let reports: Vec<Value> = reports
        .iter()
        .map(|r| {
            json!({
                "receipt_id_b64": B64.encode(r.receipt_id),
                "receipt_hex": hex(&r.receipt_id),
                "reason": reason_byte_name(r.reason),
                "reason_code": r.reason,
                "conversation_kind": conversation_kind_name(r.conversation_kind),
                "evidence_count": r.evidence_count,
                "list_version": r.list_version,
                "day": date(i64::from(r.received_day)),
                "status": r.status.name(),
                "report_b64": B64.encode(&r.report),
                "checks": r.checks.map(|c| json!({ "checked": c.checked, "unchecked": c.unchecked })),
            })
        })
        .collect();

    let history = match case_history(&state, kind, &target, &decisions).await {
        Ok(h) => h,
        Err(e) => return store_error(&e),
    };
    let account = if kind == TargetKind::Account {
        account_reads(&state.forwarders, &target).await
    } else {
        Value::Null
    };
    Json(json!({
        "case": case_json(list, &case),
        "reports": reports,
        "history": history,
        "account": account,
    }))
    .into_response()
}

/// The earlier decisions of a case, oldest first: the log's statements about
/// its target, the statements decided from it about another target (a name
/// taken back from an account case), and its no-action decisions.
async fn case_history(
    state: &PanelState,
    kind: TargetKind,
    target: &[u8],
    decisions: &[Decision],
) -> Result<Vec<Value>, StoreError> {
    let list = &state.list.list;
    // Earlier decisions: the log's statements about this target, the
    // statements decided from this case about another target (a name taken
    // back from an account case), and the no-action decisions.
    let decided_here: HashMap<[u8; 16], u8> = decisions
        .iter()
        .filter(|d| d.outcome != OUTCOME_NO_ACTION)
        .map(|d| (d.decision_id, d.reason))
        .collect();
    let leaves = state.log.leaves().await;
    let statements: Vec<LoggedStatement> = leaves
        .iter()
        .enumerate()
        .filter_map(|(i, l)| statement_of_leaf(i as u64, l))
        .filter(|s| {
            (s.statement.target.kind() == kind && s.statement.target.bytes() == target)
                || decided_here.contains_key(&s.statement.statement_id)
        })
        .collect();
    let no_actions: Vec<&Decision> = decisions
        .iter()
        .filter(|d| d.outcome == OUTCOME_NO_ACTION)
        .collect();
    let ids: Vec<[u8; 16]> = statements
        .iter()
        .map(|s| s.statement.statement_id)
        .chain(no_actions.iter().map(|d| d.decision_id))
        .collect();
    let notes = state.store.notes(&ids).await?;
    let note_json = |id: &[u8; 16]| {
        notes.get(id).map(|n| {
            json!({
                "text": n.note,
                "moderator_name": moderator_name(list, &n.moderator_id),
                "at_ms": n.at_ms,
            })
        })
    };
    let mut history: Vec<(u64, Value)> = statements
        .iter()
        .map(|s| {
            let id = s.statement.statement_id;
            let note = note_json(&id);
            (
                s.statement.issued_at_ms,
                json!({
                    "type": "statement",
                    "at_ms": s.statement.issued_at_ms,
                    "statement": statement_json(list, s),
                    "reason": decided_here.get(&id).map(|r| reason_byte_name(*r)),
                    "note": note,
                }),
            )
        })
        .collect();
    history.extend(no_actions.iter().map(|d| {
        (
            d.at_ms,
            json!({
                "type": "no-action",
                "at_ms": d.at_ms,
                "reason": reason_byte_name(d.reason),
                "moderator_id_b64": B64.encode(d.moderator_id),
                "moderator_name": moderator_name(list, &d.moderator_id),
                "note": note_json(&d.decision_id),
            }),
        )
    }));
    history.sort_by_key(|(at, _)| *at);
    Ok(history.into_iter().map(|(_, v)| v).collect())
}

/// `POST /panel/api/cases/{kind}/{target_hex}/lock`.
pub async fn lock(
    State(state): State<PanelState>,
    Extension(me): Extension<Moderator>,
    Path((kind, target_hex)): Path<(String, String)>,
) -> Response {
    let (kind, target) = match parse_target(&kind, &target_hex) {
        Ok(t) => t,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    let now = now_ms();
    let list = &state.list.list;
    match state
        .store
        .lock_case(
            kind.as_u8(),
            &target,
            &me.id,
            now,
            now.saturating_sub(LOCK_IDLE_MS),
        )
        .await
    {
        Ok(LockOutcome::Held(l)) => {
            Json(json!({ "lock": lock_json(list, Some(&l)) })).into_response()
        },
        Ok(LockOutcome::LockedBy(l)) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "locked", "lock": lock_json(list, Some(&l)) })),
        )
            .into_response(),
        Ok(LockOutcome::NoCase) => error(StatusCode::NOT_FOUND, "unknown-case"),
        Err(e) => store_error(&e),
    }
}

/// `POST /panel/api/cases/{kind}/{target_hex}/unlock`.
pub async fn unlock(
    State(state): State<PanelState>,
    Extension(me): Extension<Moderator>,
    Path((kind, target_hex)): Path<(String, String)>,
) -> Response {
    let (kind, target) = match parse_target(&kind, &target_hex) {
        Ok(t) => t,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    match state.store.unlock_case(kind.as_u8(), &target, &me.id).await {
        Ok(released) => Json(json!({ "unlocked": released })).into_response(),
        Err(e) => store_error(&e),
    }
}

/// Body of `POST /panel/api/reports/{receipt_hex}/checks`.
#[derive(Deserialize)]
pub struct ChecksReq {
    checked: u16,
    unchecked: u16,
}

/// `POST /panel/api/reports/{receipt_hex}/checks`.
pub async fn checks(
    State(state): State<PanelState>,
    Path(receipt_hex): Path<String>,
    Json(req): Json<ChecksReq>,
) -> Response {
    let Some(receipt) = unhex(&receipt_hex).and_then(|b| <[u8; 16]>::try_from(b.as_slice()).ok())
    else {
        return error(StatusCode::BAD_REQUEST, "bad-receipt");
    };
    if req.checked > MAX_CHECKED || req.unchecked > MAX_CHECKED {
        return error(StatusCode::BAD_REQUEST, "bad-checks");
    }
    let checks = ReportChecks {
        checked: req.checked,
        unchecked: req.unchecked,
    };
    match state.store.set_checks(&receipt, checks).await {
        Ok(true) => Json(json!({ "checked": checks.checked, "unchecked": checks.unchecked }))
            .into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, "unknown-report"),
        Err(e) => store_error(&e),
    }
}

// --- decisions -------------------------------------------------------------

/// A case named in a decision body.
#[derive(Deserialize)]
pub struct CaseRef {
    kind: String,
    target_hex: String,
}

/// Body of `POST /panel/api/decide`.
#[derive(Deserialize)]
pub struct DecideReq {
    signed_statement_b64: String,
    reason: u8,
    reason_salt_b64: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    case: Option<CaseRef>,
}

/// Actions that undo an earlier one: they close no case.
const fn undoes(action: Action) -> bool {
    matches!(
        action,
        Action::Lift
            | Action::ReleaseName
            | Action::ShowInSearch
            | Action::ChannelRestore
            | Action::GroupRestore
    )
}

/// Whether deciding `action` closes its case and the case's open reports.
/// An undo, a blob delete and a kept suspension (an appeal decision, not a
/// new action on the reports) close nothing.
const fn closes_case(action: Action) -> bool {
    !undoes(action) && !matches!(action, Action::BlobDelete | Action::KeepSuspension)
}

/// The case a decision about `statement` is recorded under (and, when its
/// action closes one, closes). Always the statement's own target, except a
/// TakeBackName, which may name the account case the username was taken
/// from: that account's discovery read must list the name's lookup key, or
/// the case must already hold this statement (a resend, after the name was
/// taken back). The error is the response.
async fn decision_case(
    state: &PanelState,
    statement: &Statement,
    asked: Option<&CaseRef>,
) -> Result<(TargetKind, Vec<u8>), Response> {
    let own = (statement.target.kind(), statement.target.bytes().to_vec());
    let Some(asked) = asked else {
        return Ok(own);
    };
    let (kind, target) = parse_target(&asked.kind, &asked.target_hex)
        .map_err(|c| error(StatusCode::BAD_REQUEST, c))?;
    if (kind, target.as_slice()) == (own.0, own.1.as_slice()) {
        return Ok(own);
    }
    if statement.action != Action::TakeBackName || kind != TargetKind::Account {
        return Err(error(StatusCode::BAD_REQUEST, "case-mismatch"));
    }
    let known = state
        .store
        .decisions(kind.as_u8(), &target)
        .await
        .map_err(|e| store_error(&e))?
        .iter()
        .any(|d| d.outcome != OUTCOME_NO_ACTION && d.decision_id == statement.statement_id);
    if known {
        return Ok((kind, target));
    }
    let Some(discovery) = state.forwarders.discovery.as_ref() else {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "discovery-unavailable",
        ));
    };
    let body = json!({ "target_b64": B64.encode(&target) });
    let Some((200, read)) = discovery.call(DISCOVERY_ACCOUNT_PATH, Some(&body)).await else {
        return Err(error(StatusCode::BAD_GATEWAY, "discovery-failed"));
    };
    let listed = read
        .get("name_lookup_keys_b64")
        .and_then(Value::as_array)
        .is_some_and(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .filter_map(b64)
                .any(|k| k.as_slice() == statement.target.bytes())
        });
    if listed {
        Ok((kind, target))
    } else {
        Err(error(StatusCode::BAD_REQUEST, "case-mismatch"))
    }
}

/// A note, trimmed; `None` when empty. The error is the code of a 400.
fn clean_note(note: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err("note-too-long");
    }
    Ok(Some(note.to_owned()))
}

/// Whether the applying service took the statement and changed something
/// (`applied: true`).
fn applied_now(submitted: &crate::internal::Submitted) -> bool {
    matches!(
        &submitted.forwarded,
        Some(ForwardOutcome::Applied(v)) if v.get("applied").and_then(Value::as_bool) == Some(true)
    )
}

/// `POST /panel/api/decide`.
pub async fn decide(
    State(state): State<PanelState>,
    Extension(me): Extension<Moderator>,
    Json(req): Json<DecideReq>,
) -> Response {
    let signed = b64(&req.signed_statement_b64).unwrap_or_default();
    let salt = b64(&req.reason_salt_b64).unwrap_or_default();
    let Some(statement) = SignedStatement::decode(&signed)
        .ok()
        .and_then(|s| Statement::decode(&s.statement).ok())
    else {
        return error(StatusCode::BAD_REQUEST, "bad-statement");
    };
    if statement.moderator_id != me.id {
        return error(StatusCode::FORBIDDEN, "not-your-statement");
    }
    let note = match clean_note(req.note.as_deref()) {
        Ok(n) => n,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    // Before anything is logged: a decision closes only its own case.
    let (case_kind, case_target) = match decision_case(&state, &statement, req.case.as_ref()).await
    {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let now = now_ms();
    let submitted = match submit_statement(
        &state.log,
        &state.list,
        &state.forwarders,
        &signed,
        req.reason,
        &salt,
        now,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let st = &submitted.statement;
    let store = &state.store;
    // A resend (to retry the forward) records nothing twice.
    let known = match store.decisions(case_kind.as_u8(), &case_target).await {
        Ok(d) => d
            .iter()
            .any(|d| d.outcome != OUTCOME_NO_ACTION && d.decision_id == st.statement_id),
        Err(e) => return store_error(&e),
    };
    let mut reports_closed = 0;
    if !known {
        let decision = Decision {
            target_kind: case_kind.as_u8(),
            target: case_target.clone(),
            outcome: st.action as u8,
            reason: req.reason,
            moderator_id: me.id,
            decision_id: st.statement_id,
            at_ms: now,
        };
        let close_as = closes_case(st.action).then_some(ReportStatus::Action);
        match store.decide(&decision, close_as).await {
            Ok(n) => reports_closed = n,
            Err(e) => return store_error(&e),
        }
    }
    if let Some(note) = &note {
        if let Err(e) = store.put_note(&st.statement_id, &me.id, note, now).await {
            return store_error(&e);
        }
    }
    // A lift ends the account's pending appeals as lifted; a kept
    // suspension, once auth declined the appeal, as declined.
    let appeal_outcome = match st.action {
        Action::Lift => Some(APPEAL_LIFTED),
        Action::KeepSuspension if applied_now(&submitted) => Some(APPEAL_DECLINED),
        _ => None,
    };
    if let (Some(outcome), Ok(target)) = (appeal_outcome, <[u8; 32]>::try_from(st.target.bytes())) {
        if let Err(e) = store.set_appeal_outcome(None, &target, outcome, now).await {
            return store_error(&e);
        }
    }
    tracing::info!(
        action = st.action.name(),
        new = submitted.new,
        reports_closed,
        "panel decision"
    );
    let (status, mut body) = submitted.response_parts();
    body["statement_id_b64"] = json!(B64.encode(st.statement_id));
    body["reports_closed"] = json!(reports_closed);
    (status, Json(body)).into_response()
}

/// Body of `POST .../no-action`.
#[derive(Deserialize)]
pub struct NoActionReq {
    reason: u8,
    #[serde(default)]
    note: Option<String>,
}

/// `POST /panel/api/cases/{kind}/{target_hex}/no-action`.
pub async fn no_action(
    State(state): State<PanelState>,
    Extension(me): Extension<Moderator>,
    Path((kind, target_hex)): Path<(String, String)>,
    Json(req): Json<NoActionReq>,
) -> Response {
    let (kind, target) = match parse_target(&kind, &target_hex) {
        Ok(t) => t,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    if Reason::from_u8(req.reason).is_err() {
        return error(StatusCode::BAD_REQUEST, "bad-reason");
    }
    let note = match clean_note(req.note.as_deref()) {
        Ok(n) => n,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    match state.store.case(kind.as_u8(), &target).await {
        Ok(Some(_)) => {},
        Ok(None) => return error(StatusCode::NOT_FOUND, "unknown-case"),
        Err(e) => return store_error(&e),
    }
    // No log entry, so no statement id: a random id names the decision and
    // its note.
    let mut decision_id = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut decision_id);
    let now = now_ms();
    let decision = Decision {
        target_kind: kind.as_u8(),
        target,
        outcome: OUTCOME_NO_ACTION,
        reason: req.reason,
        moderator_id: me.id,
        decision_id,
        at_ms: now,
    };
    if let Some(note) = &note {
        if let Err(e) = state.store.put_note(&decision_id, &me.id, note, now).await {
            return store_error(&e);
        }
    }
    match state
        .store
        .decide(&decision, Some(ReportStatus::NoAction))
        .await
    {
        Ok(n) => {
            tracing::info!(reports_closed = n, "panel decision: no action");
            Json(json!({ "reports_closed": n })).into_response()
        },
        Err(e) => store_error(&e),
    }
}

// --- appeals -----------------------------------------------------------------

/// `GET /panel/api/appeals`.
pub async fn appeals(
    State(state): State<PanelState>,
    Extension(me): Extension<Moderator>,
) -> Response {
    let Some(auth) = state.forwarders.auth.as_ref() else {
        return Json(json!({ "available": false, "appeals": [] })).into_response();
    };
    let Some((200, listed)) = auth.call(AUTH_APPEALS_PATH, None).await else {
        return error(StatusCode::BAD_GATEWAY, "auth-failed");
    };
    let rows = listed
        .get("appeals")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let list = &state.list.list;
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for row in rows {
        let field = |k: &str| row.get(k).and_then(Value::as_str).and_then(b64);
        let (Some(target), Some(statement_id)) = (
            field("target_b64").and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()),
            field("statement_id_b64").and_then(|b| <[u8; 16]>::try_from(b.as_slice()).ok()),
        ) else {
            continue;
        };
        let log_index = row.get("log_index").and_then(Value::as_u64);
        let appeal_at_ms = row.get("appeal_at_ms").and_then(Value::as_u64);
        // The decision appealed, from the log, only when the entry at that
        // index is that statement.
        let decision = match log_index {
            Some(i) => state
                .log
                .entry(i, None)
                .await
                .ok()
                .and_then(|e| statement_of_leaf(i, &e.leaf))
                .filter(|s| s.statement.statement_id == statement_id),
            None => None,
        };
        let case = match state.store.case(TargetKind::Account.as_u8(), &target).await {
            Ok(c) => c,
            Err(e) => return store_error(&e),
        };
        if let Some(at) = appeal_at_ms {
            seen.push(AppealSeen {
                statement_id,
                target,
                appeal_at_ms: at,
            });
        }
        out.push(json!({
            "target_b64": B64.encode(target),
            "target_hex": hex(&target),
            "sealed_b64": row.get("sealed_b64").cloned().unwrap_or(Value::Null),
            "statement_id_b64": B64.encode(statement_id),
            "log_index": log_index,
            "until_ms": row.get("until_ms").cloned().unwrap_or(Value::Null),
            "appeal_at_ms": appeal_at_ms,
            "decision": decision.as_ref().map(|s| statement_json(list, s)),
            "decided_by_you": decision.as_ref().is_some_and(|s| s.statement.moderator_id == me.id),
            "case": case.as_ref().map(|c| case_json(list, c)),
        }));
    }
    if let Err(e) = state.store.record_appeals(&seen).await {
        return store_error(&e);
    }
    Json(json!({ "available": true, "appeals": out })).into_response()
}

// --- evidence files ------------------------------------------------------------

/// `GET /panel/api/blob/{blob_hex}`: the ciphertext of a blob a report's
/// evidence names, from storage's internal listener. The browser decrypts it
/// with the key the evidence carries; this service never sees that key. At
/// most [`crate::panel::MAX_BLOB_READS`] run at once; another is `503 busy`.
pub async fn blob(State(state): State<PanelState>, Path(blob_hex): Path<String>) -> Response {
    let hex_id = blob_hex.trim().to_ascii_lowercase();
    if hex_id.len() != 64 || unhex(&hex_id).is_none() {
        return error(StatusCode::BAD_REQUEST, "bad-blob-id");
    }
    let Some(storage) = state.forwarders.storage.as_ref() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "storage-unavailable");
    };
    // At most MAX_BLOB_READS reads at once, each buffering up to 50 MiB.
    let Ok(_slot) = state.blob_slots.clone().try_acquire_owned() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "busy");
    };
    match storage
        .get_bytes(
            &format!("{STORAGE_BLOB_PATH}{hex_id}"),
            MAX_EVIDENCE_BLOB_BYTES,
            BLOB_TIMEOUT,
        )
        .await
    {
        Some((200, bytes)) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Some((404, _)) => error(StatusCode::NOT_FOUND, "blob-not-found"),
        Some((400, _)) => error(StatusCode::BAD_REQUEST, "bad-blob-id"),
        _ => error(StatusCode::BAD_GATEWAY, "storage-failed"),
    }
}

// --- the log and the numbers -------------------------------------------------

/// Query of `GET /panel/api/log`.
#[derive(Debug, Deserialize)]
pub struct LogQuery {
    target_hex: Option<String>,
    before: Option<u64>,
}

/// `GET /panel/api/log`.
pub async fn log(State(state): State<PanelState>, Query(q): Query<LogQuery>) -> Response {
    let target = match q
        .target_hex
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(t) => match unhex(t) {
            Some(b) => Some(b),
            None => return error(StatusCode::BAD_REQUEST, "bad-target"),
        },
        None => None,
    };
    let leaves = state.log.leaves().await;
    let size = leaves.len() as u64;
    let start = q.before.unwrap_or(size).min(size);
    let list = &state.list.list;
    let mut entries = Vec::new();
    let mut next_before = None;
    let mut i = start;
    while i > 0 {
        i -= 1;
        let Some(leaf) = usize::try_from(i).ok().and_then(|ix| leaves.get(ix)) else {
            break;
        };
        if let Some(t) = &target {
            let matches = statement_of_leaf(i, leaf)
                .is_some_and(|s| s.statement.target.bytes() == t.as_slice());
            if !matches {
                continue;
            }
        }
        if entries.len() == LOG_PAGE {
            next_before = Some(i + 1);
            break;
        }
        entries.push(leaf_json(list, i, leaf));
    }
    Json(json!({ "log_size": size, "entries": entries, "next_before": next_before }))
        .into_response()
}

/// Query of `GET /panel/api/numbers`.
#[derive(Debug, Deserialize)]
pub struct NumbersQuery {
    weeks: Option<u32>,
}

/// Counts of one ISO week.
#[derive(Default)]
struct Week {
    reports: BTreeMap<&'static str, u64>,
    decisions: BTreeMap<&'static str, u64>,
    sent: u64,
    declined: u64,
    lifted: u64,
}

/// `GET /panel/api/numbers`.
pub async fn numbers(State(state): State<PanelState>, Query(q): Query<NumbersQuery>) -> Response {
    let weeks = q.weeks.unwrap_or(DEFAULT_WEEKS).clamp(1, MAX_WEEKS);
    let today = day_of_ms(now_ms());
    let this_monday = iso_monday(today);
    let first_monday = this_monday - 7 * i64::from(weeks - 1);
    let since_ms = u64::try_from(first_monday.max(0)).unwrap_or(0) * DAY_MS;
    let mut by_week: BTreeMap<i64, Week> = (0..i64::from(weeks))
        .map(|w| (first_monday + 7 * w, Week::default()))
        .collect();
    let week_of = |day: i64| iso_monday(day);

    let store = &state.store;
    let days = match store
        .report_days_since(u32::try_from(first_monday.max(0)).unwrap_or(0))
        .await
    {
        Ok(d) => d,
        Err(e) => return store_error(&e),
    };
    for (day, reason, n) in days {
        if let Some(w) = by_week.get_mut(&week_of(i64::from(day))) {
            *w.reports.entry(reason_byte_name(reason)).or_default() += n;
        }
    }
    // Decisions with an action: the public log, whoever submitted them.
    for (i, leaf) in state.log.leaves().await.iter().enumerate() {
        let Some(s) = statement_of_leaf(i as u64, leaf) else {
            continue;
        };
        if let Some(w) = by_week.get_mut(&week_of(day_of_ms(s.statement.issued_at_ms))) {
            *w.decisions.entry(s.statement.action.name()).or_default() += 1;
        }
    }
    // "No action" has no log entry.
    match store.decisions_since(since_ms).await {
        Ok(ds) => {
            for d in ds.iter().filter(|d| d.outcome == OUTCOME_NO_ACTION) {
                if let Some(w) = by_week.get_mut(&week_of(day_of_ms(d.at_ms))) {
                    *w.decisions.entry("no-action").or_default() += 1;
                }
            }
        },
        Err(e) => return store_error(&e),
    }
    match store.appeals_since(since_ms).await {
        Ok(rows) => {
            for a in rows {
                if let Some(w) = by_week.get_mut(&week_of(day_of_ms(a.appeal_at_ms))) {
                    w.sent += 1;
                }
                if let Some(at) = a.outcome_at_ms {
                    if let Some(w) = by_week.get_mut(&week_of(day_of_ms(at))) {
                        match a.outcome {
                            APPEAL_DECLINED => w.declined += 1,
                            APPEAL_LIFTED => w.lifted += 1,
                            _ => {},
                        }
                    }
                }
            }
        },
        Err(e) => return store_error(&e),
    }
    let out: Vec<Value> = by_week
        .iter()
        .rev()
        .map(|(monday, w)| {
            json!({
                "week": iso_week(*monday),
                "start_day": date(*monday),
                "reports": w.reports,
                "decisions": w.decisions,
                "appeals": { "sent": w.sent, "declined": w.declined, "lifted": w.lifted },
            })
        })
        .collect();
    Json(json!({ "weeks": out })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel_view::{kind_byte_name, kind_name};
    use std::collections::HashSet;

    #[test]
    fn undoing_actions_are_exactly_the_reverses() {
        let undo: HashSet<Action> = (1..=14u8)
            .filter_map(|b| Action::from_u8(b).ok())
            .filter(|a| undoes(*a))
            .collect();
        assert_eq!(undo.len(), 5);
        assert!(undo.contains(&Action::Lift));
        assert!(!undo.contains(&Action::Suspend));
        assert!(!undo.contains(&Action::BlobDelete));
    }

    #[test]
    fn notes_are_trimmed_and_bounded() {
        assert_eq!(clean_note(None).ok(), Some(None));
        assert_eq!(clean_note(Some("  ")).ok(), Some(None));
        assert_eq!(clean_note(Some(" a ")).ok(), Some(Some("a".to_owned())));
        assert!(clean_note(Some(&"x".repeat(MAX_NOTE_CHARS + 1))).is_err());
        assert_eq!(kind_name(TargetKind::Account), kind_byte_name(1));
    }
}
