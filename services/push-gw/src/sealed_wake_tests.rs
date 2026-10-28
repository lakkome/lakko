// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0078 section 3, the tests its Phasing list names for push-gw:
//!
//! - the push-gw tables hold no token in the clear and no device pseudonym;
//! - a wake not signed by the wake key is refused;
//!
//! plus the full path (subscribe, a genuine wake through the internal
//! listener, the delivered payload naming the subscription), replay
//! refusal, and the removal of a provider-rejected token.
//!
//! Gated on `LAKKO_PUSH_GW_TEST_DATABASE_URL`: each test skips (does not
//! fail) when it is unset.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_anon_token::{MasterSecret, TokenContext, DEFAULT_WINDOW_SECS, TOKEN_HEADER};
use lakko_anon_token_server::testing::mint_header;
use lakko_anon_token_server::{current_epoch, AnonTokenServerConfig, TokenVerifier};
use lakko_contacts::push_wake::{
    seal_push_subscription_for_secret, seal_push_wake, seal_push_wake_with_nonce,
    seal_with_claimed_key, seal_with_signer, PushGwKey, PushProvider, PushTokenEntry, PushWakeKind,
};
use lakko_contacts::{derive_push_gid, derive_push_wake_public_key};
use lakko_crypto::signing::SigningKey;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tower::ServiceExt as _;

use crate::internal::{build_internal_app, SECRET_HEADER};
use crate::provider::{DeliverError, Provider, ProviderKind, WakePayload};
use crate::{build_app, AppState};

const MASTER_HEX: &str = "7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c";
const SECRET_B64: &str = "c2hhcmVkLXdha2Utc2VjcmV0LWZvci10ZXN0cy0zMmI=";

/// What a recording provider saw: the token and the payload.
type Seen = Arc<Mutex<Vec<(ProviderKind, Vec<u8>, WakePayload)>>>;

struct Recording {
    kind: ProviderKind,
    seen: Seen,
    invalid: bool,
}

#[async_trait]
impl Provider for Recording {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError> {
        self.seen
            .lock()
            .unwrap()
            .push((self.kind, token.to_vec(), payload.clone()));
        if self.invalid {
            Err(DeliverError::TokenInvalid)
        } else {
            Ok(())
        }
    }
}

async fn pool(name: &str) -> Option<MySqlPool> {
    let Ok(url) = std::env::var("LAKKO_PUSH_GW_TEST_DATABASE_URL") else {
        eprintln!("skipping {name}: LAKKO_PUSH_GW_TEST_DATABASE_URL unset");
        return None;
    };
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect test DB");
    crate::tests::MIGRATED
        .get_or_init(|| async {
            crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        })
        .await;
    Some(pool)
}

fn state(db: MySqlPool, gw: &PushGwKey, seen: &Seen, invalid_fcm: bool) -> AppState {
    let mut providers: HashMap<ProviderKind, Box<dyn Provider>> = HashMap::new();
    for kind in [
        ProviderKind::Fcm,
        ProviderKind::Apns,
        ProviderKind::ApnsVoip,
    ] {
        providers.insert(
            kind,
            Box::new(Recording {
                kind,
                seen: Arc::clone(seen),
                invalid: invalid_fcm && kind == ProviderKind::Fcm,
            }),
        );
    }
    AppState {
        anon_tokens: TokenVerifier::new(
            db.clone(),
            AnonTokenServerConfig::from_values(Some(MASTER_HEX), None).unwrap(),
        ),
        db,
        providers: Arc::new(providers),
        internal_secret: Some(Arc::new(B64.decode(SECRET_B64).unwrap())),
        wake_key: Arc::new(gw.clone()),
        wake_guard: Arc::new(crate::wake_guard::WakeGuard::new(30, 60)),
    }
}

fn fresh<const N: usize>() -> [u8; N] {
    use rand::RngCore as _;
    let mut b = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

async fn subscribe(
    state: &AppState,
    secret: &[u8; 32],
    sub_id: &[u8; 16],
    entries: &[PushTokenEntry],
) -> StatusCode {
    let sealed = seal_push_subscription_for_secret(secret, sub_id, entries).unwrap();
    let body = serde_json::json!({
        "sub_id": B64URL.encode(sub_id),
        "push_gid": B64URL.encode(derive_push_gid(secret)),
        "wake_pk": B64URL.encode(derive_push_wake_public_key(secret)),
        "sealed": B64URL.encode(sealed),
    })
    .to_string();
    let token = mint_header(
        &MasterSecret::from_hex(MASTER_HEX).unwrap(),
        TokenContext::PushRegister,
        current_epoch(DEFAULT_WINDOW_SECS),
    );
    build_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/push/subscribe")
                .header("content-type", "application/json")
                .header(TOKEN_HEADER, token)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn notify(state: &AppState, wake: &[u8], pointer: u64) -> (StatusCode, serde_json::Value) {
    let body = serde_json::json!({
        "wake": B64URL.encode(wake),
        "envelope_pointer": pointer,
    })
    .to_string();
    let resp = build_internal_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/push/notify-group")
                .header("content-type", "application/json")
                .header(SECRET_HEADER, SECRET_B64)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn rows_of(db: &MySqlPool, push_gid: &[u8; 32]) -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)> {
    sqlx::query_as(
        "SELECT sub_id, push_gid, wake_pk, sealed FROM push_subscriptions WHERE push_gid = ?",
    )
    .bind(&push_gid[..])
    .fetch_all(db)
    .await
    .unwrap()
}

async fn cleanup(db: &MySqlPool, push_gid: &[u8; 32]) {
    sqlx::query("DELETE FROM push_subscriptions WHERE push_gid = ?")
        .bind(&push_gid[..])
        .execute(db)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_push_gw_tables_hold_no_token_in_the_clear_and_no_device_pseudonym() {
    let Some(db) = pool("no_token_in_the_clear").await else {
        return;
    };
    // The schema: the two tables that held the bipartite graph are gone, and
    // the one subscription table has exactly the four ADR-0078 columns.
    let tables: Vec<(String,)> = sqlx::query_as("SHOW TABLES").fetch_all(&db).await.unwrap();
    let names: Vec<String> = tables.into_iter().map(|(t,)| t.to_lowercase()).collect();
    assert!(!names.contains(&"push_tokens".to_string()), "{names:?}");
    assert!(
        !names.contains(&"push_group_subscriptions".to_string()),
        "{names:?}"
    );
    let columns: Vec<(String,)> = sqlx::query_as(
        "SELECT COLUMN_NAME FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'push_subscriptions' \
         ORDER BY ORDINAL_POSITION",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    let columns: Vec<String> = columns.into_iter().map(|(c,)| c).collect();
    assert_eq!(columns, ["sub_id", "push_gid", "wake_pk", "sealed"]);
    // Nothing in any push-gw table is named after a device or a token.
    let all: Vec<(String,)> = sqlx::query_as(
        "SELECT LOWER(COLUMN_NAME) FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = DATABASE()",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    for (c,) in all {
        assert!(!c.contains("device") && !c.contains("token"), "{c}");
    }

    // The data: one device subscribes to two groups with one recognisable
    // token. No stored byte string contains the token, and the two rows
    // share no value but nothing a device could be named by.
    let gw = PushGwKey::generate();
    let seen: Seen = Arc::default();
    let st = state(db.clone(), &gw, &seen, false);
    let token = b"fcm-token-that-would-be-recognised-in-a-dump".to_vec();
    let entries = [PushTokenEntry {
        provider: PushProvider::Fcm,
        token: token.clone(),
    }];
    let (g1, g2) = (fresh::<32>(), fresh::<32>());
    let (s1, s2) = (fresh::<16>(), fresh::<16>());
    assert_eq!(subscribe(&st, &g1, &s1, &entries).await, StatusCode::OK);
    assert_eq!(subscribe(&st, &g2, &s2, &entries).await, StatusCode::OK);
    let r1 = rows_of(&db, &derive_push_gid(&g1)).await;
    let r2 = rows_of(&db, &derive_push_gid(&g2)).await;
    cleanup(&db, &derive_push_gid(&g1)).await;
    cleanup(&db, &derive_push_gid(&g2)).await;
    assert_eq!((r1.len(), r2.len()), (1, 1));
    for (sub, gid, pk, sealed) in [&r1[0], &r2[0]] {
        for column in [sub, gid, pk, sealed] {
            assert!(!column.windows(token.len()).any(|w| w == token.as_slice()));
        }
    }
    assert_ne!(r1[0].0, r2[0].0, "no shared device id");
    assert_ne!(r1[0].3, r2[0].3, "no sealed list equal across groups");
}

#[tokio::test]
async fn a_genuine_wake_reaches_the_subscription_and_names_it() {
    let Some(db) = pool("a_genuine_wake").await else {
        return;
    };
    let gw = PushGwKey::generate();
    let seen: Seen = Arc::default();
    let st = state(db.clone(), &gw, &seen, false);
    let secret = fresh::<32>();
    let sub_id = fresh::<16>();
    let entries = [
        PushTokenEntry {
            provider: PushProvider::Apns,
            token: vec![1; 32],
        },
        PushTokenEntry {
            provider: PushProvider::ApnsVoip,
            token: vec![2; 32],
        },
    ];
    assert_eq!(
        subscribe(&st, &secret, &sub_id, &entries).await,
        StatusCode::OK
    );

    let wake = seal_push_wake(&gw.public(), &secret, PushWakeKind::Call, now_ms()).unwrap();
    let (status, body) = notify(&st, &wake, 42).await;
    // The same wake again is a replay.
    let (replay, _) = notify(&st, &wake, 42).await;
    cleanup(&db, &derive_push_gid(&secret)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["subscriptions_woken"], 1);
    assert_eq!(replay, StatusCode::CONFLICT);
    let seen = seen.lock().unwrap();
    // A call reaches the call token only, never both Apple tokens.
    assert_eq!(seen.len(), 1);
    let (kind, token, payload) = &seen[0];
    assert_eq!(*kind, ProviderKind::ApnsVoip);
    assert_eq!(token, &vec![2u8; 32]);
    assert_eq!(payload.subscription_id, sub_id);
    assert_eq!(payload.group_pseudonym, Some(derive_push_gid(&secret)));
    assert_eq!(payload.envelope_pointer, Some(42));
}

#[tokio::test]
async fn a_wake_not_signed_by_the_wake_key_is_refused() {
    let Some(db) = pool("a_wake_not_signed").await else {
        return;
    };
    let gw = PushGwKey::generate();
    let seen: Seen = Arc::default();
    let st = state(db.clone(), &gw, &seen, false);
    let secret = fresh::<32>();
    let sub_id = fresh::<16>();
    let entries = [PushTokenEntry {
        provider: PushProvider::Fcm,
        token: vec![9; 40],
    }];
    assert_eq!(
        subscribe(&st, &secret, &sub_id, &entries).await,
        StatusCode::OK
    );
    let push_gid = derive_push_gid(&secret);

    // 1. Somebody who knows the push_gid and claims the group's wake key,
    //    but signs with a key of its own: refused before any row is read.
    let attacker = SigningKey::from_seed(&fresh::<32>());
    let forged = seal_with_claimed_key(
        &gw.public(),
        &push_gid,
        PushWakeKind::Message,
        now_ms(),
        &fresh::<16>(),
        &fresh::<32>(),
        &derive_push_wake_public_key(&secret),
        &attacker,
    )
    .unwrap();
    let (forged_status, _) = notify(&st, &forged, 1).await;

    // 2. The same somebody signing honestly under its own key: the wake
    //    verifies, but selects only rows registered under that key - none.
    let own = seal_with_signer(
        &gw.public(),
        &push_gid,
        PushWakeKind::Message,
        now_ms(),
        &fresh::<16>(),
        &fresh::<32>(),
        &attacker,
    )
    .unwrap();
    let (own_status, own_body) = notify(&st, &own, 1).await;

    // 3. A wake sealed to another push-gw key does not open here.
    let elsewhere = seal_push_wake(
        &PushGwKey::generate().public(),
        &secret,
        PushWakeKind::Message,
        now_ms(),
    )
    .unwrap();
    let (elsewhere_status, _) = notify(&st, &elsewhere, 1).await;

    // 4. A genuine wake with a stale clock.
    let stale = seal_push_wake_with_nonce(
        &gw.public(),
        &secret,
        PushWakeKind::Message,
        now_ms() - 60 * 60 * 1000,
        &fresh::<16>(),
    )
    .unwrap();
    let (stale_status, _) = notify(&st, &stale, 1).await;

    cleanup(&db, &push_gid).await;
    assert_eq!(forged_status, StatusCode::FORBIDDEN);
    assert_eq!(own_status, StatusCode::OK);
    assert_eq!(own_body["subscriptions_woken"], 0);
    assert_eq!(elsewhere_status, StatusCode::FORBIDDEN);
    assert_eq!(stale_status, StatusCode::FORBIDDEN);
    assert!(seen.lock().unwrap().is_empty(), "nothing was delivered");
}

#[tokio::test]
async fn a_rejected_token_is_removed_from_its_row() {
    let Some(db) = pool("a_rejected_token").await else {
        return;
    };
    let gw = PushGwKey::generate();
    let seen: Seen = Arc::default();
    // FCM reports every token invalid; APNs accepts.
    let st = state(db.clone(), &gw, &seen, true);
    let secret = fresh::<32>();
    let push_gid = derive_push_gid(&secret);
    let (both, fcm_only) = (fresh::<16>(), fresh::<16>());
    let apns = PushTokenEntry {
        provider: PushProvider::Apns,
        token: vec![3; 32],
    };
    let fcm = PushTokenEntry {
        provider: PushProvider::Fcm,
        token: vec![4; 32],
    };
    assert_eq!(
        subscribe(&st, &secret, &both, &[apns.clone(), fcm.clone()]).await,
        StatusCode::OK
    );
    assert_eq!(
        subscribe(&st, &secret, &fcm_only, &[fcm]).await,
        StatusCode::OK
    );

    let wake = seal_push_wake(&gw.public(), &secret, PushWakeKind::Message, now_ms()).unwrap();
    let (status, _) = notify(&st, &wake, 7).await;
    let rows = rows_of(&db, &push_gid).await;
    cleanup(&db, &push_gid).await;

    assert_eq!(status, StatusCode::OK);
    // The FCM-only row is gone; the other is re-sealed with APNs alone.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, both.to_vec());
    let unlock = lakko_contacts::derive_push_wake_unlock_key(&secret);
    let left =
        lakko_contacts::open_push_subscription(unlock.as_bytes(), &push_gid, &both, &rows[0].3)
            .unwrap();
    assert_eq!(left, vec![apns]);
}
