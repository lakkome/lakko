// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Account suspension ([ADR-0099] section 4, UX section 4).
//!
//! A moderator suspends an account by signing a statement with a security
//! key; the moderation service logs it and forwards it here, on an internal
//! listener nginx does not proxy. Auth checks the statement itself with
//! `lakko_moderation::apply` (the moderator list under the pinned moderation
//! root, the WebAuthn assertion, the action, the reason commitment), finds
//! the accounts by the statement's target (`accounts.identity_target`, the
//! account-target hash of the recorded identity key, step 5) and keeps one
//! row per suspended account in `account_suspensions`.
//!
//! The same listener serves the moderators' panel two reads (step 5): an
//! account by its target, and the appeals waiting, with a decline.
//!
//! What a suspension does here:
//!
//! - every landing route (password and recovery login, password reset,
//!   passkey sign-in, TOTP recovery, device redeem, the recovery-email
//!   password finish) refuses after it authenticated the person and before
//!   it issues a pair or changes anything, with `403` and a
//!   [`SuspendedBody`]: the `s1` restricted session and an ordinary `r2`
//!   refresh credential, so the client lands in suspended mode;
//! - `POST /auth/session/refresh` answers the same `403` with fresh
//!   credentials while the account stays suspended, and a normal pair once
//!   it is not;
//! - every bearer route refuses with `403 {"error":"account-suspended"}`
//!   ([`crate::webauthn::require_session`]);
//! - the restricted session opens exactly the routes behind
//!   [`crate::webauthn::require_session_or_restricted`]: read the
//!   suspension ([`status`]), send one appeal ([`appeal`]), delete the
//!   account, sign out, and the passkey action ceremony for the delete;
//! - the anonymous recall routes refuse after their anchor statement check
//!   with the plain `403`.
//!
//! An expired row (`until_ms` in the past) is not a suspension; the hourly
//! sweep deletes it.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_moderation::apply::{
    list_floor, ApplyError, ApplyingService, CheckedStatement, FloorStep, ListBody, StatementBody,
};
use lakko_moderation::moderators::ModeratorList;
use lakko_moderation::statement::Action;
use lakko_moderation::Reason;
use serde::{Deserialize, Serialize};
use sqlx::{MySql, MySqlPool};
use subtle::ConstantTimeEq as _;

use crate::session::{now_ms, DeviceId, FreshGrant};
use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// The body code of every suspension refusal.
pub(crate) const CODE_ACCOUNT_SUSPENDED: &str = "account-suspended";

/// The longest sealed appeal accepted (16 KiB; the largest appeal to the
/// largest moderator list is below it, `lakko_moderation::appeal` tests).
pub(crate) const MAX_APPEAL_BYTES: usize = 16 * 1024;

/// Body limit of the appeal route: the base64 of [`MAX_APPEAL_BYTES`] and a
/// little JSON.
pub(crate) const MAX_APPEAL_BODY_BYTES: usize = 32 * 1024;

/// `account_suspensions.appeal_state`: no appeal.
const APPEAL_NONE: u8 = 0;
/// `account_suspensions.appeal_state`: an appeal was sent.
const APPEAL_SENT: u8 = 1;
/// `account_suspensions.appeal_state`: the moderators declined it.
const APPEAL_DECLINED: u8 = 2;

/// How often the sweep of expired suspensions runs.
const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

// ---------------------------------------------------------------------------
// The refusal
// ---------------------------------------------------------------------------

/// The `403 account-suspended` body of a route that authenticated the person:
/// what the client keeps to run in suspended mode.
#[derive(Clone, Serialize)]
pub(crate) struct SuspendedBody {
    /// URL-safe base64 of the 16-byte account id (a device that named the
    /// account by handle learns it here, as from a landing).
    pub(crate) account_id_b64: String,
    /// The `s1` restricted session: the bearer of the routes a suspended
    /// account may call. Lives as long as an access token.
    pub(crate) restricted_token: String,
    /// Absolute unix-millisecond expiry of `restricted_token`.
    pub(crate) restricted_expires_at_ms: i64,
    /// An ordinary `r2` refresh credential; the refresh route keeps answering
    /// this `403` while the account is suspended and a pair once it is not.
    pub(crate) refresh_token: String,
    /// Absolute unix-millisecond expiry of `refresh_token`.
    pub(crate) refresh_expires_at_ms: i64,
    /// The key generation both were issued under.
    pub(crate) key_generation: u64,
    /// On the password and passkey sign-ins only: the fresh-auth grant those
    /// two mint, so a suspended account without a passkey can still delete
    /// itself (`DELETE /account` needs one). Every other route that takes a
    /// grant is closed to a suspended account.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub(crate) grant: Option<FreshGrant>,
}

impl std::fmt::Debug for SuspendedBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the credentials: an `ApiError` may be debug-logged.
        f.debug_struct("SuspendedBody")
            .field("key_generation", &self.key_generation)
            .field("grant", &self.grant.is_some())
            .finish_non_exhaustive()
    }
}

/// The response of [`ApiError::AccountSuspended`].
pub(crate) fn suspended_response(body: Option<SuspendedBody>) -> Response {
    let value = match body {
        None => serde_json::json!({ "error": CODE_ACCOUNT_SUSPENDED }),
        Some(body) => {
            let mut value = serde_json::to_value(&body).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(map) = value.as_object_mut() {
                map.insert(
                    "error".into(),
                    serde_json::Value::String(CODE_ACCOUNT_SUSPENDED.into()),
                );
            }
            value
        },
    };
    (StatusCode::FORBIDDEN, Json(value)).into_response()
}

/// The account's key generation and whether it is suspended at `now`, in
/// one query. `None` when the account does not exist.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn generation_and_suspension<'e, E>(
    executor: E,
    account_id: &[u8; 16],
    now: i64,
) -> Result<Option<(u64, bool)>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let row: Option<(u64, i64)> = sqlx::query_as(
        "SELECT a.identity_version, CAST(EXISTS( \
             SELECT 1 FROM account_suspensions s \
             WHERE s.account_id = a.id AND (s.until_ms IS NULL OR s.until_ms > ?)) AS SIGNED) \
         FROM accounts a WHERE a.id = ?",
    )
    .bind(now)
    .bind(&account_id[..])
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|(generation, suspended)| (generation, suspended != 0)))
}

/// Whether the account is suspended at `now`.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn is_suspended<'e, E>(
    executor: E,
    account_id: &[u8; 16],
    now: i64,
) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM account_suspensions \
         WHERE account_id = ? AND (until_ms IS NULL OR until_ms > ?)",
    )
    .bind(&account_id[..])
    .bind(now)
    .fetch_optional(executor)
    .await?;
    Ok(row.is_some())
}

/// Issue the credentials of a [`SuspendedBody`] for `account_id` at
/// `key_generation`, naming `device_id`, stamped now.
///
/// # Errors
///
/// [`ApiError::Internal`] when the token crate refuses.
pub(crate) fn suspended_body(
    state: &AppState,
    account_id: &[u8; 16],
    key_generation: u64,
    device_id: &DeviceId,
    grant: Option<FreshGrant>,
) -> Result<SuspendedBody, ApiError> {
    let now = now_ms();
    let restricted_expires_at_ms = now.saturating_add(state.session_token_ttl_ms);
    let refresh_expires_at_ms = now.saturating_add(state.refresh_token_ttl_ms);
    let secret = &state.session_token_secret;
    let restricted_token = secret
        .issue_restricted(
            account_id,
            key_generation,
            device_id,
            now,
            restricted_expires_at_ms,
        )
        .map_err(|e| {
            tracing::error!(error = %e, "restricted-session issuance failed");
            ApiError::Internal
        })?;
    let refresh_token = secret
        .issue_refresh(
            account_id,
            key_generation,
            device_id,
            now,
            refresh_expires_at_ms,
        )
        .map_err(|e| {
            tracing::error!(error = %e, "refresh-credential issuance failed");
            ApiError::Internal
        })?;
    Ok(SuspendedBody {
        account_id_b64: URL_SAFE_NO_PAD.encode(account_id),
        restricted_token,
        restricted_expires_at_ms,
        refresh_token,
        refresh_expires_at_ms,
        key_generation,
        grant,
    })
}

/// Whether a landing route's refusal carries a fresh-auth grant.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LandingGrant {
    /// The route mints none (recovery login, password reset, TOTP recovery,
    /// device redeem, recovery-email password, refresh).
    None,
    /// The password or passkey sign-in: the route mints one on success, and
    /// the refusal carries one too, for `DELETE /account`.
    Fresh,
}

/// The landing-route gate: `Ok(())` when the account is not suspended, else
/// [`ApiError::AccountSuspended`] carrying a [`SuspendedBody`] at
/// `key_generation` naming `device_id`. Called after the route
/// authenticated the person and before it issues a pair or changes anything
/// that is not part of authenticating (consuming a single-use proof, a
/// replay counter, a passkey's signature counter).
///
/// # Errors
///
/// [`ApiError::AccountSuspended`], [`ApiError::Db`], [`ApiError::Internal`].
pub(crate) async fn refuse_landing_if_suspended(
    state: &AppState,
    account_id: &[u8; 16],
    key_generation: u64,
    device_id: &DeviceId,
    grant: LandingGrant,
) -> Result<(), ApiError> {
    if !is_suspended(&state.db, account_id, now_ms())
        .await
        .map_err(ApiError::Db)?
    {
        return Ok(());
    }
    tracing::info!("landing refused: the account is suspended");
    let grant = match grant {
        LandingGrant::None => None,
        LandingGrant::Fresh => Some(crate::session::issue_fresh_grant(
            state,
            account_id,
            key_generation,
        )?),
    };
    Err(ApiError::AccountSuspended(Some(Box::new(suspended_body(
        state,
        account_id,
        key_generation,
        device_id,
        grant,
    )?))))
}

/// The gate of the anonymous recall routes: the plain `403`, no
/// credentials (a recalling device holds keys the account no longer has).
///
/// # Errors
///
/// [`ApiError::AccountSuspended`], [`ApiError::Db`].
pub(crate) async fn refuse_recall_if_suspended(
    pool: &MySqlPool,
    account_id: &[u8; 16],
) -> Result<(), ApiError> {
    if is_suspended(pool, account_id, now_ms())
        .await
        .map_err(ApiError::Db)?
    {
        tracing::info!("recall refused: the account is suspended");
        return Err(ApiError::AccountSuspended(None));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /account/suspension, POST /account/suspension/appeal
// ---------------------------------------------------------------------------

/// `(until_ms, reason, appeal_state)` of a live suspension row.
type SuspensionRow = (Option<i64>, u8, u8);

async fn live_row<'e, E>(
    executor: E,
    account_id: &[u8; 16],
    now: i64,
    for_update: bool,
) -> Result<Option<SuspensionRow>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let sql = if for_update {
        "SELECT until_ms, reason, appeal_state FROM account_suspensions \
         WHERE account_id = ? AND (until_ms IS NULL OR until_ms > ?) FOR UPDATE"
    } else {
        "SELECT until_ms, reason, appeal_state FROM account_suspensions \
         WHERE account_id = ? AND (until_ms IS NULL OR until_ms > ?)"
    };
    sqlx::query_as(sql)
        .bind(&account_id[..])
        .bind(now)
        .fetch_optional(executor)
        .await
}

const fn appeal_name(state: u8) -> &'static str {
    match state {
        APPEAL_SENT => "sent",
        APPEAL_DECLINED => "declined",
        _ => "none",
    }
}

/// `GET /account/suspension` — the access token or the restricted session.
/// `{"suspended": false}`, or `{"suspended": true, "reason": <u8>,
/// "reason_name": "<kebab>", "until_ms": n | null, "appeal":
/// "none" | "sent" | "declined"}`.
///
/// # Errors
///
/// [`ApiError::Db`].
pub(crate) async fn status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = live_row(&state.db, &auth.0, now_ms(), false)
        .await
        .map_err(ApiError::Db)?;
    let Some((until_ms, reason, appeal_state)) = row else {
        return Ok(Json(serde_json::json!({ "suspended": false })));
    };
    let reason_name = Reason::from_u8(reason).map_or("unknown", Reason::name);
    Ok(Json(serde_json::json!({
        "suspended": true,
        "reason": reason,
        "reason_name": reason_name,
        "until_ms": until_ms,
        "appeal": appeal_name(appeal_state),
    })))
}

/// Body of `POST /account/suspension/appeal`.
#[derive(Deserialize)]
pub(crate) struct AppealReq {
    /// Base64 (URL-safe or standard, padded or not) of the appeal sealed to
    /// the moderator list (`sealModerationAppeal`).
    pub(crate) sealed_b64: String,
}

/// `POST /account/suspension/appeal` — the access token or the restricted
/// session. Stores the sealed appeal once: `400` for a body that is not a
/// sealed part of at most [`MAX_APPEAL_BYTES`]; `409 not-suspended`; `409
/// appeal-already-sent` (also after a decline: an appeal is sent once);
/// else `204`. Auth never opens the appeal.
///
/// # Errors
///
/// [`ApiError::BadInput`], [`ApiError::Coded`], [`ApiError::Db`].
pub(crate) async fn appeal(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<AppealReq>,
) -> Result<StatusCode, ApiError> {
    let sealed = decode_b64(&req.sealed_b64).ok_or(ApiError::BadInput("sealed_b64 not base64"))?;
    if sealed.len() > MAX_APPEAL_BYTES {
        return Err(ApiError::BadInput("appeal too long"));
    }
    if lakko_moderation::seal::split(&sealed).is_err() {
        return Err(ApiError::BadInput("appeal is not a sealed part"));
    }
    let now = now_ms();
    let mut tx = state.db.begin().await.map_err(ApiError::Db)?;
    let row = live_row(&mut *tx, &auth.0, now, true)
        .await
        .map_err(ApiError::Db)?;
    let Some((_, _, appeal_state)) = row else {
        return Err(ApiError::Coded(StatusCode::CONFLICT, "not-suspended"));
    };
    if appeal_state != APPEAL_NONE {
        return Err(ApiError::Coded(StatusCode::CONFLICT, "appeal-already-sent"));
    }
    sqlx::query(
        "UPDATE account_suspensions SET appeal = ?, appeal_state = ?, appeal_at_ms = ? \
         WHERE account_id = ?",
    )
    .bind(&sealed)
    .bind(APPEAL_SENT)
    .bind(now)
    .bind(&auth.0[..])
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;
    tracing::info!("suspension appeal stored");
    Ok(StatusCode::NO_CONTENT)
}

/// Decode base64 in any of the four common spellings (URL-safe or standard,
/// padded or not). The internal caller is another service, and the public
/// caller is three clients; neither should fail on the alphabet.
fn decode_b64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    URL_SAFE_NO_PAD
        .decode(s)
        .or_else(|_| STANDARD.decode(s))
        .or_else(|_| URL_SAFE.decode(s))
        .or_else(|_| STANDARD_NO_PAD.decode(s))
        .ok()
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

/// Hourly: delete suspensions whose end has passed (they already stopped
/// counting at `until_ms`), and the identity-key history rows kept after a
/// suspended account's deletion whose suspension has ended.
pub(crate) fn spawn_sweep(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SWEEP_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            match sweep(&pool, now_ms()).await {
                Ok(0) => {},
                Ok(rows) => tracing::info!(rows, "expired suspensions swept"),
                Err(e) => tracing::warn!(error = %e, "suspension sweep failed"),
            }
            // ADR-0099 F3: identity keys of accounts deleted while suspended
            // stay taken until the suspension's end.
            match crate::identity_target::sweep_kept(&pool, now_ms()).await {
                Ok(0) => {},
                Ok(rows) => tracing::info!(rows, "kept identity targets released"),
                Err(e) => tracing::warn!(error = %e, "kept identity target sweep failed"),
            }
        }
    })
}

/// Delete the suspensions that ended at or before `now`.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn sweep(pool: &MySqlPool, now: i64) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM account_suspensions WHERE until_ms IS NOT NULL AND until_ms <= ?")
        .bind(now)
        .execute(pool)
        .await
        .map(|r| r.rows_affected())
}

// ---------------------------------------------------------------------------
// The internal listener
// ---------------------------------------------------------------------------

/// Default bind of the internal listener: loopback, not proxied by nginx.
pub(crate) const DEFAULT_INTERNAL_BIND: &str = "127.0.0.1:8101";
/// The header the shared secret is presented in.
pub(crate) const SECRET_HEADER: &str = "x-lakko-internal-secret";
/// Minimum decoded length of the shared secret.
pub(crate) const MIN_SECRET_LEN: usize = 16;
/// Body limit of the internal route: a signed statement, a moderator list of
/// up to 64 entries, and a few short fields, in base64.
const MAX_INTERNAL_BODY_BYTES: usize = 256 * 1024;

const BIND_VAR: &str = "LAKKO_AUTH_INTERNAL_BIND";
const SECRET_VAR: &str = "LAKKO_AUTH_INTERNAL_SECRET";
const ROOT_KEY_VAR: &str = "LAKKO_AUTH_MODERATION_ROOT_KEY";

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
/// `Ok(None)` when the secret or the root key is unset (the listener then
/// stays off, logged once as a warning by the caller).
///
/// # Errors
///
/// A message naming the variable when one is set but malformed; the service
/// refuses to start on it.
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

#[derive(Clone)]
struct InternalState {
    app: AppState,
    config: InternalConfig,
}

/// The internal router, behind the secret gate: the statement route, the
/// list push (F1) and the moderators' panel reads (ADR-0099 step 5).
pub(crate) fn internal_router(app: AppState, config: InternalConfig) -> Router {
    let state = InternalState { app, config };
    Router::new()
        .route("/internal/moderation/statement", post(apply_statement))
        .route("/internal/moderation/list", post(push_list))
        .route("/internal/moderation/account", post(account_read))
        .route("/internal/moderation/appeals", get(appeals_list))
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

fn coded(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

/// `POST /internal/moderation/statement` — apply a Suspend, a Lift or a
/// KeepSuspension. The body is [`StatementBody`]; it names no account key.
///
/// In order: the moderator list verifies under the pinned root (`400
/// bad-list`); it is not older than the stored floor (`409 stale-list`), and
/// a newer one raises the floor in its own committed transaction; then
/// ([`lakko_moderation::apply`]) the signed statement decodes and verifies
/// under the list (`403 not-authorised` for a moderator the list does not
/// authorise, `400 bad-statement` otherwise); it was signed within
/// [`lakko_moderation::apply::MAX_STATEMENT_AGE_MS`] before now and no more
/// than [`lakko_moderation::apply::MAX_STATEMENT_FUTURE_MS`] after (`400
/// stale-statement`, `400 future-statement`); its action is one of the three
/// (`400 wrong-service`); the reason and salt open its commitment (`400
/// reason-mismatch`). Then, in one transaction: the floor again (`409
/// stale-list` when a newer list arrived meanwhile); a statement already
/// applied is `200 {"applied": false, "repeat": true}`; no live account ever
/// held the statement's target is `404 unknown-account`, except for a Lift
/// that finds kept history (below); then by action:
///
/// - Suspend or Lift older than the newest decision applied to the account
///   is recorded and answered `200 {"applied": false, "superseded": true}`;
///   otherwise the account is suspended (the row replaced, the appeal reset)
///   or lifted (the row deleted): `200 {"applied": true}`;
/// - a Lift whose target no live account holds, but which an account deleted
///   while suspended left kept (F3), releases every kept row of that
///   account: `200 {"applied": true, "released_kept": true}`, or `200
///   {"applied": false, "superseded": true}` when the Lift was issued before
///   the newest decision applied to the account before its deletion;
/// - KeepSuspension declines the appeal of the account's current
///   suspension when one was sent and the suspension's statement was issued
///   no later than the keep: `200 {"applied": true}`; otherwise nothing
///   changes: `200 {"applied": false, "no_pending_appeal": true}`.
///
/// The statement id is recorded either way.
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
    /// It changed the accounts it names.
    Changed,
    /// Its id was applied before; nothing changed.
    Repeat,
    /// Every account it names already had a newer decision applied (a
    /// Suspend resent after a later Lift, or the other way round); its id is
    /// recorded and nothing else changed.
    Superseded,
    /// A KeepSuspension with no appeal pending against the account's current
    /// suspension; its id is recorded and nothing else changed.
    NoPendingAppeal,
    /// A Lift whose target no live account holds released the identity keys
    /// kept after a suspended account's deletion.
    ReleasedKept,
}

impl Applied {
    fn json(self) -> serde_json::Value {
        match self {
            Self::Changed => serde_json::json!({ "applied": true }),
            Self::Repeat => serde_json::json!({ "applied": false, "repeat": true }),
            Self::Superseded => serde_json::json!({ "applied": false, "superseded": true }),
            Self::NoPendingAppeal => {
                serde_json::json!({ "applied": false, "no_pending_appeal": true })
            },
            Self::ReleasedKept => serde_json::json!({ "applied": true, "released_kept": true }),
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

/// Record `issued_at_ms` as the newest decision applied to `account`, unless
/// a newer one already was; `false` when it was (the caller then leaves the
/// account alone). Runs inside the apply transaction, under the row lock.
async fn take_decision(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    account: &[u8],
    issued_at_ms: u64,
) -> Result<bool, Refusal> {
    let newest: Option<u64> = sqlx::query_scalar(
        "SELECT issued_at_ms FROM moderation_account_decisions WHERE account_id = ? FOR UPDATE",
    )
    .bind(account)
    .fetch_optional(&mut **tx)
    .await
    .map_err(Refusal::Db)?;
    if newest.is_some_and(|n| issued_at_ms < n) {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO moderation_account_decisions (account_id, issued_at_ms) VALUES (?, ?) \
         ON DUPLICATE KEY UPDATE issued_at_ms = VALUES(issued_at_ms)",
    )
    .bind(account)
    .bind(issued_at_ms)
    .execute(&mut **tx)
    .await
    .map_err(Refusal::Db)?;
    Ok(true)
}

/// Suspend `account` (the row replaced, the appeal reset) or lift it (the
/// row deleted), as `checked` says. Runs inside the apply transaction.
async fn write_decision(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    account: &[u8],
    checked: &CheckedStatement,
    log_index: u64,
    now: i64,
) -> Result<(), Refusal> {
    let until_ms: Option<i64> = checked
        .until_ms()
        .map(|u| i64::try_from(u).unwrap_or(i64::MAX));
    match checked.statement.action {
        Action::Suspend => {
            sqlx::query(
                "INSERT INTO account_suspensions \
                    (account_id, until_ms, reason, reason_salt, statement_id, log_index, \
                     suspended_at_ms, appeal_state, appeal, appeal_at_ms, target) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 0, NULL, NULL, ?) \
                 ON DUPLICATE KEY UPDATE until_ms = VALUES(until_ms), \
                    reason = VALUES(reason), reason_salt = VALUES(reason_salt), \
                    statement_id = VALUES(statement_id), log_index = VALUES(log_index), \
                    suspended_at_ms = VALUES(suspended_at_ms), appeal_state = 0, \
                    appeal = NULL, appeal_at_ms = NULL, target = VALUES(target)",
            )
            .bind(account)
            .bind(until_ms)
            .bind(checked.reason.as_u8())
            .bind(&checked.reason_salt[..])
            .bind(&checked.statement.statement_id[..])
            .bind(log_index)
            .bind(now)
            .bind(checked.statement.target.bytes())
            .execute(&mut **tx)
            .await
            .map_err(Refusal::Db)?;
        },
        _ => {
            sqlx::query("DELETE FROM account_suspensions WHERE account_id = ?")
                .bind(account)
                .execute(&mut **tx)
                .await
                .map_err(Refusal::Db)?;
        },
    }
    Ok(())
}

/// Compare `list` with the stored floor and raise the floor to it when it is
/// newer, in a transaction of its own that commits whatever happens next.
/// `409 stale-list` for a list older than the floor.
async fn raise_floor(pool: &MySqlPool, list: &ModeratorList) -> Result<(), Refusal> {
    let mut tx = pool.begin().await.map_err(Refusal::Db)?;
    let stored: Option<u64> = sqlx::query_scalar(
        "SELECT list_version FROM moderation_list_state WHERE id = 1 FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(Refusal::Db)?;
    match list_floor(stored, list.list_version)? {
        FloorStep::Keep => {},
        FloorStep::Raise => {
            sqlx::query(
                "INSERT INTO moderation_list_state (id, list_version) VALUES (1, ?) \
                 ON DUPLICATE KEY UPDATE list_version = GREATEST(list_version, VALUES(list_version))",
            )
            .bind(list.list_version)
            .execute(&mut *tx)
            .await
            .map_err(Refusal::Db)?;
            tracing::info!(
                list_version = list.list_version,
                "moderator list floor raised"
            );
        },
    }
    tx.commit().await.map_err(Refusal::Db)
}

/// `POST /internal/moderation/list` — raise the list floor without a
/// statement. The body is [`ListBody`]. `200 {"list_version": n}` (the
/// floor after the call); `400 bad-list` for a list that does not verify
/// under the pinned root; `409 stale-list` for one older than the floor.
async fn push_list(State(state): State<InternalState>, Json(req): Json<ListBody>) -> Response {
    match push(&state.app.db, &state.config.root_key, &req).await {
        Ok(version) => Json(serde_json::json!({ "list_version": version })).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// The work of [`push_list`]: the floor after the call.
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

/// The checks and the write of [`apply_statement`], separate so tests drive
/// them with an explicit clock.
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
    let checked = req.check_against(&list, ApplyingService::Auth.actions(), now)?;
    let statement = &checked.statement;
    let list_version = checked.list_version;

    let mut tx = pool.begin().await.map_err(Refusal::Db)?;
    // The floor again, under its row lock: one statement applies at a time,
    // and a newer list that arrived since the raise above wins.
    let stored: Option<u64> = sqlx::query_scalar(
        "SELECT list_version FROM moderation_list_state WHERE id = 1 FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(Refusal::Db)?;
    list_floor(stored, list_version)?;
    let repeat: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM moderation_applied_statements WHERE statement_id = ? FOR UPDATE",
    )
    .bind(&statement.statement_id[..])
    .fetch_optional(&mut *tx)
    .await
    .map_err(Refusal::Db)?;
    if repeat.is_some() {
        return Ok(Applied::Repeat);
    }
    // The one live account that ever held this target (F3: a target
    // belongs to one account, proven by its key, and the history keeps the
    // targets of keys it had before).
    let account =
        crate::identity_target::account_of_target(&mut *tx, statement.target.bytes(), true)
            .await
            .map_err(Refusal::Db)?;
    let outcome = if let Some(account) = account {
        decide_for_account(&mut tx, &account, &checked, req.log_index, now).await?
    } else if statement.action == Action::Lift {
        // No live account: a Lift may still release the keys of an account
        // deleted while suspended (F3). Nothing kept is an unknown account.
        match crate::identity_target::release_kept(
            &mut tx,
            statement.target.bytes(),
            statement.issued_at_ms,
        )
        .await
        .map_err(Refusal::Db)?
        {
            crate::identity_target::Release::Released => Applied::ReleasedKept,
            crate::identity_target::Release::Superseded => Applied::Superseded,
            crate::identity_target::Release::NothingKept => {
                return Err(Refusal::Coded(StatusCode::NOT_FOUND, "unknown-account"));
            },
        }
    } else {
        return Err(Refusal::Coded(StatusCode::NOT_FOUND, "unknown-account"));
    };
    sqlx::query(
        "INSERT INTO moderation_applied_statements (statement_id, applied_at_ms) VALUES (?, ?)",
    )
    .bind(&statement.statement_id[..])
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(Refusal::Db)?;
    tx.commit().await.map_err(Refusal::Db)?;
    tracing::info!(
        action = statement.action.name(),
        applied = matches!(outcome, Applied::Changed | Applied::ReleasedKept),
        "moderation statement applied",
    );
    Ok(outcome)
}

/// The decision for a live account: a KeepSuspension declines the pending
/// appeal; a Suspend or a Lift applies in the order moderators made them, not
/// the order they arrive (a statement older than the newest one already
/// applied to the account changes nothing for it).
async fn decide_for_account(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    account: &[u8],
    checked: &CheckedStatement,
    log_index: u64,
    now: i64,
) -> Result<Applied, Refusal> {
    if checked.statement.action == Action::KeepSuspension {
        return keep_suspension(tx, account, checked, now).await;
    }
    if take_decision(tx, account, checked.statement.issued_at_ms).await? {
        write_decision(tx, account, checked, log_index, now).await?;
        Ok(Applied::Changed)
    } else {
        Ok(Applied::Superseded)
    }
}

/// KeepSuspension: decline the appeal of `account`'s current suspension,
/// when one was sent and the newest decision applied to the account (the
/// suspension's own statement, or a later one) was issued no later than the
/// keep. A keep that arrives after a newer suspension replaced the one it
/// was about leaves that newer suspension's appeal alone. Runs inside the
/// apply transaction.
async fn keep_suspension(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    account: &[u8],
    checked: &CheckedStatement,
    now: i64,
) -> Result<Applied, Refusal> {
    let newest: Option<u64> = sqlx::query_scalar(
        "SELECT issued_at_ms FROM moderation_account_decisions WHERE account_id = ? FOR UPDATE",
    )
    .bind(account)
    .fetch_optional(&mut **tx)
    .await
    .map_err(Refusal::Db)?;
    if newest.is_some_and(|n| n > checked.statement.issued_at_ms) {
        return Ok(Applied::NoPendingAppeal);
    }
    let declined = sqlx::query(
        "UPDATE account_suspensions SET appeal_state = ? \
         WHERE account_id = ? AND appeal_state = ? AND (until_ms IS NULL OR until_ms > ?)",
    )
    .bind(APPEAL_DECLINED)
    .bind(account)
    .bind(APPEAL_SENT)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(Refusal::Db)?
    .rows_affected();
    Ok(if declined == 0 {
        Applied::NoPendingAppeal
    } else {
        Applied::Changed
    })
}

// ---------------------------------------------------------------------------
// The panel reads (ADR-0099 step 5)
// ---------------------------------------------------------------------------

/// The most appeals one list returns, oldest first. Chosen, not measured.
pub(crate) const MAX_APPEALS_LISTED: u32 = 1000;

/// Age bands of an account, by `accounts.created_at`.
const DAY_SECS: i64 = 86_400;

/// The band of an account `age_secs` old.
pub(crate) const fn age_band(age_secs: i64) -> &'static str {
    if age_secs < DAY_SECS {
        "day"
    } else if age_secs < 7 * DAY_SECS {
        "week"
    } else if age_secs < 30 * DAY_SECS {
        "month"
    } else {
        "older"
    }
}

/// A 32-byte target in any base64 spelling.
fn target_32(text: &str) -> Option<[u8; 32]> {
    decode_b64(text).and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
}

/// Body of `POST /internal/moderation/account`.
#[derive(Deserialize)]
pub(crate) struct AccountReadReq {
    /// Base64 of the 32-byte account target.
    pub(crate) target_b64: String,
}

/// `(age_secs, until_ms, suspended)` of one account with a target.
type AccountReadRow = (i64, Option<i64>, i64);

/// `POST /internal/moderation/account` — what the panel shows about an
/// account target: `{"exists", "age_band", "suspended", "until_ms"}`. The
/// account is the live one that ever held the target
/// (`account_identity_targets`), so a target of a key the account had before
/// a change still finds it. `age_band` is `"day"`, `"week"`, `"month"` or
/// `"older"` by `accounts.created_at` (under 1, 7, 30 days), `null` when no
/// account exists; `suspended` is true when it is suspended now, and
/// `until_ms` is then its end, `null` for no end. `400 bad-target` for a
/// target that is not 32 bytes.
async fn account_read(
    State(state): State<InternalState>,
    Json(req): Json<AccountReadReq>,
) -> Response {
    let Some(target) = target_32(&req.target_b64) else {
        return coded(StatusCode::BAD_REQUEST, "bad-target");
    };
    match read_account(&state.app.db, &target, now_ms()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => Refusal::Db(e).into_response(),
    }
}

/// The read of [`account_read`].
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn read_account(
    pool: &MySqlPool,
    target: &[u8; 32],
    now: i64,
) -> Result<serde_json::Value, sqlx::Error> {
    let rows: Vec<AccountReadRow> = sqlx::query_as(
        "SELECT CAST(TIMESTAMPDIFF(SECOND, a.created_at, NOW()) AS SIGNED), s.until_ms, \
             CAST(s.account_id IS NOT NULL AS SIGNED) \
         FROM account_identity_targets t \
         JOIN accounts a ON a.id = t.account_id \
         LEFT JOIN account_suspensions s \
             ON s.account_id = a.id AND (s.until_ms IS NULL OR s.until_ms > ?) \
         WHERE t.target = ?",
    )
    .bind(now)
    .bind(&target[..])
    .fetch_all(pool)
    .await?;
    let youngest = rows.iter().map(|(age, _, _)| *age).min();
    let suspended: Vec<Option<i64>> = rows
        .iter()
        .filter(|(_, _, s)| *s != 0)
        .map(|(_, until, _)| *until)
        .collect();
    // The latest end; no end beats every end.
    let until_ms = if suspended.iter().any(Option::is_none) {
        None
    } else {
        suspended.iter().flatten().max().copied()
    };
    Ok(serde_json::json!({
        "exists": !rows.is_empty(),
        "age_band": youngest.map(age_band),
        "suspended": !suspended.is_empty(),
        "until_ms": until_ms,
    }))
}

/// `(target, appeal, statement_id, log_index, until_ms, appeal_at_ms)` of a
/// sent appeal.
type AppealRow = (Vec<u8>, Vec<u8>, Vec<u8>, u64, Option<i64>, Option<i64>);

/// `GET /internal/moderation/appeals` — the appeals waiting for a moderator:
/// live suspensions whose appeal was sent, oldest appeal first, at most
/// [`MAX_APPEALS_LISTED`]. `{"appeals": [{"target_b64", "sealed_b64",
/// "statement_id_b64", "log_index", "until_ms", "appeal_at_ms"}]}`, base64
/// standard. A moderator keeps a suspension, declining its appeal, by
/// signing a KeepSuspension statement for `target_b64` (there is no
/// unsigned decline).
async fn appeals_list(State(state): State<InternalState>) -> Response {
    match list_appeals(&state.app.db, now_ms()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => Refusal::Db(e).into_response(),
    }
}

/// The read of [`appeals_list`].
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn list_appeals(
    pool: &MySqlPool,
    now: i64,
) -> Result<serde_json::Value, sqlx::Error> {
    let rows: Vec<AppealRow> = sqlx::query_as(
        "SELECT target, appeal, statement_id, log_index, until_ms, appeal_at_ms \
         FROM account_suspensions \
         WHERE appeal_state = ? AND appeal IS NOT NULL AND target IS NOT NULL \
             AND (until_ms IS NULL OR until_ms > ?) \
         ORDER BY appeal_at_ms ASC LIMIT ?",
    )
    .bind(APPEAL_SENT)
    .bind(now)
    .bind(MAX_APPEALS_LISTED)
    .fetch_all(pool)
    .await?;
    let appeals: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(target, appeal, statement_id, log_index, until_ms, appeal_at_ms)| {
                serde_json::json!({
                    "target_b64": STANDARD.encode(target),
                    "sealed_b64": STANDARD.encode(appeal),
                    "statement_id_b64": STANDARD.encode(statement_id),
                    "log_index": log_index,
                    "until_ms": until_ms,
                    "appeal_at_ms": appeal_at_ms,
                })
            },
        )
        .collect();
    Ok(serde_json::json!({ "appeals": appeals }))
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn the_internal_config_needs_both_secret_and_root() {
        let secret = STANDARD.encode([7u8; 32]);
        let root = URL_SAFE_NO_PAD.encode([9u8; 32]);
        assert!(parse_internal(None, None, None).unwrap().is_none());
        assert!(parse_internal(None, Some(&secret), None).unwrap().is_none());
        assert!(parse_internal(None, None, Some(&root)).unwrap().is_none());
        let cfg = parse_internal(None, Some(&secret), Some(&root))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.bind.to_string(), DEFAULT_INTERNAL_BIND);
        assert_eq!(cfg.root_key, [9u8; 32]);
        assert!(parse_internal(None, Some("!!"), Some(&root)).is_err());
        assert!(parse_internal(None, Some(&STANDARD.encode([1u8; 8])), Some(&root)).is_err());
        assert!(parse_internal(None, Some(&secret), Some("short")).is_err());
        assert!(parse_internal(Some("nope"), Some(&secret), Some(&root)).is_err());
    }

    #[test]
    fn base64_in_any_spelling() {
        let bytes = [0xFBu8, 0xFF, 0x00, 0x10, 0x20];
        for s in [
            URL_SAFE_NO_PAD.encode(bytes),
            STANDARD.encode(bytes),
            URL_SAFE.encode(bytes),
            STANDARD_NO_PAD.encode(bytes),
        ] {
            assert_eq!(decode_b64(&s).as_deref(), Some(&bytes[..]), "{s}");
        }
        assert!(decode_b64("@@").is_none());
    }

    #[test]
    fn the_suspended_body_carries_its_code_and_never_debug_prints_credentials() {
        let body = SuspendedBody {
            account_id_b64: "acc".into(),
            restricted_token: "s1.secret".into(),
            restricted_expires_at_ms: 1,
            refresh_token: "r2.secret".into(),
            refresh_expires_at_ms: 2,
            key_generation: 3,
            grant: None,
        };
        assert!(!format!("{body:?}").contains("secret"));
        let mut keys: Vec<String> = serde_json::to_value(&body)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "account_id_b64",
                "key_generation",
                "refresh_expires_at_ms",
                "refresh_token",
                "restricted_expires_at_ms",
                "restricted_token",
            ]
        );
    }
}
