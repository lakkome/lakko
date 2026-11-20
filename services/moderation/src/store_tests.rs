// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! One behaviour test run over both report stores: the in-memory one
//! always, MariaDB when `LAKKO_MODERATION_TEST_DATABASE_URL` is set.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use crate::panel_view::{fold_cases, CaseFilter};
use crate::store::{
    AppealSeen, CaseStatus, Decision, LockOutcome, MemoryStore, NewReport, ReportChecks,
    ReportStatus, ReportStore, SqlStore, StoreError, APPEAL_DECLINED, APPEAL_LIFTED,
    APPEAL_PENDING, OUTCOME_NO_ACTION,
};

const A: [u8; 16] = [5; 16];
const B: [u8; 16] = [6; 16];

fn report(id: u8, kind: u8, target: &[u8], reason: u8, day: u32, evidence: u8) -> NewReport {
    NewReport {
        receipt_id: [id; 16],
        target_kind: kind,
        target: target.to_vec(),
        reason,
        conversation_kind: 1,
        evidence_count: evidence,
        list_version: 2,
        report: vec![id; 4],
        received_day: day,
    }
}

async fn exercise(store: &dyn ReportStore) {
    let acc = vec![1u8; 32];
    let chan = vec![2u8; 16];
    store.insert(report(1, 1, &acc, 1, 100, 1)).await.unwrap();
    store.insert(report(2, 1, &acc, 1, 102, 0)).await.unwrap();
    store.insert(report(3, 1, &acc, 2, 101, 2)).await.unwrap();
    store.insert(report(4, 3, &chan, 3, 99, 1)).await.unwrap();
    assert!(matches!(
        store.insert(report(4, 3, &chan, 3, 99, 1)).await,
        Err(StoreError::Collision)
    ));

    // Cases are created by the first report.
    let case = store.case(1, &acc).await.unwrap().unwrap();
    assert_eq!(case.status, CaseStatus::New);
    assert!(case.lock.is_none());
    assert!(store.case(1, &[9; 32]).await.unwrap().is_none());

    // Checks, and the aggregates the queue folds.
    assert!(store
        .set_checks(
            &[3; 16],
            ReportChecks {
                checked: 2,
                unchecked: 1
            }
        )
        .await
        .unwrap());
    assert!(store
        .set_checks(
            &[3; 16],
            ReportChecks {
                checked: 1,
                unchecked: 0
            }
        )
        .await
        .unwrap());
    assert!(!store
        .set_checks(
            &[9; 16],
            ReportChecks {
                checked: 1,
                unchecked: 0
            }
        )
        .await
        .unwrap());
    let rows = fold_cases(
        store.case_aggregates().await.unwrap(),
        &CaseFilter::default(),
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].case.target, acc);
    assert_eq!(rows[0].reports, 3);
    assert_eq!(rows[0].open_reports, 3);
    assert_eq!(rows[0].with_evidence, 2);
    assert_eq!(rows[0].checked, 1);
    assert_eq!((rows[0].first_day, rows[0].last_day), (100, 102));
    assert_eq!(rows[0].reasons.get(&1), Some(&2));
    assert_eq!(rows[0].reasons.get(&2), Some(&1));

    let reports = store.case_reports(1, &acc).await.unwrap();
    let days: Vec<u32> = reports.iter().map(|r| r.received_day).collect();
    assert_eq!(days, vec![100, 101, 102]);
    assert_eq!(
        reports[1].checks,
        Some(ReportChecks {
            checked: 1,
            unchecked: 0
        })
    );
    assert_eq!(reports[0].report, vec![1; 4]);

    // Locks: one holder; an idle one is taken over; expiry frees it.
    assert!(matches!(
        store.lock_case(1, &acc, &A, 1_000, 0).await.unwrap(),
        LockOutcome::Held(_)
    ));
    assert_eq!(
        store.case(1, &acc).await.unwrap().unwrap().status,
        CaseStatus::InReview
    );
    match store.lock_case(1, &acc, &B, 1_500, 900).await.unwrap() {
        LockOutcome::LockedBy(l) => assert_eq!(l.moderator_id, A),
        other => panic!("{other:?}"),
    }
    store.touch_lock(1, &acc, &A, 2_000).await.unwrap();
    store.touch_lock(1, &acc, &B, 9_000).await.unwrap();
    assert_eq!(
        store
            .case(1, &acc)
            .await
            .unwrap()
            .unwrap()
            .lock
            .unwrap()
            .at_ms,
        2_000
    );
    assert!(matches!(
        store.lock_case(1, &acc, &B, 3_000, 2_001).await.unwrap(),
        LockOutcome::Held(_)
    ));
    assert!(!store.unlock_case(1, &acc, &A).await.unwrap());
    store.expire_locks(3_001).await.unwrap();
    let case = store.case(1, &acc).await.unwrap().unwrap();
    assert!(case.lock.is_none());
    assert_eq!(case.status, CaseStatus::New);
    assert!(matches!(
        store.lock_case(9, &acc, &A, 1, 0).await.unwrap(),
        LockOutcome::NoCase
    ));
    assert!(matches!(
        store.lock_case(1, &acc, &A, 4_000, 0).await.unwrap(),
        LockOutcome::Held(_)
    ));
    assert!(store.unlock_case(1, &acc, &A).await.unwrap());

    // A decision closes the open reports; an undo closes nothing.
    store.lock_case(1, &acc, &A, 4_500, 0).await.unwrap();
    let decision = Decision {
        target_kind: 1,
        target: acc.clone(),
        outcome: 1,
        reason: 1,
        moderator_id: A,
        decision_id: [7; 16],
        at_ms: 5_000,
    };
    assert_eq!(
        store
            .decide(&decision, Some(ReportStatus::Action))
            .await
            .unwrap(),
        3
    );
    let case = store.case(1, &acc).await.unwrap().unwrap();
    assert_eq!(case.status, CaseStatus::Decided);
    assert_eq!(case.decided_at_ms, Some(5_000));
    assert!(case.lock.is_none());
    assert_eq!(
        store.status(&[1; 16]).await.unwrap(),
        Some(ReportStatus::Action)
    );
    let lift = Decision {
        outcome: 2,
        decision_id: [8; 16],
        at_ms: 6_000,
        ..decision.clone()
    };
    assert_eq!(store.decide(&lift, None).await.unwrap(), 0);
    assert_eq!(
        store.decisions(1, &acc).await.unwrap(),
        vec![decision.clone(), lift]
    );

    // A new report reopens a decided case.
    store.insert(report(5, 1, &acc, 3, 103, 1)).await.unwrap();
    assert_eq!(
        store.case(1, &acc).await.unwrap().unwrap().status,
        CaseStatus::New
    );
    let no_action = Decision {
        outcome: OUTCOME_NO_ACTION,
        decision_id: [9; 16],
        at_ms: 7_000,
        ..decision.clone()
    };
    assert_eq!(
        store
            .decide(&no_action, Some(ReportStatus::NoAction))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store.status(&[5; 16]).await.unwrap(),
        Some(ReportStatus::NoAction)
    );
    assert_eq!(
        store.status(&[1; 16]).await.unwrap(),
        Some(ReportStatus::Action)
    );
    // A decision with no case is still recorded.
    let elsewhere = Decision {
        target_kind: 4,
        target: vec![3; 64],
        at_ms: 8_000,
        ..decision.clone()
    };
    assert_eq!(
        store
            .decide(&elsewhere, Some(ReportStatus::Action))
            .await
            .unwrap(),
        0
    );
    let since: Vec<u64> = store
        .decisions_since(6_500)
        .await
        .unwrap()
        .iter()
        .map(|d| d.at_ms)
        .collect();
    assert_eq!(since, vec![7_000, 8_000]);

    // Notes.
    store.put_note(&[7; 16], &A, "first", 1).await.unwrap();
    store.put_note(&[7; 16], &B, "second", 2).await.unwrap();
    let notes = store.notes(&[[7; 16], [8; 16]]).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[&[7; 16]].note, "second");
    assert_eq!(notes[&[7; 16]].moderator_id, B);

    // Report days.
    let mut days = store.report_days_since(101).await.unwrap();
    days.sort_unstable();
    assert_eq!(days, vec![(101, 2, 1), (102, 1, 1), (103, 3, 1)]);

    // Appeals: recorded once, ended once.
    let t = [1u8; 32];
    let seen = [
        AppealSeen {
            statement_id: [7; 16],
            target: t,
            appeal_at_ms: 10_000,
        },
        AppealSeen {
            statement_id: [8; 16],
            target: t,
            appeal_at_ms: 11_000,
        },
    ];
    store.record_appeals(&seen).await.unwrap();
    store
        .record_appeals(&[AppealSeen {
            appeal_at_ms: 99_999,
            ..seen[0]
        }])
        .await
        .unwrap();
    assert_eq!(
        store
            .set_appeal_outcome(Some(&[7; 16]), &t, APPEAL_DECLINED, 12_000)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .set_appeal_outcome(None, &t, APPEAL_LIFTED, 13_000)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .set_appeal_outcome(None, &t, APPEAL_LIFTED, 14_000)
            .await
            .unwrap(),
        0
    );
    let mut appeals = store.appeals_since(0).await.unwrap();
    appeals.sort_by_key(|a| a.appeal_at_ms);
    assert_eq!(appeals.len(), 2);
    assert_eq!(appeals[0].appeal_at_ms, 10_000);
    assert_eq!(
        (appeals[0].outcome, appeals[0].outcome_at_ms),
        (APPEAL_DECLINED, Some(12_000))
    );
    assert_eq!(
        (appeals[1].outcome, appeals[1].outcome_at_ms),
        (APPEAL_LIFTED, Some(13_000))
    );
    assert_ne!(appeals[1].outcome, APPEAL_PENDING);
    assert_eq!(store.appeals_since(12_500).await.unwrap().len(), 1);
}

/// A pool on `url` with this service's migrations run. Two tests may start
/// on a fresh database at once and race on the migrations table; the loser
/// runs them again, when they are already applied.
pub(crate) async fn migrated_pool(url: &str) -> sqlx::MySqlPool {
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(4)
        .connect(url)
        .await
        .unwrap();
    for attempt in 0..5 {
        match sqlx::migrate!("./migrations").run(&pool).await {
            Ok(()) => return pool,
            Err(e) if attempt < 4 => {
                eprintln!("migrations raced ({e}); retrying");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            },
            Err(e) => panic!("migrations: {e}"),
        }
    }
    pool
}

#[tokio::test]
async fn the_memory_store_keeps_cases() {
    exercise(&MemoryStore::new()).await;
}

/// Gated on `LAKKO_MODERATION_TEST_DATABASE_URL`; skips without it.
#[tokio::test]
async fn the_sql_store_keeps_cases() {
    let Ok(url) = std::env::var("LAKKO_MODERATION_TEST_DATABASE_URL") else {
        eprintln!("skipping: LAKKO_MODERATION_TEST_DATABASE_URL unset");
        return;
    };
    let pool = migrated_pool(&url).await;
    for statement in [
        "DELETE FROM moderation_report_checks",
        "DELETE FROM moderation_reports",
        "DELETE FROM moderation_cases",
        "DELETE FROM moderation_notes",
        "DELETE FROM moderation_decisions",
        "DELETE FROM moderation_appeals",
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    exercise(&SqlStore::new(pool.clone())).await;
    pool.close().await;
}
