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
//! against the account id (`login_handles`, migration `0021`), so a database
//! leak without the pepper is not a dictionary over usernames.
//!
//! - **`PUT /auth/login-handle`** — bearer. Binds the caller's account to a
//!   handle, replacing its previous one (one per account). A handle another
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

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
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
    Ok(match bound {
        Some(account_id) => Resolved {
            account_id,
            bound: true,
        },
        None => Resolved {
            account_id: stand_in_account_id(&key),
            bound: false,
        },
    })
}

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
}

/// Attempts at a bind that lost to a concurrent bind of the same account.
/// The account-row lock below serialises them, so a retry is not expected;
/// it only keeps a lost race from surfacing as a spurious `409`.
const BIND_ATTEMPTS: usize = 3;

/// Why one bind attempt did not finish.
enum AttemptError {
    /// A unique violation on the account key, not the handle: a concurrent
    /// bind of the same account got in between. Retried.
    AccountRace,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for AttemptError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
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
) -> Result<BindOutcome, sqlx::Error> {
    let key = handle_key(pepper, handle);
    for _ in 0..BIND_ATTEMPTS {
        match bind_once(pool, account_id, &key, min_interval_secs).await {
            Ok(outcome) => return Ok(outcome),
            Err(AttemptError::AccountRace) => {
                tracing::debug!("login handle bind raced a bind of the same account; retrying");
            },
            Err(AttemptError::Db(e)) => return Err(e),
        }
    }
    Err(sqlx::Error::Protocol(
        "login handle bind kept racing a bind of the same account".to_string(),
    ))
}

async fn bind_once(
    pool: &MySqlPool,
    account_id: &[u8; ACCOUNT_ID_LEN],
    key: &[u8; 32],
    min_interval_secs: u32,
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
    sqlx::query("DELETE FROM login_handles WHERE account_id = ?")
        .bind(account_id.as_slice())
        .execute(&mut *tx)
        .await?;
    let inserted = sqlx::query("INSERT INTO login_handles (handle_key, account_id) VALUES (?, ?)")
        .bind(key.as_slice())
        .bind(account_id.as_slice())
        .execute(&mut *tx)
        .await;
    match inserted {
        Ok(_) => {},
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            // Rolled back on drop: the caller keeps its previous handle.
            return if e.message().contains("uq_login_handles_account") {
                Err(AttemptError::AccountRace)
            } else {
                Ok(BindOutcome::Taken)
            };
        },
        Err(e) => return Err(e.into()),
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
    Json(req): Json<PutReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pepper = pepper(&state)?;
    let handle = decode_handle(&req.handle)?;
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        crate::limits::Scope::HandleBind,
        &auth.0,
    )
    .await?;
    let outcome = bind(
        &state.db,
        &pepper,
        &auth.0,
        &handle,
        state.limits.handle_change_min_interval_secs(),
    )
    .await
    .map_err(ApiError::Db)?;
    match outcome {
        BindOutcome::Bound | BindOutcome::Unchanged => {},
        BindOutcome::Taken => return Err(ApiError::HandleTaken),
        BindOutcome::TooSoon { retry_after_secs } => {
            return Err(ApiError::Limit(crate::limits::LimitError::Limited {
                code: crate::limits::CODE_HANDLE_BIND_LIMITED,
                retry_after_secs,
            }));
        },
    }
    tracing::info!(account_id = %URL_SAFE_NO_PAD.encode(auth.0), "login handle bound");
    Ok(Json(serde_json::json!({ "bound": true })))
}

/// `DELETE /auth/login-handle` — remove the caller's handle.
///
/// The removed handle becomes the one a later bind may restore within the
/// minimum interval; the time of the last change is left as it was, so
/// releasing a handle does not reopen the interval.
#[axum::debug_handler]
pub(crate) async fn delete(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<serde_json::Value>, ApiError> {
    pepper(&state)?;
    let removed = remove(&state.db, &auth.0).await.map_err(ApiError::Db)?;
    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(auth.0),
        removed,
        "login handle removed"
    );
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
        bind(pool, &TEST_PEPPER, account_id, handle, 0)
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
