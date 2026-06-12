// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! KT-log S2 — best-effort append to the transparency log on every
//! identity-key CHANGE.
//!
//! The discovery service is the only writer of the key-transparency log:
//! whenever an account's published Ed25519 identity key CHANGES (first
//! publish, a proven rotation, or an unproven new-key reset), discovery
//! appends a leaf to `services/transparency` so a client can later prove
//! the operator never equivocated about that account's key history.
//!
//! ## Account-key derivation (decision C)
//!
//! The log leaf's `username_hash` field carries
//!
//! ```text
//! BLAKE3("lakko/transparency/account/v1" || account_id)
//! ```
//!
//! — a 32-byte key derivable by BOTH discovery (on append) AND a client
//! (on verify), because `account_id` is already public. The transparency
//! service treats this as 32 opaque bytes and never learns the
//! account_id-to-username mapping, so the log leaks nothing the public
//! `account_id` did not already.
//!
//! ## Consistency model (decision E) — best-effort, fail-open
//!
//! The identity bundle is published FIRST (the discovery upsert). The
//! append here is BEST-EFFORT: on any failure (transparency down,
//! non-2xx, network error) we `warn!` and PROCEED — we NEVER fail the
//! discovery upload because of a transparency-append failure. A missed
//! append is NOT silently lost: it is closed by the S2.5 reconciliation
//! sweep ([`spawn_transparency_reconciler`]), which periodically scans the
//! `identity_bundles` table for accounts whose CURRENT key is not yet
//! confirmed in the log (the `logged_pubkey` marker is NULL or stale) and
//! re-appends. The transparency append is idempotent (KT-log S2.5), so a
//! re-append of an already-logged key is a no-op (no duplicate leaf).
//!
//! When `LAKKO_TRANSPARENCY_URL` is unset the append is skipped entirely
//! (the log integration is off — logged once at startup, not per
//! request).

use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::Serialize;

/// Domain-separation string for the account-key derivation (decision C).
/// Both discovery (append) and the client (verify) MUST use this exact
/// string so the derived `username_hash` matches. Changing it is a wire
/// break.
pub(crate) const ACCOUNT_KEY_DOMAIN: &str = "lakko/transparency/account/v1";

/// Derive the transparency-log `username_hash` for an account:
/// `BLAKE3("lakko/transparency/account/v1" || account_id)`.
///
/// The domain string is PREPENDED to the raw `account_id` bytes and the
/// concatenation is hashed with plain (un-keyed) BLAKE3 — `account_id`
/// is public, so the result reveals nothing new; the domain separator
/// only stops this hash from colliding with any other BLAKE3 use of the
/// same `account_id`.
#[must_use]
pub(crate) fn account_key(account_id: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(ACCOUNT_KEY_DOMAIN.len() + account_id.len());
    input.extend_from_slice(ACCOUNT_KEY_DOMAIN.as_bytes());
    input.extend_from_slice(account_id);
    lakko_crypto::hash::blake3(&input)
}

/// Configuration + HTTP client for the transparency append integration.
/// `None` in `AppState` when `LAKKO_TRANSPARENCY_URL` is unset (the
/// integration is off).
#[derive(Clone)]
pub(crate) struct TransparencyClient {
    http: reqwest::Client,
    /// Base URL of the transparency service, e.g.
    /// `http://127.0.0.1:8087`. No trailing slash.
    base_url: Arc<str>,
    /// The shared service secret presented as
    /// `Authorization: Bearer <secret-b64>` — the SAME value the
    /// transparency service holds in `LAKKO_TRANSPARENCY_APPEND_SECRET`.
    /// Stored already base64-encoded so the header is cheap to build.
    secret_b64: Arc<str>,
}

/// The `POST /transparency/append` request body (decision B — the
/// version is assigned server-side, so it is NOT sent).
#[derive(Serialize)]
struct AppendReq<'a> {
    username_hash_b64: &'a str,
    identity_pubkey_b64: &'a str,
    is_reset: bool,
}

impl TransparencyClient {
    /// Build a client from the raw env values. `base_url` is the
    /// `LAKKO_TRANSPARENCY_URL` value; `secret_b64` is the
    /// `LAKKO_TRANSPARENCY_APPEND_SECRET` value (passed through verbatim
    /// — the transparency side base64-decodes it).
    ///
    /// # Errors
    ///
    /// Returns an error if the reqwest client cannot be built (TLS
    /// backend init failure).
    pub(crate) fn new(base_url: &str, secret_b64: &str) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            http,
            base_url: Arc::from(base_url.trim_end_matches('/')),
            secret_b64: Arc::from(secret_b64),
        })
    }

    /// Best-effort append of an identity-key CHANGE to the transparency
    /// log. NEVER returns an error — every failure mode is logged at
    /// `warn` and swallowed (decision E, fail-open). A missed append is
    /// closed by the S2.5 reconciliation sweep, not lost.
    ///
    /// Returns `true` iff the append was CONFIRMED (a 2xx response).
    /// `false` on any failure (service unreachable, non-2xx, network
    /// error). The caller acts on `true` — the hot-path `upload` advances
    /// the `logged_pubkey` marker only on a confirmed append; the
    /// reconciler advances its marker likewise. A `false` is fail-open:
    /// the caller proceeds (NEVER fails the upload) and leaves the marker
    /// stale so the next reconciliation sweep retries. The transparency
    /// append is idempotent (KT-log S2.5), so a retry of an
    /// already-logged pubkey is a no-op (no duplicate leaf).
    pub(crate) async fn append_key_change(
        &self,
        account_id: &[u8],
        signature_pubkey: &[u8],
        is_reset: bool,
    ) -> bool {
        let username_hash = account_key(account_id);
        let body = AppendReq {
            username_hash_b64: &B64.encode(username_hash),
            identity_pubkey_b64: &B64.encode(signature_pubkey),
            is_reset,
        };
        let url = format!("{}/transparency/append", self.base_url);

        let result = self
            .http
            .post(&url)
            .bearer_auth(&*self.secret_b64)
            .json(&body)
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(
                    account_id = %B64.encode(account_id),
                    is_reset,
                    "transparency append ok",
                );
                true
            },
            Ok(resp) => {
                // Non-2xx — log the status; the upload already succeeded so
                // we proceed. Reconciliation (S2.5) will back-fill.
                tracing::warn!(
                    account_id = %B64.encode(account_id),
                    status = %resp.status(),
                    "transparency append non-2xx — proceeding (S2.5 reconciliation will back-fill)",
                );
                false
            },
            Err(e) => {
                // Network / timeout / connect error. Same posture.
                tracing::warn!(
                    account_id = %B64.encode(account_id),
                    error = %e,
                    "transparency append failed — proceeding (S2.5 reconciliation will back-fill)",
                );
                false
            },
        }
    }
}

/// KT-log S2.5 — the per-sweep batch size for the reconciler scan. Each
/// tick reconciles at most this many un-logged / stale accounts; the next
/// tick continues. A small bound keeps each sweep cheap and the
/// transparency service un-flooded.
const RECONCILE_BATCH: i64 = 256;

/// Spawn the KT-log S2.5 reconciliation sweep — a background task that
/// eventually appends every account's CURRENT published identity to the
/// transparency log, closing the S2 best-effort-append fail-open gap.
///
/// Mirrors [`crate::pow::spawn_pruner`]'s shape: `tokio::spawn` + a
/// `tokio::time::interval`, first tick skipped, then tick-then-loop. The
/// task is aborted when the service stops (the returned handle is held in
/// `main`).
///
/// ## Each sweep
///
/// 1. SELECT up to [`RECONCILE_BATCH`] `(account_id, signature_pubkey)`
///    rows where `logged_pubkey IS NULL OR logged_pubkey <> signature_pubkey`
///    — the un-logged / stale accounts. (On a huge table this is a cheap
///    scan but not index-backed; a follow-up could add a cursor / partial
///    index. Noted, out of scope here.)
/// 2. For each row, call
///    [`TransparencyClient::append_key_change`]`(account_id,
///    signature_pubkey, is_reset = false)`. **`is_reset` is conservatively
///    `false`** on a reconciled backfill: the reconciler runs for entries
///    whose hot-path append failed and it does NOT know whether the current
///    identity was published as an unproven reset. Logging a reset as a
///    non-reset is fail-SAFE — a client treats the change as needing
///    out-of-band re-verification rather than silently trusting it — so
///    `false` is the safe default. The transparency append is idempotent
///    (KT-log S2.5), so re-appending an already-logged pubkey is a no-op.
/// 3. On a CONFIRMED append, advance the marker
///    (`UPDATE … SET logged_pubkey = signature_pubkey WHERE account_id = ?`).
///    On failure leave it stale so the next tick retries.
///
/// The sweep tallies how many accounts it reconciled and emits an `info!`
/// per non-empty sweep (and a `warn!` count of append failures), so a
/// stuck reconciler is observable.
pub(crate) fn spawn_transparency_reconciler(
    pool: sqlx::MySqlPool,
    client: TransparencyClient,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // The first tick fires immediately — skip it so we don't sweep the
        // instant the service boots (the DB may still be warming up).
        ticker.tick().await;
        loop {
            ticker.tick().await;
            reconcile_once(&pool, &client).await;
        }
    })
}

/// One reconciliation sweep. Factored out of the spawn loop so the body is
/// testable and the loop stays a thin scheduler. Best-effort throughout: a
/// failed SELECT is logged and the sweep ends (the next tick retries); a
/// failed append leaves the marker stale; a failed marker UPDATE is logged
/// and the account is simply reconciled again next tick (the idempotent
/// append makes that harmless).
async fn reconcile_once(pool: &sqlx::MySqlPool, client: &TransparencyClient) {
    // The un-logged / stale accounts: never-confirmed (NULL) or whose
    // current key differs from the last confirmed one.
    let rows: Vec<(Vec<u8>, Vec<u8>)> = match sqlx::query_as(
        "SELECT account_id, signature_pubkey FROM identity_bundles \
         WHERE logged_pubkey IS NULL OR logged_pubkey <> signature_pubkey \
         LIMIT ?",
    )
    .bind(RECONCILE_BATCH)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "KT-log S2.5 reconciler scan failed — retrying next sweep");
            return;
        },
    };

    if rows.is_empty() {
        return;
    }

    let mut reconciled: u64 = 0;
    let mut append_failures: u64 = 0;
    for (account_id, signature_pubkey) in &rows {
        // is_reset = false: conservative backfill (see fn docs) — a lost
        // reset-marker makes a client re-verify out-of-band (fail-SAFE).
        let confirmed = client
            .append_key_change(account_id, signature_pubkey, false)
            .await;
        if !confirmed {
            append_failures += 1;
            // Leave the marker stale; the next sweep retries.
            continue;
        }

        // Advance the marker so the next sweep skips this account.
        let updated =
            sqlx::query("UPDATE identity_bundles SET logged_pubkey = ? WHERE account_id = ?")
                .bind(&signature_pubkey[..])
                .bind(&account_id[..])
                .execute(pool)
                .await;
        match updated {
            Ok(_) => reconciled += 1,
            Err(e) => {
                // The append landed but the marker did not advance. Harmless:
                // the next sweep reconciles this account again and the
                // idempotent append makes the re-append a no-op.
                tracing::warn!(
                    account_id = %B64.encode(account_id),
                    error = %e,
                    "KT-log S2.5 reconciler appended but failed to advance logged_pubkey \
                     — will retry (idempotent, no duplicate)",
                );
            },
        }
    }

    if append_failures > 0 {
        tracing::warn!(
            append_failures,
            scanned = rows.len(),
            "KT-log S2.5 reconciler: some appends failed — left stale for the next sweep",
        );
    }
    tracing::info!(
        reconciled,
        scanned = rows.len(),
        "KT-log S2.5 reconciliation sweep complete",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_key_is_domain_separated_and_deterministic() {
        let id = [0xABu8; 16];
        let k1 = account_key(&id);
        let k2 = account_key(&id);
        // Deterministic.
        assert_eq!(k1, k2);

        // Domain-separated: hashing the bare account_id (no domain
        // prefix) must NOT collide with the derived key.
        let bare = lakko_crypto::hash::blake3(&id);
        assert_ne!(k1, bare);

        // Pinned domain string — a change here is a wire break with the
        // client verifier.
        assert_eq!(ACCOUNT_KEY_DOMAIN, "lakko/transparency/account/v1");

        // Sanity: matches an explicit "domain || id" hash.
        let mut expected_input = Vec::new();
        expected_input.extend_from_slice(b"lakko/transparency/account/v1");
        expected_input.extend_from_slice(&id);
        assert_eq!(k1, lakko_crypto::hash::blake3(&expected_input));
    }

    #[test]
    fn distinct_accounts_derive_distinct_keys() {
        assert_ne!(account_key(&[0u8; 16]), account_key(&[1u8; 16]));
    }

    /// KT-log S2.5 — the reconciler scan SELECT returns exactly the
    /// un-logged (NULL marker) and stale (marker != current key) accounts,
    /// and SKIPS an up-to-date account (marker == current key). DB-gated on
    /// `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skips, does not fail, when
    /// unset). This pins the predicate the live `reconcile_once` scan uses.
    #[tokio::test]
    async fn reconciler_scan_selects_unlogged_and_stale_only() {
        use sqlx::mysql::MySqlPoolOptions;

        let Ok(url) = std::env::var("LAKKO_DISCOVERY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping reconciler_scan_selects_unlogged_and_stale_only: \
                 LAKKO_DISCOVERY_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");

        // Three isolated accounts so this test never collides with other
        // rows. Clean them up at the end.
        let acct_null = [0xA0u8; 16]; // logged_pubkey NULL -> selected
        let acct_stale = [0xA1u8; 16]; // logged_pubkey != current -> selected
        let acct_uptodate = [0xA2u8; 16]; // logged_pubkey == current -> skipped
        let key_current = [0x11u8; 32];
        let key_old = [0x22u8; 32];
        let x25519 = [0x33u8; 32];

        for id in [&acct_null, &acct_stale, &acct_uptodate] {
            sqlx::query("DELETE FROM identity_bundles WHERE account_id = ?")
                .bind(&id[..])
                .execute(&pool)
                .await
                .expect("clear pre-existing row");
        }

        // NULL marker — never confirmed.
        sqlx::query(
            "INSERT INTO identity_bundles \
                (account_id, signature_pubkey, x25519_pubkey, logged_pubkey) \
             VALUES (?, ?, ?, NULL)",
        )
        .bind(&acct_null[..])
        .bind(&key_current[..])
        .bind(&x25519[..])
        .execute(&pool)
        .await
        .expect("insert null-marker row");

        // Stale marker — confirmed an OLD key, current key differs.
        sqlx::query(
            "INSERT INTO identity_bundles \
                (account_id, signature_pubkey, x25519_pubkey, logged_pubkey) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&acct_stale[..])
        .bind(&key_current[..])
        .bind(&x25519[..])
        .bind(&key_old[..])
        .execute(&pool)
        .await
        .expect("insert stale-marker row");

        // Up-to-date marker — confirmed the CURRENT key.
        sqlx::query(
            "INSERT INTO identity_bundles \
                (account_id, signature_pubkey, x25519_pubkey, logged_pubkey) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&acct_uptodate[..])
        .bind(&key_current[..])
        .bind(&x25519[..])
        .bind(&key_current[..])
        .execute(&pool)
        .await
        .expect("insert up-to-date row");

        // The SAME predicate `reconcile_once` scans with.
        let rows: Vec<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
            "SELECT account_id, signature_pubkey FROM identity_bundles \
             WHERE (logged_pubkey IS NULL OR logged_pubkey <> signature_pubkey) \
               AND account_id IN (?, ?, ?)",
        )
        .bind(&acct_null[..])
        .bind(&acct_stale[..])
        .bind(&acct_uptodate[..])
        .fetch_all(&pool)
        .await
        .expect("scan");

        let selected: std::collections::HashSet<Vec<u8>> =
            rows.into_iter().map(|(id, _)| id).collect();
        assert!(
            selected.contains(&acct_null[..]),
            "NULL marker must be selected"
        );
        assert!(
            selected.contains(&acct_stale[..]),
            "stale marker must be selected"
        );
        assert!(
            !selected.contains(&acct_uptodate[..]),
            "up-to-date marker must be SKIPPED",
        );
        assert_eq!(
            selected.len(),
            2,
            "exactly the two un-logged/stale accounts"
        );

        for id in [&acct_null, &acct_stale, &acct_uptodate] {
            sqlx::query("DELETE FROM identity_bundles WHERE account_id = ?")
                .bind(&id[..])
                .execute(&pool)
                .await
                .expect("cleanup row");
        }
        pool.close().await;
    }
}
