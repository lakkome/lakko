// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Where reports and the panel's case state are kept: MariaDB in
//! deployment, memory for tests and a database-less dev run (the
//! `services/channels` pattern).
//!
//! A report creates the case of its target, or reopens it when the case was
//! decided (ADR-0099 step 5). The rest of the trait is the moderators'
//! panel ([`crate::panel`]): case locks, the checks a moderator's browser
//! posts back, decisions, internal notes and the appeal outcomes it counts.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use sqlx::MySqlPool;

/// A report's status as its reporter sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStatus {
    /// Not decided yet.
    Received,
    /// The case was decided with an action.
    Action,
    /// The case was decided with no action.
    NoAction,
}

impl ReportStatus {
    /// The stored byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Received => 1,
            Self::Action => 2,
            Self::NoAction => 3,
        }
    }

    /// Parse the stored byte; an unknown byte reads as received.
    #[must_use]
    pub const fn from_u8(b: u8) -> Self {
        match b {
            2 => Self::Action,
            3 => Self::NoAction,
            _ => Self::Received,
        }
    }

    /// The wire name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Action => "action",
            Self::NoAction => "none",
        }
    }
}

/// A case's status in the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseStatus {
    /// No moderator has it open.
    New,
    /// A moderator holds its lock.
    InReview,
    /// Decided; reopened as new by a later report.
    Decided,
}

impl CaseStatus {
    /// The stored byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::New => 1,
            Self::InReview => 2,
            Self::Decided => 3,
        }
    }

    /// Parse the stored byte; an unknown byte reads as new.
    #[must_use]
    pub const fn from_u8(b: u8) -> Self {
        match b {
            2 => Self::InReview,
            3 => Self::Decided,
            _ => Self::New,
        }
    }

    /// The wire name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::InReview => "in-review",
            Self::Decided => "decided",
        }
    }

    /// The status of a wire name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::New, Self::InReview, Self::Decided]
            .into_iter()
            .find(|s| s.name() == name)
    }
}

/// One stored report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReport {
    /// The random receipt id handed to the reporter.
    pub receipt_id: [u8; 16],
    /// `TargetKind` byte.
    pub target_kind: u8,
    /// The target bytes.
    pub target: Vec<u8>,
    /// `Reason` byte.
    pub reason: u8,
    /// `ConversationKind` byte.
    pub conversation_kind: u8,
    /// How many evidence items the sealed part says it holds.
    pub evidence_count: u8,
    /// The moderator-list version it was sealed under.
    pub list_version: u64,
    /// The whole report as received.
    pub report: Vec<u8>,
    /// Days since the unix epoch when it arrived.
    pub received_day: u32,
}

/// Who holds a case and when they last acted on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaseLock {
    /// The holder.
    pub moderator_id: [u8; 16],
    /// The holder's last activity.
    pub at_ms: u64,
}

/// One case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseRow {
    /// `TargetKind` byte.
    pub target_kind: u8,
    /// The target bytes.
    pub target: Vec<u8>,
    /// The status.
    pub status: CaseStatus,
    /// The lock, if held.
    pub lock: Option<CaseLock>,
    /// When it was last decided.
    pub decided_at_ms: Option<u64>,
}

/// The reports of one case with one reason, counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseReasonAgg {
    /// The case.
    pub case: CaseRow,
    /// `Reason` byte.
    pub reason: u8,
    /// Reports with this reason.
    pub reports: u64,
    /// Of those, not decided yet.
    pub open_reports: u64,
    /// Of those, with at least one piece of evidence.
    pub with_evidence: u64,
    /// Of those, with at least one message checked as written by the target.
    pub checked: u64,
    /// The first day one arrived.
    pub first_day: u32,
    /// The last day one arrived.
    pub last_day: u32,
}

/// What a moderator's browser verified in one report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportChecks {
    /// Messages checked as written by the target.
    pub checked: u16,
    /// Messages that did not check out.
    pub unchecked: u16,
}

/// One report of a case, for the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    /// The receipt id.
    pub receipt_id: [u8; 16],
    /// `Reason` byte.
    pub reason: u8,
    /// `ConversationKind` byte.
    pub conversation_kind: u8,
    /// Evidence items the sealed part holds.
    pub evidence_count: u8,
    /// The moderator-list version it was sealed under.
    pub list_version: u64,
    /// The whole report as received.
    pub report: Vec<u8>,
    /// Days since the unix epoch when it arrived.
    pub received_day: u32,
    /// Its status.
    pub status: ReportStatus,
    /// What a moderator's browser checked, once posted.
    pub checks: Option<ReportChecks>,
}

/// Result of taking a case's lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOutcome {
    /// The caller holds it now.
    Held(CaseLock),
    /// Another moderator holds it.
    LockedBy(CaseLock),
    /// No such case.
    NoCase,
}

/// The outcome byte of "no action".
pub const OUTCOME_NO_ACTION: u8 = 0;

/// One decision taken in the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// `TargetKind` byte of the case it closed.
    pub target_kind: u8,
    /// The case's target bytes.
    pub target: Vec<u8>,
    /// The `Action` byte, or [`OUTCOME_NO_ACTION`].
    pub outcome: u8,
    /// `Reason` byte.
    pub reason: u8,
    /// Who decided.
    pub moderator_id: [u8; 16],
    /// The logged statement's id, or a random id for no action. Notes are
    /// kept under it.
    pub decision_id: [u8; 16],
    /// When.
    pub at_ms: u64,
}

/// An internal note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Who wrote it.
    pub moderator_id: [u8; 16],
    /// The text.
    pub note: String,
    /// When.
    pub at_ms: u64,
}

/// Appeal outcome byte: waiting.
pub const APPEAL_PENDING: u8 = 0;
/// Appeal outcome byte: declined (the suspension kept).
pub const APPEAL_DECLINED: u8 = 1;
/// Appeal outcome byte: the suspension was lifted.
pub const APPEAL_LIFTED: u8 = 2;

/// An appeal the panel listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppealSeen {
    /// The suspending statement.
    pub statement_id: [u8; 16],
    /// The account target.
    pub target: [u8; 32],
    /// When it was sent.
    pub appeal_at_ms: u64,
}

/// An appeal and how it ended, for the numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppealRecord {
    /// When it was sent.
    pub appeal_at_ms: u64,
    /// [`APPEAL_PENDING`], [`APPEAL_DECLINED`] or [`APPEAL_LIFTED`].
    pub outcome: u8,
    /// When it ended.
    pub outcome_at_ms: Option<u64>,
}

/// A store failure. Never carries report bytes.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The receipt id is already taken (a random collision).
    #[error("receipt id collision")]
    Collision,
    /// The database failed.
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

/// Report and case storage.
#[async_trait]
pub trait ReportStore: Send + Sync {
    /// Insert a new report, and create its target's case or reopen it when
    /// decided.
    async fn insert(&self, report: NewReport) -> Result<(), StoreError>;
    /// The status of the report with `receipt_id`, or `None` if unknown.
    async fn status(&self, receipt_id: &[u8; 16]) -> Result<Option<ReportStatus>, StoreError>;
    /// Whether the store is reachable.
    async fn ping(&self) -> bool;

    /// Release every lock whose holder has not acted since `cutoff_ms`; a
    /// case in review goes back to new.
    async fn expire_locks(&self, cutoff_ms: u64) -> Result<(), StoreError>;
    /// Every case with its reports counted per reason.
    async fn case_aggregates(&self) -> Result<Vec<CaseReasonAgg>, StoreError>;
    /// One case.
    async fn case(&self, kind: u8, target: &[u8]) -> Result<Option<CaseRow>, StoreError>;
    /// The reports of a case, oldest first.
    async fn case_reports(&self, kind: u8, target: &[u8]) -> Result<Vec<ReportRow>, StoreError>;
    /// Take a case's lock for `moderator`: free, held by them, or held by
    /// someone idle since before `cutoff_ms`. A new case becomes in review.
    async fn lock_case(
        &self,
        kind: u8,
        target: &[u8],
        moderator: &[u8; 16],
        now_ms: u64,
        cutoff_ms: u64,
    ) -> Result<LockOutcome, StoreError>;
    /// Release a lock `moderator` holds; `false` when they do not hold it.
    async fn unlock_case(
        &self,
        kind: u8,
        target: &[u8],
        moderator: &[u8; 16],
    ) -> Result<bool, StoreError>;
    /// Record activity of the lock holder.
    async fn touch_lock(
        &self,
        kind: u8,
        target: &[u8],
        moderator: &[u8; 16],
        now_ms: u64,
    ) -> Result<(), StoreError>;
    /// Store the checks of a report; `false` for an unknown receipt.
    async fn set_checks(
        &self,
        receipt_id: &[u8; 16],
        checks: ReportChecks,
    ) -> Result<bool, StoreError>;
    /// Record a decision. With `close_as`, also mark its case decided
    /// (releasing the lock) and close the case's open reports with that
    /// status; without it (an action that undoes one), leave the case as it
    /// is. Returns how many reports it closed; a decision with no case
    /// closes none.
    async fn decide(
        &self,
        decision: &Decision,
        close_as: Option<ReportStatus>,
    ) -> Result<u64, StoreError>;
    /// Store an internal note under a decision id, replacing an earlier one.
    async fn put_note(
        &self,
        decision_id: &[u8; 16],
        moderator: &[u8; 16],
        note: &str,
        at_ms: u64,
    ) -> Result<(), StoreError>;
    /// The notes of these decision ids.
    async fn notes(&self, ids: &[[u8; 16]]) -> Result<HashMap<[u8; 16], Note>, StoreError>;
    /// The decisions that closed a case, oldest first.
    async fn decisions(&self, kind: u8, target: &[u8]) -> Result<Vec<Decision>, StoreError>;
    /// Every decision since `since_ms`.
    async fn decisions_since(&self, since_ms: u64) -> Result<Vec<Decision>, StoreError>;
    /// `(day, reason, count)` of the reports received since `since_day`.
    async fn report_days_since(&self, since_day: u32) -> Result<Vec<(u32, u8, u64)>, StoreError>;
    /// Record the appeals the panel listed; known ones are left alone.
    async fn record_appeals(&self, seen: &[AppealSeen]) -> Result<(), StoreError>;
    /// End pending appeals of `target` with `outcome`: the one of
    /// `statement_id`, or all of them when `None`. Returns how many changed.
    async fn set_appeal_outcome(
        &self,
        statement_id: Option<&[u8; 16]>,
        target: &[u8; 32],
        outcome: u8,
        at_ms: u64,
    ) -> Result<u64, StoreError>;
    /// The appeals sent or ended since `since_ms`.
    async fn appeals_since(&self, since_ms: u64) -> Result<Vec<AppealRecord>, StoreError>;
}

/// MariaDB store.
#[derive(Debug, Clone)]
pub struct SqlStore {
    pool: MySqlPool,
}

impl SqlStore {
    /// Wrap a pool whose migrations have run.
    #[must_use]
    pub const fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}

fn id16(bytes: &[u8]) -> [u8; 16] {
    <[u8; 16]>::try_from(bytes).unwrap_or([0; 16])
}

fn lock_of(id: Option<Vec<u8>>, at: Option<u64>) -> Option<CaseLock> {
    match (id, at) {
        (Some(id), Some(at_ms)) => Some(CaseLock {
            moderator_id: id16(&id),
            at_ms,
        }),
        _ => None,
    }
}

/// `(kind, target, status, lock id, lock at, decided at)`.
type SqlCase = (u8, Vec<u8>, u8, Option<Vec<u8>>, Option<u64>, Option<u64>);

fn case_of(row: SqlCase) -> CaseRow {
    let (target_kind, target, status, lock_id, lock_at, decided_at_ms) = row;
    CaseRow {
        target_kind,
        target,
        status: CaseStatus::from_u8(status),
        lock: lock_of(lock_id, lock_at),
        decided_at_ms,
    }
}

/// `(kind, target, outcome, reason, moderator, decision id, at)`.
type SqlDecision = (u8, Vec<u8>, u8, u8, Vec<u8>, Vec<u8>, u64);

fn decision_of(row: SqlDecision) -> Decision {
    let (target_kind, target, outcome, reason, moderator_id, decision_id, at_ms) = row;
    Decision {
        target_kind,
        target,
        outcome,
        reason,
        moderator_id: id16(&moderator_id),
        decision_id: id16(&decision_id),
        at_ms,
    }
}

/// The columns of `moderation_decisions` in [`SqlDecision`] order.
macro_rules! decision_columns {
    () => {
        "target_kind, target, outcome, reason, moderator_id, decision_id, at_ms"
    };
}

#[async_trait]
impl ReportStore for SqlStore {
    async fn insert(&self, r: NewReport) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "INSERT INTO moderation_reports \
             (receipt_id, target_kind, target, reason, conversation_kind, evidence_count, \
              list_version, report, received_day) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(r.receipt_id.as_slice())
        .bind(r.target_kind)
        .bind(&r.target)
        .bind(r.reason)
        .bind(r.conversation_kind)
        .bind(r.evidence_count)
        .bind(r.list_version)
        .bind(r.report)
        .bind(r.received_day)
        .execute(&mut *tx)
        .await;
        match result {
            Ok(_) => {},
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(StoreError::Collision)
            },
            Err(e) => return Err(StoreError::Db(e)),
        }
        // Assignments run left to right: `status` last, so the conditions
        // above it still read the old status.
        sqlx::query(
            "INSERT INTO moderation_cases (target_kind, target, status) VALUES (?, ?, 1) \
             ON DUPLICATE KEY UPDATE \
                 lock_moderator_id = IF(status = 3, NULL, lock_moderator_id), \
                 lock_at_ms = IF(status = 3, NULL, lock_at_ms), \
                 status = IF(status = 3, 1, status)",
        )
        .bind(r.target_kind)
        .bind(&r.target)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn status(&self, receipt_id: &[u8; 16]) -> Result<Option<ReportStatus>, StoreError> {
        let row: Option<u8> =
            sqlx::query_scalar("SELECT status FROM moderation_reports WHERE receipt_id = ?")
                .bind(receipt_id.as_slice())
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(ReportStatus::from_u8))
    }

    async fn ping(&self) -> bool {
        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .is_ok()
    }

    async fn expire_locks(&self, cutoff_ms: u64) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE moderation_cases \
             SET status = IF(status = 2, 1, status), lock_moderator_id = NULL, lock_at_ms = NULL \
             WHERE lock_at_ms IS NOT NULL AND lock_at_ms < ?",
        )
        .bind(cutoff_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn case_aggregates(&self) -> Result<Vec<CaseReasonAgg>, StoreError> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            u8,
            Vec<u8>,
            u8,
            Option<Vec<u8>>,
            Option<u64>,
            Option<u64>,
            u8,
            u64,
            u64,
            u64,
            u64,
            u32,
            u32,
        )> = sqlx::query_as(
            "SELECT c.target_kind, c.target, c.status, c.lock_moderator_id, c.lock_at_ms, \
                 c.decided_at_ms, r.reason, \
                 CAST(COUNT(*) AS UNSIGNED), \
                 CAST(SUM(r.status = 1) AS UNSIGNED), \
                 CAST(SUM(r.evidence_count > 0) AS UNSIGNED), \
                 CAST(SUM(COALESCE(k.checked_messages, 0) > 0) AS UNSIGNED), \
                 MIN(r.received_day), MAX(r.received_day) \
             FROM moderation_cases c \
             JOIN moderation_reports r \
                 ON r.target_kind = c.target_kind AND r.target = c.target \
             LEFT JOIN moderation_report_checks k ON k.receipt_id = r.receipt_id \
             GROUP BY c.target_kind, c.target, c.status, c.lock_moderator_id, c.lock_at_ms, \
                 c.decided_at_ms, r.reason",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(k, t, s, li, la, d, reason, reports, open, ev, checked, first, last)| {
                    CaseReasonAgg {
                        case: case_of((k, t, s, li, la, d)),
                        reason,
                        reports,
                        open_reports: open,
                        with_evidence: ev,
                        checked,
                        first_day: first,
                        last_day: last,
                    }
                },
            )
            .collect())
    }

    async fn case(&self, kind: u8, target: &[u8]) -> Result<Option<CaseRow>, StoreError> {
        let row: Option<SqlCase> = sqlx::query_as(
            "SELECT target_kind, target, status, lock_moderator_id, lock_at_ms, decided_at_ms \
             FROM moderation_cases WHERE target_kind = ? AND target = ?",
        )
        .bind(kind)
        .bind(target)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(case_of))
    }

    async fn case_reports(&self, kind: u8, target: &[u8]) -> Result<Vec<ReportRow>, StoreError> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            Vec<u8>,
            u8,
            u8,
            u8,
            u64,
            Vec<u8>,
            u32,
            u8,
            Option<u16>,
            Option<u16>,
        )> = sqlx::query_as(
            "SELECT r.receipt_id, r.reason, r.conversation_kind, r.evidence_count, \
                 r.list_version, r.report, r.received_day, r.status, \
                 k.checked_messages, k.unchecked_messages \
             FROM moderation_reports r \
             LEFT JOIN moderation_report_checks k ON k.receipt_id = r.receipt_id \
             WHERE r.target_kind = ? AND r.target = ? \
             ORDER BY r.received_day ASC, r.receipt_id ASC",
        )
        .bind(kind)
        .bind(target)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, reason, ck, ev, lv, report, day, status, c, u)| ReportRow {
                    receipt_id: id16(&id),
                    reason,
                    conversation_kind: ck,
                    evidence_count: ev,
                    list_version: lv,
                    report,
                    received_day: day,
                    status: ReportStatus::from_u8(status),
                    checks: c
                        .zip(u)
                        .map(|(checked, unchecked)| ReportChecks { checked, unchecked }),
                },
            )
            .collect())
    }

    async fn lock_case(
        &self,
        kind: u8,
        target: &[u8],
        moderator: &[u8; 16],
        now_ms: u64,
        cutoff_ms: u64,
    ) -> Result<LockOutcome, StoreError> {
        let mut tx = self.pool.begin().await?;
        let row: Option<SqlCase> = sqlx::query_as(
            "SELECT target_kind, target, status, lock_moderator_id, lock_at_ms, decided_at_ms \
             FROM moderation_cases WHERE target_kind = ? AND target = ? FOR UPDATE",
        )
        .bind(kind)
        .bind(target)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(case) = row.map(case_of) else {
            return Ok(LockOutcome::NoCase);
        };
        if let Some(lock) = case.lock {
            if &lock.moderator_id != moderator && lock.at_ms >= cutoff_ms {
                return Ok(LockOutcome::LockedBy(lock));
            }
        }
        sqlx::query(
            "UPDATE moderation_cases \
             SET lock_moderator_id = ?, lock_at_ms = ?, status = IF(status = 1, 2, status) \
             WHERE target_kind = ? AND target = ?",
        )
        .bind(moderator.as_slice())
        .bind(now_ms)
        .bind(kind)
        .bind(target)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(LockOutcome::Held(CaseLock {
            moderator_id: *moderator,
            at_ms: now_ms,
        }))
    }

    async fn unlock_case(
        &self,
        kind: u8,
        target: &[u8],
        moderator: &[u8; 16],
    ) -> Result<bool, StoreError> {
        let done = sqlx::query(
            "UPDATE moderation_cases \
             SET status = IF(status = 2, 1, status), lock_moderator_id = NULL, lock_at_ms = NULL \
             WHERE target_kind = ? AND target = ? AND lock_moderator_id = ?",
        )
        .bind(kind)
        .bind(target)
        .bind(moderator.as_slice())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn touch_lock(
        &self,
        kind: u8,
        target: &[u8],
        moderator: &[u8; 16],
        now_ms: u64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE moderation_cases SET lock_at_ms = ? \
             WHERE target_kind = ? AND target = ? AND lock_moderator_id = ?",
        )
        .bind(now_ms)
        .bind(kind)
        .bind(target)
        .bind(moderator.as_slice())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn set_checks(
        &self,
        receipt_id: &[u8; 16],
        checks: ReportChecks,
    ) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "INSERT INTO moderation_report_checks (receipt_id, checked_messages, unchecked_messages) \
             VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE checked_messages = VALUES(checked_messages), \
                 unchecked_messages = VALUES(unchecked_messages)",
        )
        .bind(receipt_id.as_slice())
        .bind(checks.checked)
        .bind(checks.unchecked)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(true),
            // The foreign key refuses a receipt that names no report.
            Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => Ok(false),
            Err(e) => Err(StoreError::Db(e)),
        }
    }

    async fn decide(
        &self,
        d: &Decision,
        close_as: Option<ReportStatus>,
    ) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(concat!(
            "INSERT INTO moderation_decisions (",
            decision_columns!(),
            ") VALUES (?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(d.target_kind)
        .bind(&d.target)
        .bind(d.outcome)
        .bind(d.reason)
        .bind(d.moderator_id.as_slice())
        .bind(d.decision_id.as_slice())
        .bind(d.at_ms)
        .execute(&mut *tx)
        .await?;
        let Some(close_as) = close_as else {
            tx.commit().await?;
            return Ok(0);
        };
        sqlx::query(
            "UPDATE moderation_cases \
             SET status = 3, decided_at_ms = ?, lock_moderator_id = NULL, lock_at_ms = NULL \
             WHERE target_kind = ? AND target = ?",
        )
        .bind(d.at_ms)
        .bind(d.target_kind)
        .bind(&d.target)
        .execute(&mut *tx)
        .await?;
        let closed = sqlx::query(
            "UPDATE moderation_reports SET status = ? \
             WHERE target_kind = ? AND target = ? AND status = 1",
        )
        .bind(close_as.as_u8())
        .bind(d.target_kind)
        .bind(&d.target)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(closed)
    }

    async fn put_note(
        &self,
        decision_id: &[u8; 16],
        moderator: &[u8; 16],
        note: &str,
        at_ms: u64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO moderation_notes (decision_id, moderator_id, note, at_ms) \
             VALUES (?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE moderator_id = VALUES(moderator_id), \
                 note = VALUES(note), at_ms = VALUES(at_ms)",
        )
        .bind(decision_id.as_slice())
        .bind(moderator.as_slice())
        .bind(note)
        .bind(at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn notes(&self, ids: &[[u8; 16]]) -> Result<HashMap<[u8; 16], Note>, StoreError> {
        let mut out = HashMap::new();
        for id in ids {
            let row: Option<(Vec<u8>, String, u64)> = sqlx::query_as(
                "SELECT moderator_id, note, at_ms FROM moderation_notes WHERE decision_id = ?",
            )
            .bind(id.as_slice())
            .fetch_optional(&self.pool)
            .await?;
            if let Some((m, note, at_ms)) = row {
                out.insert(
                    *id,
                    Note {
                        moderator_id: id16(&m),
                        note,
                        at_ms,
                    },
                );
            }
        }
        Ok(out)
    }

    async fn decisions(&self, kind: u8, target: &[u8]) -> Result<Vec<Decision>, StoreError> {
        let rows: Vec<SqlDecision> = sqlx::query_as(concat!(
            "SELECT ",
            decision_columns!(),
            " FROM moderation_decisions WHERE target_kind = ? AND target = ? ORDER BY id ASC"
        ))
        .bind(kind)
        .bind(target)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(decision_of).collect())
    }

    async fn decisions_since(&self, since_ms: u64) -> Result<Vec<Decision>, StoreError> {
        let rows: Vec<SqlDecision> = sqlx::query_as(concat!(
            "SELECT ",
            decision_columns!(),
            " FROM moderation_decisions WHERE at_ms >= ? ORDER BY id ASC"
        ))
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(decision_of).collect())
    }

    async fn report_days_since(&self, since_day: u32) -> Result<Vec<(u32, u8, u64)>, StoreError> {
        let rows: Vec<(u32, u8, u64)> = sqlx::query_as(
            "SELECT received_day, reason, CAST(COUNT(*) AS UNSIGNED) FROM moderation_reports \
             WHERE received_day >= ? GROUP BY received_day, reason",
        )
        .bind(since_day)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn record_appeals(&self, seen: &[AppealSeen]) -> Result<(), StoreError> {
        for a in seen {
            sqlx::query(
                "INSERT IGNORE INTO moderation_appeals (statement_id, target, appeal_at_ms) \
                 VALUES (?, ?, ?)",
            )
            .bind(a.statement_id.as_slice())
            .bind(a.target.as_slice())
            .bind(a.appeal_at_ms)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    async fn set_appeal_outcome(
        &self,
        statement_id: Option<&[u8; 16]>,
        target: &[u8; 32],
        outcome: u8,
        at_ms: u64,
    ) -> Result<u64, StoreError> {
        let done = match statement_id {
            Some(id) => {
                sqlx::query(
                    "UPDATE moderation_appeals SET outcome = ?, outcome_at_ms = ? \
                     WHERE statement_id = ? AND target = ? AND outcome = 0",
                )
                .bind(outcome)
                .bind(at_ms)
                .bind(id.as_slice())
                .bind(target.as_slice())
                .execute(&self.pool)
                .await?
            },
            None => {
                sqlx::query(
                    "UPDATE moderation_appeals SET outcome = ?, outcome_at_ms = ? \
                     WHERE target = ? AND outcome = 0",
                )
                .bind(outcome)
                .bind(at_ms)
                .bind(target.as_slice())
                .execute(&self.pool)
                .await?
            },
        };
        Ok(done.rows_affected())
    }

    async fn appeals_since(&self, since_ms: u64) -> Result<Vec<AppealRecord>, StoreError> {
        let rows: Vec<(u64, u8, Option<u64>)> = sqlx::query_as(
            "SELECT appeal_at_ms, outcome, outcome_at_ms FROM moderation_appeals \
             WHERE appeal_at_ms >= ? OR outcome_at_ms >= ?",
        )
        .bind(since_ms)
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(appeal_at_ms, outcome, outcome_at_ms)| AppealRecord {
                appeal_at_ms,
                outcome,
                outcome_at_ms,
            })
            .collect())
    }
}

#[derive(Debug, Default)]
struct Inner {
    reports: HashMap<[u8; 16], (NewReport, ReportStatus)>,
    cases: BTreeMap<(u8, Vec<u8>), CaseRow>,
    checks: HashMap<[u8; 16], ReportChecks>,
    notes: HashMap<[u8; 16], Note>,
    decisions: Vec<Decision>,
    appeals: BTreeMap<([u8; 16], [u8; 32]), AppealRecord>,
}

/// In-memory store.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Set a report's status (tests).
    pub fn set_status(&self, receipt_id: &[u8; 16], status: ReportStatus) {
        if let Some(row) = self.lock().reports.get_mut(receipt_id) {
            row.1 = status;
        }
    }

    /// Every stored report (tests).
    #[must_use]
    pub fn all(&self) -> Vec<NewReport> {
        self.lock()
            .reports
            .values()
            .map(|(r, _)| r.clone())
            .collect()
    }
}

#[async_trait]
impl ReportStore for MemoryStore {
    async fn insert(&self, report: NewReport) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.reports.contains_key(&report.receipt_id) {
            return Err(StoreError::Collision);
        }
        let key = (report.target_kind, report.target.clone());
        let case = inner.cases.entry(key).or_insert_with(|| CaseRow {
            target_kind: report.target_kind,
            target: report.target.clone(),
            status: CaseStatus::New,
            lock: None,
            decided_at_ms: None,
        });
        if case.status == CaseStatus::Decided {
            case.status = CaseStatus::New;
            case.lock = None;
        }
        inner
            .reports
            .insert(report.receipt_id, (report, ReportStatus::Received));
        Ok(())
    }

    async fn status(&self, receipt_id: &[u8; 16]) -> Result<Option<ReportStatus>, StoreError> {
        Ok(self.lock().reports.get(receipt_id).map(|(_, s)| *s))
    }

    async fn ping(&self) -> bool {
        true
    }

    async fn expire_locks(&self, cutoff_ms: u64) -> Result<(), StoreError> {
        for case in self.lock().cases.values_mut() {
            if case.lock.is_some_and(|l| l.at_ms < cutoff_ms) {
                case.lock = None;
                if case.status == CaseStatus::InReview {
                    case.status = CaseStatus::New;
                }
            }
        }
        Ok(())
    }

    async fn case_aggregates(&self) -> Result<Vec<CaseReasonAgg>, StoreError> {
        let inner = self.lock();
        let mut out: BTreeMap<(u8, Vec<u8>, u8), CaseReasonAgg> = BTreeMap::new();
        for (r, status) in inner.reports.values() {
            let Some(case) = inner.cases.get(&(r.target_kind, r.target.clone())) else {
                continue;
            };
            let agg = out
                .entry((r.target_kind, r.target.clone(), r.reason))
                .or_insert_with(|| CaseReasonAgg {
                    case: case.clone(),
                    reason: r.reason,
                    reports: 0,
                    open_reports: 0,
                    with_evidence: 0,
                    checked: 0,
                    first_day: r.received_day,
                    last_day: r.received_day,
                });
            agg.reports += 1;
            agg.open_reports += u64::from(*status == ReportStatus::Received);
            agg.with_evidence += u64::from(r.evidence_count > 0);
            agg.checked += u64::from(
                inner
                    .checks
                    .get(&r.receipt_id)
                    .is_some_and(|c| c.checked > 0),
            );
            agg.first_day = agg.first_day.min(r.received_day);
            agg.last_day = agg.last_day.max(r.received_day);
        }
        Ok(out.into_values().collect())
    }

    async fn case(&self, kind: u8, target: &[u8]) -> Result<Option<CaseRow>, StoreError> {
        Ok(self.lock().cases.get(&(kind, target.to_vec())).cloned())
    }

    async fn case_reports(&self, kind: u8, target: &[u8]) -> Result<Vec<ReportRow>, StoreError> {
        let inner = self.lock();
        let mut rows: Vec<ReportRow> = inner
            .reports
            .values()
            .filter(|(r, _)| r.target_kind == kind && r.target == target)
            .map(|(r, status)| ReportRow {
                receipt_id: r.receipt_id,
                reason: r.reason,
                conversation_kind: r.conversation_kind,
                evidence_count: r.evidence_count,
                list_version: r.list_version,
                report: r.report.clone(),
                received_day: r.received_day,
                status: *status,
                checks: inner.checks.get(&r.receipt_id).copied(),
            })
            .collect();
        rows.sort_by_key(|r| (r.received_day, r.receipt_id));
        Ok(rows)
    }

    async fn lock_case(
        &self,
        kind: u8,
        target: &[u8],
        moderator: &[u8; 16],
        now_ms: u64,
        cutoff_ms: u64,
    ) -> Result<LockOutcome, StoreError> {
        let mut inner = self.lock();
        let Some(case) = inner.cases.get_mut(&(kind, target.to_vec())) else {
            return Ok(LockOutcome::NoCase);
        };
        if let Some(lock) = case.lock {
            if &lock.moderator_id != moderator && lock.at_ms >= cutoff_ms {
                return Ok(LockOutcome::LockedBy(lock));
            }
        }
        let lock = CaseLock {
            moderator_id: *moderator,
            at_ms: now_ms,
        };
        case.lock = Some(lock);
        if case.status == CaseStatus::New {
            case.status = CaseStatus::InReview;
        }
        Ok(LockOutcome::Held(lock))
    }

    async fn unlock_case(
        &self,
        kind: u8,
        target: &[u8],
        moderator: &[u8; 16],
    ) -> Result<bool, StoreError> {
        let mut inner = self.lock();
        let Some(case) = inner.cases.get_mut(&(kind, target.to_vec())) else {
            return Ok(false);
        };
        if case.lock.is_some_and(|l| &l.moderator_id == moderator) {
            case.lock = None;
            if case.status == CaseStatus::InReview {
                case.status = CaseStatus::New;
            }
            return Ok(true);
        }
        Ok(false)
    }

    async fn touch_lock(
        &self,
        kind: u8,
        target: &[u8],
        moderator: &[u8; 16],
        now_ms: u64,
    ) -> Result<(), StoreError> {
        if let Some(case) = self.lock().cases.get_mut(&(kind, target.to_vec())) {
            if let Some(lock) = case.lock.as_mut() {
                if &lock.moderator_id == moderator {
                    lock.at_ms = now_ms;
                }
            }
        }
        Ok(())
    }

    async fn set_checks(
        &self,
        receipt_id: &[u8; 16],
        checks: ReportChecks,
    ) -> Result<bool, StoreError> {
        let mut inner = self.lock();
        if !inner.reports.contains_key(receipt_id) {
            return Ok(false);
        }
        inner.checks.insert(*receipt_id, checks);
        Ok(true)
    }

    async fn decide(
        &self,
        d: &Decision,
        close_as: Option<ReportStatus>,
    ) -> Result<u64, StoreError> {
        let mut inner = self.lock();
        inner.decisions.push(d.clone());
        let Some(close_as) = close_as else {
            return Ok(0);
        };
        if let Some(case) = inner.cases.get_mut(&(d.target_kind, d.target.clone())) {
            case.status = CaseStatus::Decided;
            case.decided_at_ms = Some(d.at_ms);
            case.lock = None;
        }
        let mut closed = 0;
        for (r, status) in inner.reports.values_mut() {
            if r.target_kind == d.target_kind
                && r.target == d.target
                && *status == ReportStatus::Received
            {
                *status = close_as;
                closed += 1;
            }
        }
        Ok(closed)
    }

    async fn put_note(
        &self,
        decision_id: &[u8; 16],
        moderator: &[u8; 16],
        note: &str,
        at_ms: u64,
    ) -> Result<(), StoreError> {
        self.lock().notes.insert(
            *decision_id,
            Note {
                moderator_id: *moderator,
                note: note.to_owned(),
                at_ms,
            },
        );
        Ok(())
    }

    async fn notes(&self, ids: &[[u8; 16]]) -> Result<HashMap<[u8; 16], Note>, StoreError> {
        let inner = self.lock();
        Ok(ids
            .iter()
            .filter_map(|id| inner.notes.get(id).map(|n| (*id, n.clone())))
            .collect())
    }

    async fn decisions(&self, kind: u8, target: &[u8]) -> Result<Vec<Decision>, StoreError> {
        Ok(self
            .lock()
            .decisions
            .iter()
            .filter(|d| d.target_kind == kind && d.target == target)
            .cloned()
            .collect())
    }

    async fn decisions_since(&self, since_ms: u64) -> Result<Vec<Decision>, StoreError> {
        Ok(self
            .lock()
            .decisions
            .iter()
            .filter(|d| d.at_ms >= since_ms)
            .cloned()
            .collect())
    }

    async fn report_days_since(&self, since_day: u32) -> Result<Vec<(u32, u8, u64)>, StoreError> {
        let mut counts: BTreeMap<(u32, u8), u64> = BTreeMap::new();
        for (r, _) in self.lock().reports.values() {
            if r.received_day >= since_day {
                *counts.entry((r.received_day, r.reason)).or_default() += 1;
            }
        }
        Ok(counts.into_iter().map(|((d, r), n)| (d, r, n)).collect())
    }

    async fn record_appeals(&self, seen: &[AppealSeen]) -> Result<(), StoreError> {
        let mut inner = self.lock();
        for a in seen {
            inner
                .appeals
                .entry((a.statement_id, a.target))
                .or_insert(AppealRecord {
                    appeal_at_ms: a.appeal_at_ms,
                    outcome: APPEAL_PENDING,
                    outcome_at_ms: None,
                });
        }
        Ok(())
    }

    async fn set_appeal_outcome(
        &self,
        statement_id: Option<&[u8; 16]>,
        target: &[u8; 32],
        outcome: u8,
        at_ms: u64,
    ) -> Result<u64, StoreError> {
        let mut changed = 0;
        for ((sid, t), rec) in &mut self.lock().appeals {
            if t == target
                && statement_id.map_or(true, |s| s == sid)
                && rec.outcome == APPEAL_PENDING
            {
                rec.outcome = outcome;
                rec.outcome_at_ms = Some(at_ms);
                changed += 1;
            }
        }
        Ok(changed)
    }

    async fn appeals_since(&self, since_ms: u64) -> Result<Vec<AppealRecord>, StoreError> {
        Ok(self
            .lock()
            .appeals
            .values()
            .filter(|a| {
                a.appeal_at_ms >= since_ms || a.outcome_at_ms.is_some_and(|t| t >= since_ms)
            })
            .copied()
            .collect())
    }
}
