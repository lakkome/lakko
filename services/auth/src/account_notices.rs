// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Telling the account's own devices what happened to it ([ADR-0075] section
//! 9).
//!
//! `account_notices (id, account_id, kind, at_ms)` is a server table because
//! the device that acts (a new device holding a mailed code) may hold no keys
//! and so cannot tell the others inside the account's `DeviceGroup`. A notice
//! carries a kind and a time and nothing else: no address, no device detail.
//!
//! `GET /account/notices?after=<id>&since_ms=<unix ms>` (bearer, both
//! optional) returns the notices with an id above `after` and a time at or
//! after `since_ms`, oldest first, at most [`PAGE_LIMIT`] of them; a client
//! fetches at session landing, keeps the highest id it has shown as a local
//! cursor and asks again from there. A device with no cursor omits `after`
//! and sets `since_ms` to thirty days ago. Rows are pruned after
//! [`RETENTION_MS`] (60 days).
//!
//! The route is not gated on the recovery-email pepper: a "recovery phrase
//! saved" notice is written on a server that sends no mail too.
//!
//! [ADR-0075]: ../../../docs/adr/0075-recovery-email.md

use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::Json;
use axum::Extension;
use serde::{Deserialize, Serialize};
use sqlx::{MySql, MySqlPool, Transaction};

use crate::session::now_ms;
use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Most notices one `GET /account/notices` returns.
pub(crate) const PAGE_LIMIT: i64 = 100;

/// How long a notice is kept: 60 days (ADR-0075 section 9).
pub(crate) const RETENTION_MS: i64 = 60 * 86_400_000;

/// How often the pruner runs.
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

/// What a notice says. The discriminant is stored in `account_notices.kind`;
/// never renumber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum NoticeKind {
    /// The password was replaced with a mailed code
    /// (`/recovery-email/recover/password/finish`).
    PasswordChangedByEmail = 1,
    /// The email vault left the server in answer to a mailed code
    /// (`/recovery-email/recover/open`, vault mode).
    AccountOpenedByEmail = 2,
    /// A recovery email was added.
    EmailAdded = 3,
    /// The recovery email was replaced with another address.
    EmailReplaced = 4,
    /// The recovery email was removed (by the person, or by a change of
    /// username).
    EmailRemoved = 5,
    /// A recovery phrase was saved for the account.
    PhraseSaved = 6,
}

impl NoticeKind {
    /// The wire name.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PasswordChangedByEmail => "password-changed-by-email",
            Self::AccountOpenedByEmail => "account-opened-by-email",
            Self::EmailAdded => "recovery-email-added",
            Self::EmailReplaced => "recovery-email-replaced",
            Self::EmailRemoved => "recovery-email-removed",
            Self::PhraseSaved => "recovery-phrase-saved",
        }
    }

    const fn from_db(kind: u8) -> Option<Self> {
        Some(match kind {
            1 => Self::PasswordChangedByEmail,
            2 => Self::AccountOpenedByEmail,
            3 => Self::EmailAdded,
            4 => Self::EmailReplaced,
            5 => Self::EmailRemoved,
            6 => Self::PhraseSaved,
            _ => return None,
        })
    }
}

/// Write one notice for `account_id` inside the caller's transaction, so a
/// change never lands without its notice.
///
/// # Errors
///
/// Any `sqlx::Error` from the insert.
pub(crate) async fn write(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
    kind: NoticeKind,
    at_ms: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO account_notices (account_id, kind, at_ms) VALUES (?, ?, ?)")
        .bind(&account_id[..])
        .bind(kind as u8)
        .bind(at_ms)
        .execute(&mut **tx)
        .await
        .map(|_| ())
}

/// Mark the account phrase-protected (`accounts.phrase_saved`) and, when it
/// was not already, write the "recovery phrase saved" notice. The caller's
/// transaction holds the account row's lock and has deleted the email vault.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn mark_phrase_saved(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
    at_ms: i64,
) -> Result<(), sqlx::Error> {
    let newly =
        sqlx::query("UPDATE accounts SET phrase_saved = 1 WHERE id = ? AND phrase_saved = 0")
            .bind(&account_id[..])
            .execute(&mut **tx)
            .await?
            .rows_affected()
            == 1;
    if newly {
        write(tx, account_id, NoticeKind::PhraseSaved, at_ms).await?;
    }
    Ok(())
}

/// Query of `GET /account/notices`.
#[derive(Deserialize)]
pub(crate) struct ListQuery {
    /// Return notices with an id above this. Absent: from the oldest kept.
    #[serde(default)]
    pub(crate) after: Option<u64>,
    /// Return only notices at or after this time (unix milliseconds). For a
    /// device with no cursor yet: it omits `after` and asks for the last 30
    /// days, so it does not show everything the table still keeps. Combines
    /// with `after`; paging is unchanged (pass `since_ms` again with the
    /// `next_after` of each page). Absent: no lower bound in time.
    #[serde(default)]
    pub(crate) since_ms: Option<i64>,
}

/// One notice on the wire.
#[derive(Serialize)]
pub(crate) struct Notice {
    /// Increasing per server; the client's cursor.
    pub(crate) id: u64,
    /// One of `password-changed-by-email`, `account-opened-by-email`,
    /// `recovery-email-added`, `recovery-email-replaced`,
    /// `recovery-email-removed`, `recovery-phrase-saved`.
    pub(crate) kind: &'static str,
    /// Unix milliseconds of the event.
    pub(crate) at_ms: i64,
}

/// Answer of `GET /account/notices`.
#[derive(Serialize)]
pub(crate) struct ListRes {
    /// Oldest first, at most [`PAGE_LIMIT`].
    pub(crate) notices: Vec<Notice>,
    /// The cursor to pass as `after` next: the highest id returned, or the
    /// request's own `after` when the page is empty.
    pub(crate) next_after: u64,
    /// Whether more notices were waiting past this page.
    pub(crate) more: bool,
}

/// `GET /account/notices?after=<id>&since_ms=<unix ms>` — bearer. The session's account only.
///
/// # Errors
///
/// [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ListRes>, ApiError> {
    let after = query.after.unwrap_or(0);
    let rows: Vec<(u64, u8, i64)> = sqlx::query_as(
        "SELECT id, kind, at_ms FROM account_notices \
         WHERE account_id = ? AND id > ? AND at_ms >= ? ORDER BY id LIMIT ?",
    )
    .bind(&auth.0[..])
    .bind(after)
    .bind(query.since_ms.unwrap_or(i64::MIN))
    .bind(PAGE_LIMIT + 1)
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::Db)?;
    let page = usize::try_from(PAGE_LIMIT).unwrap_or(usize::MAX);
    let more = rows.len() > page;
    // The cursor moves past every row of the page, known kind or not.
    let next_after = rows
        .iter()
        .take(page)
        .next_back()
        .map_or(after, |row| row.0);
    let notices: Vec<Notice> = rows
        .into_iter()
        .take(page)
        // A kind this build does not know (written by a newer one) is
        // skipped; the cursor above has moved past it.
        .filter_map(|(id, kind, at_ms)| {
            NoticeKind::from_db(kind).map(|kind| Notice {
                id,
                kind: kind.as_str(),
                at_ms,
            })
        })
        .collect();
    Ok(Json(ListRes {
        notices,
        next_after,
        more,
    }))
}

/// Hourly pruner: notices older than [`RETENTION_MS`].
pub(crate) fn spawn_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    const JOB: &str = crate::metrics::jobs::ACCOUNT_NOTICE_PRUNE;
    lakko_metrics::job_registered(JOB);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = prune(&pool, now_ms()).await;
            if let Err(e) = &result {
                tracing::warn!(error = %e, "account_notices prune failed");
            }
            crate::metrics::job_ran(JOB, result.is_ok());
        }
    })
}

/// Delete the notices older than the retention at `now`; returns how many.
///
/// # Errors
///
/// Any `sqlx::Error` from the delete.
pub(crate) async fn prune(pool: &MySqlPool, now: i64) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM account_notices WHERE at_ms < ?")
        .bind(now.saturating_sub(RETENTION_MS))
        .execute(pool)
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_pinned_and_round_trip() {
        let all = [
            (NoticeKind::PasswordChangedByEmail, 1u8),
            (NoticeKind::AccountOpenedByEmail, 2),
            (NoticeKind::EmailAdded, 3),
            (NoticeKind::EmailReplaced, 4),
            (NoticeKind::EmailRemoved, 5),
            (NoticeKind::PhraseSaved, 6),
        ];
        for (kind, stored) in all {
            assert_eq!(kind as u8, stored);
            assert_eq!(NoticeKind::from_db(stored), Some(kind));
        }
        assert_eq!(NoticeKind::from_db(0), None);
        assert_eq!(NoticeKind::from_db(7), None);
    }
}
