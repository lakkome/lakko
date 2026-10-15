// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Username registry — the table mapping `OPRF(K, normalize(@username))`
//! to `account_id`. Plaintext usernames are never stored.
//!
//! ## Authorisation
//!
//! `/discovery/register` is session-token-gated (Phase 1.5b) and the
//! body's `account_id` must equal the token's; it is counted per account
//! by the write limiter and, since ADR-0054 S3, carries no proof of work.
//! `/discovery/lookup` is anonymous and rate-limited per source address, and
//! since ADR-0060 answers only for a person who chose Everyone;
//! the enumeration step it follows (`/discovery/oprf`) spends an
//! anonymous token.
//!
//! ## One current name per account, and renames (ADR-0076)
//!
//! A row of `usernames` is **current** (`retired_at IS NULL`) or **retired**.
//!
//!   * An account holds at most one current name (a unique key on the
//!     generated `current_account` column, migration `0028`). `register`
//!     binds the first; a second `register` is `409 username-already-set`.
//!   * `POST /discovery/username/replace` renames: the current row is
//!     retired and the new hash becomes current, in one transaction under a
//!     lock on the account's current row. At most one rename per
//!     [`NamePolicy::rename_interval_secs`] (30 days by default; `429
//!     rename-too-soon`).
//!   * A retired row answers no lookup and no name search, its `is_public`
//!     is cleared, and it still occupies its hash: nobody else can register
//!     or rename to it (`409 username-taken`) until the hourly pruner deletes
//!     it, [`NamePolicy::retired_hold_secs`] after it was retired (30 days by
//!     default). Freed at once, a stranger could take the name the same hour
//!     and be mistaken for the person. The account that retired it may take
//!     it back with a rename.
//!   * First-come-first-served: a hash that is present is `409
//!     username-taken`, whoever holds it.
//!   * A rename takes the account out of Everyone (its public record and
//!     name terms are keyed over the old name): the client publishes again.
//!
//! Both intervals are **chosen, not measured**.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;

use crate::AppState;

/// Default minimum interval between two renames of one account, and default
/// time a retired name stays unclaimable: 30 days (ADR-0076 D1, D2). Chosen,
/// not measured.
pub(crate) const DEFAULT_NAME_INTERVAL_SECS: u64 = 30 * 86_400;

/// Env knob: minimum seconds between two renames of one account (0 = no
/// minimum).
pub(crate) const RENAME_INTERVAL_VAR: &str = "LAKKO_DISCOVERY_RENAME_INTERVAL_SECS";

/// Env knob: seconds a retired name stays held before the pruner frees it.
pub(crate) const RETIRED_HOLD_VAR: &str = "LAKKO_DISCOVERY_RETIRED_NAME_HOLD_SECS";

/// The unique key that makes "one current name per account" structural
/// (migration `0028`), as the database names it in a duplicate-key error.
const CURRENT_ACCOUNT_KEY: &str = "uq_usernames_current_account";

/// How often retired names past their hold are deleted.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Body codes of this module's refusals (`{"error": <code>}`).
const CODE_USERNAME_TAKEN: &str = "username-taken";
const CODE_USERNAME_ALREADY_SET: &str = "username-already-set";
const CODE_NO_USERNAME: &str = "no-username";
const CODE_RENAME_TOO_SOON: &str = "rename-too-soon";

/// The rename policy (ADR-0076 D1, D2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NamePolicy {
    /// Minimum seconds between two renames of one account (0 = none).
    pub(crate) rename_interval_secs: u64,
    /// Seconds a retired name stays unclaimable by others.
    pub(crate) retired_hold_secs: u64,
}

impl NamePolicy {
    /// Both intervals at their 30-day default.
    pub(crate) const fn defaults() -> Self {
        Self {
            rename_interval_secs: DEFAULT_NAME_INTERVAL_SECS,
            retired_hold_secs: DEFAULT_NAME_INTERVAL_SECS,
        }
    }

    /// Read both knobs; an unset one takes its default.
    ///
    /// # Errors
    ///
    /// A knob that is set and is not an unsigned integer of seconds.
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        fn read(var: &str, default: u64) -> anyhow::Result<u64> {
            match std::env::var(var) {
                Ok(v) if !v.trim().is_empty() => v
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| anyhow::anyhow!("{var} must be an unsigned integer of seconds")),
                _ => Ok(default),
            }
        }
        let defaults = Self::defaults();
        Ok(Self {
            rename_interval_secs: read(RENAME_INTERVAL_VAR, defaults.rename_interval_secs)?,
            retired_hold_secs: read(RETIRED_HOLD_VAR, defaults.retired_hold_secs)?,
        })
    }
}

/// A refusal that is a status and one JSON body code.
fn coded(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

/// A plain-text refusal (malformed input, the session's account mismatch).
fn plain((status, text): (StatusCode, String)) -> Response {
    (status, text).into_response()
}

fn internal(what: &'static str) -> impl FnOnce(sqlx::Error) -> Response {
    move |e| {
        tracing::error!(error = %e, what, "usernames db error");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
            .into_response()
    }
}

/// Decode the two fields both name routes carry and hold the body's account
/// to the session's.
#[allow(clippy::result_large_err)] // the error is the handler's own response, returned as is
fn decode_binding(
    payload: &RegisterRequest,
    authed: &crate::auth::AuthenticatedAccount,
) -> Result<([u8; ACCOUNT_ID_LEN], [u8; OPRF_HASH_LEN]), Response> {
    let account_id =
        decode_fixed::<ACCOUNT_ID_LEN>(&payload.account_id, "account_id").map_err(plain)?;
    let oprf_hash =
        decode_fixed::<OPRF_HASH_LEN>(&payload.oprf_hash, "oprf_hash").map_err(plain)?;
    // Anti-impersonation: the body's account_id must match the token's.
    // Otherwise an authenticated user could bind a name under someone
    // else's account_id (useless to the attacker, but it would burn the
    // name for the victim).
    if account_id != authed.0 {
        tracing::warn!(
            body_account_id = %B64.encode(account_id),
            token_account_id = %B64.encode(authed.0),
            "username binding: body account_id does not match token's"
        );
        return Err(plain((
            StatusCode::FORBIDDEN,
            "account_id mismatch between body and session token".to_string(),
        )));
    }
    Ok((account_id, oprf_hash))
}

/// VOPRF Ristretto255-SHA512 output is 64 bytes (one SHA-512 digest).
const OPRF_HASH_LEN: usize = 64;
/// `account_id` length in bytes (UUIDv7 raw, same as `services/auth`).
const ACCOUNT_ID_LEN: usize = 16;

/// Decode a fixed-length base64 input.
fn decode_fixed<const N: usize>(
    b64: &str,
    name: &'static str,
) -> Result<[u8; N], (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid {name} base64: {e}"),
        )
    })?;
    if bytes.len() != N {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{name} must be {N} bytes after base64 decode"),
        ));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

// ============================================================================
// /discovery/register
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct RegisterRequest {
    /// Base64-encoded UUIDv7 raw bytes (16 bytes).
    pub(crate) account_id: String,
    /// Base64-encoded 64-byte lookup key: the client finalises a
    /// `/discovery/oprf` round trip and derives this from the OPRF output
    /// (`lakko_discovery_client::discovery_lookup_key`). The output itself
    /// never reaches the server (ADR-0060 section 5).
    pub(crate) oprf_hash: String,
}

#[derive(Serialize)]
pub(crate) struct RegisterResponse {
    pub(crate) ok: bool,
    /// ADR-0060: standard base64 of the 153-byte username certificate
    /// (`lakko_attest::username_cert`) binding this OPRF hash to the account.
    pub(crate) username_certificate_b64: String,
}

/// `POST /discovery/register` — bind an account's FIRST username: an
/// OPRF-hashed name to the session's `account_id`.
///
/// `409 {"error":"username-taken"}` when the hash is present, current or
/// retired, whoever holds it; `409 {"error":"username-already-set"}` when the
/// account already holds a current name (a rename is
/// `POST /discovery/username/replace`). Both are decided by unique keys, so
/// two concurrent registers of one account bind one name.
pub(crate) async fn register(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, Response> {
    let (account_id, oprf_hash) = decode_binding(&payload, &authed)?;

    let result = sqlx::query("INSERT INTO usernames (oprf_hash, account_id) VALUES (?, ?)")
        .bind(&oprf_hash[..])
        .bind(&account_id[..])
        .execute(&state.db)
        .await;

    match result {
        Ok(_) => {
            tracing::info!(
                account_id = %B64.encode(account_id),
                "username registered",
            );
            Ok(Json(RegisterResponse {
                ok: true,
                username_certificate_b64: crate::visibility::sign_certificate(
                    &state.username_cert_key,
                    oprf_hash,
                    account_id,
                ),
            }))
        },
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => Err(coded(
            StatusCode::CONFLICT,
            if db_err.message().contains(CURRENT_ACCOUNT_KEY) {
                CODE_USERNAME_ALREADY_SET
            } else {
                CODE_USERNAME_TAKEN
            },
        )),
        Err(e) => Err(internal("insert usernames")(e)),
    }
}

// ============================================================================
// /discovery/username/replace
// ============================================================================

/// `POST /discovery/username/replace` — rename (ADR-0076 section 1). The body
/// and the answer are `register`'s: `{account_id, oprf_hash}` ->
/// `{ok, username_certificate_b64}` for the NEW name.
///
/// One transaction under a lock on the account's current row (so two renames
/// of one account run one after the other), in this order:
///
/// 1. the account holds a current name, else `409 no-username`;
/// 2. its last rename was at least the rename interval ago, else `429
///    {"error":"rename-too-soon","retry_after_secs":N}` (+ `Retry-After`);
/// 3. the new hash is free, or is this account's own RETIRED row; a hash
///    that is present otherwise (another account's, current or retired, or
///    this account's current one) is `409 username-taken`;
/// 4. the current row is retired (`retired_at` now, `is_public` cleared);
/// 5. the new hash becomes current: inserted, or the own retired row revived;
/// 6. the account leaves Everyone (its name terms and public record were
///    keyed over the old name); the client publishes again;
/// 7. the time of the rename is recorded.
#[allow(clippy::too_many_lines)] // one transaction, its steps numbered as in the doc above
pub(crate) async fn replace(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, Response> {
    let (account_id, new_hash) = decode_binding(&payload, &authed)?;
    let account = &account_id[..];

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(internal("acquire rename connection"))?;
    // READ COMMITTED for this transaction only, as `PUT /discovery/visibility`
    // does: no gap locks on the `account_id` indexes, which deadlocked two
    // accounts writing at once under REPEATABLE READ.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *conn)
        .await
        .map_err(internal("set rename isolation"))?;
    let mut tx = sqlx::Connection::begin(&mut *conn)
        .await
        .map_err(internal("begin rename"))?;

    // 1. The current row, locked.
    let current: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT oprf_hash FROM usernames          WHERE account_id = ? AND retired_at IS NULL FOR UPDATE",
    )
    .bind(account)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal("lock current username"))?;
    let Some((old_hash,)) = current else {
        return Err(coded(StatusCode::CONFLICT, CODE_NO_USERNAME));
    };

    // 2. The interval since the last rename.
    let interval = i64::try_from(state.name_policy.rename_interval_secs).unwrap_or(i64::MAX);
    let since: Option<(i64,)> = sqlx::query_as(
        "SELECT CAST(TIMESTAMPDIFF(SECOND, last_rename_at, NOW(3)) AS SIGNED)          FROM username_renames WHERE account_id = ? FOR UPDATE",
    )
    .bind(account)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal("read last rename"))?;
    if let Some((elapsed,)) = since {
        if elapsed < interval {
            let retry_after_secs = u64::try_from(interval.saturating_sub(elapsed))
                .unwrap_or(1)
                .max(1);
            let mut resp = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": CODE_RENAME_TOO_SOON,
                    "retry_after_secs": retry_after_secs,
                })),
            )
                .into_response();
            if let Ok(v) = axum::http::HeaderValue::from_str(&retry_after_secs.to_string()) {
                resp.headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, v);
            }
            return Err(resp);
        }
    }

    // 3. The new hash: free, or this account's own retired row.
    let existing: Option<(Vec<u8>, bool)> = sqlx::query_as(
        "SELECT account_id, retired_at IS NOT NULL FROM usernames          WHERE oprf_hash = ? FOR UPDATE",
    )
    .bind(&new_hash[..])
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal("read new username"))?;
    let revive = match existing {
        None => false,
        Some((owner, retired)) if retired && owner == account => true,
        Some(_) => return Err(coded(StatusCode::CONFLICT, CODE_USERNAME_TAKEN)),
    };

    // 4. Retire the current row. Before the new one becomes current: the
    //    unique key admits one current row per account.
    sqlx::query("UPDATE usernames SET retired_at = NOW(3), is_public = FALSE WHERE oprf_hash = ?")
        .bind(old_hash.as_slice())
        .execute(&mut *tx)
        .await
        .map_err(internal("retire username"))?;

    // 5. The new current row.
    let written = if revive {
        sqlx::query(
            "UPDATE usernames SET retired_at = NULL, is_public = FALSE              WHERE oprf_hash = ? AND account_id = ?",
        )
        .bind(&new_hash[..])
        .bind(account)
        .execute(&mut *tx)
        .await
    } else {
        sqlx::query("INSERT INTO usernames (oprf_hash, account_id) VALUES (?, ?)")
            .bind(&new_hash[..])
            .bind(account)
            .execute(&mut *tx)
            .await
    };
    match written {
        Ok(_) => {},
        // Another account took the hash between the read and the insert.
        // Rolled back on drop: the old name stays current.
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(coded(StatusCode::CONFLICT, CODE_USERNAME_TAKEN));
        },
        Err(e) => return Err(internal("write new username")(e)),
    }

    // 6. Out of Everyone: both were keyed over the old name.
    for sql in [
        "DELETE FROM display_names WHERE account_id = ?",
        "DELETE FROM public_records WHERE account_id = ?",
    ] {
        sqlx::query(sql)
            .bind(account)
            .execute(&mut *tx)
            .await
            .map_err(internal("leave Everyone on rename"))?;
    }

    // 7. The rename's time.
    sqlx::query(
        "INSERT INTO username_renames (account_id, last_rename_at) VALUES (?, NOW(3))          ON DUPLICATE KEY UPDATE last_rename_at = NOW(3)",
    )
    .bind(account)
    .execute(&mut *tx)
    .await
    .map_err(internal("record rename"))?;
    tx.commit().await.map_err(internal("commit rename"))?;

    tracing::info!(
        account_id = %B64.encode(account_id),
        revived = revive,
        "username replaced",
    );
    Ok(Json(RegisterResponse {
        ok: true,
        username_certificate_b64: crate::visibility::sign_certificate(
            &state.username_cert_key,
            new_hash,
            account_id,
        ),
    }))
}

/// Delete the names retired more than `hold_secs` ago; returns how many.
///
/// # Errors
///
/// Any `sqlx::Error` from the delete.
pub(crate) async fn prune_retired(pool: &MySqlPool, hold_secs: u64) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "DELETE FROM usernames          WHERE retired_at IS NOT NULL AND retired_at <= NOW(3) - INTERVAL ? SECOND",
    )
    .bind(i64::try_from(hold_secs).unwrap_or(i64::MAX))
    .execute(pool)
    .await?
    .rows_affected())
}

/// Hourly pruner: frees the names retired longer ago than the hold.
pub(crate) fn spawn_retired_pruner(pool: MySqlPool, hold_secs: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            match prune_retired(&pool, hold_secs).await {
                Ok(0) => {},
                Ok(rows) => tracing::info!(rows, "freed retired usernames past their hold"),
                Err(e) => tracing::warn!(error = %e, "retired usernames prune failed"),
            }
        }
    })
}

// ============================================================================
// /discovery/lookup
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct LookupRequest {
    pub(crate) oprf_hash: String,
}

#[derive(Serialize)]
pub(crate) struct LookupResponse {
    /// Base64-encoded UUIDv7 raw bytes — the `account_id` registered
    /// under this OPRF-hashed username.
    pub(crate) account_id: String,
    /// Standard base64 of the account's public record (ADR-0060 section 5):
    /// the searcher opens it with the OPRF output it just used for the
    /// username (`openPublicRecord`).
    pub(crate) public_record_b64: String,
}

/// `POST /discovery/lookup` — resolve an OPRF-hashed username to its
/// `account_id`. Returns **404 Not Found** for unknown hashes, for a retired
/// name (ADR-0076), and for a
/// username whose owner is not in Everyone (ADR-0060: the same status and
/// body, so a lookup cannot tell hidden from unregistered).
///
/// Exact-match only: there is intentionally no prefix search /
/// autocomplete from the server (which would either need plaintext
/// access or leak partial information about the username space).
/// Clients implement local autocomplete from their own contact list.
pub(crate) async fn lookup(
    State(state): State<AppState>,
    Json(payload): Json<LookupRequest>,
) -> Result<Json<LookupResponse>, (StatusCode, String)> {
    let oprf_hash = decode_fixed::<OPRF_HASH_LEN>(&payload.oprf_hash, "oprf_hash")?;

    // ADR-0060: only a person in Everyone answers; a hidden username gets the
    // very response an unregistered one does. `PUT /discovery/visibility`
    // stores a public record in the transaction that sets `is_public`, so the
    // join drops nobody who entered Everyone through it.
    let row: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT u.account_id, p.record FROM usernames u \
         JOIN public_records p ON p.account_id = u.account_id \
         WHERE u.oprf_hash = ? AND u.is_public = TRUE AND u.retired_at IS NULL",
    )
    .bind(&oprf_hash[..])
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "lookup usernames");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    match row {
        Some((account_id, record)) => Ok(Json(LookupResponse {
            account_id: B64.encode(account_id),
            public_record_b64: B64.encode(record),
        })),
        None => Err((StatusCode::NOT_FOUND, "username not found".to_string())),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_fixed_validates_length() {
        // Right length.
        let s = B64.encode([0u8; ACCOUNT_ID_LEN]);
        decode_fixed::<ACCOUNT_ID_LEN>(&s, "account_id").unwrap();

        // Too short.
        let s = B64.encode([0u8; 8]);
        let err = decode_fixed::<ACCOUNT_ID_LEN>(&s, "account_id").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Right length for OPRF hash.
        let s = B64.encode([0u8; OPRF_HASH_LEN]);
        decode_fixed::<OPRF_HASH_LEN>(&s, "oprf_hash").unwrap();

        // Bad base64.
        let err = decode_fixed::<ACCOUNT_ID_LEN>("not-valid!!!", "account_id").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn the_default_policy_is_thirty_days_for_both() {
        let policy = NamePolicy::defaults();
        assert_eq!(policy.rename_interval_secs, 2_592_000);
        assert_eq!(policy.retired_hold_secs, 2_592_000);
    }
}

/// Route tests of ADR-0076 section 1, through the production router. Gated on
/// `LAKKO_DISCOVERY_TEST_DATABASE_URL`; each test skips without it.
#[cfg(test)]
mod rename_tests {
    use super::*;
    use crate::anon_token_route_tests::{state, test_pool};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use lakko_attest::verify_username_binding;
    use rand::RngCore as _;
    use serde_json::{json, Value};
    use tower::ServiceExt as _;

    fn random<const N: usize>() -> [u8; N] {
        let mut out = [0u8; N];
        rand::thread_rng().fill_bytes(&mut out);
        out
    }

    struct Client {
        app: axum::Router,
        state: AppState,
    }

    impl Client {
        async fn new(name: &str, policy: NamePolicy) -> Option<Self> {
            let pool = test_pool(name).await?;
            let mut state = state(pool, true);
            state.name_policy = policy;
            Some(Self {
                app: crate::build_app(state.clone()),
                state,
            })
        }

        async fn call(
            &self,
            method: &str,
            uri: &str,
            account: Option<[u8; 16]>,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header("x-real-ip", "192.0.2.91")
                .header("content-type", "application/json");
            if let Some(account) = account {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis();
                let token = self
                    .state
                    .session_token_secret
                    .issue(
                        &account,
                        1,
                        &[0u8; 32],
                        0,
                        i64::try_from(now).unwrap() + 60_000,
                    )
                    .unwrap();
                b = b.header("authorization", format!("Bearer {token}"));
            }
            let body = body.map_or_else(Body::empty, |v| Body::from(v.to_string()));
            let resp = self
                .app
                .clone()
                .oneshot(b.body(body).unwrap())
                .await
                .unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let json = serde_json::from_str(&text).unwrap_or(Value::String(text));
            (status, json)
        }

        async fn bind(
            &self,
            route: &str,
            account: [u8; 16],
            hash: [u8; 64],
        ) -> (StatusCode, Value) {
            self.call(
                "POST",
                route,
                Some(account),
                Some(json!({
                    "account_id": B64.encode(account),
                    "oprf_hash": B64.encode(hash),
                })),
            )
            .await
        }

        async fn register(&self, account: [u8; 16], hash: [u8; 64]) -> (StatusCode, Value) {
            self.bind("/discovery/register", account, hash).await
        }

        async fn replace(&self, account: [u8; 16], hash: [u8; 64]) -> (StatusCode, Value) {
            self.bind("/discovery/username/replace", account, hash)
                .await
        }

        async fn lookup(&self, hash: [u8; 64]) -> StatusCode {
            self.call(
                "POST",
                "/discovery/lookup",
                None,
                Some(json!({ "oprf_hash": B64.encode(hash) })),
            )
            .await
            .0
        }

        /// Enter or leave Everyone with one name term.
        async fn visibility(&self, account: [u8; 16], public: bool, term: [u8; 64]) -> StatusCode {
            use lakko_contacts::introduction::{compose_public_record, IntroductionPayload};
            let mut body = json!({ "public": public });
            if public {
                let payload = IntroductionPayload {
                    identity_pk: lakko_crypto::signing::SigningKey::from_seed(&[7; 32])
                        .verifying_key()
                        .to_bytes(),
                    account_id: account,
                    display_name: Some("Anna Weber".to_string()),
                    username: None,
                    username_certificate: None,
                    mailbox_binding: lakko_contacts::mailbox::MailboxBinding::for_account_seed(
                        &[7; 32],
                    ),
                    fof: None,
                };
                let record = compose_public_record(&payload, &[random::<64>()], 1).unwrap();
                body["public_record_b64"] = json!(B64.encode(record));
                body["name_terms_b64"] = json!([B64.encode(term)]);
            }
            self.call("PUT", "/discovery/visibility", Some(account), Some(body))
                .await
                .0
        }

        async fn name_hits(&self, term: [u8; 64]) -> usize {
            let (status, json) = self
                .call(
                    "POST",
                    "/discovery/name/lookup",
                    None,
                    Some(json!({ "oprf_hash_b64": B64.encode(term) })),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
            json["results"].as_array().unwrap().len()
        }

        /// The hash the certificate route certifies for `account`, checked
        /// against the service's key.
        async fn certified(&self, account: [u8; 16]) -> Option<[u8; 64]> {
            let (status, json) = self
                .call(
                    "GET",
                    "/discovery/username-certificate",
                    Some(account),
                    None,
                )
                .await;
            if status == StatusCode::NOT_FOUND {
                return None;
            }
            assert_eq!(status, StatusCode::OK, "{json}");
            Some(self.hash_of_certificate(&json, account))
        }

        fn hash_of_certificate(&self, answer: &Value, account: [u8; 16]) -> [u8; 64] {
            let bytes = B64
                .decode(answer["username_certificate_b64"].as_str().unwrap())
                .unwrap();
            let key = self.state.username_cert_key.verifying_key();
            let hash: [u8; 64] = bytes[1..65].try_into().unwrap();
            verify_username_binding(&bytes, &key, &hash, &account).expect("certificate verifies");
            hash
        }

        /// `(oprf_hash, retired, is_public)` of every row of the account.
        async fn rows(&self, account: [u8; 16]) -> Vec<(Vec<u8>, bool, bool)> {
            sqlx::query_as(
                "SELECT oprf_hash, retired_at IS NOT NULL, is_public FROM usernames \
                 WHERE account_id = ? ORDER BY oprf_hash",
            )
            .bind(&account[..])
            .fetch_all(&self.state.db)
            .await
            .unwrap()
        }

        /// Move the account's rename and its retired rows `secs` into the
        /// past: the test's clock.
        async fn age(&self, account: [u8; 16], secs: i64) {
            for sql in [
                "UPDATE username_renames SET last_rename_at = last_rename_at - INTERVAL ? SECOND \
                 WHERE account_id = ?",
                "UPDATE usernames SET retired_at = retired_at - INTERVAL ? SECOND \
                 WHERE account_id = ? AND retired_at IS NOT NULL",
            ] {
                sqlx::query(sql)
                    .bind(secs)
                    .bind(&account[..])
                    .execute(&self.state.db)
                    .await
                    .unwrap();
            }
        }
    }

    /// Rows in the order [`Client::rows`] returns them (by hash).
    fn sorted(mut rows: Vec<(Vec<u8>, bool, bool)>) -> Vec<(Vec<u8>, bool, bool)> {
        rows.sort();
        rows
    }

    fn code(json: &Value) -> &str {
        json["error"].as_str().unwrap_or_default()
    }

    const DAY: i64 = 86_400;

    #[tokio::test]
    async fn a_rename_retires_the_old_name_and_holds_it_until_the_hold_passes() {
        let Some(c) = Client::new(
            "a_rename_retires_the_old_name_and_holds_it",
            NamePolicy::defaults(),
        )
        .await
        else {
            return;
        };
        let account = random::<16>();
        let stranger = random::<16>();
        let (old, new) = (random::<64>(), random::<64>());
        let term = random::<64>();

        let (st, registered) = c.register(account, old).await;
        assert_eq!(st, StatusCode::OK, "{registered}");
        assert_eq!(c.hash_of_certificate(&registered, account), old);
        assert_eq!(
            c.visibility(account, true, term).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(c.lookup(old).await, StatusCode::OK);
        assert_eq!(c.name_hits(term).await, 1);

        // The rename answers the NEW name's certificate.
        let (st, replaced) = c.replace(account, new).await;
        assert_eq!(st, StatusCode::OK, "{replaced}");
        assert_eq!(replaced["ok"], true);
        assert_eq!(c.hash_of_certificate(&replaced, account), new);
        assert_eq!(c.certified(account).await, Some(new));

        // One current row, one retired; neither public: the account left
        // Everyone with the rename.
        assert_eq!(
            c.rows(account).await,
            sorted(vec![
                (old.to_vec(), true, false),
                (new.to_vec(), false, false)
            ])
        );
        assert_eq!(c.lookup(old).await, StatusCode::NOT_FOUND);
        assert_eq!(c.lookup(new).await, StatusCode::NOT_FOUND);
        assert_eq!(c.name_hits(term).await, 0);

        // Entering Everyone again makes the NEW name resolve and never the
        // old one: visibility touches the current row only.
        assert_eq!(
            c.visibility(account, true, term).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(c.lookup(new).await, StatusCode::OK);
        assert_eq!(c.lookup(old).await, StatusCode::NOT_FOUND);
        assert_eq!(c.name_hits(term).await, 1);
        assert_eq!(
            c.rows(account).await,
            sorted(vec![
                (old.to_vec(), true, false),
                (new.to_vec(), false, true)
            ])
        );
        // Leaving likewise.
        assert_eq!(
            c.visibility(account, false, term).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            c.rows(account).await,
            sorted(vec![
                (old.to_vec(), true, false),
                (new.to_vec(), false, false)
            ])
        );

        // Nobody else can take the old name while it is held: not by
        // registering, not by renaming to it.
        let (st, body) = c.register(stranger, old).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(code(&body), "username-taken");
        let other = random::<16>();
        assert_eq!(c.register(other, random::<64>()).await.0, StatusCode::OK);
        let (st, body) = c.replace(other, old).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(code(&body), "username-taken");
        // The failed rename changed nothing for that account.
        assert_eq!(c.rows(other).await.len(), 1);

        // Twenty-nine days later the pruner still leaves it; after thirty it
        // frees it and the stranger takes it.
        c.age(account, 29 * DAY).await;
        prune_retired(&c.state.db, c.state.name_policy.retired_hold_secs)
            .await
            .unwrap();
        assert_eq!(c.rows(account).await.len(), 2);
        c.age(account, 2 * DAY).await;
        prune_retired(&c.state.db, c.state.name_policy.retired_hold_secs)
            .await
            .unwrap();
        assert_eq!(c.rows(account).await, vec![(new.to_vec(), false, false)]);
        let (st, body) = c.register(stranger, old).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(c.certified(account).await, Some(new));
    }

    #[tokio::test]
    async fn one_rename_per_interval_and_the_own_retired_name_can_be_taken_back() {
        let Some(c) = Client::new(
            "one_rename_per_interval_and_the_own_retired_name_can_be_taken_back",
            NamePolicy::defaults(),
        )
        .await
        else {
            return;
        };
        let account = random::<16>();
        let (first, second) = (random::<64>(), random::<64>());
        assert_eq!(c.register(account, first).await.0, StatusCode::OK);
        assert_eq!(c.replace(account, second).await.0, StatusCode::OK);

        // A second rename the same day: refused with the wait, nothing moves.
        for target in [random::<64>(), first] {
            let (st, body) = c.replace(account, target).await;
            assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{body}");
            assert_eq!(code(&body), "rename-too-soon");
            let wait = body["retry_after_secs"].as_u64().unwrap();
            assert!((2_591_000..=2_592_000).contains(&wait), "{wait}");
        }
        assert_eq!(c.certified(account).await, Some(second));

        // Thirty days later (the retired row not yet pruned): the account
        // takes its own old name back. The row is revived, not duplicated,
        // and the name it left is retired in its place.
        c.age(account, 30 * DAY + 1).await;
        let (st, body) = c.replace(account, first).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(c.hash_of_certificate(&body, account), first);
        assert_eq!(
            c.rows(account).await,
            sorted(vec![
                (first.to_vec(), false, false),
                (second.to_vec(), true, false)
            ])
        );
        assert_eq!(c.certified(account).await, Some(first));

        // Renaming to the name already held is not a rename.
        c.age(account, 30 * DAY + 1).await;
        let (st, body) = c.replace(account, first).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(code(&body), "username-taken");
    }

    #[tokio::test]
    async fn register_binds_one_name_and_replace_needs_one() {
        let Some(c) = Client::new(
            "register_binds_one_name_and_replace_needs_one",
            NamePolicy::defaults(),
        )
        .await
        else {
            return;
        };
        let account = random::<16>();
        let first = random::<64>();

        // No name yet: nothing to replace, nothing to certify.
        let (st, body) = c.replace(account, random::<64>()).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(code(&body), "no-username");
        assert_eq!(c.certified(account).await, None);

        assert_eq!(c.register(account, first).await.0, StatusCode::OK);
        // A second register, of another name: refused, one row.
        let (st, body) = c.register(account, random::<64>()).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(code(&body), "username-already-set");
        assert_eq!(c.rows(account).await, vec![(first.to_vec(), false, false)]);
        // A name someone holds.
        let (st, body) = c.register(random::<16>(), first).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(code(&body), "username-taken");

        // Another account's session cannot rename this one.
        let (st, _) = c
            .call(
                "POST",
                "/discovery/username/replace",
                Some(random::<16>()),
                Some(json!({
                    "account_id": B64.encode(account),
                    "oprf_hash": B64.encode(random::<64>()),
                })),
            )
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        // No session at all.
        let (st, _) = c
            .call(
                "POST",
                "/discovery/username/replace",
                None,
                Some(json!({
                    "account_id": B64.encode(account),
                    "oprf_hash": B64.encode(random::<64>()),
                })),
            )
            .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(c.certified(account).await, Some(first));

        // Account deletion removes every row, the retired one included, and
        // the rename's time.
        assert_eq!(c.replace(account, random::<64>()).await.0, StatusCode::OK);
        assert_eq!(c.rows(account).await.len(), 2);
        let (st, _) = c
            .call("DELETE", "/discovery/account", Some(account), None)
            .await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        assert!(c.rows(account).await.is_empty());
        let (renames,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM username_renames WHERE account_id = ?")
                .bind(&account[..])
                .fetch_one(&c.state.db)
                .await
                .unwrap();
        assert_eq!(renames, 0);
        // The freed name is free at once: the account is gone, nobody can be
        // mistaken for it by its devices' own record.
        assert_eq!(c.register(random::<16>(), first).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn the_interval_knob_zero_allows_back_to_back_renames() {
        let Some(c) = Client::new(
            "the_interval_knob_zero_allows_back_to_back_renames",
            NamePolicy {
                rename_interval_secs: 0,
                retired_hold_secs: DEFAULT_NAME_INTERVAL_SECS,
            },
        )
        .await
        else {
            return;
        };
        let account = random::<16>();
        let (a, b) = (random::<64>(), random::<64>());
        assert_eq!(c.register(account, a).await.0, StatusCode::OK);
        assert_eq!(c.replace(account, b).await.0, StatusCode::OK);
        assert_eq!(c.replace(account, a).await.0, StatusCode::OK);
        assert_eq!(c.certified(account).await, Some(a));
        assert_eq!(c.rows(account).await.len(), 2);
    }
}
