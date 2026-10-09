// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Session issuance, refresh and per-device revocation ([ADR-0065] section
//! 3, [ADR-0066] sections 0.4 and 2.1).
//!
//! Every route that lands a session issues the same pair: a short-lived
//! **access token** (`v3`, carried as the bearer on every service) and a
//! **refresh credential** (`r2`, accepted by `POST /auth/session/refresh`
//! only), both minted at the account's current key generation
//! (`accounts.identity_version`), naming the device the client asserted
//! (`device_id_b64`, all zero for none yet) and stamped with the issue time.
//!
//! Two things revoke a credential before it expires:
//!
//! - **The key generation** (every device): a reset bumps it; from then on
//!   auth refuses every credential of a lower generation.
//! - **A device session floor** (one device): [`logout`] and the device
//!   removal (`crate::devices`) write `device_session_floors (account,
//!   device) -> not_before_ms`; a credential naming that device and issued at
//!   or before the floor is refused. A later sign-in on the same device is
//!   issued above it.
//!
//! Both are checked by `require_session` on every bearer request and by
//! [`refresh`] when a device tries to renew.
//!
//! The other verifiers (discovery, storage, gif-proxy) keep their stateless
//! verify, so their revocation latency is the access TTL
//! (`LAKKO_SESSION_TOKEN_TTL_SECS`, 15 minutes by default, chosen not
//! measured). A refresh credential lives `LAKKO_REFRESH_TOKEN_TTL_DAYS`
//! (default 30) and slides on every refresh.
//!
//! [ADR-0065]: ../../../docs/adr/0065-account-key-reset.md
//! [ADR-0066]: ../../../docs/adr/0066-account-takeover-resistance.md

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_auth_token::{VerifiedToken, DEVICE_ID_LEN, UNBOUND_DEVICE_ID};
use serde::{Deserialize, Serialize};
use sqlx::{MySql, MySqlPool};

use crate::webauthn::{ApiError, AuthenticatedAccount, AuthenticatedDevice};
use crate::AppState;

/// A device id: the device's conversation leaf signature key, the public id
/// of discovery's device directory.
pub(crate) type DeviceId = [u8; DEVICE_ID_LEN];

/// `device_session_floors.reason`: the device logged out.
pub(crate) const FLOOR_REASON_LOGOUT: u8 = 1;

/// `device_session_floors.reason`: the device was removed from the account.
pub(crate) const FLOOR_REASON_REMOVED: u8 = 2;

/// How often the floor pruner runs.
const FLOOR_PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

/// Decode the `device_id_b64` a session-landing route carries (URL-safe
/// base64, no padding, 32 bytes). Absent means "no device identity yet" and
/// yields [`UNBOUND_DEVICE_ID`]; the client then calls
/// `POST /auth/session/bind-device` once it has keys.
///
/// # Errors
///
/// [`ApiError::BadInput`] for a value that is not 32 bytes of URL-safe
/// base64. Every landing route decodes this before it consumes any
/// single-use state, so a malformed id costs the caller nothing.
pub(crate) fn decode_device_id(b64: Option<&str>) -> Result<DeviceId, ApiError> {
    let Some(b64) = b64 else {
        return Ok(UNBOUND_DEVICE_ID);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("device_id_b64 not URL-safe base64"))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::BadInput("device_id_b64 must decode to 32 bytes"))
}

/// What a device's session floor says about one credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FloorVerdict {
    /// No floor, or the credential was issued after it.
    Above,
    /// Issued at or before a floor the device's own logout wrote.
    LoggedOut,
    /// Issued at or before a floor a device removal wrote.
    Removed,
}

/// Compare a verified credential with its device's session floor (one
/// primary-key read). A credential that names no device has no floor.
///
/// The comparison is `issued_at_ms <= not_before_ms`: a credential issued in
/// the very millisecond of the logout or the removal is refused with the
/// rest, since nothing orders the two inside one millisecond.
///
/// # Errors
///
/// Any `sqlx::Error` from the read.
pub(crate) async fn floor_verdict<'e, E>(
    executor: E,
    credential: &VerifiedToken,
) -> Result<FloorVerdict, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    if !credential.is_device_bound() {
        return Ok(FloorVerdict::Above);
    }
    let row: Option<(i64, u8)> = sqlx::query_as(
        "SELECT not_before_ms, reason FROM device_session_floors \
         WHERE account_id = ? AND device_id = ?",
    )
    .bind(&credential.account_id[..])
    .bind(&credential.device_id[..])
    .fetch_optional(executor)
    .await?;
    Ok(match row {
        Some((not_before_ms, reason)) if credential.issued_at_ms <= not_before_ms => {
            if reason == FLOOR_REASON_REMOVED {
                FloorVerdict::Removed
            } else {
                FloorVerdict::LoggedOut
            }
        },
        _ => FloorVerdict::Above,
    })
}

/// Raise the session floor of `(account_id, device_id)` to `not_before_ms`.
/// The floor only moves up, and a removal is never softened into a logout:
/// the stored reason is the greater of the two.
///
/// # Errors
///
/// Any `sqlx::Error` from the upsert.
pub(crate) async fn raise_floor<'e, E>(
    executor: E,
    account_id: &[u8; 16],
    device_id: &DeviceId,
    not_before_ms: i64,
    reason: u8,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    sqlx::query(
        "INSERT INTO device_session_floors (account_id, device_id, not_before_ms, reason) \
         VALUES (?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE \
           not_before_ms = GREATEST(not_before_ms, VALUES(not_before_ms)), \
           reason = GREATEST(reason, VALUES(reason))",
    )
    .bind(&account_id[..])
    .bind(&device_id[..])
    .bind(not_before_ms)
    .bind(reason)
    .execute(executor)
    .await
    .map(|_| ())
}

/// Background pruner of `device_session_floors`: a row is dropped once its
/// floor is older than the refresh lifetime, because every credential it
/// could refuse has expired by then. Hourly.
pub(crate) fn spawn_floor_pruner(
    pool: MySqlPool,
    refresh_token_ttl_ms: i64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(FLOOR_PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let cutoff = now_ms().saturating_sub(refresh_token_ttl_ms);
            match sqlx::query("DELETE FROM device_session_floors WHERE not_before_ms < ?")
                .bind(cutoff)
                .execute(&pool)
                .await
            {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(
                        rows = r.rows_affected(),
                        "pruned expired device session floors"
                    );
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "device_session_floors prune failed"),
            }
        }
    })
}

/// The wire shape every session-landing route carries (flattened into its
/// own response), and the whole of the refresh route's answer.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionPair {
    /// The `v3` access token: the bearer for every service.
    pub(crate) session_token: String,
    /// Absolute unix-millisecond expiry of `session_token`, so the client
    /// refreshes ahead of it without parsing the token.
    pub(crate) session_expires_at_ms: i64,
    /// The `r2` refresh credential, accepted by `POST /auth/session/refresh`
    /// alone.
    pub(crate) refresh_token: String,
    /// Absolute unix-millisecond expiry of `refresh_token`.
    pub(crate) refresh_expires_at_ms: i64,
    /// The key generation both were issued under: the account's current
    /// one at issue time.
    pub(crate) key_generation: u64,
}

/// Current wall-clock time in unix milliseconds, saturating.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Read the account's current key generation, `None` when the account does
/// not exist.
///
/// # Errors
///
/// Any `sqlx::Error` from the read.
pub(crate) async fn key_generation_of<'e, E>(
    executor: E,
    account_id: &[u8; 16],
) -> Result<Option<u64>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    sqlx::query_scalar::<_, u64>("SELECT identity_version FROM accounts WHERE id = ?")
        .bind(&account_id[..])
        .fetch_optional(executor)
        .await
}

/// Issue the access + refresh pair for `account_id` at `key_generation`,
/// naming `device_id`, stamped now, with the configured TTLs from now. The
/// caller supplies the generation it
/// has already read (or just assigned) so this stays synchronous and usable
/// inside a transaction.
///
/// # Errors
///
/// [`ApiError::Internal`] when the token crate refuses (an account id of the
/// wrong length, which the `[u8; 16]` type rules out).
pub(crate) fn issue_pair_at(
    state: &AppState,
    account_id: &[u8; 16],
    key_generation: u64,
    device_id: &DeviceId,
) -> Result<SessionPair, ApiError> {
    let now = now_ms();
    let session_expires_at_ms = now.saturating_add(state.session_token_ttl_ms);
    let refresh_expires_at_ms = now.saturating_add(state.refresh_token_ttl_ms);
    let session_token = state
        .session_token_secret
        .issue(
            account_id,
            key_generation,
            device_id,
            now,
            session_expires_at_ms,
        )
        .map_err(|e| {
            tracing::error!(error = %e, "session-token issuance failed");
            ApiError::Internal
        })?;
    let refresh_token = state
        .session_token_secret
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
    Ok(SessionPair {
        session_token,
        session_expires_at_ms,
        refresh_token,
        refresh_expires_at_ms,
        key_generation,
    })
}

/// Issue the pair at the account's CURRENT generation (one indexed read),
/// naming `device_id`. The entry point for every login route. Issued now, so
/// above any floor the device has: whoever can sign in afresh is the owner as
/// far as the server can tell.
///
/// # Errors
///
/// [`ApiError::Db`] on the read; [`ApiError::InvalidCredential`] (the
/// enumeration-safe `401`) when the account no longer exists, which a login
/// that raced an account deletion can see.
pub(crate) async fn issue_pair(
    state: &AppState,
    account_id: &[u8; 16],
    device_id: &DeviceId,
) -> Result<SessionPair, ApiError> {
    let generation = key_generation_of(&state.db, account_id)
        .await
        .map_err(ApiError::Db)?
        .ok_or(ApiError::InvalidCredential)?;
    issue_pair_at(state, account_id, generation, device_id)
}

/// A pooled connection whose NEXT transaction runs at `READ COMMITTED`.
///
/// For the transactions that change an account's standing (the key reset,
/// the password reset, the recall, the account deletion). They read the
/// recall anchors of one account with locking reads and then insert or
/// delete there. At MariaDB's default `REPEATABLE READ` a locking read of a
/// range with no rows takes a gap lock, and two such transactions for
/// neighbouring account ids (UUIDv7 ids of accounts made in the same moment
/// are neighbours) each hold the gap and wait for the other's insert: a
/// deadlock, seen as a `500` in the first build of sprint T3. `READ
/// COMMITTED` takes no gap locks for searches. Nothing in those transactions
/// relies on a repeatable snapshot: what must not change under them is the
/// account row, and they hold its lock.
///
/// The caller begins its transaction on the returned connection at once
/// (`sqlx::Acquire::begin`); the level applies to that transaction only.
///
/// # Errors
///
/// Any `sqlx::Error` from acquiring the connection or setting the level.
pub(crate) async fn read_committed_conn(
    pool: &MySqlPool,
) -> Result<sqlx::pool::PoolConnection<MySql>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *conn)
        .await?;
    Ok(conn)
}

/// Begin a transaction that holds the account row's lock and has checked
/// that the account is still at `expected_generation`: the fence a bearer
/// route puts around a write that must not survive a key change (ADR-0066
/// "T3 as built").
///
/// `require_session` compares the generation before the handler runs; a key
/// reset or a recall can commit between that read and the handler's write.
/// For most writes that is harmless (the reset deletes what matters and the
/// session is dead on its next call). For a write that would give the
/// displaced party a way back in (a passkey, a device grant, a floor on
/// another device) it is not, so those writes run inside this transaction:
/// the reset and the recall take the same row lock first, so either they
/// commit before it (and the comparison here refuses) or after it (and they
/// delete what it wrote).
///
/// # Errors
///
/// [`ApiError::KeyGenerationSuperseded`] when the account has moved past
/// `expected_generation`; [`ApiError::InvalidCredential`] when it does not
/// exist or is somehow below it; [`ApiError::Db`].
pub(crate) async fn begin_at_generation(
    pool: &MySqlPool,
    account_id: &[u8; 16],
    expected_generation: u64,
) -> Result<sqlx::Transaction<'static, MySql>, ApiError> {
    let mut tx = pool.begin().await.map_err(ApiError::Db)?;
    let current: Option<u64> =
        sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ? FOR UPDATE")
            .bind(&account_id[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    match current {
        Some(current) if current == expected_generation => Ok(tx),
        Some(current) if current > expected_generation => Err(ApiError::KeyGenerationSuperseded),
        _ => Err(ApiError::InvalidCredential),
    }
}

/// Request header that carries a fresh-auth grant on a route with no JSON
/// body (`DELETE /account`). The value is the grant string verbatim. Read
/// only for an account without a passkey: an account with one is deleted on
/// its passkey action grant and this header is ignored.
pub(crate) const FRESH_GRANT_HEADER: &str = "x-lakko-fresh-grant";

/// A fresh-auth grant as it rides on the wire, flattened into the answer of
/// `POST /login/finish` (the password), `POST /webauthn/auth/finish` (a
/// passkey) and `POST /signup/finish` (the password was set in that very
/// exchange) and of no other route (ADR-0065, amended 2026-10-04 and
/// 2026-10-06).
///
/// Deliberately NOT part of [`SessionPair`]: every session-landing route
/// shares that type and [`issue_pair`], and a grant must be reachable only
/// from an exchange in which the person proved who they are with the
/// password or a passkey. Recovery login, TOTP recovery, the device-authorize
/// redeem, password reset, refresh and the key reset's own answer never
/// construct this type.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct FreshGrant {
    /// The `k1` fresh-auth grant: opaque to the client, sent back as
    /// `fresh_grant` in the JSON body of the key-reset and add-device routes
    /// and as the [`FRESH_GRANT_HEADER`] header of `DELETE /account`.
    pub(crate) fresh_grant: String,
    /// Absolute unix-millisecond expiry of `fresh_grant`, so the client can
    /// ask for the password or the passkey again ahead of a `403`.
    pub(crate) fresh_grant_expires_at_ms: i64,
}

/// Mint a fresh-auth grant for `account_id` at `key_generation` with the
/// configured TTL from now. The ONLY callers are the password login's finish
/// handler, after `ServerLogin::finish` accepted the password proof, the
/// passkey login's finish handler, after the assertion verified, and the
/// signup's finish handler, after the account and its password envelope
/// committed.
///
/// # Errors
///
/// [`ApiError::Internal`] when the token crate refuses.
pub(crate) fn issue_fresh_grant(
    state: &AppState,
    account_id: &[u8; 16],
    key_generation: u64,
) -> Result<FreshGrant, ApiError> {
    let fresh_grant_expires_at_ms = now_ms().saturating_add(state.fresh_grant_ttl_ms);
    let fresh_grant = state
        .session_token_secret
        .issue_fresh_grant(account_id, key_generation, fresh_grant_expires_at_ms)
        .map_err(|e| {
            tracing::error!(error = %e, "fresh-auth grant issuance failed");
            ApiError::Internal
        })?;
    Ok(FreshGrant {
        fresh_grant,
        fresh_grant_expires_at_ms,
    })
}

/// Check the fresh-auth grant a request carries: present, well-formed,
/// tagged by us, unexpired, issued for `account_id` (the bearer's), and
/// issued at the account's CURRENT key generation (equal: a grant from
/// before a reset, or one somehow above the row, is refused). Returns that
/// generation, which the key reset's transaction compares again under the
/// row lock.
///
/// The one gate of the four actions that need the person to have proven
/// who they are within the grant's lifetime: reset the account keys, delete
/// the account (when it has no passkey; with one, the ADR-0014 action grant
/// is the proof and this gate is not consulted), add a device, add a passkey.
/// Only the reset spends a grant (it bumps the generation the grant names);
/// the others leave it valid until it expires.
///
/// Stateless but for one indexed read; charges no limiter and changes
/// nothing, so a stolen session without the password or a passkey can call
/// the gated routes as often as it likes and spends none of the owner's
/// budgets.
///
/// # Errors
///
/// [`ApiError::FreshAuthRequired`] for every refusal (one code, so the
/// caller learns nothing about which check failed); [`ApiError::Db`] on the
/// read.
pub(crate) async fn require_fresh_grant(
    state: &AppState,
    account_id: &[u8; 16],
    grant: Option<&str>,
) -> Result<u64, ApiError> {
    let grant = grant.ok_or(ApiError::FreshAuthRequired)?;
    let verified = state
        .session_token_secret
        .verify_fresh_grant(grant, now_ms())
        .map_err(|e| {
            tracing::debug!(error = %e, "fresh-auth grant refused");
            ApiError::FreshAuthRequired
        })?;
    if &verified.account_id != account_id {
        tracing::warn!("fresh-auth grant of another account presented");
        return Err(ApiError::FreshAuthRequired);
    }
    let current = key_generation_of(&state.db, account_id)
        .await
        .map_err(ApiError::Db)?
        .ok_or(ApiError::FreshAuthRequired)?;
    if verified.key_generation != current {
        return Err(ApiError::FreshAuthRequired);
    }
    Ok(current)
}

/// The fresh-auth grant a request carries in [`FRESH_GRANT_HEADER`], if any.
/// A header that is not visible ASCII reads as absent, which the gate
/// answers like every other unusable grant.
pub(crate) fn fresh_grant_header(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers.get(FRESH_GRANT_HEADER)?.to_str().ok()
}

/// Body of `POST /auth/session/refresh`.
#[derive(Deserialize)]
pub(crate) struct RefreshReq {
    /// The `r2` refresh credential a login or an earlier refresh issued.
    pub(crate) refresh_token: String,
    /// The upgrade of a kept device after a key rotation ([ADR-0066] section
    /// 3.3): URL-safe base64 of a 138-byte account statement, kind
    /// `session-upgrade`, body `BLAKE3(refresh_token)`, signed by the
    /// account's CURRENT identity key. Read only when the credential is of a
    /// lower key generation than the account's; ignored otherwise.
    ///
    /// [ADR-0066]: ../../../docs/adr/0066-account-takeover-resistance.md
    #[serde(default)]
    pub(crate) key_statement_b64: Option<String>,
}

/// What a `session-upgrade` statement proves about the device that sent it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpgradeVerdict {
    /// Signed by the account's current identity key, inside the window, over
    /// this very refresh credential.
    Holds,
    /// Signed by the current identity key over this credential, but with a
    /// time outside the window: the device's clock is wrong.
    Stale,
    /// Anything else: no statement, no recorded key, another key, another
    /// credential, a malformed statement.
    Refused,
}

/// Judge the `key_statement_b64` of a refresh whose credential is of an
/// older key generation, against the identity key the account has NOW.
fn upgrade_verdict(
    statement_b64: Option<&str>,
    identity_pubkey: Option<&[u8]>,
    account_id: &[u8; 16],
    refresh_token: &str,
    now: i64,
) -> UpgradeVerdict {
    let (Some(statement_b64), Some(identity_pubkey)) = (statement_b64, identity_pubkey) else {
        return UpgradeVerdict::Refused;
    };
    let Ok(statement) = URL_SAFE_NO_PAD.decode(statement_b64) else {
        return UpgradeVerdict::Refused;
    };
    let Ok(key) = lakko_crypto::signing::VerifyingKey::from_bytes(identity_pubkey) else {
        return UpgradeVerdict::Refused;
    };
    let body = lakko_attest::session_upgrade_body(refresh_token);
    let kind = lakko_attest::AccountStatementKind::SessionUpgrade;
    match lakko_attest::verify_account_statement(&statement, &key, kind, account_id, &body, now) {
        Ok(_) => UpgradeVerdict::Holds,
        Err(lakko_attest::AccountStatementError::OutsideWindow)
            if lakko_attest::account_statement_is_stale(
                &statement, &key, kind, account_id, &body, now,
            ) =>
        {
            UpgradeVerdict::Stale
        },
        Err(e) => {
            tracing::debug!(error = %e, "session upgrade: account statement refused");
            UpgradeVerdict::Refused
        },
    }
}

/// `POST /auth/session/refresh` — anonymous; the refresh credential is the
/// authorisation. Per-address limited (`Category::Attempt`).
///
/// Verifies the credential (version, tag, expiry), then, in this order (the
/// generation first since sprint T3, for the reason
/// [`crate::webauthn::require_session`] gives: a superseded device must be
/// told it was superseded, never that it was removed, or whoever reset the
/// keys could make the owner's devices destroy their recall key):
///
/// 1. the account's current key generation: no such account -> `401
///    refresh-invalid`, the same body a bad tag or an expired credential
///    gets, so an attacker cannot tell a deleted account from a wrong
///    credential; lower -> see "the upgrade" below;
/// 2. the device's session floor: issued at or before a removal ->
///    `401 device-revoked`; at or before a logout -> `401 refresh-invalid`;
///    otherwise a fresh pair naming the same device (the refresh credential
///    slides to a fresh 30 days).
///
/// # The upgrade (ADR-0066 section 3.3)
///
/// A credential of a LOWER generation is normally answered `401
/// key-generation-superseded`: the account's keys changed and this device's
/// are not the account's any more. After a key **rotation**, though, the
/// account's other devices are handed the new seed inside their DeviceGroup
/// and must not be signed out. Such a device sends `key_statement_b64`: an
/// account statement of kind `session-upgrade` over `BLAKE3(refresh_token)`,
/// signed with the account's NEW identity key. When it verifies under the
/// key auth has recorded for the account, the credential is treated as one
/// of the current generation: the floor of step 2 applies (a device removed
/// in the rotation is `device-revoked`; a logged-out one `refresh-invalid`),
/// and otherwise a pair at the current generation is issued, naming the same
/// device.
///
/// What this cannot be used for: a device that does not hold the current
/// identity key cannot sign the statement. That is every device after a
/// reset or a recall (the new key is random to them), and the stolen device
/// after a rotation (it was removed from the DeviceGroup before the seed was
/// sent there). The statement covers the refresh credential, so it upgrades
/// that credential and no other, and the credential must itself be genuine
/// and unexpired. A statement that verifies but carries a time outside the
/// two-minute window is answered `401 {"error":"statement-stale",
/// "server_time_ms"}` (the device's clock is wrong; it signs again with the
/// server's time) and only then, so the answer goes to a holder of the key
/// and to nobody else. Everything else about a lower generation is
/// `key-generation-superseded`, exactly as without the field.
///
/// # Errors
///
/// [`ApiError::RefreshInvalid`], [`ApiError::DeviceRevoked`],
/// [`ApiError::KeyGenerationSuperseded`], [`ApiError::StatementStale`],
/// [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshReq>,
) -> Result<Json<SessionPair>, ApiError> {
    let now = now_ms();
    let verified = state
        .session_token_secret
        .verify_refresh(&req.refresh_token, now)
        .map_err(|e| {
            tracing::debug!(error = %e, "session refresh: credential refused");
            ApiError::RefreshInvalid
        })?;
    // The generation and the identity key in one read, so the statement is
    // judged against the key of the very generation it is compared with.
    let row: Option<(u64, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT identity_version, identity_pubkey FROM accounts WHERE id = ?")
            .bind(&verified.account_id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::Db)?;
    let (current, identity_pubkey) = row.ok_or(ApiError::RefreshInvalid)?;
    if verified.key_generation > current {
        // Unreachable for an honestly issued credential (the generation only
        // grows); refuse rather than issue a pair above the row.
        tracing::error!(
            token_generation = verified.key_generation,
            current,
            "session refresh: credential generation above the account's"
        );
        return Err(ApiError::RefreshInvalid);
    }
    if verified.key_generation < current {
        match upgrade_verdict(
            req.key_statement_b64.as_deref(),
            identity_pubkey.as_deref(),
            &verified.account_id,
            &req.refresh_token,
            now,
        ) {
            UpgradeVerdict::Holds => {},
            UpgradeVerdict::Stale => {
                return Err(ApiError::StatementStale {
                    server_time_ms: now,
                })
            },
            UpgradeVerdict::Refused => return Err(ApiError::KeyGenerationSuperseded),
        }
    }
    match floor_verdict(&state.db, &verified)
        .await
        .map_err(ApiError::Db)?
    {
        FloorVerdict::Above => {},
        FloorVerdict::LoggedOut => return Err(ApiError::RefreshInvalid),
        FloorVerdict::Removed => return Err(ApiError::DeviceRevoked),
    }
    if verified.key_generation < current {
        tracing::info!(
            from = verified.key_generation,
            to = current,
            "session upgraded by a device holding the rotated identity key",
        );
    }
    Ok(Json(issue_pair_at(
        &state,
        &verified.account_id,
        current,
        &verified.device_id,
    )?))
}

/// `POST /auth/session/logout` — bearer; no body, no grant ([ADR-0066]
/// section 2.1). Raises the session floor of the token's own `(account,
/// device)` to now, so the access token and every refresh credential this
/// device holds stop working at auth at once; the other devices of the
/// account are untouched. Answers `204`.
///
/// A session that names no device has nothing to name a floor by: the call
/// is a `204` that changes nothing, and that session ends when its
/// credentials expire or the generation moves. (An honest client binds its
/// device before it can do anything worth revoking.)
///
/// # Errors
///
/// [`ApiError::Db`].
///
/// [ADR-0066]: ../../../docs/adr/0066-account-takeover-resistance.md
#[axum::debug_handler]
pub(crate) async fn logout(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Extension(device): Extension<AuthenticatedDevice>,
) -> Result<StatusCode, ApiError> {
    if device.device_id != UNBOUND_DEVICE_ID {
        raise_floor(
            &state.db,
            &auth.0,
            &device.device_id,
            now_ms(),
            FLOOR_REASON_LOGOUT,
        )
        .await
        .map_err(ApiError::Db)?;
        tracing::info!("device session logged out");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Body of `POST /auth/session/bind-device`.
#[derive(Deserialize)]
pub(crate) struct BindDeviceReq {
    /// URL-safe base64 of the 32-byte device id to bind: the device's
    /// conversation leaf signature key. Not all zero.
    pub(crate) device_id_b64: String,
    /// The session's `r2` refresh credential. The route issues a new refresh
    /// credential, so it asks for the one being replaced: a bearer alone (15
    /// minutes) must not be exchangeable for 30 days.
    pub(crate) refresh_token: String,
}

/// `POST /auth/session/bind-device` — bearer ([ADR-0066] section 0.4). A
/// session that landed before the device had keys names no device; once the
/// device has its conversation leaf key it calls this once and uses the pair
/// it returns, which names the device. The unbound pair it replaces is not
/// revoked (it names nothing a floor could be keyed on); the client drops it.
///
/// Checks, in order: the body's device id is 32 bytes and not all zero
/// (`400`); the refresh credential verifies, is of the bearer's account and
/// of the account's current generation (`401 refresh-invalid`, or `401
/// key-generation-superseded` for a lower generation); then by the device the
/// refresh credential names:
///
/// - none: issue a pair naming the requested device;
/// - the requested device: issue a fresh pair for it (a retry after a lost
///   answer), subject to that device's floor;
/// - another device: `409 device-already-bound`. A session is bound once; a
///   device whose key changed signs in again.
///
/// # Errors
///
/// [`ApiError::BadInput`], [`ApiError::RefreshInvalid`],
/// [`ApiError::KeyGenerationSuperseded`], [`ApiError::DeviceRevoked`],
/// [`ApiError::DeviceAlreadyBound`], [`ApiError::Db`].
///
/// [ADR-0066]: ../../../docs/adr/0066-account-takeover-resistance.md
#[axum::debug_handler]
pub(crate) async fn bind_device(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Extension(bearer): Extension<AuthenticatedDevice>,
    Json(req): Json<BindDeviceReq>,
) -> Result<Json<SessionPair>, ApiError> {
    let device_id = decode_device_id(Some(&req.device_id_b64))?;
    if device_id == UNBOUND_DEVICE_ID {
        return Err(ApiError::BadInput("device_id_b64 must not be all zero"));
    }
    let refresh = state
        .session_token_secret
        .verify_refresh(&req.refresh_token, now_ms())
        .map_err(|e| {
            tracing::debug!(error = %e, "bind-device: refresh credential refused");
            ApiError::RefreshInvalid
        })?;
    if refresh.account_id != auth.0 {
        tracing::warn!("bind-device: refresh credential of another account presented");
        return Err(ApiError::RefreshInvalid);
    }
    if refresh.key_generation < bearer.key_generation {
        return Err(ApiError::KeyGenerationSuperseded);
    }
    if refresh.key_generation > bearer.key_generation {
        return Err(ApiError::RefreshInvalid);
    }
    // The bearer and the refresh credential must be of one session: both
    // unbound, or both already naming the requested device.
    if bearer.device_id != refresh.device_id {
        return Err(ApiError::DeviceAlreadyBound);
    }
    if refresh.device_id == device_id {
        match floor_verdict(&state.db, &refresh)
            .await
            .map_err(ApiError::Db)?
        {
            FloorVerdict::Above => {},
            FloorVerdict::LoggedOut => return Err(ApiError::RefreshInvalid),
            FloorVerdict::Removed => return Err(ApiError::DeviceRevoked),
        }
    } else if refresh.device_id != UNBOUND_DEVICE_ID {
        return Err(ApiError::DeviceAlreadyBound);
    }
    Ok(Json(issue_pair_at(
        &state,
        &auth.0,
        bearer.key_generation,
        &device_id,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session pair every landing route shares never carries a fresh-auth
    /// grant: the grant is a separate type only the password login, the
    /// passkey login and the signup finish build. This pins the wire keys, so a grant field added
    /// to the shared type (and so to recovery login, TOTP recovery, the
    /// device redeem, refresh) fails here. ADR-0066 added no key: the device
    /// rides inside the two credentials.
    #[test]
    fn the_shared_session_pair_has_no_fresh_grant_field() {
        let pair = SessionPair {
            session_token: "a".into(),
            session_expires_at_ms: 1,
            refresh_token: "b".into(),
            refresh_expires_at_ms: 2,
            key_generation: 3,
        };
        let value = serde_json::to_value(&pair).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "key_generation",
                "refresh_expires_at_ms",
                "refresh_token",
                "session_expires_at_ms",
                "session_token",
            ]
        );
    }
}

#[cfg(test)]
mod device_id_tests {
    use super::*;

    #[test]
    fn an_absent_device_id_is_the_unbound_one_and_a_bad_one_is_refused() {
        assert_eq!(decode_device_id(None).unwrap(), UNBOUND_DEVICE_ID);
        let id = [0xD1u8; 32];
        assert_eq!(
            decode_device_id(Some(&URL_SAFE_NO_PAD.encode(id))).unwrap(),
            id
        );
        for bad in [
            String::new(),
            "!!!".to_string(),
            URL_SAFE_NO_PAD.encode([1u8; 31]),
            URL_SAFE_NO_PAD.encode([1u8; 33]),
            // Standard-alphabet padding is not the wire form.
            format!("{}=", URL_SAFE_NO_PAD.encode(id)),
        ] {
            assert!(
                matches!(decode_device_id(Some(&bad)), Err(ApiError::BadInput(_))),
                "{bad:?}"
            );
        }
    }
}
