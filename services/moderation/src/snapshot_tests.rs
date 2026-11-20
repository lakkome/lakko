// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tests of the suspension snapshot builder, its reload and its route
//! (ADR-0099 step 4), over the in-memory stores.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use lakko_anon_token_server::TokenVerifier;
use lakko_moderation::moderators::{ModeratorEntry, ModeratorList};
use lakko_moderation::snapshot::{
    account_prefix, account_prefix_hex, check_account, period_of, prefix_hex, SnapshotRoot,
    Standing,
};
use lakko_moderation::software_authenticator::SoftwareAuthenticator;
use lakko_moderation::statement::{reason_commitment, Action, Statement};
use lakko_moderation::target::Target;
use lakko_moderation::Reason;
use serde_json::Value;
use tower::ServiceExt as _;

use crate::ip_limit::IpLimiter;
use crate::log::{statement_key, ModerationLog, LEAF_SNAPSHOT_ROOT, LEAF_STATEMENT};
use crate::log_store::MemoryLogStore;
use crate::snapshots::Snapshots;
use crate::store::MemoryStore;
use crate::{build_app, AppState};

const PERIOD_SECS: u32 = 3_600;
const T0: u64 = 1_800_000_000_000;
const ACCOUNT: [u8; 32] = [7; 32];
const OTHER: [u8; 32] = [8; 32];

fn moderator() -> SoftwareAuthenticator {
    SoftwareAuthenticator::from_seed(&[21; 32])
}

fn moderator_list() -> ModeratorList {
    ModeratorList {
        list_version: 1,
        issued_at_ms: 1,
        rp_id: "moderation.example".into(),
        origin: "https://moderation.example".into(),
        entries: vec![ModeratorEntry {
            moderator_id: [5; 16],
            name: "dev".into(),
            key: moderator().moderator_key(),
            credential_id: vec![1],
            x25519_public: [4; 32],
            added_at_ms: 0,
            revoked_at_ms: 0,
        }],
    }
}

async fn append_statement(log: &ModerationLog, id: u8, action: Action, issued_at_ms: u64) {
    let s = Statement {
        statement_id: [id; 16],
        action,
        target: Target::account(&ACCOUNT).unwrap(),
        reason_commitment: reason_commitment(Reason::Spam, &[3; 16]),
        until_ms: 0,
        issued_at_ms,
        moderator_id: [5; 16],
        list_version: 1,
    };
    let signed = moderator()
        .sign_statement(&moderator_list(), &s)
        .unwrap()
        .encode()
        .unwrap();
    log.append(LEAF_STATEMENT, statement_key(&s.statement_id), &signed, 1)
        .await
        .unwrap();
}

async fn empty_log() -> Arc<ModerationLog> {
    Arc::new(
        ModerationLog::open(Arc::new(MemoryLogStore::new()), Vec::new())
            .await
            .unwrap(),
    )
}

fn app(log: &Arc<ModerationLog>, snapshots: &Arc<Snapshots>) -> Router {
    build_app(AppState {
        store: Arc::new(MemoryStore::new()),
        list: None,
        tokens: TokenVerifier::in_memory(None),
        limits: Arc::new(IpLimiter::new(1_000, 1_000.0)),
        log: log.clone(),
        snapshots: snapshots.clone(),
    })
}

async fn get(app: &Router, path: &str) -> (StatusCode, Option<String>, Vec<u8>) {
    let resp = app
        .clone()
        .oneshot(
            Request::get(path)
                .header("x-real-ip", "198.51.100.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let cache = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, cache, bytes)
}

fn error_code(bytes: &[u8]) -> String {
    serde_json::from_slice::<Value>(bytes).unwrap()["error"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn standing(log: &ModerationLog, body: &[u8], account: &[u8; 32], now_ms: u64) -> Standing {
    check_account(
        body,
        account,
        &log.operator_public(),
        &[],
        1,
        i64::try_from(now_ms).unwrap(),
    )
    .unwrap()
    .standing
}

#[tokio::test]
async fn a_snapshot_is_built_served_and_checked() {
    assert_ne!(account_prefix(&ACCOUNT), account_prefix(&OTHER));
    let log = empty_log().await;
    append_statement(&log, 1, Action::Suspend, 10).await;
    let snapshots = Arc::new(Snapshots::new(PERIOD_SECS));
    let app = app(&log, &snapshots);

    // Nothing held yet.
    let path = format!(
        "/moderation/snapshot/latest/bucket/{}",
        account_prefix_hex(&ACCOUNT)
    );
    let (s, _, b) = get(&app, &path).await;
    assert_eq!(
        (s, error_code(&b).as_str()),
        (StatusCode::NOT_FOUND, "no-snapshot")
    );

    assert!(snapshots.tick(&log, T0).await.unwrap());
    // A second tick in the same period builds nothing.
    assert!(!snapshots.tick(&log, T0 + 1_000).await.unwrap());
    assert_eq!(log.size().await, 2);
    let leaves = log.leaves().await;
    let root = SnapshotRoot::decode_log_leaf(&leaves[1]).unwrap();
    assert_eq!(root.covered_log_size, 1);
    assert_eq!(root.entry_count, 1);

    let (s, cache, body) = get(&app, &path).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("public, max-age=300"));
    assert_eq!(
        standing(&log, &body, &ACCOUNT, T0 + 5_000),
        Standing::Suspended { until_ms: None }
    );

    let period = period_of(T0, PERIOD_SECS);
    let other_path = format!(
        "/moderation/snapshot/{period}/bucket/{}",
        account_prefix_hex(&OTHER)
    );
    let (s, _, body) = get(&app, &other_path).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        standing(&log, &body, &OTHER, T0 + 5_000),
        Standing::NotSuspended
    );

    for (p, code) in [
        (
            format!("/moderation/snapshot/{}/bucket/0000", period + 1),
            "no-snapshot",
        ),
        (
            "/moderation/snapshot/latest/bucket/zzzz".to_owned(),
            "bad-prefix",
        ),
        (
            "/moderation/snapshot/latest/bucket/abc".to_owned(),
            "bad-prefix",
        ),
        (
            "/moderation/snapshot/latest/bucket/abcde".to_owned(),
            "bad-prefix",
        ),
        (
            "/moderation/snapshot/newest/bucket/0000".to_owned(),
            "bad-period",
        ),
        (
            "/moderation/snapshot/-1/bucket/0000".to_owned(),
            "bad-period",
        ),
        (
            "/moderation/snapshot/99999999999999999999999/bucket/0000".to_owned(),
            "bad-period",
        ),
    ] {
        let (s, _, b) = get(&app, &p).await;
        let expect = if code == "no-snapshot" {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        assert_eq!((s, error_code(&b)), (expect, code.to_owned()), "{p}");
    }
    // An upper-case prefix is accepted and answered in lower case.
    let upper = format!(
        "/moderation/snapshot/latest/bucket/{}",
        account_prefix_hex(&ACCOUNT).to_uppercase()
    );
    let (s, _, body) = get(&app, &upper).await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["prefix"], prefix_hex(account_prefix(&ACCOUNT)));
}

#[tokio::test]
async fn a_lift_in_the_same_period_wins_and_the_previous_snapshot_stays() {
    let log = empty_log().await;
    let snapshots = Arc::new(Snapshots::new(PERIOD_SECS));
    let app = app(&log, &snapshots);
    let path = |period: &str| {
        format!(
            "/moderation/snapshot/{period}/bucket/{}",
            account_prefix_hex(&ACCOUNT)
        )
    };

    // A suspension, then a later lift, both before the build.
    append_statement(&log, 1, Action::Suspend, 10).await;
    append_statement(&log, 2, Action::Lift, 20).await;
    assert!(snapshots.tick(&log, T0).await.unwrap());
    let (_, _, body) = get(&app, &path("latest")).await;
    assert_eq!(
        standing(&log, &body, &ACCOUNT, T0 + 1),
        Standing::NotSuspended
    );

    // A new suspension is in the next period's snapshot; the previous
    // period is still served.
    append_statement(&log, 3, Action::Suspend, 30).await;
    let t1 = T0 + u64::from(PERIOD_SECS) * 1000;
    assert!(snapshots.tick(&log, t1).await.unwrap());
    let (_, _, body) = get(&app, &path("latest")).await;
    assert_eq!(
        standing(&log, &body, &ACCOUNT, t1 + 1),
        Standing::Suspended { until_ms: None }
    );
    let previous = period_of(T0, PERIOD_SECS).to_string();
    let (s, _, body) = get(&app, &path(&previous)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        standing(&log, &body, &ACCOUNT, t1 + 1),
        Standing::NotSuspended
    );

    // A third period drops the first.
    let t2 = t1 + u64::from(PERIOD_SECS) * 1000;
    assert!(snapshots.tick(&log, t2).await.unwrap());
    let (s, _, _) = get(&app, &path(&previous)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_restart_reloads_and_rechecks_the_stored_snapshots() {
    let store = Arc::new(MemoryLogStore::new());
    let log = Arc::new(
        ModerationLog::open(store.clone(), Vec::new())
            .await
            .unwrap(),
    );
    append_statement(&log, 1, Action::Suspend, 10).await;
    let snapshots = Snapshots::new(PERIOD_SECS);
    assert!(snapshots.tick(&log, T0).await.unwrap());
    let t1 = T0 + u64::from(PERIOD_SECS) * 1000;
    append_statement(&log, 2, Action::Lift, 20).await;
    assert!(snapshots.tick(&log, t1).await.unwrap());

    let reopened = ModerationLog::open(store.clone(), Vec::new())
        .await
        .unwrap();
    let again = Snapshots::new(PERIOD_SECS);
    assert_eq!(again.reload(&reopened).await, 2);
    let newest = again.get(None).await.unwrap();
    let before = snapshots.get(None).await.unwrap();
    assert_eq!(newest.index, before.index);
    assert_eq!(newest.snapshot.root(), before.snapshot.root());
    assert!(again.get(Some(period_of(T0, PERIOD_SECS))).await.is_some());
    // Both periods held: no build.
    assert!(!again.tick(&reopened, t1 + 1).await.unwrap());

    // A stored root that does not rebuild is dropped, and a fresh snapshot
    // is built for the current period although one is held.
    let mut forged = newest.snapshot.root().clone();
    forged.entry_count += 1;
    let payload = forged.encode();
    reopened
        .append(
            LEAF_SNAPSHOT_ROOT,
            lakko_crypto::hash::blake3(&payload),
            &payload,
            1,
        )
        .await
        .unwrap();
    let third = Snapshots::new(PERIOD_SECS);
    assert_eq!(third.reload(&reopened).await, 1);
    let size = reopened.size().await;
    assert!(third.tick(&reopened, t1 + 2).await.unwrap());
    assert_eq!(reopened.size().await, size + 1);
    assert!(!third.tick(&reopened, t1 + 3).await.unwrap());
}

#[tokio::test]
async fn a_snapshot_root_is_never_appended_after_a_newer_leaf() {
    let log = empty_log().await;
    append_statement(&log, 1, Action::Suspend, 10).await;
    let leaves = log.leaves().await;
    append_statement(&log, 2, Action::Lift, 20).await;
    let refs: Vec<&[u8]> = leaves.iter().map(Vec::as_slice).collect();
    let snapshot = lakko_moderation::snapshot::build_snapshot(&refs, T0, PERIOD_SECS).unwrap();
    let payload = snapshot.root().encode();
    let appended = log
        .append_at(
            LEAF_SNAPSHOT_ROOT,
            lakko_crypto::hash::blake3(&payload),
            &payload,
            leaves.len() as u64,
            1,
        )
        .await
        .unwrap();
    assert!(appended.is_none());
    assert_eq!(log.size().await, 2);
}
