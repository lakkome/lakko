// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Auth-signed backup-head commitment — the mint endpoint (G-11, [ADR-0037]).
//!
//! `POST /auth/backup/head` lets an OPAQUE-authenticated account obtain a signed
//! [`BackupHead`] committing the encrypted backup envelope it is about to store
//! as its **latest**, carrying an auth-assigned monotone `backup_version`. A
//! restoring client verifies the head to detect a storage-tier rollback (a
//! returning client refuses any head below its persisted version floor; a
//! zero-anchor fresh install falls back to the head's `signed_at` freshness).
//!
//! ## What the head proves (state it plainly)
//!
//! > the party that OPAQUE-authenticated as `account_id` committed THIS envelope
//! > hash at backup version `N`, where `N` is auth's monotone counter's next
//! > value for the account, at the mint timestamp.
//!
//! ## Rate-limit (ADR-0037 F5b)
//!
//! The mint is bearer-authed and own-account only, but a token holder can call
//! it repeatedly for their own account — harmless to others, but unbounded DB
//! writes. A generous, fail-open per-account cap ([`check_mint_rate`]) bounds it
//! as defence-in-depth: "at most N mints per account per window W". It ships
//! DISABLED (default limit `0`, the repo's default-OFF convention for a new
//! gated feature) — an operator arms it with a positive
//! `LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT` (a generous `60`/`60s` is well above any
//! legitimate auto-backup cadence) after verifying the window-reset upsert
//! against a live MariaDB. It fails OPEN on a store error — a rate-limit is an
//! availability gate, not a security invariant.
//!
//! The signature proves the head is **authentic**, not that it is **current** —
//! two mechanisms turn authenticity into a rollback defence ([ADR-0037]): the
//! **auth-assigned monotone `backup_version`** (a regression is structurally
//! impossible because the version is never client-supplied) and the signed
//! **`signed_at`** mint timestamp (the client applies its own staleness window).
//!
//! ## Distinct from the genesis mint: STATEFUL
//!
//! Unlike [`crate::genesis_attest`] (stateless), this endpoint is **stateful**:
//! it read-bumps a per-account counter in the `backup_head_versions` table
//! (migration `0015`) inside one transaction, so no version is ever signed twice
//! and versions strictly increase per account. The table holds ONLY
//! `(account_id, version)` — no key material, no envelope bytes, no hash
//! (invariant #1 / #2). This is the sole material deviation from the ADR-0036
//! stateless template and the reason a migration is required.
//!
//! ## Authentication + signing key
//!
//! Bearer-authenticated via the [`AuthenticatedAccount`] extractor only —
//! holding a valid session token IS proof the caller OPAQUE-authenticated as
//! `account_id`. The head binds `account_id` directly (unlike the genesis
//! attestation's `username_hash`, since the client, storage, and auth all speak
//! `account_id` natively at restore time — [ADR-0037] §2). The signing key +
//! published JWKS are REUSED from the device-attestation role ([ADR-0037] §3);
//! the distinct `lakko/backup-head/v1` domain separator makes the three
//! attestation kinds (device / genesis / backup-head) cryptographically
//! non-interchangeable under the shared key.
//!
//! [ADR-0037]: ../../../docs/adr/0037-backup-head-rollback-commitment.md

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_attest::BackupHead;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;

use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Length of the `BLAKE3` envelope hash the head commits to.
const ENVELOPE_HASH_LEN: usize = 32;

// ----------------------------------------------------------------
// Mint rate-limit (ADR-0037 F5b)
// ----------------------------------------------------------------

/// Env var — max mints per account per window. `0` disables the limit.
const MINT_RATE_LIMIT_VAR: &str = "LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT";
/// Env var — the fixed rate-limit window in seconds.
const MINT_RATE_WINDOW_VAR: &str = "LAKKO_BACKUP_HEAD_MINT_RATE_WINDOW_SECS";

/// Default limit — `0`, i.e. the rate-limit ships DISABLED (unbounded), the
/// repo's default-OFF convention for a new gated feature. An operator enables it
/// by setting `LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT` to a positive value AFTER
/// verifying the window-reset upsert against a live MariaDB (the DB-backed tests
/// here are `LAKKO_AUTH_TEST_DATABASE_URL`-gated and have not run live yet). A
/// GENEROUS operational value is 60 mints/account/window: far above any
/// legitimate auto-backup cadence (a client uploads its whole sealed state blob
/// on significant events, coalesced — nowhere near one per second sustained), so
/// a legitimate client never trips it. It is a pure abuse cap on unbounded
/// own-account DB writes, not a security gate.
const DEFAULT_MINT_RATE_LIMIT: u64 = 0;
/// Default window — 60 seconds (only consulted once a positive limit is set).
const DEFAULT_MINT_RATE_WINDOW_SECS: u64 = 60;

/// Configuration for the per-account backup-head mint rate-limit. Loaded once at
/// startup from the environment (see [`MintRateConfig::from_env`]) and carried
/// on `AppState`. All-`Copy` — no allocation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MintRateConfig {
    /// Max mints per account per window. `0` disables the limit entirely
    /// (unbounded — the pre-ADR-0037-F5b behaviour).
    pub(crate) limit: u64,
    /// The fixed window (seconds) over which mints are counted per account.
    pub(crate) window_secs: u64,
}

impl MintRateConfig {
    /// Load the mint rate-limit configuration from the environment. Defaults to
    /// DISABLED (limit `0`, unbounded) for any unset / unparseable var — set
    /// `LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT` to a positive value (e.g. 60) to arm
    /// it, over `LAKKO_BACKUP_HEAD_MINT_RATE_WINDOW_SECS` (default 60).
    pub(crate) fn from_env() -> Self {
        Self {
            limit: parse_env_u64(MINT_RATE_LIMIT_VAR, DEFAULT_MINT_RATE_LIMIT),
            window_secs: parse_env_u64(MINT_RATE_WINDOW_VAR, DEFAULT_MINT_RATE_WINDOW_SECS),
        }
    }

    /// A disabled configuration (`limit == 0`). Used by the test `AppState`
    /// builders + as the safe value where a real config is not wired.
    #[cfg(test)]
    pub(crate) const fn disabled() -> Self {
        Self {
            limit: 0,
            window_secs: DEFAULT_MINT_RATE_WINDOW_SECS,
        }
    }

    /// Whether the limit is armed (`limit != 0`).
    pub(crate) const fn enabled(self) -> bool {
        self.limit != 0
    }
}

/// Read a `u64` env var, falling back to `default` when unset or unparseable.
fn parse_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

/// The outcome of the mint rate-limit gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MintRateDecision {
    /// Proceed to mint. Returned when the limit is disabled, the account is
    /// under its per-window cap, or the store errored (fail-open).
    Allow,
    /// Refuse the mint — the account is over its per-window cap (`429`).
    Limited,
}

/// Run the per-account mint rate-limit gate.
///
/// **Fails OPEN**: any store error returns [`MintRateDecision::Allow`] (logged),
/// so a DB blip can never block a legitimate backup. The limit is anti-abuse
/// defence-in-depth on unbounded own-account DB writes, not a security gate;
/// failing closed would self-DoS legitimate backups. This is the deliberate
/// exception to the fail-closed default (invariant #4), which governs the E2EE
/// handshake path — not this availability gate, which never touches key material
/// or ciphertext (invariant #2: only per-account mint bookkeeping is stored).
pub(crate) async fn check_mint_rate(
    pool: &MySqlPool,
    cfg: MintRateConfig,
    account_id: &[u8; 16],
) -> MintRateDecision {
    // Disabled — never touch the DB.
    if !cfg.enabled() {
        return MintRateDecision::Allow;
    }

    match record_and_count(pool, account_id, cfg.window_secs).await {
        Ok(count) => {
            if count > cfg.limit {
                tracing::warn!(
                    count,
                    limit = cfg.limit,
                    "backup-head mint rate-limit tripped — refusing mint"
                );
                MintRateDecision::Limited
            } else {
                MintRateDecision::Allow
            }
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                "backup-head mint rate-limit store error — failing OPEN (minting)"
            );
            MintRateDecision::Allow
        },
    }
}

/// Record this mint attempt against the account's fixed window and return the
/// resulting in-window count.
///
/// A single atomic upsert per account: on the duplicate-key branch, `mint_count`
/// resets to `1` when the prior window has elapsed (else increments) and
/// `window_start` resets to now in the same case (else stays). Both branch
/// conditions read the PRE-update `window_start`: MySQL evaluates
/// `ON DUPLICATE KEY UPDATE` assignments left-to-right and `window_start` is not
/// referenced by an earlier assignment, so both `IF`s see the old value. The
/// row is then read back for the count. One row per account — bounded by
/// construction (no event rows, no pruner needed).
///
/// # Errors
///
/// Any `sqlx::Error` from the transaction (begin / upsert / select / commit).
async fn record_and_count(
    pool: &MySqlPool,
    account_id: &[u8; 16],
    window_secs: u64,
) -> Result<u64, sqlx::Error> {
    // A pathological env value saturates rather than wrapping. The `INTERVAL ?
    // SECOND` bind wants a signed integer.
    let window = i64::try_from(window_secs).unwrap_or(i64::MAX);

    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO backup_head_mint_limits (account_id, window_start, mint_count) \
         VALUES (?, NOW(3), 1) \
         ON DUPLICATE KEY UPDATE \
           mint_count = IF(window_start <= NOW(3) - INTERVAL ? SECOND, 1, mint_count + 1), \
           window_start = IF(window_start <= NOW(3) - INTERVAL ? SECOND, NOW(3), window_start)",
    )
    .bind(&account_id[..])
    .bind(window)
    .bind(window)
    .execute(&mut *tx)
    .await?;

    let count: u64 =
        sqlx::query_scalar("SELECT mint_count FROM backup_head_mint_limits WHERE account_id = ?")
            .bind(&account_id[..])
            .fetch_one(&mut *tx)
            .await?;

    tx.commit().await?;
    Ok(count)
}

/// Decode the URL-safe base64 32-byte envelope hash from the wire.
fn decode_envelope_hash(b64: &str) -> Result<[u8; ENVELOPE_HASH_LEN], ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("envelope_hash_b64 not URL-safe base64"))?;
    if bytes.len() != ENVELOPE_HASH_LEN {
        return Err(ApiError::BadInput("envelope_hash must decode to 32 bytes"));
    }
    let mut out = [0u8; ENVELOPE_HASH_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Current time in unix milliseconds, saturating on the (impossible) overflow.
/// Milliseconds (not seconds) because [`BackupHead`] works in milliseconds
/// natively — the clients and the freshness window are all millisecond-based.
fn now_unix_millis() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(i64::MAX)
}

/// Atomically read-and-bump the per-account monotone `backup_version` counter,
/// returning the newly-assigned version (`1` on the first mint for an account,
/// `previous + 1` thereafter).
///
/// Uses the canonical MariaDB `LAST_INSERT_ID(expr)` idiom on a SINGLE
/// transaction connection: the upsert sets the session `LAST_INSERT_ID` to the
/// assigned value (`1` on insert, `version + 1` on the duplicate-key update),
/// and the immediately-following `SELECT LAST_INSERT_ID()` reads it back on the
/// same connection. Wrapping both in one transaction serialises concurrent
/// mints for the same account (multi-device backups), so no version is ever
/// signed twice and versions strictly increase.
///
/// # Errors
///
/// Any `sqlx::Error` from the transaction (begin / upsert / select / commit).
async fn bump_backup_version(pool: &MySqlPool, account_id: &[u8; 16]) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO backup_head_versions (account_id, version) VALUES (?, LAST_INSERT_ID(1)) \
         ON DUPLICATE KEY UPDATE version = LAST_INSERT_ID(version + 1)",
    )
    .bind(&account_id[..])
    .execute(&mut *tx)
    .await?;

    let new_version: u64 = sqlx::query_scalar("SELECT LAST_INSERT_ID()")
        .fetch_one(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(new_version)
}

/// `POST /auth/backup/head` — mint an auth-signed backup head (G-11,
/// [ADR-0037]).
///
/// Bearer-authenticated. Atomically bumps the per-account monotone
/// `backup_version` counter, then signs a [`BackupHead`] over
/// `(account_id, backup_version, envelope_hash, signed_at)` with the shared
/// attestation key and returns it base64-encoded alongside the assigned version.
///
/// # Errors
///
/// [`ApiError::BadInput`] when `envelope_hash_b64` is not 32-byte URL-safe
/// base64; [`ApiError::Db`] on any counter-transaction failure.
///
/// [ADR-0037]: ../../../docs/adr/0037-backup-head-rollback-commitment.md
#[axum::debug_handler]
pub(crate) async fn backup_head(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<BackupHeadReq>,
) -> Result<Json<BackupHeadRes>, ApiError> {
    let envelope_hash = decode_envelope_hash(&req.envelope_hash_b64)?;

    // ADR-0037 F5b — generous, fail-open per-account mint rate-limit. Runs AFTER
    // cheap input validation, BEFORE the counter bump + signing, so a limited
    // request does not consume a monotone version or do the mint write. Disabled
    // by default only when `LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT=0`; fails OPEN on a
    // store error (see `check_mint_rate`).
    if check_mint_rate(&state.db, state.backup_head_mint_rate, &auth.0).await
        == MintRateDecision::Limited
    {
        crate::metrics::backup_head_mint_limited();
        return Err(ApiError::RateLimited);
    }

    // STATEFUL: the auth-assigned version is the monotone counter's next value —
    // never client-supplied, so a regression is structurally impossible.
    let backup_version = bump_backup_version(&state.db, &auth.0)
        .await
        .map_err(ApiError::Db)?;

    // Reuse the device-attestation signing key ([ADR-0037] §3); the distinct
    // `lakko/backup-head/v1` domain keeps the attestation kinds cryptographically
    // non-interchangeable under the shared key.
    let head = BackupHead::new(auth.0, backup_version, envelope_hash, now_unix_millis())
        .sign(&state.attest_key.signing);

    Ok(Json(BackupHeadRes {
        backup_head_b64: URL_SAFE_NO_PAD.encode(head),
        backup_version,
    }))
}

// ----------------------------------------------------------------
// Wire shapes — /auth/backup/head
// ----------------------------------------------------------------

/// Request body for `POST /auth/backup/head`.
#[derive(Deserialize)]
pub(crate) struct BackupHeadReq {
    /// URL-safe base64 (no padding) of the 32-byte `BLAKE3` of the envelope the
    /// client is about to store. The client computes it locally before
    /// uploading; auth signs it without persisting it.
    pub envelope_hash_b64: String,
}

/// Response body for `POST /auth/backup/head`.
#[derive(Serialize)]
pub(crate) struct BackupHeadRes {
    /// URL-safe base64 (no padding) of the signed 129-byte [`BackupHead`]. The
    /// client prepends it (behind the container tag) to the envelope in the
    /// single storage `PUT`.
    pub backup_head_b64: String,
    /// The auth-assigned monotone version this head carries. The client persists
    /// it as the floor a future restore on this device lineage compares against.
    pub backup_version: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_attest::verify_backup_head;
    use lakko_call_jwt::CallSigningKey;

    /// Fixed seed for the test attestation key so the test reconstructs the
    /// verifying key that signed.
    const ATTEST_SEED: [u8; 32] = [0xCD; 32];

    // ---- DB-free sign/verify contract test ----------------------------------
    //
    // The handler mints exactly `BackupHead::new(account, version, hash, now)
    // .sign(&attest_key.signing)`. This proves that wire/binding path (the same
    // signing key + primitive the handler uses) round-trips through the client
    // verifier for a given version, WITHOUT needing the DB-backed counter — so
    // it runs in DB-less CI.

    #[test]
    fn signed_head_verifies_and_carries_the_assigned_version() {
        // The SAME signing key the handler pulls from `state.attest_key.signing`.
        let sk = CallSigningKey::from_seed(&ATTEST_SEED);
        let account_id: [u8; 16] = [0x11; 16];
        let envelope_hash: [u8; 32] = [0x22; 32];
        let backup_version: u64 = 7;
        let signed_at: i64 = 1_000_000;

        // Mirror the handler's mint (fixed `signed_at` for determinism).
        let head =
            BackupHead::new(account_id, backup_version, envelope_hash, signed_at).sign(&sk.signing);

        // The client's restore-path verify: signature + account + envelope-hash
        // + freshness (now == signed_at → fresh), returning the assigned version.
        let got = verify_backup_head(
            &head,
            &sk.verifying,
            &account_id,
            &envelope_hash,
            signed_at,
            10_000,
        )
        .expect("minted head verifies + binds to this account/envelope");
        assert_eq!(got.backup_version, backup_version);
        assert_eq!(got.account_id, account_id);
        assert_eq!(got.envelope_hash, envelope_hash);

        // A head minted for a DIFFERENT version does not satisfy the same-shape
        // check against the first — versions are distinct commitments.
        let head_v8 = BackupHead::new(account_id, 8, envelope_hash, signed_at).sign(&sk.signing);
        let got8 = verify_backup_head(
            &head_v8,
            &sk.verifying,
            &account_id,
            &envelope_hash,
            signed_at,
            10_000,
        )
        .expect("v8 head verifies");
        assert_eq!(got8.backup_version, 8);
        assert_ne!(got.backup_version, got8.backup_version);
    }

    // ---- mint rate-limit: pure (no DB) --------------------------------------

    #[test]
    fn mint_rate_config_defaults_and_disabled() {
        let disabled = MintRateConfig::disabled();
        assert_eq!(disabled.limit, 0);
        assert!(!disabled.enabled());

        let armed = MintRateConfig {
            limit: 60,
            window_secs: 60,
        };
        assert!(armed.enabled());
    }

    /// A disabled limit short-circuits BEFORE any DB access — proven by passing
    /// a pool that could never connect (port 1) and still getting `Allow`
    /// without an error / hang.
    #[tokio::test]
    async fn disabled_mint_rate_never_touches_db() {
        use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
        use std::time::Duration;
        let pool = MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy_with(
                MySqlConnectOptions::new()
                    .host("127.0.0.1")
                    .port(1)
                    .username("nobody")
                    .database("nodb"),
            );
        let decision = check_mint_rate(&pool, MintRateConfig::disabled(), &[1u8; 16]).await;
        assert_eq!(decision, MintRateDecision::Allow);
    }

    /// The fail-open branch. An ARMED limit whose store errors (unreachable DB)
    /// must return `Allow` (mint), never `Limited` — a store blip can never
    /// block a legitimate backup.
    #[tokio::test]
    async fn armed_mint_rate_fails_open_on_store_error() {
        use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
        use std::time::Duration;
        let pool = MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy_with(
                MySqlConnectOptions::new()
                    .host("127.0.0.1")
                    .port(1) // connection refused -> sqlx error -> fail-open
                    .username("nobody")
                    .database("nodb"),
            );
        let cfg = MintRateConfig {
            limit: 3,
            window_secs: 60,
        };
        assert!(cfg.enabled());
        let decision = check_mint_rate(&pool, cfg, &[1u8; 16]).await;
        assert_eq!(
            decision,
            MintRateDecision::Allow,
            "an armed mint rate-limit must FAIL OPEN when the store is unreachable"
        );
    }

    #[test]
    fn rejects_a_malformed_envelope_hash() {
        // 31 bytes — one short of the 32-byte BLAKE3.
        let err = decode_envelope_hash(&URL_SAFE_NO_PAD.encode([0x22; 31]));
        assert!(matches!(err, Err(ApiError::BadInput(_))));
        // Not base64 at all.
        assert!(matches!(
            decode_envelope_hash("not base64!!!"),
            Err(ApiError::BadInput(_))
        ));
    }

    // ---- DB-backed monotonicity test ----------------------------------------
    //
    // Gated on `LAKKO_AUTH_TEST_DATABASE_URL` — skip (do not fail) when no test
    // database is provided, so `cargo test` stays green in DB-less CI. Mirrors
    // the gating harness in `src/throttle.rs`. Run with, e.g.:
    //   LAKKO_AUTH_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_auth \
    //     cargo test -p lakko-svc-auth backup_head

    /// A fresh account row (the backup-head tables reference `accounts` since
    /// migration 0039).
    async fn new_account(pool: &MySqlPool) -> [u8; 16] {
        let id: [u8; 16] = uuid::Uuid::now_v7().into_bytes();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(id.as_slice())
            .execute(pool)
            .await
            .expect("insert account");
        id
    }

    /// Migration 0039: deleting the account deletes its backup-head rows.
    #[tokio::test]
    async fn deleting_the_account_deletes_its_backup_head_rows() {
        let Some(pool) = test_pool("deleting_the_account_deletes_its_backup_head_rows").await
        else {
            return;
        };
        let acct = new_account(&pool).await;
        bump_backup_version(&pool, &acct).await.expect("bump");
        sqlx::query("INSERT INTO backup_head_mint_limits (account_id, mint_count) VALUES (?, 1)")
            .bind(acct.as_slice())
            .execute(&pool)
            .await
            .expect("insert limit row");
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(acct.as_slice())
            .execute(&pool)
            .await
            .expect("delete account");
        for (table, sql) in [
            (
                "backup_head_versions",
                "SELECT COUNT(*) FROM backup_head_versions WHERE account_id = ?",
            ),
            (
                "backup_head_mint_limits",
                "SELECT COUNT(*) FROM backup_head_mint_limits WHERE account_id = ?",
            ),
        ] {
            let left: i64 = sqlx::query_scalar(sql)
                .bind(acct.as_slice())
                .fetch_one(&pool)
                .await
                .expect("count");
            assert_eq!(left, 0, "{table} kept a row of a deleted account");
        }
    }

    /// Connect + migrate a fresh test pool, or `None` when the gating env var is
    /// unset (skip).
    async fn test_pool(test_name: &str) -> Option<MySqlPool> {
        use sqlx::mysql::MySqlPoolOptions;
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping {test_name}: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return None;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        Some(pool)
    }

    /// The per-account counter starts at 1, increments strictly on each mint,
    /// and is isolated per account (a different account starts fresh at 1).
    #[tokio::test]
    async fn version_is_monotone_and_per_account_isolated() {
        let Some(pool) = test_pool("version_is_monotone_and_per_account_isolated").await else {
            return;
        };
        // Fresh random account ids so the test is re-runnable against a
        // persistent dev DB (mirrors throttle.rs's per-run uuids).
        let acct_a: [u8; 16] = new_account(&pool).await;
        let acct_b: [u8; 16] = new_account(&pool).await;

        // First mint for account A → version 1; second → 2; third → 3.
        assert_eq!(
            bump_backup_version(&pool, &acct_a).await.expect("bump a1"),
            1
        );
        assert_eq!(
            bump_backup_version(&pool, &acct_a).await.expect("bump a2"),
            2
        );
        assert_eq!(
            bump_backup_version(&pool, &acct_a).await.expect("bump a3"),
            3
        );

        // A DIFFERENT account starts its own counter at 1 (per-account isolation).
        assert_eq!(
            bump_backup_version(&pool, &acct_b).await.expect("bump b1"),
            1
        );
        // …and continues independently of A.
        assert_eq!(
            bump_backup_version(&pool, &acct_b).await.expect("bump b2"),
            2
        );
        assert_eq!(
            bump_backup_version(&pool, &acct_a).await.expect("bump a4"),
            4
        );
    }

    // ---- mint rate-limit: DB-backed ------------------------------------------
    //
    // Same `LAKKO_AUTH_TEST_DATABASE_URL` gating as above — skip when unset.

    /// Under-limit mints are all allowed; the (limit+1)-th mint within the same
    /// window is `Limited`, and every further mint stays `Limited` until the
    /// window elapses.
    #[tokio::test]
    async fn mint_rate_allows_under_limit_then_refuses_the_rest_of_the_window() {
        let Some(pool) =
            test_pool("mint_rate_allows_under_limit_then_refuses_the_rest_of_the_window").await
        else {
            return;
        };
        // A long window so the test never rolls over mid-run.
        let cfg = MintRateConfig {
            limit: 3,
            window_secs: 3_600,
        };
        let acct: [u8; 16] = new_account(&pool).await;

        // Counts 1, 2, 3 are all <= limit -> Allow.
        for _ in 0..3 {
            assert_eq!(
                check_mint_rate(&pool, cfg, &acct).await,
                MintRateDecision::Allow
            );
        }
        // Count 4 > limit -> Limited.
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct).await,
            MintRateDecision::Limited
        );
        // Still Limited for the rest of the window.
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct).await,
            MintRateDecision::Limited
        );
    }

    /// The window is per-account: one account tripping its cap never affects a
    /// different account, which starts its own count fresh.
    #[tokio::test]
    async fn mint_rate_is_per_account_isolated() {
        let Some(pool) = test_pool("mint_rate_is_per_account_isolated").await else {
            return;
        };
        let cfg = MintRateConfig {
            limit: 2,
            window_secs: 3_600,
        };
        let acct_a: [u8; 16] = new_account(&pool).await;
        let acct_b: [u8; 16] = new_account(&pool).await;

        // Trip account A (counts 1, 2 allowed; 3 limited).
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct_a).await,
            MintRateDecision::Allow
        );
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct_a).await,
            MintRateDecision::Allow
        );
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct_a).await,
            MintRateDecision::Limited
        );

        // Account B is unaffected — its own window starts fresh.
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct_b).await,
            MintRateDecision::Allow
        );
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct_b).await,
            MintRateDecision::Allow
        );
    }

    /// An elapsed window resets the counter: with a sub-second window a short
    /// sleep rolls it over and the next mint is `Allow` again (count back to 1).
    #[tokio::test]
    async fn mint_rate_window_resets_after_it_elapses() {
        let Some(pool) = test_pool("mint_rate_window_resets_after_it_elapses").await else {
            return;
        };
        // 1-second window (the smallest whole-second window). Trip it, wait for
        // it to elapse, and confirm the next mint is allowed (reset to count 1).
        let cfg = MintRateConfig {
            limit: 1,
            window_secs: 1,
        };
        let acct: [u8; 16] = new_account(&pool).await;

        // Count 1 -> Allow; count 2 in the same window -> Limited.
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct).await,
            MintRateDecision::Allow
        );
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct).await,
            MintRateDecision::Limited
        );

        // Wait for the window to elapse (INTERVAL 1 SECOND is inclusive via
        // `<=`, plus margin), then the next mint resets to count 1 -> Allow.
        tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
        assert_eq!(
            check_mint_rate(&pool, cfg, &acct).await,
            MintRateDecision::Allow
        );
    }

    /// With `limit == 0` the gate is fully disabled — an unbounded burst on one
    /// account is all allowed and nothing is written.
    #[tokio::test]
    async fn mint_rate_disabled_allows_unbounded_burst() {
        let Some(pool) = test_pool("mint_rate_disabled_allows_unbounded_burst").await else {
            return;
        };
        let cfg = MintRateConfig::disabled();
        let acct: [u8; 16] = new_account(&pool).await;
        for _ in 0..12 {
            assert_eq!(
                check_mint_rate(&pool, cfg, &acct).await,
                MintRateDecision::Allow
            );
        }
        // The disabled path never touches the table.
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM backup_head_mint_limits WHERE account_id = ?")
                .bind(&acct[..])
                .fetch_one(&pool)
                .await
                .expect("count rows");
        assert_eq!(rows, 0);
    }
}
