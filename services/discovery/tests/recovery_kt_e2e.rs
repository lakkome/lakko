// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Recovery + KT-log live e2e — drives the **publish -> genesis-log-append ->
//! fetch-verify -> recovery -> rotation-chain** pipeline against a LIVE
//! dockerised `discovery` + `transparency` stack (`ci/recovery-e2e/`).
//!
//! This is the server + crypto closure of the deferred "live 2-device e2e"
//! residual carried by ADR-0016 / 0017 / 0018 + G-30 #2 (ADR-0019). It runs the
//! REAL client-side crypto (`Identity`, `sign_leaf_genesis`,
//! `SupersessionProof`, `verify_inclusion` / `verify_leaf_genesis` /
//! `verify_supersession_chain`) against the live log; it does NOT drive the
//! three client runtimes (web / iOS / Android) — that stays deferred.
//!
//! ## Why `#[ignore]`?
//!
//! It needs the docker-compose stack up (discovery + transparency + their
//! MariaDB), far above the per-PR `cargo test` budget. CI runs it from
//! `.github/workflows/recovery-e2e.yml` with `--ignored`; the dev flow is in
//! [`ci/recovery-e2e/README.md`](../../../ci/recovery-e2e/README.md).
//!
//! ## No auth / OPAQUE
//!
//! Discovery's `/discovery/identity` is gated by a SHARED-SECRET session token
//! (`lakko-auth-token`, `LAKKO_SESSION_TOKEN_SECRET`), so the driver MINTS the
//! token directly — the auth service is not in the stack. The shared secret
//! defaults to the value the committed compose sets; override with
//! `LAKKO_RECOVERY_E2E_SESSION_SECRET`.

#![cfg(test)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use lakko_auth_token::TokenSecret;
use lakko_crypto::hash::blake3;
use lakko_crypto::signing::SigningKey;
use lakko_mls::Identity;
use lakko_pow::solve;
use lakko_transparency::{
    sign_leaf_genesis, verify_inclusion, verify_leaf_genesis, verify_supersession_chain,
    verify_witness_cosig, verify_witness_quorum, InclusionProof, LogEntry, PinnedIdentity,
    RotationEvidence, SignedRoot, SupersessionProof, TreeHash,
};
use serde::Deserialize;
use serde_json::json;

// ---------------------------------------------------------------------------
// Config (env-overridable; defaults match the committed compose)
// ---------------------------------------------------------------------------

fn discovery_url() -> String {
    std::env::var("LAKKO_RECOVERY_E2E_DISCOVERY_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8084".to_owned())
}

fn transparency_url() -> String {
    std::env::var("LAKKO_RECOVERY_E2E_TRANSPARENCY_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8087".to_owned())
}

fn witness_url() -> String {
    std::env::var("LAKKO_RECOVERY_E2E_WITNESS_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8088".to_owned())
}

/// The 2nd + 3rd witnesses of the ADR-0039 2-of-3 quorum (published host ports
/// 8089 / 8090 in the committed compose).
fn witness2_url() -> String {
    std::env::var("LAKKO_RECOVERY_E2E_WITNESS2_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8089".to_owned())
}

fn witness3_url() -> String {
    std::env::var("LAKKO_RECOVERY_E2E_WITNESS3_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8090".to_owned())
}

/// The witness cosign shared secret (base64), presented as `Bearer <b64>` on
/// `POST /witness/cosign`. Must match the witness container's
/// `LAKKO_WITNESS_COSIGN_SECRET` (the compose default is base64(16x 0x77)).
fn witness_cosign_secret() -> String {
    std::env::var("LAKKO_RECOVERY_E2E_WITNESS_COSIGN_SECRET")
        .unwrap_or_else(|_| B64.encode([0x77_u8; 16]))
}

/// The FIXED operator seed the e2e transparency container runs with
/// (`LAKKO_TRANSPARENCY_OPERATOR_SEED_HEX = 0xAB x 32`). The fork-refusal step
/// reconstructs the operator SIGNING key from it to mint a valid operator
/// signature over a forged root — so the witness rejects the root on the
/// consistency check (a real fork), not on the operator-signature gate.
const E2E_OPERATOR_SEED: [u8; 32] = [0xAB; 32];

/// The shared session-token secret (base64). Must match the discovery
/// container's `LAKKO_SESSION_TOKEN_SECRET`.
fn session_secret() -> String {
    std::env::var("LAKKO_RECOVERY_E2E_SESSION_SECRET").unwrap_or_else(|_| B64.encode([0x5e_u8; 32]))
}

/// The account-key domain — MUST stay byte-identical to discovery's
/// `ACCOUNT_KEY_DOMAIN` + the FFI `transparencyAccountKey`.
const ACCOUNT_KEY_DOMAIN: &[u8] = b"lakko/transparency/account/v1";

/// `BLAKE3(domain || account_id)` — the leaf `username_hash`.
fn account_key(account_id: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(ACCOUNT_KEY_DOMAIN.len() + account_id.len());
    input.extend_from_slice(ACCOUNT_KEY_DOMAIN);
    input.extend_from_slice(account_id);
    blake3(&input)
}

fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
    .unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// Wire shapes (match the live JSON)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChallengeResponse {
    seed: String,
    effort_bits: u8,
}

#[derive(Deserialize)]
struct OperatorKeyRes {
    operator_pubkey_b64: String,
}

#[derive(Deserialize)]
struct RootRes {
    tree_root_b64: String,
    log_size: u64,
    timestamp_unix: u64,
    signature_b64: String,
    /// ADR-0020 + ADR-0039: the K-of-N witness co-signature BUNDLE (0..N
    /// entries), present when the witness integration is on. The clean-replace
    /// of the pre-ADR-0039 singular `witness_cosig_b64`.
    #[serde(default)]
    witness_cosigs_b64: Vec<String>,
}

#[derive(Deserialize)]
struct WitnessKeyRes {
    witness_pubkey_b64: String,
}

#[derive(Deserialize)]
struct LatestRes {
    idx: u64,
    version: u64,
    identity_pubkey_b64: String,
    #[allow(dead_code)]
    is_reset: bool,
    #[serde(default)]
    leaf_auth_b64: Option<String>,
}

#[derive(Deserialize)]
struct ProofRes {
    #[allow(dead_code)]
    idx: u64,
    audit_path: Vec<String>,
    #[allow(dead_code)]
    log_size: u64,
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// A fresh PoW proof solved against a just-issued discovery challenge, in the
/// `{ seed, nonce, solution }` base64 wire shape discovery deserializes.
async fn fresh_pow(client: &reqwest::Client) -> serde_json::Value {
    let chall: ChallengeResponse = client
        .post(format!("{}/pow/challenge", discovery_url()))
        .send()
        .await
        .expect("pow/challenge request")
        .error_for_status()
        .expect("pow/challenge status")
        .json()
        .await
        .expect("pow/challenge body");

    let seed_vec = B64.decode(chall.seed.as_bytes()).expect("seed b64");
    let seed: [u8; 16] = seed_vec.as_slice().try_into().expect("seed is 16 bytes");
    let proof = solve(&seed, chall.effort_bits).expect("solve pow");

    json!({
        "seed": B64.encode(proof.seed),
        "nonce": B64.encode(proof.nonce),
        "solution": B64.encode(proof.solution),
    })
}

/// Upload an identity bundle to discovery. Returns the raw HTTP status so the
/// negative test can assert a refusal. `leaf_auth` / `supersession_proof` are
/// base64 when present.
async fn upload_bundle(
    client: &reqwest::Client,
    token: &str,
    account_id: &[u8],
    signature_pubkey: &[u8],
    x25519_pubkey: &[u8],
    leaf_auth: Option<&[u8]>,
    supersession_proof: Option<&[u8]>,
) -> reqwest::StatusCode {
    let mut body = json!({
        "pow": fresh_pow(client).await,
        "account_id": B64.encode(account_id),
        "signature_pubkey": B64.encode(signature_pubkey),
        "x25519_pubkey": B64.encode(x25519_pubkey),
    });
    if let Some(la) = leaf_auth {
        body["leaf_auth"] = json!(B64.encode(la));
    }
    if let Some(sp) = supersession_proof {
        body["supersession_proof"] = json!(B64.encode(sp));
    }

    client
        .post(format!("{}/discovery/identity", discovery_url()))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("identity upload request")
        .status()
}

/// Poll `GET /transparency/latest/:account_key` until an entry whose
/// `identity_pubkey` equals `want_pubkey` appears at `>= want_version`, or time
/// out. Discovery's append is best-effort + a reconciler sweeps, so the log is
/// eventually-consistent — poll up to ~30 s.
async fn poll_latest(
    client: &reqwest::Client,
    account_key: &[u8; 32],
    want_pubkey: &[u8],
    want_min_version: u64,
) -> LatestRes {
    let slug = B64URL.encode(account_key);
    let url = format!("{}/transparency/latest/{slug}", transparency_url());
    for _ in 0..60 {
        let resp = client.get(&url).send().await.expect("latest request");
        if resp.status().is_success() {
            let latest: LatestRes = resp.json().await.expect("latest body");
            let pk = B64
                .decode(latest.identity_pubkey_b64.as_bytes())
                .expect("latest pubkey b64");
            if pk.as_slice() == want_pubkey && latest.version >= want_min_version {
                return latest;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("timed out waiting for the log to commit the expected (pubkey, version)");
}

async fn fetch_root(client: &reqwest::Client) -> SignedRoot {
    fetch_root_with_cosigs(client).await.0
}

/// Fetch the signed root AND its K-of-N witness co-signature BUNDLE
/// (ADR-0020 + ADR-0039). The bundle is 0..N base64 cosigs decoded to bytes.
async fn fetch_root_with_cosigs(client: &reqwest::Client) -> (SignedRoot, Vec<Vec<u8>>) {
    let r: RootRes = client
        .get(format!("{}/transparency/root", transparency_url()))
        .send()
        .await
        .expect("root request")
        .error_for_status()
        .expect("root status")
        .json()
        .await
        .expect("root body");
    let tree_root: TreeHash = B64
        .decode(r.tree_root_b64.as_bytes())
        .expect("tree_root b64")
        .as_slice()
        .try_into()
        .expect("tree_root is 32 bytes");
    let signature = lakko_crypto::signing::Signature::from_bytes(
        &B64.decode(r.signature_b64.as_bytes()).expect("sig b64"),
    )
    .expect("signature is 64 bytes");
    let cosigs: Vec<Vec<u8>> = r
        .witness_cosigs_b64
        .iter()
        .map(|c| B64.decode(c.as_bytes()).expect("witness cosig b64"))
        .collect();
    (
        SignedRoot {
            tree_root,
            log_size: r.log_size,
            timestamp_unix: r.timestamp_unix,
            signature,
        },
        cosigs,
    )
}

/// Fetch a witness's published verifying key (ADR-0020) from `base_url`.
async fn fetch_witness_key(client: &reqwest::Client, base_url: &str) -> Vec<u8> {
    let r: WitnessKeyRes = client
        .get(format!("{base_url}/witness/key"))
        .send()
        .await
        .expect("witness key request")
        .error_for_status()
        .expect("witness key status")
        .json()
        .await
        .expect("witness key body");
    B64.decode(r.witness_pubkey_b64.as_bytes())
        .expect("witness pubkey b64")
}

/// Fetch all THREE witnesses' published verifying keys — the ADR-0039 K-of-N
/// pinned set. In this e2e the "pin" is each witness's live-published key
/// (a real deployment would hard-pin them build-time).
async fn fetch_all_witness_keys(client: &reqwest::Client) -> Vec<Vec<u8>> {
    let mut keys = Vec::with_capacity(3);
    for base in [witness_url(), witness2_url(), witness3_url()] {
        keys.push(fetch_witness_key(client, &base).await);
    }
    keys
}

/// Fetch + flatten the audit path for a leaf index.
async fn fetch_proof(client: &reqwest::Client, idx: u64) -> InclusionProof {
    let p: ProofRes = client
        .get(format!("{}/transparency/proof/{idx}", transparency_url()))
        .send()
        .await
        .expect("proof request")
        .error_for_status()
        .expect("proof status")
        .json()
        .await
        .expect("proof body");
    let audit_path: Vec<TreeHash> = p
        .audit_path
        .iter()
        .map(|h| {
            B64.decode(h.as_bytes())
                .expect("audit hash b64")
                .as_slice()
                .try_into()
                .expect("audit hash is 32 bytes")
        })
        .collect();
    InclusionProof {
        leaf_index: idx,
        audit_path,
    }
}

async fn operator_pubkey(client: &reqwest::Client) -> [u8; 32] {
    let r: OperatorKeyRes = client
        .get(format!("{}/transparency/operator-key", transparency_url()))
        .send()
        .await
        .expect("operator-key request")
        .error_for_status()
        .expect("operator-key status")
        .json()
        .await
        .expect("operator-key body");
    B64.decode(r.operator_pubkey_b64.as_bytes())
        .expect("operator pubkey b64")
        .as_slice()
        .try_into()
        .expect("operator pubkey is 32 bytes")
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "needs the ci/recovery-e2e docker-compose stack up"]
#[allow(clippy::too_many_lines)] // a single sequential e2e flow reads best whole
async fn recovery_kt_pipeline_publish_log_fetch_verify_rotate() {
    let client = reqwest::Client::new();
    let secret = TokenSecret::from_base64(&session_secret()).expect("session secret b64");

    // A run-unique account_id + identity seeds, so re-running without
    // `compose down -v` does not collide with a prior run's log state.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let run = blake3(&nonce.to_le_bytes());
    let account_id: [u8; 16] = run[..16].try_into().expect("16-byte account_id");
    let ak = account_key(&account_id);

    let token = secret
        .issue(&account_id, now_secs() * 1000 + 3_600_000)
        .expect("issue session token");

    // -- Identity v1 (genesis) ------------------------------------------------
    let old_seed = blake3(&[&run[..], b"old"].concat());
    let id_old = Identity::from_seed(&old_seed).expect("identity v1");
    let old_pubkey = id_old.signature_public_key().to_vec();
    let old_sk = SigningKey::from_seed(&id_old.ed25519_seed_bytes());
    // The AccountIdentity Ed25519 key MUST equal the key the ed25519 seed
    // derives (the genesis self-sig is verified against the served key).
    assert_eq!(
        old_sk.verifying_key().to_bytes().as_slice(),
        old_pubkey.as_slice(),
        "AccountIdentity pubkey must match SigningKey::from_seed(ed25519_seed)"
    );

    // Step 1 — publish the genesis bundle with a self-sig, then wait for the
    // log to commit it (discovery best-effort append + reconciler sweep).
    let genesis_sig = sign_leaf_genesis(&old_sk, &ak, &old_pubkey).to_vec();
    let status = upload_bundle(
        &client,
        &token,
        &account_id,
        &old_pubkey,
        &id_old.x25519_public_key(),
        Some(&genesis_sig),
        None,
    )
    .await;
    assert!(status.is_success(), "genesis publish failed: {status}");

    let latest_v1 = poll_latest(&client, &ak, &old_pubkey, 1).await;
    assert_eq!(latest_v1.version, 1, "genesis is version 1");

    // Step 2 — fetch-verify the genesis entry: inclusion + the genesis self-sig.
    let operator = operator_pubkey(&client).await;
    let root_v1 = fetch_root(&client).await;
    let proof_v1 = fetch_proof(&client, latest_v1.idx).await;
    let entry_v1 = LogEntry {
        username_hash: ak,
        identity_pubkey: old_pubkey.clone(),
        version: 1,
    };
    let operator_vk =
        lakko_crypto::signing::VerifyingKey::from_bytes(&operator).expect("operator vk");
    root_v1
        .verify(&operator_vk)
        .expect("root v1 operator-signed");
    verify_inclusion(&entry_v1, &proof_v1, &root_v1).expect("genesis inclusion verifies");
    let served_leaf_auth = B64
        .decode(
            latest_v1
                .leaf_auth_b64
                .as_ref()
                .expect("genesis entry carries leaf_auth")
                .as_bytes(),
        )
        .expect("leaf_auth b64");
    verify_leaf_genesis(&served_leaf_auth, &ak, &old_pubkey)
        .expect("served genesis self-sig authorises the served key");

    // Step 3 — recovery (same key): re-publish is idempotent, version stays 1.
    let status = upload_bundle(
        &client,
        &token,
        &account_id,
        &old_pubkey,
        &id_old.x25519_public_key(),
        Some(&genesis_sig),
        None,
    )
    .await;
    assert!(status.is_success(), "same-key re-publish failed: {status}");
    let latest_again = poll_latest(&client, &ak, &old_pubkey, 1).await;
    assert_eq!(
        latest_again.version, 1,
        "a same-key recovery must NOT advance the log version"
    );

    // Step 4 — rotation: new key + a supersession proof, verified by the chain.
    let new_seed = blake3(&[&run[..], b"new"].concat());
    let id_new = Identity::from_seed(&new_seed).expect("identity v2");
    let new_pubkey = id_new.signature_public_key().to_vec();
    let issued = now_secs();
    let proof_bytes = SupersessionProof::new(
        old_sk.verifying_key().to_bytes(),
        SigningKey::from_seed(&id_new.ed25519_seed_bytes())
            .verifying_key()
            .to_bytes(),
        issued,
        issued + 14 * 24 * 3600,
    )
    .sign(&old_sk)
    .to_vec();

    let status = upload_bundle(
        &client,
        &token,
        &account_id,
        &new_pubkey,
        &id_new.x25519_public_key(),
        Some(&proof_bytes),
        Some(&proof_bytes),
    )
    .await;
    assert!(status.is_success(), "rotation publish failed: {status}");

    let latest_v2 = poll_latest(&client, &ak, &new_pubkey, 2).await;
    assert!(latest_v2.version >= 2, "rotation advances the log version");

    let root_v2 = fetch_root(&client).await;
    let proof_v2 = fetch_proof(&client, latest_v2.idx).await;
    let entry_v2 = LogEntry {
        username_hash: ak,
        identity_pubkey: new_pubkey.clone(),
        version: latest_v2.version,
    };
    let old_pub_arr: [u8; 32] = old_pubkey.as_slice().try_into().expect("old pubkey 32");
    let pinned = PinnedIdentity {
        username_hash: &ak,
        old_pubkey: &old_pub_arr,
        version: 1,
        last_seen_root: &root_v1,
    };
    let evidence = RotationEvidence {
        operator: &operator_vk,
        new_root: &root_v2,
        new_entry: &entry_v2,
        inclusion: &proof_v2,
        supersession_proof: &proof_bytes,
    };
    let rotated =
        verify_supersession_chain(&pinned, &evidence, now_secs()).expect("rotation chain verifies");
    assert_eq!(
        rotated.new_pubkey.as_slice(),
        new_pubkey.as_slice(),
        "the chain authorises exactly the rotated-to key"
    );

    // Step 5 — negative: a fresh account uploading a NEW key with NO leaf_auth
    // is refused at the transparency append (REQUIRE_LEAF_AUTH=1), so the key
    // never appears in the log. We use a fresh account so the absence is
    // unambiguous (an existing account's prior entry would otherwise serve).
    let bogus_id: [u8; 16] = blake3(&[&run[..], b"bogus"].concat())[..16]
        .try_into()
        .expect("16-byte bogus id");
    let bogus_ak = account_key(&bogus_id);
    let bogus_token = secret
        .issue(&bogus_id, now_secs() * 1000 + 3_600_000)
        .expect("issue bogus token");
    let bogus_seed = blake3(&[&run[..], b"bogus-seed"].concat());
    let id_bogus = Identity::from_seed(&bogus_seed).expect("bogus identity");
    // Upload may be accepted by discovery (it stores opaque), but the
    // transparency append refuses the unauthorised leaf, so it never logs.
    let _ = upload_bundle(
        &client,
        &bogus_token,
        &bogus_id,
        id_bogus.signature_public_key(),
        &id_bogus.x25519_public_key(),
        None,
        None,
    )
    .await;
    let slug = B64URL.encode(bogus_ak);
    let url = format!("{}/transparency/latest/{slug}", transparency_url());
    // Give the (failing) append + a reconciler cycle time, then assert absent.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let resp = client.get(&url).send().await.expect("bogus latest request");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a keyless (no leaf_auth) binding must never enter the log"
    );

    // Step 6 — ADR-0020 + ADR-0039 K-of-N witness quorum. The served root now
    // carries a BUNDLE of witness co-signatures (`witness_cosigs_b64`). Pin all
    // THREE witness keys, assert the bundle carries >= K=2 cosigs each verifying
    // against a pinned key, and that the 2-of-3 quorum verifies over the served
    // (tree_root, log_size), AND the operator signature verifies too. This
    // proves the transparency -> N-witness fan-out round-trip live: the operator
    // cannot serve a root that fewer than K independent witnesses have co-signed
    // (the K-of-N split-view defence). K=2 tolerates one witness momentarily
    // absent from the best-effort bundle.
    let witness_keys = fetch_all_witness_keys(&client).await;
    assert_eq!(witness_keys.len(), 3, "three witnesses are configured");
    let (served_root, cosigs) = fetch_root_with_cosigs(&client).await;
    served_root
        .verify(&operator_vk)
        .expect("operator signature on the served root verifies");
    assert!(
        cosigs.len() >= 2,
        "the served bundle must carry at least K=2 witness cosigs; got {}",
        cosigs.len()
    );
    // Every served cosig verifies against exactly one of the pinned witness keys.
    for cosig in &cosigs {
        let matches = witness_keys.iter().any(|k| {
            verify_witness_cosig(cosig, k, &served_root.tree_root, served_root.log_size).is_ok()
        });
        assert!(
            matches,
            "every bundle cosig must verify against a pinned witness key"
        );
    }
    let cosig_refs: Vec<&[u8]> = cosigs.iter().map(Vec::as_slice).collect();
    let key_refs: Vec<&[u8]> = witness_keys.iter().map(Vec::as_slice).collect();
    // POSITIVE: the 2-of-3 quorum is satisfied over the served root.
    verify_witness_quorum(
        &cosig_refs,
        &key_refs,
        2,
        &served_root.tree_root,
        served_root.log_size,
    )
    .expect("the 2-of-3 K-of-N witness quorum verifies over the served root");

    // Step 6b — the meaningful NEGATIVE: a client that pins only ONE of the
    // three witness keys but demands a threshold of 2 can NEVER meet the quorum
    // (at most one distinct pinned key is satisfiable), so `verify_witness_quorum`
    // fails closed — a single witness's cosig(s) cannot fake a 2-of-N quorum.
    let one_key: [&[u8]; 1] = [key_refs[0]];
    assert!(
        verify_witness_quorum(
            &cosig_refs,
            &one_key,
            2,
            &served_root.tree_root,
            served_root.log_size,
        )
        .is_err(),
        "a threshold of 2 with only ONE pinned witness key must fail closed"
    );

    // Step 7 — ADR-0020 split-view defence, LIVE fork-refusal. A validly
    // operator-signed but FORKED root (the SAME log_size as the witness's
    // last-co-signed head, a DIFFERENT tree_root) is refused by the witness
    // with 409. This proves against the REAL witness service that the honest
    // witness will not co-sign two mutually-inconsistent roots — a malicious
    // operator cannot fork even WITH a valid operator signature.
    //
    // The witness pins the operator key derived from the compose's fixed seed
    // (E2E_OPERATOR_SEED); reconstruct the same SIGNING key here so the forged
    // root carries a VALID operator signature (else it would be rejected at the
    // operator-sig gate, masking the consistency-check assertion). Assert the
    // derived verifying key matches the live operator key first, so a seed
    // drift fails loud.
    let operator_signing_key = SigningKey::from_seed(&E2E_OPERATOR_SEED);
    assert_eq!(
        operator_signing_key.verifying_key().to_bytes(),
        operator_vk.to_bytes(),
        "the test's E2E_OPERATOR_SEED must match the transparency container's \
         LAKKO_TRANSPARENCY_OPERATOR_SEED_HEX (0xAB x 32)"
    );

    // The witness co-signed the current head (served_root) via co-sign-on-read
    // in step 6, so its high-water is (served_root.tree_root, log_size). Forge
    // a DIFFERENT root at the SAME size and operator-sign it.
    let mut forged_tree_root = served_root.tree_root;
    forged_tree_root[0] ^= 0xFF; // flip a byte -> a divergent same-size root
    let forged_payload = SignedRoot::signing_payload(
        &forged_tree_root,
        served_root.log_size,
        served_root.timestamp_unix,
    );
    let forged_sig = operator_signing_key.sign(&forged_payload);
    let fork_status = client
        .post(format!("{}/witness/cosign", witness_url()))
        .bearer_auth(witness_cosign_secret())
        .json(&json!({
            "new_root": {
                "tree_root_b64": B64.encode(forged_tree_root),
                "log_size": served_root.log_size,
                "timestamp_unix": served_root.timestamp_unix,
                "signature_b64": B64.encode(forged_sig.to_bytes()),
            },
            // A same-size fork: no consistency proof can reconcile N == N with
            // two different roots, so an empty proof is correct — the
            // same-size-requires-equal-roots check rejects it regardless.
            "consistency_proof_b64": [],
        }))
        .send()
        .await
        .expect("witness cosign (fork) request")
        .status();
    assert_eq!(
        fork_status,
        reqwest::StatusCode::CONFLICT,
        "the witness MUST refuse a same-size forked root (split-view defence, 409)"
    );

    // And a root carrying an INVALID operator signature is refused at the
    // operator-sig gate (400) — never reaching the consistency check.
    let bad_sig_status = client
        .post(format!("{}/witness/cosign", witness_url()))
        .bearer_auth(witness_cosign_secret())
        .json(&json!({
            "new_root": {
                "tree_root_b64": B64.encode(forged_tree_root),
                "log_size": served_root.log_size + 1,
                "timestamp_unix": served_root.timestamp_unix,
                "signature_b64": B64.encode([0u8; 64]),
            },
            "consistency_proof_b64": [],
        }))
        .send()
        .await
        .expect("witness cosign (bad sig) request")
        .status();
    assert_eq!(
        bad_sig_status,
        reqwest::StatusCode::BAD_REQUEST,
        "the witness MUST refuse a root with an invalid operator signature (400)"
    );
}
