// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A deleted account is purged from the other services by the server.
//!
//! An account's rows are spread over three databases: this one, the
//! directory in `services/discovery` (username, identity bundle,
//! KeyPackages, search terms, public record) and the backup blob in
//! `services/storage`. Before this module the client deleted the other two
//! itself after `DELETE /account`, best-effort: a failed call left the
//! username and the published record behind for good.
//!
//! Now every account deletion (`DELETE /account`, the unclaimed-account
//! sweep) inserts a row into `account_purges` in the same transaction that
//! deletes the account, so the purge is owed exactly when the deletion took
//! place, and a refused deletion (a recall window, no fresh proof) owes
//! nothing. A worker spawned at startup takes due rows every
//! [`INTERVAL_VAR`] seconds and calls:
//!
//! - `POST {LAKKO_AUTH_DISCOVERY_INTERNAL_URL}/internal/accounts/{hex}/purge`
//! - `DELETE {LAKKO_AUTH_STORAGE_INTERNAL_URL}/internal/accounts/{hex}/backup`
//!
//! each with `x-lakko-internal-secret: <LAKKO_INTERNAL_PURGE_SECRET>` (the
//! one value all three services hold). Both routes are idempotent. Each
//! success is recorded on the row; a failure keeps the row and moves its next
//! attempt back by an exponential backoff (capped at [`MAX_BACKOFF_SECS`]);
//! the row is deleted when both are done. Nothing is ever given up.
//!
//! With the URLs and the secret unset the worker does not start, loudly, and
//! rows keep queueing: setting them later purges every account deleted in
//! between. Setting only some of the three is a startup error.
//!
//! Logs carry counts and the target's name only, never an account id (the
//! request URL names one, so a transport error is logged without its URL).

use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use sqlx::{MySql, MySqlPool};

/// Seconds between worker passes.
pub(crate) const INTERVAL_VAR: &str = "LAKKO_AUTH_PURGE_INTERVAL_SECS";
/// The directory service's internal listener, e.g. `http://127.0.0.1:8094`.
pub(crate) const DISCOVERY_URL_VAR: &str = "LAKKO_AUTH_DISCOVERY_INTERNAL_URL";
/// The storage service's internal listener, e.g. `http://127.0.0.1:8095`.
pub(crate) const STORAGE_URL_VAR: &str = "LAKKO_AUTH_STORAGE_INTERNAL_URL";
/// The shared secret, base64 of at least [`MIN_SECRET_LEN`] bytes. The same
/// variable name is read by discovery and storage.
pub(crate) const SECRET_VAR: &str = "LAKKO_INTERNAL_PURGE_SECRET";
/// The header the secret is presented in.
pub(crate) const SECRET_HEADER: &str = "x-lakko-internal-secret";
/// Minimum decoded length of the secret.
pub(crate) const MIN_SECRET_LEN: usize = 16;

/// Default seconds between passes.
pub(crate) const DEFAULT_INTERVAL_SECS: u64 = 60;
/// The longest a failing row waits between attempts: six hours.
pub(crate) const MAX_BACKOFF_SECS: u64 = 6 * 3600;
/// The most rows one pass takes; the next pass takes the rest.
const BATCH: u32 = 200;
/// Per-request timeout towards discovery and storage.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Where and how to reach the two services.
#[derive(Clone)]
pub(crate) struct PurgeConfig {
    /// Base URL of discovery's internal listener, no trailing slash.
    pub(crate) discovery_url: String,
    /// Base URL of storage's internal listener, no trailing slash.
    pub(crate) storage_url: String,
    /// The secret as sent in [`SECRET_HEADER`] (canonical base64).
    pub(crate) secret_header: Arc<str>,
    /// Seconds between passes; also the first backoff step.
    pub(crate) interval_secs: u64,
}

impl PurgeConfig {
    /// Read the configuration from the environment.
    ///
    /// # Errors
    ///
    /// When only some of the three variables are set, the secret is not
    /// base64 of at least [`MIN_SECRET_LEN`] bytes, a URL is not `http(s)`,
    /// or the interval is not a positive whole number.
    pub(crate) fn from_env() -> anyhow::Result<Option<Self>> {
        let var = |name: &str| std::env::var(name).ok();
        Self::from_lookup(&var)
    }

    fn from_lookup(var: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let get = |name: &str| {
            var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let interval_secs = match get(INTERVAL_VAR) {
            None => DEFAULT_INTERVAL_SECS,
            Some(raw) => match raw.parse::<u64>() {
                Ok(n) if n > 0 => n,
                _ => anyhow::bail!("{INTERVAL_VAR} must be a positive whole number of seconds"),
            },
        };
        let discovery = get(DISCOVERY_URL_VAR);
        let storage = get(STORAGE_URL_VAR);
        let secret = get(SECRET_VAR);
        let (discovery_url, storage_url, secret) = match (discovery, storage, secret) {
            (None, None, None) => return Ok(None),
            (Some(d), Some(s), Some(k)) => (d, s, k),
            _ => anyhow::bail!(
                "{DISCOVERY_URL_VAR}, {STORAGE_URL_VAR} and {SECRET_VAR} must be set together \
                 or not at all"
            ),
        };
        let bytes = B64
            .decode(secret.as_bytes())
            .map_err(|_| anyhow::anyhow!("{SECRET_VAR} is not valid base64"))?;
        if bytes.len() < MIN_SECRET_LEN {
            anyhow::bail!(
                "{SECRET_VAR} decodes to {} bytes; need >= {MIN_SECRET_LEN}",
                bytes.len()
            );
        }
        for (name, url) in [
            (DISCOVERY_URL_VAR, &discovery_url),
            (STORAGE_URL_VAR, &storage_url),
        ] {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                anyhow::bail!("{name} must be an http:// or https:// URL");
            }
        }
        Ok(Some(Self {
            discovery_url: discovery_url.trim_end_matches('/').to_owned(),
            storage_url: storage_url.trim_end_matches('/').to_owned(),
            secret_header: B64.encode(&bytes).into(),
            interval_secs,
        }))
    }
}

/// Queue the purge of `account_id`, inside the caller's transaction (the one
/// that deletes the account). A row already there is left as it is.
///
/// # Errors
///
/// A database error; the caller's transaction then rolls back.
pub(crate) async fn enqueue(
    conn: &mut sqlx::MySqlConnection,
    account_id: &[u8; 16],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO account_purges (account_id) VALUES (?) \
         ON DUPLICATE KEY UPDATE account_id = account_id",
    )
    .bind(account_id.as_slice())
    .execute(conn)
    .await?;
    Ok(())
}

/// Seconds a row waits after its `attempts`-th failed attempt:
/// `interval * 2^(attempts - 1)`, capped at [`MAX_BACKOFF_SECS`].
pub(crate) fn backoff_secs(interval_secs: u64, attempts: u32) -> u64 {
    let shift = attempts.saturating_sub(1).min(20);
    interval_secs
        .saturating_mul(1u64 << shift)
        .min(MAX_BACKOFF_SECS)
}

/// What one pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PassReport {
    /// Rows whose purge finished (row deleted).
    pub(crate) completed: u64,
    /// Rows with at least one target still failing (row kept, backed off).
    pub(crate) retrying: u64,
}

/// Whether one row finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Both services purged; the row is gone.
    Completed,
    /// At least one service failed; the row stays.
    Retrying,
}

/// The two services, as the worker names them in logs.
#[derive(Debug, Clone, Copy)]
enum Target {
    Discovery,
    Storage,
}

impl Target {
    const fn name(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Storage => "storage",
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Build the HTTP client the worker uses.
///
/// # Errors
///
/// When the TLS backend cannot be initialised.
pub(crate) fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// Ask one service to purge `account_id`. `true` on a 2xx answer.
async fn call(
    client: &reqwest::Client,
    config: &PurgeConfig,
    target: Target,
    account_id: &[u8; 16],
) -> bool {
    let id = hex(account_id);
    let request = match target {
        Target::Discovery => client.post(format!(
            "{}/internal/accounts/{id}/purge",
            config.discovery_url
        )),
        Target::Storage => client.delete(format!(
            "{}/internal/accounts/{id}/backup",
            config.storage_url
        )),
    };
    match request
        .header(SECRET_HEADER, config.secret_header.as_ref())
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => true,
        Ok(resp) => {
            tracing::warn!(
                target_service = target.name(),
                status = resp.status().as_u16(),
                "account purge refused"
            );
            false
        },
        Err(e) => {
            // The URL names the account: log the error without it.
            tracing::warn!(
                target_service = target.name(),
                error = %e.without_url(),
                "account purge request failed"
            );
            false
        },
    }
}

/// Purge one queued account: call each service not yet done, record each
/// success, then delete the row or back it off.
///
/// # Errors
///
/// A database error; the services may have been called (both routes are
/// idempotent, so the next pass repeats them harmlessly).
pub(crate) async fn process_one(
    pool: &MySqlPool,
    client: &reqwest::Client,
    config: &PurgeConfig,
    account_id: &[u8; 16],
) -> Result<Outcome, sqlx::Error> {
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT CAST(discovery_done_at IS NOT NULL AS SIGNED), \
                CAST(storage_done_at IS NOT NULL AS SIGNED) \
         FROM account_purges WHERE account_id = ?",
    )
    .bind(account_id.as_slice())
    .fetch_optional(pool)
    .await?;
    let Some((discovery_done, storage_done)) = row else {
        return Ok(Outcome::Completed);
    };
    let mut discovery_done = discovery_done != 0;
    let mut storage_done = storage_done != 0;

    if !discovery_done && call(client, config, Target::Discovery, account_id).await {
        sqlx::query(
            "UPDATE account_purges SET discovery_done_at = NOW() \
             WHERE account_id = ? AND discovery_done_at IS NULL",
        )
        .bind(account_id.as_slice())
        .execute(pool)
        .await?;
        discovery_done = true;
    }
    if !storage_done && call(client, config, Target::Storage, account_id).await {
        sqlx::query(
            "UPDATE account_purges SET storage_done_at = NOW() \
             WHERE account_id = ? AND storage_done_at IS NULL",
        )
        .bind(account_id.as_slice())
        .execute(pool)
        .await?;
        storage_done = true;
    }

    if discovery_done && storage_done {
        sqlx::query("DELETE FROM account_purges WHERE account_id = ?")
            .bind(account_id.as_slice())
            .execute(pool)
            .await?;
        return Ok(Outcome::Completed);
    }

    let attempts: u32 =
        sqlx::query_scalar("SELECT attempts FROM account_purges WHERE account_id = ?")
            .bind(account_id.as_slice())
            .fetch_optional(pool)
            .await?
            .unwrap_or(0);
    let attempts = attempts.saturating_add(1);
    sqlx::query(
        "UPDATE account_purges \
         SET attempts = ?, last_attempt_at = NOW(), \
             next_attempt_at = NOW() + INTERVAL ? SECOND \
         WHERE account_id = ?",
    )
    .bind(attempts)
    .bind(backoff_secs(config.interval_secs, attempts))
    .bind(account_id.as_slice())
    .execute(pool)
    .await?;
    Ok(Outcome::Retrying)
}

/// The account ids due now, oldest due first.
///
/// # Errors
///
/// A database error.
pub(crate) async fn due(pool: &MySqlPool, limit: u32) -> Result<Vec<[u8; 16]>, sqlx::Error> {
    let rows: Vec<Vec<u8>> = sqlx::query_scalar::<MySql, Vec<u8>>(
        "SELECT account_id FROM account_purges WHERE next_attempt_at <= NOW() \
         ORDER BY next_attempt_at LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| <[u8; 16]>::try_from(r.as_slice()).ok())
        .collect())
}

/// One pass over the due rows.
///
/// # Errors
///
/// A database error; the rows already processed keep their new state.
pub(crate) async fn run_once(
    pool: &MySqlPool,
    client: &reqwest::Client,
    config: &PurgeConfig,
) -> Result<PassReport, sqlx::Error> {
    let mut report = PassReport::default();
    for account_id in due(pool, BATCH).await? {
        match process_one(pool, client, config, &account_id).await? {
            Outcome::Completed => report.completed += 1,
            Outcome::Retrying => report.retrying += 1,
        }
    }
    Ok(report)
}

/// Start the worker, or warn that it is off when `config` is `None`.
pub(crate) fn spawn_worker(
    pool: MySqlPool,
    config: Option<PurgeConfig>,
) -> Option<tokio::task::JoinHandle<()>> {
    let Some(config) = config else {
        tracing::warn!(
            "account purge worker is OFF ({DISCOVERY_URL_VAR}, {STORAGE_URL_VAR}, {SECRET_VAR} \
             unset): deleted accounts are queued in account_purges but their discovery rows and \
             backup blobs are not removed until these are set"
        );
        return None;
    };
    let client = match client() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e.without_url(), "account purge worker: HTTP client");
            return None;
        },
    };
    tracing::info!(
        interval_secs = config.interval_secs,
        "account purge worker is on"
    );
    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(config.interval_secs));
        loop {
            interval.tick().await;
            match run_once(&pool, &client, &config).await {
                Ok(PassReport {
                    completed: 0,
                    retrying: 0,
                }) => {},
                Ok(report) => tracing::info!(
                    completed = report.completed,
                    retrying = report.retrying,
                    "account purge pass"
                ),
                Err(e) => tracing::warn!(error = %e, "account purge pass failed"),
            }
        }
    }))
}

/// Whether a purge row exists for `account_id` (tests).
#[cfg(test)]
pub(crate) async fn is_queued(pool: &MySqlPool, account_id: &[u8; 16]) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM account_purges WHERE account_id = ?")
        .bind(account_id.as_slice())
        .fetch_one(pool)
        .await
        .expect("count purge rows")
        == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{delete, post};
    use axum::Router;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const SECRET: &str = "cHVyZ2Utc2VjcmV0LWZvci10ZXN0cy0zMi1ieXRlcyE=";

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn nothing_set_is_dormant_and_a_partial_set_is_an_error() {
        assert!(PurgeConfig::from_lookup(&lookup(&[])).unwrap().is_none());
        for partial in [
            vec![(DISCOVERY_URL_VAR, "http://127.0.0.1:8094")],
            vec![(SECRET_VAR, SECRET)],
            vec![
                (DISCOVERY_URL_VAR, "http://127.0.0.1:8094"),
                (STORAGE_URL_VAR, "http://127.0.0.1:8095"),
            ],
        ] {
            assert!(PurgeConfig::from_lookup(&lookup(&partial)).is_err());
        }
    }

    #[test]
    fn a_full_set_parses_and_bad_values_are_refused() {
        let ok = PurgeConfig::from_lookup(&lookup(&[
            (DISCOVERY_URL_VAR, "http://127.0.0.1:8094/"),
            (STORAGE_URL_VAR, "http://127.0.0.1:8095"),
            (SECRET_VAR, SECRET),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(ok.discovery_url, "http://127.0.0.1:8094");
        assert_eq!(ok.interval_secs, DEFAULT_INTERVAL_SECS);
        assert_eq!(ok.secret_header.as_ref(), SECRET);

        let with = |secret: &str, url: &str, interval: &str| {
            PurgeConfig::from_lookup(&lookup(&[
                (DISCOVERY_URL_VAR, url),
                (STORAGE_URL_VAR, "http://127.0.0.1:8095"),
                (SECRET_VAR, secret),
                (INTERVAL_VAR, interval),
            ]))
        };
        assert!(with("not base64!", "http://x", "60").is_err());
        assert!(with("c2hvcnQ=", "http://x", "60").is_err());
        assert!(with(SECRET, "ftp://x", "60").is_err());
        assert!(with(SECRET, "http://x", "0").is_err());
        assert!(with(SECRET, "http://x", "soon").is_err());
        assert_eq!(
            with(SECRET, "http://x", "5")
                .unwrap()
                .unwrap()
                .interval_secs,
            5
        );
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!(backoff_secs(60, 1), 60);
        assert_eq!(backoff_secs(60, 2), 120);
        assert_eq!(backoff_secs(60, 5), 960);
        assert_eq!(backoff_secs(60, 30), MAX_BACKOFF_SECS);
        assert_eq!(backoff_secs(60, u32::MAX), MAX_BACKOFF_SECS);
    }

    async fn test_pool() -> Option<MySqlPool> {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping account-purge test: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return None;
        };
        let pool = MySqlPool::connect(&url).await.expect("connect");
        crate::MIGRATIONS.run(&pool).await.expect("migrate");
        Some(pool)
    }

    /// What a stub service saw, and how it should answer.
    #[derive(Default)]
    struct Stub {
        calls: Mutex<Vec<(String, String, Option<String>)>>,
        fail: Mutex<bool>,
    }

    async fn record(
        State(stub): State<Arc<Stub>>,
        method: axum::http::Method,
        Path(id): Path<String>,
        headers: HeaderMap,
    ) -> StatusCode {
        let secret = headers
            .get(SECRET_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        stub.calls
            .lock()
            .unwrap()
            .push((method.to_string(), id, secret));
        if *stub.fail.lock().unwrap() {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::NO_CONTENT
        }
    }

    /// One local server playing both services; returns its base URL.
    async fn serve(discovery: Arc<Stub>, storage: Arc<Stub>) -> String {
        let app = Router::new()
            .route(
                "/internal/accounts/{id}/purge",
                post(record).with_state(discovery),
            )
            .route(
                "/internal/accounts/{id}/backup",
                delete(record).with_state(storage),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    fn config(base: &str) -> PurgeConfig {
        PurgeConfig {
            discovery_url: base.to_owned(),
            storage_url: base.to_owned(),
            secret_header: SECRET.into(),
            interval_secs: 60,
        }
    }

    async fn queue(pool: &MySqlPool) -> [u8; 16] {
        let id = *uuid::Uuid::now_v7().as_bytes();
        let mut conn = pool.acquire().await.unwrap();
        enqueue(&mut conn, &id).await.unwrap();
        // A second enqueue of the same account is harmless.
        enqueue(&mut conn, &id).await.unwrap();
        id
    }

    #[tokio::test]
    async fn the_worker_calls_both_services_and_clears_the_row() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let (d, s) = (Arc::new(Stub::default()), Arc::new(Stub::default()));
        let cfg = config(&serve(d.clone(), s.clone()).await);
        let id = queue(&pool).await;
        assert!(due(&pool, 10_000).await.unwrap().contains(&id));

        let outcome = process_one(&pool, &client().unwrap(), &cfg, &id)
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Completed);
        assert!(!is_queued(&pool, &id).await);

        let expect = |stub: &Stub, method: &str| {
            let calls = stub.calls.lock().unwrap();
            assert_eq!(calls.len(), 1, "{method} called once");
            assert_eq!(calls[0].0, method);
            assert_eq!(calls[0].1, hex(&id));
            assert_eq!(calls[0].2.as_deref(), Some(SECRET));
        };
        expect(&d, "POST");
        expect(&s, "DELETE");
    }

    #[tokio::test]
    async fn a_failing_service_is_retried_and_the_row_is_kept() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let (d, s) = (Arc::new(Stub::default()), Arc::new(Stub::default()));
        *s.fail.lock().unwrap() = true;
        let cfg = config(&serve(d.clone(), s.clone()).await);
        let id = queue(&pool).await;
        let http = client().unwrap();

        assert_eq!(
            process_one(&pool, &http, &cfg, &id).await.unwrap(),
            Outcome::Retrying
        );
        assert!(is_queued(&pool, &id).await);
        let (attempts, discovery_done, storage_done, waits): (u32, i64, i64, i64) = sqlx::query_as(
            "SELECT attempts, CAST(discovery_done_at IS NOT NULL AS SIGNED), \
                        CAST(storage_done_at IS NOT NULL AS SIGNED), \
                        CAST(next_attempt_at > NOW() AS SIGNED) \
                 FROM account_purges WHERE account_id = ?",
        )
        .bind(id.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (attempts, discovery_done, storage_done, waits),
            (1, 1, 0, 1)
        );
        assert!(
            !due(&pool, 10_000).await.unwrap().contains(&id),
            "a backed-off row is not due"
        );

        // Storage recovers; the next attempt calls only storage.
        *s.fail.lock().unwrap() = false;
        assert_eq!(
            process_one(&pool, &http, &cfg, &id).await.unwrap(),
            Outcome::Completed
        );
        assert!(!is_queued(&pool, &id).await);
        assert_eq!(
            d.calls.lock().unwrap().len(),
            1,
            "discovery not called again"
        );
        assert_eq!(s.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_unreachable_service_keeps_the_row() {
        let Some(pool) = test_pool().await else {
            return;
        };
        // Port 1 on loopback: nothing listens there.
        let cfg = config("http://127.0.0.1:1");
        let id = queue(&pool).await;
        assert_eq!(
            process_one(&pool, &client().unwrap(), &cfg, &id)
                .await
                .unwrap(),
            Outcome::Retrying
        );
        assert!(is_queued(&pool, &id).await);
        sqlx::query("DELETE FROM account_purges WHERE account_id = ?")
            .bind(id.as_slice())
            .execute(&pool)
            .await
            .unwrap();
    }
}
