// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sign in and recover by name (ADR-0054 S5).
//!
//! Since ADR-0054 S3 a username lookup at `services/discovery` spends an
//! anonymous token, and a token needs a session, so a device without a
//! cached account id could no longer find the account to sign into. The
//! pre-session routes therefore take a **login handle** instead of an
//! account id:
//!
//! ```text
//!   handle     = BLAKE3-derive-key("lakko/auth/login-handle/v1", canonical_username)
//!   handle_key = BLAKE3-keyed(LAKKO_AUTH_HANDLE_PEPPER, handle)
//! ```
//!
//! The client computes the handle (`lakko_contacts::username::login_handle`,
//! `loginHandle` on both FFI surfaces); this module stores only `handle_key`
//! against the account id (`login_handles`, migrations `0021` and `0028`), so a database
//! leak without the pepper is not a dictionary over usernames.
//!
//! - **`PUT /auth/login-handle`** — bearer. Binds the caller's account to a
//!   handle, replacing its previous one (one per account). Since ADR-0076 a
//!   bind that CHANGES the handle (the account already holds another) needs
//!   `fresh_grant` (`403 fresh-auth-required`, before any counter); a first
//!   bind and a re-bind of the held handle do not. With `recovery_email`
//!   (and `previous_handle`) a change moves the recovery email to the new
//!   handle instead of dropping it (`403 address-mismatch`, `409
//!   handle-mismatch`; either refuses the whole bind). A change that would
//!   drop the recovery email when it is the account's last second factor is
//!   `409 {"error":"last-factor"}` (ADR-0075, amended 2026-11-26).
//! - **`POST /auth/login-handle/check {handle}`** — bearer. `{"state":
//!   "same" | "other" | "none"}` for the session's own account. A handle another
//!   account holds is `409 {"error":"handle-taken"}` and changes nothing.
//!   That answer is an existence oracle clients need (the claim order), so
//!   it is rationed rather than removed: a per-account counter charged on
//!   every bind, a per-address bucket, and a minimum interval between two
//!   binds that change the handle (`429 {"error":"handle-bind-limited"}`).
//! - **`DELETE /auth/login-handle`** — bearer. Removes the caller's handle.
//!   Account deletion removes it too (FK cascade).
//!
//! **Resolution on the name routes** ([`resolve`]): `handle -> handle_key ->
//! account_id`. A handle nobody holds resolves to a *stand-in* account id
//! derived from its `handle_key`, stable per handle, and the caller runs its
//! ordinary path on it: the same queries, the same OPAQUE fake-credential
//! response (with a credential identifier that is stable per handle, so the
//! same unknown name gets the same OPRF key every time), the same dummy
//! verify. Nothing a caller can observe says whether a handle is bound.
//!
//! **A change of handle turns the recovery email off** (ADR-0075 section 6):
//! re-binding to another handle, or removing the handle, deletes the
//! account's `recovery_emails` row (and its challenges and tickets) in the
//! same transaction and writes the "recovery email removed" notice: its key
//! was derived from the old name.
//!
//! **Without the pepper** (`LAKKO_AUTH_HANDLE_PEPPER` unset) every name route
//! and both handle routes answer `503 {"error":"login-by-name-disabled"}`,
//! logged loudly at startup.

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash::{blake3_derive_key_from_material, blake3_keyed};
use serde::Deserialize;
use sqlx::MySqlPool;

use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Environment variable holding the pepper: 64 hex characters (32 bytes).
pub(crate) const PEPPER_VAR: &str = "LAKKO_AUTH_HANDLE_PEPPER";

/// Length of a login handle on the wire, in bytes.
pub(crate) const HANDLE_LEN: usize = 32;

/// Length of an account id, in bytes.
const ACCOUNT_ID_LEN: usize = 16;

/// The pepper every test `AppState` carries.
#[cfg(test)]
pub(crate) const TEST_PEPPER: [u8; 32] = [0x5E; 32];

/// Derive-key context of the stand-in account id an unbound handle resolves
/// to. Frozen: a change moves every unknown name to a new OPRF key.
const STAND_IN_DOMAIN: &str = "lakko/auth/unknown-handle/v1";

/// Load the pepper. `Ok(None)` when unset (name routes answer 503).
///
/// # Errors
///
/// When the variable is set but is not exactly 64 hex characters.
pub(crate) fn load_handle_pepper() -> anyhow::Result<Option<[u8; 32]>> {
    let raw = match std::env::var(PEPPER_VAR) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return Ok(None),
    };
    parse_hex32(raw.trim())
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("{PEPPER_VAR} must be 64 hex characters (32 bytes)"))
}

pub(crate) fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

/// Log the startup state, loudly when dormant.
pub(crate) fn log_startup(pepper: Option<&[u8; 32]>) {
    if pepper.is_some() {
        tracing::info!("sign-in by name enabled");
    } else {
        tracing::warn!(
            "sign-in by name DISABLED: {PEPPER_VAR} is not set; the login, recovery and \
             reset routes answer 503 and a device without a cached account id cannot sign in"
        );
    }
}

/// The pepper, or the `503` every name route answers without one.
pub(crate) fn pepper(state: &AppState) -> Result<[u8; 32], ApiError> {
    state
        .handle_pepper
        .as_ref()
        .map(|p| **p)
        .ok_or(ApiError::LoginByNameDisabled)
}

/// Decode a wire handle: base64url without padding, exactly 32 bytes.
pub(crate) fn decode_handle(b64: &str) -> Result<[u8; HANDLE_LEN], ApiError> {
    URL_SAFE_NO_PAD
        .decode(b64)
        .ok()
        .and_then(|v| <[u8; HANDLE_LEN]>::try_from(v.as_slice()).ok())
        .ok_or(ApiError::BadInput(
            "handle must be 32 bytes, base64url without padding",
        ))
}

/// `keyed-BLAKE3(pepper, handle)` — what `login_handles.handle_key` holds.
fn handle_key(pepper: &[u8; 32], handle: &[u8; HANDLE_LEN]) -> [u8; 32] {
    blake3_keyed(pepper, handle)
}

/// The account id an unbound handle stands in as. Derived from the peppered
/// key, so it is stable per handle and unrelated to any real UUIDv7 (a
/// collision is 2^-128).
fn stand_in_account_id(key: &[u8; 32]) -> [u8; ACCOUNT_ID_LEN] {
    let d = blake3_derive_key_from_material(STAND_IN_DOMAIN, key);
    let mut out = [0u8; ACCOUNT_ID_LEN];
    out.copy_from_slice(&d[..ACCOUNT_ID_LEN]);
    out
}

/// A handle resolved for a name route.
pub(crate) struct Resolved {
    /// The bound account id, or the handle's stand-in.
    pub(crate) account_id: [u8; ACCOUNT_ID_LEN],
    /// The handle's stand-in whether or not it is bound: where a route
    /// that must not act on the account (a suspended device link) files the
    /// request instead, so it answers exactly as for an unknown name.
    pub(crate) stand_in: [u8; ACCOUNT_ID_LEN],
    /// Whether a real account holds the handle. Callers MUST NOT branch on
    /// it in a way a client can observe; it exists for tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) bound: bool,
}

/// Resolve `handle` to the account to act on: the one bound to it, or a
/// stand-in that makes every downstream lookup miss exactly as for an
/// account without the factor. One indexed read either way.
pub(crate) async fn resolve(
    pool: &MySqlPool,
    pepper: &[u8; 32],
    handle: &[u8; HANDLE_LEN],
) -> Result<Resolved, ApiError> {
    let key = handle_key(pepper, handle);
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT account_id FROM login_handles WHERE handle_key = ?")
            .bind(key.as_slice())
            .fetch_optional(pool)
            .await
            .map_err(ApiError::Db)?;
    let bound = row.and_then(|(v,)| <[u8; ACCOUNT_ID_LEN]>::try_from(v.as_slice()).ok());
    let stand_in = stand_in_account_id(&key);
    Ok(match bound {
        Some(account_id) => Resolved {
            account_id,
            stand_in,
            bound: true,
        },
        None => Resolved {
            account_id: stand_in,
            stand_in,
            bound: false,
        },
    })
}

/// What a bind may do when it turns out to CHANGE the account's handle
/// (ADR-0076 section 2). Decided by the caller before the transaction; the
/// transaction applies it to what it finds under the account row's lock.
#[derive(Default)]
pub(crate) struct ChangeTerms {
    /// The request carried a fresh-auth grant that verified. Without it a
    /// change is refused ([`BindOutcome::GrantRequired`]).
    pub(crate) grant: bool,
    /// `handle_key` of the handle the client says the account holds now
    /// (`previous_handle`). Checked against the stored one before a re-key.
    pub(crate) previous_key: Option<[u8; 32]>,
    /// Move the recovery email to the new handle instead of dropping it.
    pub(crate) rekey: Option<crate::recovery_email::RekeyPlan>,
    /// The beta account cap (`crate::account_cap`), applied to a FIRST bind
    /// only; 0 = no cap.
    pub(crate) max_accounts: u32,
    /// ADR-0097: a verified `beta-place` token the bind carried. Spent in the
    /// bind's transaction, and only when the cap refuses a first bind.
    pub(crate) supporter: Option<crate::supporter_codes::SupporterPlace>,
}

/// Error code of a first bind past the cap whose supporter place was already
/// spent (ADR-0097).
pub(crate) const CODE_SUPPORTER_CODE_USED: &str = "supporter-code-used";

/// How a bind ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindOutcome {
    /// The account now holds the handle (it held another, or none).
    Bound,
    /// The account already held this handle; nothing changed.
    Unchanged,
    /// Another account holds the handle; nothing changed (`409`).
    Taken,
    /// The account changed its handle less than the minimum interval ago
    /// and this bind would change it again; nothing changed (`429`).
    TooSoon {
        /// Seconds until a change is admitted (>= 1).
        retry_after_secs: u64,
    },
    /// The bind would change the account's handle and carried no verified
    /// fresh-auth grant; nothing changed (`403 fresh-auth-required`).
    GrantRequired,
    /// A re-key was asked for and `previous_handle` is not the handle the
    /// account holds; nothing changed (`409 handle-mismatch`).
    PreviousHandleMismatch,
    /// A re-key was asked for and the address is not the stored one's;
    /// nothing changed (`403 address-mismatch`).
    AddressMismatch,
    /// A first bind found the beta account cap reached; nothing changed
    /// (`403 registration-closed`, `crate::account_cap`).
    RegistrationClosed,
    /// A first bind past the cap carried a `beta-place` token that was
    /// already spent; nothing changed (`403 supporter-code-used`, ADR-0097).
    SupporterCodeUsed,
    /// The change would drop the recovery email without moving it, and it is
    /// the account's last second factor; nothing changed (`409 last-factor`,
    /// ADR-0075, amended 2026-11-26).
    LastFactor,
}

/// Attempts at a bind that lost a race: a concurrent bind of the same
/// account (not expected, the account-row lock serialises them) or an
/// `InnoDB` deadlock (not expected either, see [`bind_once`]; kept because a
/// deadlock is the database's decision, not this code's). A bind runs in one
/// transaction that the database rolls back whole on a deadlock, so running
/// it again is safe.
const BIND_ATTEMPTS: usize = 3;

/// Why one bind attempt did not finish.
enum AttemptError {
    /// A unique violation on the account key, not the handle: a concurrent
    /// bind of the same account got in between. Retried.
    AccountRace,
    /// The database chose this transaction as a deadlock victim and rolled
    /// it back. Retried.
    Deadlock,
    Db(sqlx::Error),
}

/// The unique index on `login_handles.handle_key` (migration `0028`), as the
/// database names it in a duplicate-key error.
const HANDLE_UNIQUE_KEY: &str = "uq_login_handles_handle";

/// `MariaDB` / `MySQL` `ER_LOCK_DEADLOCK`.
const ER_LOCK_DEADLOCK: &str = "1213";

/// SQLSTATE of a transaction rolled back as a deadlock victim.
const SQLSTATE_DEADLOCK: &str = "40001";

fn is_deadlock(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db) => {
            db.code().as_deref() == Some(SQLSTATE_DEADLOCK)
                || db.message().contains(ER_LOCK_DEADLOCK)
                || db.message().contains("Deadlock found")
        },
        _ => false,
    }
}

impl From<sqlx::Error> for AttemptError {
    fn from(e: sqlx::Error) -> Self {
        if is_deadlock(&e) {
            Self::Deadlock
        } else {
            Self::Db(e)
        }
    }
}

/// Bind `account_id` to `handle`, replacing its previous handle.
///
/// The account's binds are serialised on its `accounts` row, so two
/// concurrent binds of one account run one after the other and only a real
/// handle conflict (the primary key, held by another account) answers
/// [`BindOutcome::Taken`]. A change within `min_interval_secs` of the
/// account's last change is refused ([`BindOutcome::TooSoon`]) unless it
/// restores the handle the account held just before; re-binding the handle
/// it holds is [`BindOutcome::Unchanged`] and not a change.
pub(crate) async fn bind(
    pool: &MySqlPool,
    pepper: &[u8; 32],
    account_id: &[u8; ACCOUNT_ID_LEN],
    handle: &[u8; HANDLE_LEN],
    min_interval_secs: u32,
    terms: &ChangeTerms,
) -> Result<BindOutcome, sqlx::Error> {
    let key = handle_key(pepper, handle);
    for _ in 0..BIND_ATTEMPTS {
        match bind_once(pool, account_id, &key, min_interval_secs, terms).await {
            Ok(outcome) => return Ok(outcome),
            Err(AttemptError::AccountRace) => {
                tracing::debug!("login handle bind raced a bind of the same account; retrying");
            },
            Err(AttemptError::Deadlock) => {
                tracing::warn!("login handle bind was a deadlock victim; retrying");
            },
            Err(AttemptError::Db(e)) => return Err(e),
        }
    }
    Err(sqlx::Error::Protocol(
        "login handle bind kept losing to concurrent binds".to_string(),
    ))
}

/// ADR-0097: a first bind the cap refused may still go through on a
/// supporter place, which is spent here, in the bind's transaction, so it
/// goes with the bind or not at all. `Some(outcome)` refuses the bind.
async fn take_supporter_place(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    terms: &ChangeTerms,
) -> Result<Option<BindOutcome>, AttemptError> {
    let Some(place) = terms.supporter else {
        return Ok(Some(BindOutcome::RegistrationClosed));
    };
    match lakko_anon_token_server::spend_on(
        tx,
        lakko_anon_token::TokenContext::BetaPlace,
        &place.spend_id,
        place.epoch,
    )
    .await
    {
        Ok(()) => Ok(None),
        Err(lakko_anon_token_server::SpendError::AlreadySpent) => {
            Ok(Some(BindOutcome::SupporterCodeUsed))
        },
        Err(lakko_anon_token_server::SpendError::Db(e)) => Err(e.into()),
    }
}

/// One bind attempt, in one transaction.
///
/// **Lock order.** The account row first (`FOR UPDATE`), then the account's
/// own `login_handles` row, found by primary key (`account_id`, migration
/// `0028`): an insert when the account holds no handle, an update in place
/// when it holds one. No statement here searches for a row that may not
/// exist, and none deletes and re-inserts a row, because both take gap or
/// next-key locks on an index whose neighbouring entries belong to other
/// accounts (account ids are `UUIDv7`, so accounts created together sit
/// together). Under the schema of migration `0021` both happened, and two
/// accounts binding at once could be rolled back with `1213 Deadlock found`;
/// that was reachable from `PUT /auth/login-handle` (every signup binds a
/// first handle), not only from the tests that found it.
#[allow(clippy::too_many_lines)] // one transaction, read top to bottom
async fn bind_once(
    pool: &MySqlPool,
    account_id: &[u8; ACCOUNT_ID_LEN],
    key: &[u8; 32],
    min_interval_secs: u32,
    terms: &ChangeTerms,
) -> Result<BindOutcome, AttemptError> {
    let mut tx = pool.begin().await?;
    // Serialise this account's binds: the second waits here until the first
    // commits or rolls back.
    let locked: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT id FROM accounts WHERE id = ? FOR UPDATE")
            .bind(account_id.as_slice())
            .fetch_optional(&mut *tx)
            .await?;
    if locked.is_none() {
        return Err(AttemptError::Db(sqlx::Error::RowNotFound));
    }
    let current: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT handle_key FROM login_handles WHERE account_id = ?")
            .bind(account_id.as_slice())
            .fetch_optional(&mut *tx)
            .await?;
    let current = current.map(|(k,)| k);
    if current.as_deref() == Some(key.as_slice()) {
        tx.commit().await?;
        return Ok(BindOutcome::Unchanged);
    }
    // ADR-0076 section 2: a CHANGE of handle (the account holds another one)
    // needs a fresh-auth grant: a session alone must not move the name the
    // account is signed into by. Decided here, under the lock, on the handle
    // the account holds NOW; a first bind needs none. Before the interval
    // check, so restoring the previous handle needs it too.
    if current.is_some() && !terms.grant {
        return Ok(BindOutcome::GrantRequired);
    }
    let change: Option<(Option<i64>, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT CAST(? - TIMESTAMPDIFF(SECOND, last_changed_at, NOW(3)) AS SIGNED), \
                previous_handle_key \
         FROM login_handle_changes WHERE account_id = ?",
    )
    .bind(i64::from(min_interval_secs))
    .bind(account_id.as_slice())
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((Some(remaining), previous)) = change {
        let restoring = previous.as_deref() == Some(key.as_slice());
        if remaining > 0 && !restoring {
            return Ok(BindOutcome::TooSoon {
                retry_after_secs: u64::try_from(remaining).unwrap_or(1).max(1),
            });
        }
    }
    let written = if current.is_some() {
        sqlx::query("UPDATE login_handles SET handle_key = ? WHERE account_id = ?")
            .bind(key.as_slice())
            .bind(account_id.as_slice())
            .execute(&mut *tx)
            .await
    } else {
        // ADR-0046 amendment: a first handle takes a place under the beta
        // cap. Decided under the cap lock, after the account row's lock.
        if !crate::account_cap::admits_one_more(&mut tx, terms.max_accounts).await? {
            if let Some(refused) = take_supporter_place(&mut tx, terms).await? {
                return Ok(refused);
            }
        }
        sqlx::query("INSERT INTO login_handles (handle_key, account_id) VALUES (?, ?)")
            .bind(key.as_slice())
            .bind(account_id.as_slice())
            .execute(&mut *tx)
            .await
    };
    match written {
        Ok(_) => {},
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            // Rolled back on drop: the caller keeps its previous handle.
            return if e.message().contains(HANDLE_UNIQUE_KEY) {
                Ok(BindOutcome::Taken)
            } else {
                // The primary key: a row for this account appeared between
                // the read above and the insert.
                Err(AttemptError::AccountRace)
            };
        },
        Err(e) => return Err(e.into()),
    }
    if let Some(current) = current.as_deref() {
        // ADR-0075 section 6: the recovery email was made from the old name
        // (the server's `email_key` takes the handle), so a change of handle
        // turns it off in this transaction,
        // unless the bind carries the confirmed address (ADR-0076 section
        // 2): then the key is moved to the new handle here, with no letter.
        match terms.rekey.as_ref() {
            None => {
                // ADR-0075 (amended 2026-11-26): the last second factor stays;
                // the change must carry the address to keep it.
                if crate::factors::is_last(
                    &mut tx,
                    account_id,
                    crate::factors::Leaving::RecoveryEmail,
                )
                .await?
                {
                    // Rolled back on drop: the handle written above too.
                    return Ok(BindOutcome::LastFactor);
                }
                crate::recovery_email::drop_for_handle_change(&mut tx, account_id).await?;
            },
            Some(plan) => {
                // The server stores only the keyed handle, so the client
                // names the old one; it must be the one the account holds.
                if terms.previous_key.as_ref().map(<[u8; 32]>::as_slice) != Some(current) {
                    return Ok(BindOutcome::PreviousHandleMismatch);
                }
                let outcome =
                    crate::recovery_email::rekey_for_handle_change(&mut tx, account_id, plan)
                        .await?;
                if outcome == crate::recovery_email::RekeyOutcome::AddressMismatch {
                    // Rolled back on drop: the handle written above too.
                    return Ok(BindOutcome::AddressMismatch);
                }
            },
        }
    }
    sqlx::query(
        "INSERT INTO login_handle_changes (account_id, last_changed_at, previous_handle_key) \
         VALUES (?, NOW(3), ?) \
         ON DUPLICATE KEY UPDATE last_changed_at = NOW(3), \
           previous_handle_key = VALUES(previous_handle_key)",
    )
    .bind(account_id.as_slice())
    .bind(current.as_deref())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(BindOutcome::Bound)
}

/// `PUT /auth/login-handle` body.
#[derive(Deserialize)]
pub(crate) struct PutReq {
    /// The 32-byte handle, base64url without padding.
    pub handle: String,
    /// The `k1` fresh-auth grant. Needed only when the bind CHANGES the
    /// account's handle (ADR-0076 section 2).
    #[serde(default)]
    pub fresh_grant: Option<String>,
    /// The handle the account holds now, base64url without padding. Needed
    /// with `recovery_email`: the server stores only a keyed hash of the
    /// handle and cannot recompute the stored `email_key` without it.
    #[serde(default)]
    pub previous_handle: Option<String>,
    /// Keep the recovery email across the change by re-keying it to the new
    /// handle. Absent: a change drops it.
    #[serde(default)]
    pub recovery_email: Option<crate::recovery_email::RekeyReq>,
}

/// `handle_key` of the handle `account_id` holds, if any.
async fn current_key(
    pool: &MySqlPool,
    account_id: &[u8; ACCOUNT_ID_LEN],
) -> Result<Option<Vec<u8>>, sqlx::Error> {
    sqlx::query_scalar("SELECT handle_key FROM login_handles WHERE account_id = ?")
        .bind(account_id.as_slice())
        .fetch_optional(pool)
        .await
}

/// `PUT /auth/login-handle` — bind the caller's account to a handle.
///
/// The `409` a taken handle answers is an existence oracle a client needs
/// (the claim order of ADR-0054 S5 stops on it), so it is kept and made
/// expensive instead: every bind is charged to the account's
/// [`crate::limits::Scope::HandleBind`] counter (default 5 per 24 h) and to
/// the per-address `HandleBind` bucket, and a bind that would change the
/// handle within the minimum interval of the last change is refused. Each
/// refusal is `429 {"error":"handle-bind-limited","retry_after_secs":N}`.
#[axum::debug_handler]
pub(crate) async fn put(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PutReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pepper = pepper(&state)?;
    let handle = decode_handle(&req.handle)?;
    let previous = req
        .previous_handle
        .as_deref()
        .map(decode_handle)
        .transpose()?;
    let rekey = req
        .recovery_email
        .as_ref()
        .map(|r| crate::recovery_email::rekey_plan(&state, previous.as_ref(), &handle, r))
        .transpose()?;

    // ADR-0076 section 2: a change needs the grant, and the refusal comes
    // before any counter, so a session without one spends none of the
    // owner's bind budget. Whether this IS a change is read here and decided
    // again under the row lock in `bind_once`.
    let held = current_key(&state.db, &auth.0)
        .await
        .map_err(ApiError::Db)?;
    let changes = held
        .as_deref()
        .is_some_and(|held| held != handle_key(&pepper, &handle).as_slice());
    let grant = if changes || req.fresh_grant.is_some() {
        match crate::session::require_fresh_grant(&state, &auth.0, req.fresh_grant.as_deref()).await
        {
            Ok(_) => true,
            Err(ApiError::FreshAuthRequired) if !changes => false,
            Err(e) => return Err(e),
        }
    } else {
        false
    };

    crate::limits::charge_name(
        &state.db,
        &state.limits,
        crate::limits::Scope::HandleBind,
        &auth.0,
    )
    .await?;
    let terms = ChangeTerms {
        grant,
        previous_key: previous.as_ref().map(|p| handle_key(&pepper, p)),
        rekey,
        max_accounts: state.limits.max_accounts(),
        supporter: crate::supporter_codes::place_from_headers(state.anon_tokens.as_ref(), &headers),
    };
    let outcome = bind(
        &state.db,
        &pepper,
        &auth.0,
        &handle,
        state.limits.handle_change_min_interval_secs(),
        &terms,
    )
    .await
    .map_err(ApiError::Db)?;
    match outcome {
        BindOutcome::Bound | BindOutcome::Unchanged => {},
        BindOutcome::Taken => return Err(ApiError::HandleTaken),
        BindOutcome::GrantRequired => return Err(ApiError::FreshAuthRequired),
        BindOutcome::PreviousHandleMismatch => {
            return Err(ApiError::Coded(
                axum::http::StatusCode::CONFLICT,
                "handle-mismatch",
            ));
        },
        BindOutcome::AddressMismatch => {
            return Err(ApiError::Coded(
                axum::http::StatusCode::FORBIDDEN,
                "address-mismatch",
            ));
        },
        BindOutcome::LastFactor => {
            return Err(ApiError::Coded(
                axum::http::StatusCode::CONFLICT,
                crate::factors::CODE_LAST_FACTOR,
            ));
        },
        BindOutcome::RegistrationClosed => {
            tracing::info!("login handle first bind refused: account cap reached");
            return Err(ApiError::Coded(
                axum::http::StatusCode::FORBIDDEN,
                crate::account_cap::CODE_REGISTRATION_CLOSED,
            ));
        },
        BindOutcome::SupporterCodeUsed => {
            tracing::info!("login handle first bind refused: supporter place already used");
            return Err(ApiError::Coded(
                axum::http::StatusCode::FORBIDDEN,
                CODE_SUPPORTER_CODE_USED,
            ));
        },
        BindOutcome::TooSoon { retry_after_secs } => {
            crate::metrics::handle_change_too_soon();
            return Err(ApiError::Limit(crate::limits::LimitError::Limited {
                code: crate::limits::CODE_HANDLE_BIND_LIMITED,
                retry_after_secs,
            }));
        },
    }
    tracing::info!("login handle bound");
    Ok(Json(serde_json::json!({ "bound": true })))
}

/// `POST /auth/login-handle/check` body.
#[derive(Deserialize)]
pub(crate) struct CheckReq {
    /// The 32-byte handle the device holds, base64url without padding.
    pub handle: String,
}

/// `POST /auth/login-handle/check {handle}` — bearer (ADR-0076 section 2).
/// How the handle a device holds relates to the one its account is bound to:
///
/// - `{"state":"same"}`: it is the bound one; nothing to do;
/// - `{"state":"other"}`: the account is bound to ANOTHER handle (it was
///   renamed on another device): bind nothing and wait for the name from the
///   account's other devices;
/// - `{"state":"none"}`: the account has no handle; the device may bind.
///
/// It compares with the session's own account only, so it says nothing about
/// any other account. One primary-key read, counted per account by
/// [`crate::limits::Scope::HandleCheck`] (its own counter: every device asks
/// at every landing, and the bind's budget is five a day).
///
/// # Errors
///
/// `503 login-by-name-disabled`, [`ApiError::BadInput`], [`ApiError::Limit`],
/// [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn check_handle(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<CheckReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pepper = pepper(&state)?;
    let handle = decode_handle(&req.handle)?;
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        crate::limits::Scope::HandleCheck,
        &auth.0,
    )
    .await?;
    let held = current_key(&state.db, &auth.0)
        .await
        .map_err(ApiError::Db)?;
    let state_name = match held {
        None => "none",
        Some(held) if held == handle_key(&pepper, &handle).as_slice() => "same",
        Some(_) => "other",
    };
    Ok(Json(serde_json::json!({ "state": state_name })))
}

/// `DELETE /auth/login-handle` — remove the caller's handle.
///
/// The removed handle becomes the one a later bind may restore within the
/// minimum interval; the time of the last change is left as it was, so
/// releasing a handle does not reopen the interval.
///
/// An account older than the unclaimed-account lifetime needs the fresh-auth
/// grant in `x-lakko-fresh-grant` (`403 fresh-auth-required` otherwise).
#[axum::debug_handler]
pub(crate) async fn delete(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    pepper(&state)?;
    // An account past the unclaimed-account lifetime may not drop its name
    // on a session alone (see `unclaimed::is_past_unclaimed_lifetime`).
    if crate::unclaimed::is_past_unclaimed_lifetime(&state.db, &auth.0)
        .await
        .map_err(ApiError::Db)?
    {
        crate::session::require_fresh_grant(
            &state,
            &auth.0,
            crate::session::fresh_grant_header(&headers),
        )
        .await?;
    }
    let removed = remove(&state.db, &auth.0).await.map_err(ApiError::Db)?;
    tracing::info!(removed, "login handle removed");
    Ok(Json(serde_json::json!({ "removed": removed })))
}

async fn remove(pool: &MySqlPool, account_id: &[u8; ACCOUNT_ID_LEN]) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let locked: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT id FROM accounts WHERE id = ? FOR UPDATE")
            .bind(account_id.as_slice())
            .fetch_optional(&mut *tx)
            .await?;
    if locked.is_none() {
        return Ok(false);
    }
    let current: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT handle_key FROM login_handles WHERE account_id = ?")
            .bind(account_id.as_slice())
            .fetch_optional(&mut *tx)
            .await?;
    let Some((current,)) = current else {
        tx.commit().await?;
        return Ok(false);
    };
    sqlx::query("DELETE FROM login_handles WHERE account_id = ?")
        .bind(account_id.as_slice())
        .execute(&mut *tx)
        .await?;
    // ADR-0075 section 6: without the handle the recovery email names nobody.
    crate::recovery_email::drop_for_handle_change(&mut tx, account_id).await?;
    sqlx::query(
        "INSERT INTO login_handle_changes (account_id, last_changed_at, previous_handle_key) \
         VALUES (?, NULL, ?) \
         ON DUPLICATE KEY UPDATE previous_handle_key = VALUES(previous_handle_key)",
    )
    .bind(account_id.as_slice())
    .bind(current.as_slice())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// A fresh random handle and its wire form (tests).
#[cfg(test)]
pub(crate) fn random_handle() -> ([u8; HANDLE_LEN], String) {
    use rand::RngCore as _;
    let mut h = [0u8; HANDLE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut h);
    (h, URL_SAFE_NO_PAD.encode(h))
}

/// Bind `handle` to `account_id` under [`TEST_PEPPER`] (tests).
#[cfg(test)]
pub(crate) async fn bind_for_test(
    pool: &MySqlPool,
    account_id: &[u8; ACCOUNT_ID_LEN],
    handle: &[u8; HANDLE_LEN],
) {
    assert_eq!(
        bind(
            pool,
            &TEST_PEPPER,
            account_id,
            handle,
            0,
            &ChangeTerms {
                grant: true,
                ..ChangeTerms::default()
            },
        )
        .await
        .expect("bind handle"),
        BindOutcome::Bound
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_pepper_parses_strictly() {
        assert_eq!(parse_hex32(&"ab".repeat(32)), Some([0xAB; 32]));
        assert_eq!(parse_hex32(&"AB".repeat(32)), Some([0xAB; 32]));
        assert_eq!(parse_hex32(&"ab".repeat(31)), None);
        assert_eq!(parse_hex32(&"zz".repeat(32)), None);
    }

    #[test]
    fn handle_decoding_is_strict() {
        let h = [7u8; HANDLE_LEN];
        assert_eq!(
            decode_handle(&URL_SAFE_NO_PAD.encode(h)).ok(),
            Some(h),
            "base64url, no padding"
        );
        assert!(decode_handle(&URL_SAFE_NO_PAD.encode([7u8; 16])).is_err());
        assert!(decode_handle("not base64!").is_err());
        let padded = base64::engine::general_purpose::URL_SAFE.encode(h);
        assert!(decode_handle(&padded).is_err());
    }

    #[test]
    fn stand_in_is_stable_per_handle_and_peppered() {
        let p1 = [1u8; 32];
        let p2 = [2u8; 32];
        let a = [0xA0u8; HANDLE_LEN];
        let b = [0xB0u8; HANDLE_LEN];
        let s = stand_in_account_id(&handle_key(&p1, &a));
        assert_eq!(s, stand_in_account_id(&handle_key(&p1, &a)));
        assert_ne!(s, stand_in_account_id(&handle_key(&p1, &b)));
        assert_ne!(s, stand_in_account_id(&handle_key(&p2, &a)));
        assert_ne!(handle_key(&p1, &a), handle_key(&p2, &a));
    }
}

/// DB-backed: binds of different accounts never deadlock each other. Runs
/// only with `LAKKO_AUTH_TEST_DATABASE_URL` set.
#[cfg(test)]
mod bind_concurrency_tests {
    use super::*;
    use sqlx::mysql::MySqlPoolOptions;
    use uuid::Uuid;

    /// Many accounts bind their first handle at once, the shape of a burst
    /// of signups, and then all change it at once. Each attempt runs
    /// [`bind_once`] directly, so a deadlock the retry in [`bind`] would
    /// hide fails the test. Under the schema of migration `0021` both
    /// rounds were rolled back with `1213` (see [`bind_once`]).
    /// ADR-0046 amendment: at the cap a FIRST bind is refused and nothing is
    /// written, while a change of an existing handle still goes through. The
    /// database is shared with other tests, so the cap is set to 1 after this
    /// test bound one handle of its own: the count is at least 1 whatever
    /// else runs.
    #[tokio::test]
    async fn the_account_cap_refuses_a_first_bind_and_never_a_change() {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping the_account_cap_refuses_a_first_bind_and_never_a_change: \
                 LAKKO_AUTH_TEST_DATABASE_URL unset"
            );
            return;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect");
        crate::MIGRATIONS.run(&pool).await.expect("migrate");
        let a: [u8; ACCOUNT_ID_LEN] = *Uuid::now_v7().as_bytes();
        let b: [u8; ACCOUNT_ID_LEN] = *Uuid::now_v7().as_bytes();
        for id in [a, b] {
            sqlx::query("INSERT INTO accounts (id) VALUES (?)")
                .bind(id.as_slice())
                .execute(&pool)
                .await
                .expect("insert account");
        }
        let capped = |max| ChangeTerms {
            grant: true,
            max_accounts: max,
            ..ChangeTerms::default()
        };
        let (h1, _) = random_handle();
        assert_eq!(
            bind(&pool, &TEST_PEPPER, &a, &h1, 0, &capped(0))
                .await
                .expect("bind a"),
            BindOutcome::Bound
        );
        let (h2, _) = random_handle();
        assert_eq!(
            bind(&pool, &TEST_PEPPER, &b, &h2, 0, &capped(1))
                .await
                .expect("bind b"),
            BindOutcome::RegistrationClosed
        );
        assert_eq!(current_key(&pool, &b).await.expect("read b"), None);
        let (h3, _) = random_handle();
        assert_eq!(
            bind(&pool, &TEST_PEPPER, &a, &h3, 0, &capped(1))
                .await
                .expect("change a"),
            BindOutcome::Bound
        );
        assert_eq!(
            bind(&pool, &TEST_PEPPER, &a, &h3, 0, &capped(1))
                .await
                .expect("rebind a"),
            BindOutcome::Unchanged
        );
        assert!(!crate::account_cap::is_open_for_test(&pool, 1).await);
        assert!(crate::account_cap::is_open_for_test(&pool, 0).await);
        assert_eq!(
            bind(&pool, &TEST_PEPPER, &b, &h2, 0, &capped(0))
                .await
                .expect("bind b uncapped"),
            BindOutcome::Bound
        );
        for id in [a, b] {
            sqlx::query("DELETE FROM accounts WHERE id = ?")
                .bind(id.as_slice())
                .execute(&pool)
                .await
                .expect("delete account");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn first_binds_of_different_accounts_do_not_deadlock() {
        const ACCOUNTS: usize = 24;
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping first_binds_of_different_accounts_do_not_deadlock: \
                 LAKKO_AUTH_TEST_DATABASE_URL unset"
            );
            return;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(12)
            .connect(&url)
            .await
            .expect("connect");
        crate::MIGRATIONS.run(&pool).await.expect("migrate");

        let mut ids = Vec::with_capacity(ACCOUNTS);
        for _ in 0..ACCOUNTS {
            let id: [u8; ACCOUNT_ID_LEN] = *Uuid::now_v7().as_bytes();
            sqlx::query("INSERT INTO accounts (id) VALUES (?)")
                .bind(id.as_slice())
                .execute(&pool)
                .await
                .expect("insert account");
            ids.push(id);
        }

        // Two rounds: a first bind for every account, then a change of
        // handle for every account (delete + insert).
        for round in 0..2 {
            let mut tasks = Vec::with_capacity(ACCOUNTS);
            for id in ids.clone() {
                let pool = pool.clone();
                tasks.push(tokio::spawn(async move {
                    let (handle, _) = random_handle();
                    let key = handle_key(&TEST_PEPPER, &handle);
                    let terms = ChangeTerms {
                        grant: true,
                        ..ChangeTerms::default()
                    };
                    match bind_once(&pool, &id, &key, 0, &terms).await {
                        Ok(outcome) => Ok(outcome),
                        Err(AttemptError::Deadlock) => Err("deadlock".to_string()),
                        Err(AttemptError::AccountRace) => Err("account race".to_string()),
                        Err(AttemptError::Db(e)) => Err(e.to_string()),
                    }
                }));
            }
            for task in tasks {
                assert_eq!(
                    task.await.expect("join"),
                    Ok(BindOutcome::Bound),
                    "round {round}"
                );
            }
        }

        for id in &ids {
            sqlx::query("DELETE FROM accounts WHERE id = ?")
                .bind(id.as_slice())
                .execute(&pool)
                .await
                .expect("delete account");
        }
    }

    #[test]
    fn a_deadlock_is_recognised_and_other_errors_are_not() {
        assert!(!is_deadlock(&sqlx::Error::RowNotFound));
        assert!(!is_deadlock(&sqlx::Error::Protocol("1213".into())));
    }
}
