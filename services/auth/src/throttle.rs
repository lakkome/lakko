// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call-admission anti-spam throttle (ADR-0033).
//!
//! ADR-0032 lets an anonymous `/c/<slug>` visitor auto-create a full account to
//! join a call, which is an account-creation-spam surface: a burst of guest
//! joins on one link mints many accounts. This module throttles new-account
//! call admissions per `call_id` at the `POST /call/jwt` mint point.
//!
//! The abuse signal is **account-age x `call_id`** — "many brand-new accounts on
//! ONE link, fast". A legitimate ad-hoc call is a handful of participants,
//! mostly not brand-new; a spam flood is a burst of fresh mints on one link.
//! Keying on both avoids throttling (a) a large legit call of existing users and
//! (b) organic new-user growth spread across many links.
//!
//! The gate (see [`check_admission`]):
//!
//! 1. If [`ThrottleConfig::burst_limit`] is `0` the throttle is **disabled** and
//!    the gate returns [`ThrottleDecision::Allow`] without touching the DB.
//! 2. If the `call_id` is already throttled (a live `throttled_calls` row) the
//!    gate refuses **ALL** admissions (fresh or not) — the "end the call".
//! 3. If the requesting account is **fresh** (its `created_at` is within the
//!    guest window) the attempt is recorded and the distinct-fresh-account count
//!    for that `call_id` in the sliding window is checked; exceeding
//!    `burst_limit` trips the throttle and refuses.
//! 4. Otherwise the gate allows the mint.
//!
//! **Fail-OPEN on a store error.** If the throttle DB work errors, the gate
//! **allows** the mint (logs a warning) rather than refusing. The throttle is
//! anti-abuse, not a security invariant; failing closed would turn a
//! throttle-store blip into a self-DoS on all calls. This is the deliberate
//! exception to the project's fail-closed default (invariant #4), which governs
//! the E2EE handshake path — not this availability gate, which never touches key
//! material or ciphertext (invariant #2: only admission bookkeeping is stored).

use std::time::Duration;

use sqlx::MySqlPool;

/// Env var — an account younger than this (seconds) counts as fresh/guest.
const GUEST_AGE_VAR: &str = "LAKKO_AUTH_GUEST_ACCOUNT_AGE_SECS";
/// Env var — the sliding window (seconds) over which fresh admissions are
/// counted per `call_id`.
const WINDOW_VAR: &str = "LAKKO_AUTH_CALL_ADMISSION_WINDOW_SECS";
/// Env var — max distinct fresh accounts per `call_id` per window before
/// throttling. `0` disables the throttle entirely.
const BURST_LIMIT_VAR: &str = "LAKKO_AUTH_CALL_ADMISSION_BURST_LIMIT";
/// Env var — how long (seconds) a call stays throttled once tripped.
const THROTTLE_SECS_VAR: &str = "LAKKO_AUTH_CALL_THROTTLE_SECS";

/// Default guest window — one hour.
const DEFAULT_GUEST_AGE_SECS: u64 = 3_600;
/// Default sliding window — one minute.
const DEFAULT_WINDOW_SECS: u64 = 60;
/// Default burst limit — 8 distinct fresh accounts per `call_id` per window.
const DEFAULT_BURST_LIMIT: u64 = 8;
/// Default throttle duration — five minutes.
const DEFAULT_THROTTLE_SECS: u64 = 300;

/// Period between background prune runs.
const PRUNE_INTERVAL: Duration = Duration::from_secs(300);

/// Configuration for the call-admission throttle. Loaded once at startup from
/// the environment (see [`ThrottleConfig::from_env`]) and carried on
/// `AppState`. All-`Copy` — no allocation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ThrottleConfig {
    /// An account whose `created_at` is within this many seconds of now counts
    /// as fresh/guest.
    pub(crate) guest_age_secs: u64,
    /// The sliding window (seconds) over which fresh admissions are counted.
    pub(crate) window_secs: u64,
    /// Max distinct fresh accounts per `call_id` per window before throttling.
    /// `0` disables the throttle entirely.
    pub(crate) burst_limit: u64,
    /// How long (seconds) a call stays throttled once tripped.
    pub(crate) throttle_secs: u64,
}

impl ThrottleConfig {
    /// Load the throttle configuration from the environment, falling back to the
    /// safe defaults for any unset / unparseable var. An unset / `0`
    /// `LAKKO_AUTH_CALL_ADMISSION_BURST_LIMIT` disables the throttle.
    pub(crate) fn from_env() -> Self {
        Self {
            guest_age_secs: parse_env_u64(GUEST_AGE_VAR, DEFAULT_GUEST_AGE_SECS),
            window_secs: parse_env_u64(WINDOW_VAR, DEFAULT_WINDOW_SECS),
            burst_limit: parse_env_u64(BURST_LIMIT_VAR, DEFAULT_BURST_LIMIT),
            throttle_secs: parse_env_u64(THROTTLE_SECS_VAR, DEFAULT_THROTTLE_SECS),
        }
    }

    /// A disabled configuration (`burst_limit == 0`). Used by tests + as the
    /// safe value where a real config is not wired.
    #[cfg(test)]
    pub(crate) const fn disabled() -> Self {
        Self {
            guest_age_secs: DEFAULT_GUEST_AGE_SECS,
            window_secs: DEFAULT_WINDOW_SECS,
            burst_limit: 0,
            throttle_secs: DEFAULT_THROTTLE_SECS,
        }
    }

    /// Whether the throttle is armed (`burst_limit != 0`).
    pub(crate) const fn enabled(self) -> bool {
        self.burst_limit != 0
    }
}

/// Read a `u64` env var, falling back to `default` when unset or unparseable.
fn parse_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

/// The outcome of the admission gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThrottleDecision {
    /// Proceed to mint the JWT. Returned when the throttle is disabled, the
    /// account is not fresh, the fresh-account count is under the limit, or the
    /// throttle store errored (fail-open).
    Allow,
    /// Refuse the admission — the call is throttled (`429`).
    Throttled,
}

/// Decode a validated 32-char hex `call_id` to its 16 raw bytes.
///
/// The caller (`issue_call_jwt`) has already validated the string is 32 ASCII
/// hex digits, so `None` here is a defence-in-depth guard, not the primary
/// validation.
pub(crate) fn decode_call_id(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = hex.get(i * 2..i * 2 + 2)?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

/// Run the call-admission throttle gate for one `(call_id, account_id)` pair.
///
/// See the module docs for the sequence. **Fails OPEN**: any throttle-store
/// error returns [`ThrottleDecision::Allow`] (logged) so a DB blip cannot
/// self-DoS the call feature.
pub(crate) async fn check_admission(
    pool: &MySqlPool,
    cfg: ThrottleConfig,
    call_id: &[u8; 16],
    account_id: &[u8; 16],
) -> ThrottleDecision {
    // (a) Disabled — never touch the DB.
    if !cfg.enabled() {
        return ThrottleDecision::Allow;
    }

    match evaluate(pool, cfg, call_id, account_id).await {
        Ok(decision) => decision,
        Err(e) => {
            // FAIL-OPEN. The throttle is anti-abuse, not a security gate. A
            // false-negative is a spam window; a false-positive (failing
            // closed) would deny legitimate calls whenever the store hiccups,
            // so we lean permissive. This is the deliberate exception to the
            // fail-closed default — it applies to security gates, and this
            // availability throttle never touches the E2EE path or key
            // material (invariants #2 / #4 are unaffected).
            tracing::warn!(
                error = %e,
                "call-admission throttle store error — failing OPEN (minting)"
            );
            ThrottleDecision::Allow
        },
    }
}

/// The DB body of [`check_admission`] — separated so the caller can wrap ANY
/// `sqlx::Error` into the single fail-open branch.
async fn evaluate(
    pool: &MySqlPool,
    cfg: ThrottleConfig,
    call_id: &[u8; 16],
    account_id: &[u8; 16],
) -> Result<ThrottleDecision, sqlx::Error> {
    // Config seconds -> i64 for the SQL `INTERVAL ? SECOND` binds. A pathological
    // env value saturates rather than wrapping.
    let guest_age = i64::try_from(cfg.guest_age_secs).unwrap_or(i64::MAX);
    let window = i64::try_from(cfg.window_secs).unwrap_or(i64::MAX);
    let burst_limit = i64::try_from(cfg.burst_limit).unwrap_or(i64::MAX);
    let throttle_secs = i64::try_from(cfg.throttle_secs).unwrap_or(i64::MAX);

    // (b) Is this call_id already throttled? A throttled call refuses ALL
    //     admissions (fresh or not) until the TTL expires.
    let already_throttled: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM throttled_calls WHERE call_id = ? AND until > NOW(3)")
            .bind(&call_id[..])
            .fetch_optional(pool)
            .await?;
    if already_throttled.is_some() {
        return Ok(ThrottleDecision::Throttled);
    }

    // (c) Is the requesting account fresh (created within the guest window)?
    //     The comparison runs on the DB clock (consistent with the `at`
    //     timestamps written below), returning 1/0 which we read as i64 because
    //     a MySQL boolean expression is a BIGINT, not TINYINT(1).
    let fresh_flag: Option<i64> = sqlx::query_scalar(
        "SELECT (created_at > NOW() - INTERVAL ? SECOND) FROM accounts WHERE id = ?",
    )
    .bind(guest_age)
    .bind(&account_id[..])
    .fetch_optional(pool)
    .await?;
    // A missing account row (should not happen for a bearer-authed account) or a
    // non-fresh account is never recorded and never throttled-by-count — only
    // the (b) check can refuse it once a call is already throttled.
    if !fresh_flag.is_some_and(|v| v != 0) {
        return Ok(ThrottleDecision::Allow);
    }

    // Record this fresh admission attempt. The PK (call_id, account_id) dedups
    // per account, so the windowed COUNT below is a distinct-fresh-account count.
    sqlx::query(
        "INSERT INTO call_admission_events (call_id, account_id) VALUES (?, ?) \
         ON DUPLICATE KEY UPDATE at = NOW(3)",
    )
    .bind(&call_id[..])
    .bind(&account_id[..])
    .execute(pool)
    .await?;

    // Count distinct fresh accounts admitted to this call in the window.
    let fresh_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM call_admission_events \
         WHERE call_id = ? AND at >= NOW(3) - INTERVAL ? SECOND",
    )
    .bind(&call_id[..])
    .bind(window)
    .fetch_one(pool)
    .await?;

    if fresh_count > burst_limit {
        // Trip the throttle: refuse this admission + every later one until the
        // TTL expires. Upsert so a re-trip extends the window.
        sqlx::query(
            "INSERT INTO throttled_calls (call_id, until) VALUES (?, NOW(3) + INTERVAL ? SECOND) \
             ON DUPLICATE KEY UPDATE until = NOW(3) + INTERVAL ? SECOND",
        )
        .bind(&call_id[..])
        .bind(throttle_secs)
        .bind(throttle_secs)
        .execute(pool)
        .await?;
        tracing::warn!(
            fresh_count,
            burst_limit,
            "call-admission throttle tripped — refusing new joins on this call_id"
        );
        return Ok(ThrottleDecision::Throttled);
    }

    Ok(ThrottleDecision::Allow)
}

/// Spawn the background prune loop: deletes admission events older than the
/// sliding window (they can no longer contribute to a windowed count) and
/// throttled-call rows past their `until`. Aborted when the service stops.
pub(crate) fn spawn_pruner(pool: MySqlPool, cfg: ThrottleConfig) -> tokio::task::JoinHandle<()> {
    let window = i64::try_from(cfg.window_secs).unwrap_or(i64::MAX);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        // The first tick fires immediately — skip it (the tables are usually
        // empty or near-empty on startup).
        interval.tick().await;
        loop {
            interval.tick().await;

            match sqlx::query(
                "DELETE FROM call_admission_events WHERE at < NOW(3) - INTERVAL ? SECOND",
            )
            .bind(window)
            .execute(&pool)
            .await
            {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(
                        rows = r.rows_affected(),
                        "pruned expired call_admission_events"
                    );
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "prune call_admission_events failed"),
            }

            match sqlx::query("DELETE FROM throttled_calls WHERE until < NOW(3)")
                .execute(&pool)
                .await
            {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned expired throttled_calls");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "prune throttled_calls failed"),
            }
        }
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure unit tests (no DB) --------------------------------------------

    #[test]
    fn decode_call_id_round_trips_and_rejects_bad_input() {
        let bytes = decode_call_id("0102030405060708090a0b0c0d0e0f10").expect("valid hex");
        assert_eq!(
            bytes,
            [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
                0x0f, 0x10
            ]
        );
        // Upper-case is accepted (from_str_radix is case-insensitive).
        assert!(decode_call_id("ABABABABABABABABABABABABABABABAB").is_some());
        // Wrong length + non-hex are rejected.
        assert!(decode_call_id("tooShort").is_none());
        assert!(decode_call_id("zz02030405060708090a0b0c0d0e0f10").is_none());
    }

    #[test]
    fn config_defaults_and_disabled() {
        let disabled = ThrottleConfig::disabled();
        assert_eq!(disabled.burst_limit, 0);
        assert!(!disabled.enabled());

        let armed = ThrottleConfig {
            burst_limit: 3,
            ..ThrottleConfig::disabled()
        };
        assert!(armed.enabled());
        assert_eq!(armed.guest_age_secs, DEFAULT_GUEST_AGE_SECS);
        assert_eq!(armed.window_secs, DEFAULT_WINDOW_SECS);
        assert_eq!(armed.throttle_secs, DEFAULT_THROTTLE_SECS);
    }

    /// A disabled throttle short-circuits BEFORE any DB access — proven by
    /// passing a pool that could never connect (port 1) and still getting
    /// `Allow` without an error / hang.
    #[tokio::test]
    async fn disabled_throttle_never_touches_db() {
        use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
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
        let decision =
            check_admission(&pool, ThrottleConfig::disabled(), &[0u8; 16], &[1u8; 16]).await;
        assert_eq!(decision, ThrottleDecision::Allow);
    }

    /// Test (e): the fail-open branch. An ARMED throttle whose store errors
    /// (unreachable DB) must return `Allow` (mint), never `Throttled`.
    #[tokio::test]
    async fn armed_throttle_fails_open_on_store_error() {
        use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
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
        let cfg = ThrottleConfig {
            burst_limit: 3,
            ..ThrottleConfig::disabled()
        };
        assert!(cfg.enabled());
        let decision = check_admission(&pool, cfg, &[0u8; 16], &[1u8; 16]).await;
        assert_eq!(
            decision,
            ThrottleDecision::Allow,
            "an armed throttle must FAIL OPEN when the store is unreachable"
        );
    }

    // ---- DB-backed integration tests ----------------------------------------
    //
    // Gated on `LAKKO_AUTH_TEST_DATABASE_URL` — skip (do not fail) when no test
    // database is provided, so `cargo test` stays green in DB-less CI. Run with,
    // e.g.:
    //   LAKKO_AUTH_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_auth_test \
    //     cargo test -p lakko-svc-auth throttle

    use sqlx::MySqlPool;

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

    /// Insert a fresh account (created_at = now) and return its id.
    async fn insert_fresh_account(pool: &MySqlPool) -> [u8; 16] {
        let id: [u8; 16] = uuid::Uuid::new_v4().into_bytes();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(&id[..])
            .execute(pool)
            .await
            .expect("insert fresh account");
        id
    }

    /// Insert an existing (non-fresh) account created two hours ago — older than
    /// the default one-hour guest window.
    async fn insert_old_account(pool: &MySqlPool) -> [u8; 16] {
        let id: [u8; 16] = uuid::Uuid::new_v4().into_bytes();
        sqlx::query("INSERT INTO accounts (id, created_at) VALUES (?, NOW() - INTERVAL 2 HOUR)")
            .bind(&id[..])
            .execute(pool)
            .await
            .expect("insert old account");
        id
    }

    fn armed_cfg(burst_limit: u64) -> ThrottleConfig {
        ThrottleConfig {
            guest_age_secs: 3_600,
            window_secs: 60,
            burst_limit,
            throttle_secs: 300,
        }
    }

    /// Test (a): a burst of > burst_limit distinct fresh accounts on ONE
    /// call_id trips the throttle; afterwards EVERY admission (a further fresh
    /// account AND an existing account) is refused.
    #[tokio::test]
    async fn burst_of_fresh_accounts_trips_and_refuses_all() {
        let Some(pool) = test_pool("burst_of_fresh_accounts_trips_and_refuses_all").await else {
            return;
        };
        let cfg = armed_cfg(3);
        let call_id: [u8; 16] = uuid::Uuid::new_v4().into_bytes();

        // The first `burst_limit` fresh accounts mint fine (count 1,2,3; not > 3).
        for _ in 0..3 {
            let acct = insert_fresh_account(&pool).await;
            assert_eq!(
                check_admission(&pool, cfg, &call_id, &acct).await,
                ThrottleDecision::Allow
            );
        }
        // The 4th distinct fresh account (count 4 > 3) trips the throttle.
        let tripper = insert_fresh_account(&pool).await;
        assert_eq!(
            check_admission(&pool, cfg, &call_id, &tripper).await,
            ThrottleDecision::Throttled
        );
        // A further fresh account is now refused (call is throttled).
        let later_fresh = insert_fresh_account(&pool).await;
        assert_eq!(
            check_admission(&pool, cfg, &call_id, &later_fresh).await,
            ThrottleDecision::Throttled
        );
        // An EXISTING (non-fresh) account is ALSO refused — a throttled call
        // refuses all admissions.
        let existing = insert_old_account(&pool).await;
        assert_eq!(
            check_admission(&pool, cfg, &call_id, &existing).await,
            ThrottleDecision::Throttled
        );
    }

    /// Test (b): a legit call of several EXISTING (non-fresh) accounts is never
    /// throttled, even beyond burst_limit — non-fresh accounts are not recorded
    /// or counted.
    #[tokio::test]
    async fn existing_accounts_are_never_throttled() {
        let Some(pool) = test_pool("existing_accounts_are_never_throttled").await else {
            return;
        };
        let cfg = armed_cfg(3);
        let call_id: [u8; 16] = uuid::Uuid::new_v4().into_bytes();
        for _ in 0..6 {
            let acct = insert_old_account(&pool).await;
            assert_eq!(
                check_admission(&pool, cfg, &call_id, &acct).await,
                ThrottleDecision::Allow
            );
        }
    }

    /// Test (c): fewer-than-limit fresh accounts all mint fine (count reaches
    /// exactly burst_limit, which is not > burst_limit).
    #[tokio::test]
    async fn fresh_accounts_under_limit_all_allowed() {
        let Some(pool) = test_pool("fresh_accounts_under_limit_all_allowed").await else {
            return;
        };
        let cfg = armed_cfg(3);
        let call_id: [u8; 16] = uuid::Uuid::new_v4().into_bytes();
        for _ in 0..3 {
            let acct = insert_fresh_account(&pool).await;
            assert_eq!(
                check_admission(&pool, cfg, &call_id, &acct).await,
                ThrottleDecision::Allow
            );
        }
    }

    /// Test (d): with burst_limit = 0 the throttle is fully disabled — a large
    /// burst of fresh accounts on one call_id is all allowed and nothing is
    /// recorded.
    #[tokio::test]
    async fn disabled_config_allows_unbounded_fresh_burst() {
        let Some(pool) = test_pool("disabled_config_allows_unbounded_fresh_burst").await else {
            return;
        };
        let cfg = ThrottleConfig::disabled();
        let call_id: [u8; 16] = uuid::Uuid::new_v4().into_bytes();
        for _ in 0..12 {
            let acct = insert_fresh_account(&pool).await;
            assert_eq!(
                check_admission(&pool, cfg, &call_id, &acct).await,
                ThrottleDecision::Allow
            );
        }
        // Nothing was recorded (the disabled path never touches the tables).
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM call_admission_events WHERE call_id = ?")
                .bind(&call_id[..])
                .fetch_one(&pool)
                .await
                .expect("count events");
        assert_eq!(events, 0);
    }
}
