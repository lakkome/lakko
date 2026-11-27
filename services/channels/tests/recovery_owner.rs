// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0101 section 7: the channel recovery key and the owner change it
//! proves. Each scenario runs against the in-memory store, and against
//! MariaDB when `LAKKO_CHANNELS_TEST_DATABASE_URL` is set (skipped without
//! it), through a real axum server driven with `reqwest`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use lakko_anon_token_server::{AnonTokenServerConfig, TokenVerifier};
use lakko_channels::owner::{ChannelOwnerSigner, OwnerRequest, OWNER_PROOF_HEADER_NAME};
use lakko_channels::{owner_handover_body, ChannelRecoveryKey, RECOVERY_PROOF_HEADER_NAME};
use lakko_svc_channels::{
    router, AppState, ChannelStore, Config, InMemoryChannelStore, SqlChannelStore, MIGRATIONS,
};
use serde_json::json;
use sqlx::mysql::MySqlPoolOptions;
use tokio::net::TcpListener;

const MASTER_HEX: &str = "0303030303030303030303030303030303030303030303030303030303030303";
const OLD_SEED: [u8; 32] = [5; 32];
const NEW_SEED: [u8; 32] = [6; 32];

fn config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_post_bytes: 64 * 1024,
        fetch_limit_default: 50,
        fetch_limit_max: 500,
        database_url: None,
        retention_secs: 0,
        subscriber_retention_secs: 0,
        prune_interval_secs: 0,
        envelope_retention_epochs: 4,
        internal_bind: "127.0.0.1:0".parse().unwrap(),
    }
}

async fn spawn_with(store: Arc<dyn ChannelStore>) -> SocketAddr {
    let verifier = TokenVerifier::in_memory(
        AnonTokenServerConfig::from_values(Some(MASTER_HEX), None).unwrap(),
    );
    let app = router(AppState::new(config(), store, verifier));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// Concurrent first runs of the migrator race on `_sqlx_migrations`.
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// The servers to run a scenario against: in-memory, and MariaDB when set.
async fn servers() -> Vec<SocketAddr> {
    let mut out = vec![spawn_with(Arc::new(InMemoryChannelStore::new())).await];
    if let Ok(url) = std::env::var("LAKKO_CHANNELS_TEST_DATABASE_URL") {
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        MIGRATED
            .get_or_init(|| async { MIGRATIONS.run(&pool).await.expect("run migrations") })
            .await;
        out.push(spawn_with(Arc::new(SqlChannelStore::new(pool))).await);
    } else {
        eprintln!("LAKKO_CHANNELS_TEST_DATABASE_URL unset; MariaDB half skipped");
    }
    out
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

fn fresh_nonce() -> [u8; 16] {
    static NEXT: AtomicU8 = AtomicU8::new(0);
    let mut n = [0u8; 16];
    lakko_crypto::random::fill(&mut n);
    n[0] = NEXT.fetch_add(1, Ordering::Relaxed);
    n
}

fn random_channel() -> ([u8; 16], String) {
    let mut id = [0u8; 16];
    lakko_crypto::random::fill(&mut id);
    (id, hex::encode(id))
}

fn owner_proof(signer: &ChannelOwnerSigner, method: &str, path: &str, body: &[u8]) -> String {
    signer
        .sign_at(
            &OwnerRequest {
                method,
                canonical_path: path,
                query: "",
                body,
            },
            now_ms(),
            fresh_nonce(),
        )
        .encode()
}

async fn code_of(resp: reqwest::Response) -> String {
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    v["code"].as_str().unwrap_or_default().to_owned()
}

/// Register a channel under the old owner key.
async fn create(client: &reqwest::Client, addr: SocketAddr, id: &[u8; 16], hex_id: &str) {
    let owner = ChannelOwnerSigner::from_account_seed(&OLD_SEED, id);
    let resp = client
        .post(format!("http://{addr}/v1/channels"))
        .json(&json!({
            "channel_id": hex_id,
            "owner_pubkey_hex": hex::encode(owner.verifying_key().to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
}

#[allow(clippy::too_many_lines)]
async fn scenario(addr: SocketAddr) {
    let client = reqwest::Client::new();
    let (id, hex_id) = random_channel();
    create(&client, addr, &id, &hex_id).await;
    let old = ChannelOwnerSigner::from_account_seed(&OLD_SEED, &id);
    let new = ChannelOwnerSigner::from_account_seed(&NEW_SEED, &id);
    // ADR-0101 section 7: a recovery key derived from the owned-channels
    // list key `R` and the channel id.
    let recovery = ChannelRecoveryKey::from_list_key(&[0x41; 32], &id);
    let stranger = ChannelRecoveryKey::from_list_key(&[0x42; 32], &id);
    let new_key = owner_handover_body(&new.verifying_key()).to_vec();
    let new_key_arr: [u8; 32] = new_key.clone().try_into().unwrap();
    let owner_path = format!("/v1/channels/{hex_id}/owner");
    let owner_url = format!("http://{addr}{owner_path}");
    let rk_path = format!("/v1/channels/{hex_id}/recovery-key");
    let rk_url = format!("http://{addr}{rk_path}");

    let by_recovery = |key: &ChannelRecoveryKey, body: Vec<u8>, named: [u8; 32], at: u64| {
        client
            .put(&owner_url)
            .header("content-type", "application/octet-stream")
            .header(
                RECOVERY_PROOF_HEADER_NAME,
                key.sign_owner_change(&id, &named, at).encode(),
            )
            .body(body)
            .send()
    };

    // No recovery key registered: a valid-looking proof is the uniform 401.
    let resp = by_recovery(&recovery, new_key.clone(), new_key_arr, now_ms())
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(code_of(resp).await, "owner-proof-required");

    // Registering a recovery key needs the owner proof over the body.
    let rk_body = recovery.verifying_key().to_bytes().to_vec();
    let resp = client
        .put(&rk_url)
        .body(rk_body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client
        .put(&rk_url)
        .header(
            OWNER_PROOF_HEADER_NAME,
            owner_proof(&new, "PUT", &rk_path, &rk_body),
        )
        .body(rk_body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "only the current owner registers it");
    for bad in [vec![1u8; 31], {
        let mut weak = vec![0u8; 32];
        weak[0] = 1;
        weak
    }] {
        let resp = client
            .put(&rk_url)
            .header(
                OWNER_PROOF_HEADER_NAME,
                owner_proof(&old, "PUT", &rk_path, &bad),
            )
            .body(bad)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
        assert_eq!(code_of(resp).await, "bad-recovery-pubkey");
    }
    // First a stranger's key, then replaced by the right one.
    for key in [&stranger, &recovery] {
        let body = key.verifying_key().to_bytes().to_vec();
        let resp = client
            .put(&rk_url)
            .header(
                OWNER_PROOF_HEADER_NAME,
                owner_proof(&old, "PUT", &rk_path, &body),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204);
    }

    // Refusals: the replaced key, a proof naming another owner, a stale
    // proof, a garbage header. All the same 401.
    for (key, named, at) in [
        (&stranger, new_key_arr, now_ms()),
        (&recovery, [9u8; 32], now_ms()),
        (&recovery, new_key_arr, now_ms() - 10 * 60 * 1000),
    ] {
        let resp = by_recovery(key, new_key.clone(), named, at).await.unwrap();
        assert_eq!(resp.status(), 401);
        assert_eq!(code_of(resp).await, "owner-proof-required");
    }
    let resp = client
        .put(&owner_url)
        .header(RECOVERY_PROOF_HEADER_NAME, "v1.1.AAAA")
        .body(new_key.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    // A body that is not a key.
    let resp = by_recovery(&recovery, vec![1u8; 31], new_key_arr, now_ms())
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(code_of(resp).await, "bad-owner-pubkey");

    // The recovery proof hands the channel to the new key.
    let at = now_ms();
    let resp = by_recovery(&recovery, new_key.clone(), new_key_arr, at)
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    // The identical request again (a lost answer): 204, nothing changes.
    let resp = by_recovery(&recovery, new_key.clone(), new_key_arr, at)
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // The new key now owns the channel and the old one is nobody.
    let rk_again = recovery.verifying_key().to_bytes().to_vec();
    let resp = client
        .put(&rk_url)
        .header(
            OWNER_PROOF_HEADER_NAME,
            owner_proof(&old, "PUT", &rk_path, &rk_again),
        )
        .body(rk_again.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client
        .put(&rk_url)
        .header(
            OWNER_PROOF_HEADER_NAME,
            owner_proof(&new, "PUT", &rk_path, &rk_again),
        )
        .body(rk_again)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // A used proof does not change the owner a second time: after the new
    // owner hands the channel to a third key, the same proof is refused.
    let third = ChannelOwnerSigner::from_account_seed(&[7; 32], &id);
    let third_key = owner_handover_body(&third.verifying_key()).to_vec();
    let resp = client
        .put(&owner_url)
        .header(
            OWNER_PROOF_HEADER_NAME,
            owner_proof(&new, "PUT", &owner_path, &third_key),
        )
        .body(third_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let resp = by_recovery(&recovery, new_key.clone(), new_key_arr, at)
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "a recovery proof is honoured once");
    // A fresh one is honoured (the recovery key survives an owner change).
    let resp = by_recovery(&recovery, new_key.clone(), new_key_arr, now_ms() + 1)
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // A deleted channel: refused like an unknown one, as the owner-proof
    // path does (the channel's rows, its recovery key included, are gone).
    let del_path = format!("/v1/channels/{hex_id}");
    let resp = client
        .delete(format!("http://{addr}{del_path}"))
        .header(
            OWNER_PROOF_HEADER_NAME,
            owner_proof(&new, "DELETE", &del_path, &[]),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let resp = by_recovery(&recovery, new_key.clone(), new_key_arr, now_ms() + 2)
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client
        .put(&owner_url)
        .header(
            OWNER_PROOF_HEADER_NAME,
            owner_proof(&new, "PUT", &owner_path, &new_key),
        )
        .body(new_key.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn the_recovery_key_hands_the_channel_to_a_new_owner() {
    for addr in servers().await {
        scenario(addr).await;
    }
}
