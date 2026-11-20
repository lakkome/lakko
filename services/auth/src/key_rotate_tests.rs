// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests of ADR-0074 sprint T5, through the router with the real
//! OPAQUE client: `POST /account/keys/rotate/{start,finish}`, the session
//! upgrade of `POST /auth/session/refresh`, and the two recall follow-ups
//! (a stale statement is told the time; a lost `finish` answer can be asked
//! again). A child of `recall_tests`, whose harness it uses.
//!
//! Gated on `LAKKO_AUTH_TEST_DATABASE_URL` like the rest; they skip without
//! it.

use lakko_attest::{session_upgrade_body, KeyResetAttestation};
use lakko_transparency::SupersessionProof;

use super::*;

const KEPT_DEVICE: [u8; 32] = [0xB7; 32];
const STOLEN_DEVICE: [u8; 32] = [0x57; 32];

fn roomy_rotation() -> Limits {
    roomy().with_name_window(Scope::KeyRotate, Window::new(1000, 900))
}

/// A supersession proof `old -> new`, valid 14 days from now.
fn proof(old: &SigningKey, new: &SigningKey) -> String {
    let now = now_ms() / 1000;
    b64u(&SupersessionProof::new(pubkey(old), pubkey(new), now, now + 14 * 86_400).sign(old))
}

/// One rotation as the rotating device runs it. `proof_b64` is given by the
/// caller so that a test can present a wrong one.
async fn rotate(
    state: &AppState,
    session: &Value,
    new_seed: &[u8],
    new_key: &SigningKey,
    proof_b64: &str,
    revoke: &[[u8; 32]],
) -> (StatusCode, Bytes) {
    let bearer = token(session);
    let fresh = session["fresh_grant"].as_str().map(str::to_owned);
    let (cs, request) = reg_start(new_seed);
    let (st, body) = post(
        state,
        "/account/keys/rotate/start",
        Some(&bearer),
        &json!({ "fresh_grant": fresh, "recovery_request": request }),
    )
    .await;
    if st != StatusCode::OK {
        return (st, body);
    }
    let upload = reg_finish(
        cs,
        new_seed,
        json_of(&body)["recovery_response"].as_str().unwrap(),
    );
    let revoke: Vec<String> = revoke.iter().map(|id| b64u(id)).collect();
    post(
        state,
        "/account/keys/rotate/finish",
        Some(&bearer),
        &json!({
            "fresh_grant": fresh,
            "recovery_upload": upload,
            "new_signature_pubkey_b64": b64u(&pubkey(new_key)),
            "identity_proof_b64": identity_proof(&account_of_token(&bearer), new_key),
            "supersession_proof_b64": proof_b64,
            "revoke_device_ids_b64": revoke,
        }),
    )
    .await
}

/// `POST /auth/session/refresh`, optionally with an upgrade statement by
/// `key` over `refresh` signed at `unix_ms`.
async fn refresh_with(
    state: &AppState,
    account: &[u8; 16],
    refresh: &str,
    statement_by: Option<(&SigningKey, &str, i64)>,
) -> (StatusCode, Bytes) {
    let mut body = json!({ "refresh_token": refresh });
    if let Some((key, covered, unix_ms)) = statement_by {
        body["key_statement_b64"] = json!(statement(
            key,
            AccountStatementKind::SessionUpgrade,
            account,
            &session_upgrade_body(covered),
            unix_ms,
        ));
    }
    post(state, "/auth/session/refresh", None, &body).await
}

async fn plant_passkey_with_vault_totp_and_grant(pool: &MySqlPool, account: &[u8; 16]) {
    sqlx::query(
        "INSERT INTO totp_secrets (account_id, secret_enc, confirmed_at, last_accepted_step) \
         VALUES (?, ?, NOW(3), 0)",
    )
    .bind(&account[..])
    .bind(&[0x5Eu8; 48][..])
    .execute(pool)
    .await
    .expect("totp");
    let mut credential_id = b"rotate-test-".to_vec();
    credential_id.extend_from_slice(account);
    sqlx::query(
        "INSERT INTO webauthn_credentials (credential_id, account_id, passkey_bytes) \
         VALUES (?, ?, ?)",
    )
    .bind(&credential_id)
    .bind(&account[..])
    .bind(&b"opaque"[..])
    .execute(pool)
    .await
    .expect("passkey");
    sqlx::query(
        "INSERT INTO passkey_vaults (account_id, credential_id, body_enc) VALUES (?, ?, ?)",
    )
    .bind(&account[..])
    .bind(&credential_id)
    .bind(&[0x7Au8; 72][..])
    .execute(pool)
    .await
    .expect("vault");
    let mut grant_hash = [0x6Eu8; 32];
    grant_hash[..16].copy_from_slice(account);
    sqlx::query(
        "INSERT INTO device_grants (account_id, grant_hash, expires_at) \
         VALUES (?, ?, NOW(3) + INTERVAL 300 SECOND)",
    )
    .bind(&account[..])
    .bind(&grant_hash[..])
    .execute(pool)
    .await
    .expect("device grant");
}

fn assert_code(st: StatusCode, body: &Bytes, expected_status: StatusCode, expected: &str) {
    assert_eq!(st, expected_status, "{}", text(body));
    assert_eq!(code(body), expected, "{}", text(body));
}

// ----------------------------------------------------------------
// The rotation
// ----------------------------------------------------------------

/// Someone who knows the password signs in and holds a fresh-auth grant.
/// They can reset; they cannot rotate, whatever proof they bring, and they
/// spend none of the owner's rotation budget trying.
#[tokio::test]
async fn a_password_only_session_cannot_rotate() {
    let pool = db_or_skip!("a_password_only_session_cannot_rotate");
    // A rotation budget of one: any charge by the attacker would lock the
    // owner out below.
    let state = build_state(
        pool.clone(),
        roomy().with_name_window(Scope::KeyRotate, Window::new(1, 900)),
    );
    let k0 = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &k0).await;
    let attacker = login(&state, &account.handle, b"pw", &ATTACKER_DEVICE)
        .await
        .unwrap();
    let theirs = fresh_key();
    let target = fresh_key();

    // A proof by their own key; a proof that names the owner's key as the
    // old key but is signed by theirs; a proof for another new key than the
    // request names; garbage of the right length.
    let now = now_ms() / 1000;
    let claiming_k0 =
        b64u(&SupersessionProof::new(pubkey(&k0), pubkey(&target), now, now + 60).sign(&theirs));
    for (what, proof_b64) in [
        ("their own key", proof(&theirs, &target)),
        ("the owner's key claimed, their signature", claiming_k0),
        ("a proof for another new key", proof(&theirs, &fresh_key())),
        ("garbage", b64u(&[0u8; 145])),
    ] {
        let (st, body) = rotate(&state, &attacker, &fresh_seed(), &target, &proof_b64, &[]).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{what}: {}", text(&body));
        assert_eq!(code(&body), "account-key-required", "{what}");
    }
    assert_eq!(
        identity_of(&pool, &account.id).await,
        (1, pubkey(&k0).to_vec()),
        "nothing moved"
    );
    assert_eq!(count(&pool, "recall_anchors", &account.id).await, 0);

    // The owner, with the old key, rotates: the one unit of budget is whole.
    let owner = login(&state, &account.handle, b"pw", &OWNER_DEVICE)
        .await
        .unwrap();
    let k1 = fresh_key();
    let (st, body) = rotate(&state, &owner, &fresh_seed(), &k1, &proof(&k0, &k1), &[]).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(
        identity_of(&pool, &account.id).await,
        (2, pubkey(&k1).to_vec())
    );
}

/// Without a grant the routes are `fresh-auth-required` before anything; an
/// account with no recorded identity key cannot rotate at all.
#[tokio::test]
async fn a_rotation_needs_the_grant_and_a_recorded_key() {
    let pool = db_or_skip!("a_rotation_needs_the_grant_and_a_recorded_key");
    let state = build_state(pool.clone(), roomy_rotation());
    let k0 = fresh_key();
    let k1 = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &k0).await;
    let session = login(&state, &account.handle, b"pw", &OWNER_DEVICE)
        .await
        .unwrap();

    // No grant: start and finish.
    let mut bare = session.clone();
    bare.as_object_mut().unwrap().remove("fresh_grant");
    let (st, body) = rotate(&state, &bare, &fresh_seed(), &k1, &proof(&k0, &k1), &[]).await;
    assert_code(st, &body, StatusCode::FORBIDDEN, "fresh-auth-required");
    let (st, body) = post(
        &state,
        "/account/keys/rotate/finish",
        Some(&token(&session)),
        &json!({
            "recovery_upload": registration_upload(&state, &account.id, KIND_RECOVERY, b"x"),
            "new_signature_pubkey_b64": b64u(&pubkey(&k1)),
            "identity_proof_b64": identity_proof(&account.id, &k1),
            "supersession_proof_b64": proof(&k0, &k1),
        }),
    )
    .await;
    assert_code(st, &body, StatusCode::FORBIDDEN, "fresh-auth-required");
    // No bearer at all.
    let (st, _) = post(&state, "/account/keys/rotate/start", None, &json!({})).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Malformed bodies are 400 and cost nothing.
    for (field, value) in [
        ("identity_proof_b64", json!(null)),
        (
            "identity_proof_b64",
            json!(identity_proof(&account.id, &k0)),
        ),
        ("supersession_proof_b64", json!(b64u(&[0u8; 144]))),
        ("new_signature_pubkey_b64", json!(b64u(&[1u8; 31]))),
        ("revoke_device_ids_b64", json!([b64u(&OWNER_DEVICE)])),
        ("revoke_device_ids_b64", json!([b64u(&[0u8; 32])])),
        (
            "revoke_device_ids_b64",
            json!((1u8..=9).map(|b| b64u(&[b; 32])).collect::<Vec<_>>()),
        ),
    ] {
        let mut body = json!({
            "fresh_grant": grant(&session),
            "recovery_upload": registration_upload(&state, &account.id, KIND_RECOVERY, b"x"),
            "new_signature_pubkey_b64": b64u(&pubkey(&k1)),
            "identity_proof_b64": identity_proof(&account.id, &k1),
            "supersession_proof_b64": proof(&k0, &k1),
        });
        body[field] = value;
        let (st, bytes) = post(
            &state,
            "/account/keys/rotate/finish",
            Some(&token(&session)),
            &body,
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{field}: {}", text(&bytes));
    }
    assert_eq!(identity_of(&pool, &account.id).await.0, 1);

    // An account whose key was never recorded.
    let id = *Uuid::now_v7().as_bytes();
    sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&id[..])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO opaque_registrations (account_id, kind, registration_bytes) VALUES (?, ?, ?)",
    )
    .bind(&id[..])
    .bind(KIND_PASSWORD)
    .bind(registration(&state, &id, KIND_PASSWORD, b"pw"))
    .execute(&pool)
    .await
    .unwrap();
    let (handle_bytes, handle) = crate::login_handle::random_handle();
    crate::login_handle::bind_for_test(&pool, &id, &handle_bytes).await;
    let unrecorded = login(&state, &handle, b"pw", &OWNER_DEVICE).await.unwrap();
    let (st, body) = rotate(
        &state,
        &unrecorded,
        &fresh_seed(),
        &k1,
        &proof(&k0, &k1),
        &[],
    )
    .await;
    assert_code(st, &body, StatusCode::FORBIDDEN, "account-key-required");
}

/// The whole of Stage A at auth: the rotation moves the generation and the
/// recorded key, replaces the recovery credential, keeps the password, TOTP
/// and the passkeys, deletes the vault and the device grant, captures the
/// anchor, floors the stolen device, and signs every other session out; a
/// kept device that holds the new key upgrades its session, and a device
/// that holds only the old key cannot.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn a_rotation_keeps_the_devices_that_hold_the_new_key_and_no_others() {
    let pool = db_or_skip!("a_rotation_keeps_the_devices_that_hold_the_new_key_and_no_others");
    let state = build_state(pool.clone(), roomy_rotation());
    let k0 = fresh_key();
    let seed0 = fresh_seed();
    let account = new_account(&state, b"pw", &seed0, &k0).await;
    let rotating = login(&state, &account.handle, b"pw", &OWNER_DEVICE)
        .await
        .unwrap();
    let kept = login(&state, &account.handle, b"pw", &KEPT_DEVICE)
        .await
        .unwrap();
    let stolen = login(&state, &account.handle, b"pw", &STOLEN_DEVICE)
        .await
        .unwrap();
    plant_passkey_with_vault_totp_and_grant(&pool, &account.id).await;
    let password_before = password_registration(&pool, &account.id).await;

    let k1 = fresh_key();
    let seed1 = fresh_seed();
    let (st, body) = rotate(
        &state,
        &rotating,
        &seed1,
        &k1,
        &proof(&k0, &k1),
        &[STOLEN_DEVICE],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let rotated = json_of(&body);
    assert_eq!(rotated["key_generation"], 2);
    assert!(
        rotated.get("fresh_grant").is_none(),
        "a rotation mints no grant"
    );

    // The account.
    assert_eq!(
        identity_of(&pool, &account.id).await,
        (2, pubkey(&k1).to_vec())
    );
    let attestation = KeyResetAttestation::open(
        &URL_SAFE_NO_PAD
            .decode(rotated["key_reset_attest_b64"].as_str().unwrap())
            .unwrap(),
        &state.attest_key.signing.verifying_key(),
    )
    .expect("the attestation verifies under the attest key");
    assert_eq!(attestation.key_generation, 2);
    assert_eq!(attestation.new_signature_pubkey, pubkey(&k1));
    assert_eq!(
        attestation.account_ref,
        lakko_attest::key_reset_account_ref(&account.id)
    );

    // Kept: the password (byte for byte), TOTP, the passkey. Gone: the
    // vault that sealed the old phrase, the device grant.
    assert_eq!(
        password_registration(&pool, &account.id).await,
        password_before
    );
    assert_eq!(count(&pool, "totp_secrets", &account.id).await, 1);
    assert_eq!(count(&pool, "webauthn_credentials", &account.id).await, 1);
    assert_eq!(count(&pool, "passkey_vaults", &account.id).await, 0);
    assert_eq!(count(&pool, "device_grants", &account.id).await, 0);
    assert_eq!(count(&pool, "device_session_floors", &account.id).await, 1);

    // The anchor: the old key and the password as it was.
    let captured = anchors(&pool, &account.id).await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].1, pubkey(&k0).to_vec());
    assert_eq!(captured[0].2, password_before);

    // The recovery credential: the old seed is dead, the new one signs in.
    assert!(!recovery_login(&state, &account.handle, &seed0).await);
    assert!(recovery_login(&state, &account.handle, &seed1).await);
    assert!(login(&state, &account.handle, b"pw", &OWNER_DEVICE)
        .await
        .is_some());

    // The rotating device's new pair works; every old bearer is superseded.
    assert_eq!(probe(&state, &token(&rotated)).await.0, StatusCode::OK);
    for old in [&rotating, &kept, &stolen] {
        let (st, body) = probe(&state, &token(old)).await;
        assert_code(
            st,
            &body,
            StatusCode::UNAUTHORIZED,
            "key-generation-superseded",
        );
    }
    // The grant was of generation 1: a second rotation with it is refused.
    let (st, body) = rotate(&state, &rotating, &fresh_seed(), &fresh_key(), "", &[]).await;
    assert_code(
        st,
        &body,
        StatusCode::UNAUTHORIZED,
        "key-generation-superseded",
    );

    // -- The kept device ------------------------------------------------
    let kept_refresh = refresh_token(&kept);
    // Without a statement, with a statement by the OLD key, with a statement
    // by the new key over ANOTHER credential, with garbage: superseded.
    let (st, body) = refresh_with(&state, &account.id, &kept_refresh, None).await;
    assert_code(
        st,
        &body,
        StatusCode::UNAUTHORIZED,
        "key-generation-superseded",
    );
    let (st, body) = refresh_with(
        &state,
        &account.id,
        &kept_refresh,
        Some((&k0, &kept_refresh, now_ms())),
    )
    .await;
    assert_code(
        st,
        &body,
        StatusCode::UNAUTHORIZED,
        "key-generation-superseded",
    );
    let (st, body) = refresh_with(
        &state,
        &account.id,
        &kept_refresh,
        Some((&k1, &refresh_token(&rotating), now_ms())),
    )
    .await;
    assert_code(
        st,
        &body,
        StatusCode::UNAUTHORIZED,
        "key-generation-superseded",
    );
    let (st, body) = post(
        &state,
        "/auth/session/refresh",
        None,
        &json!({ "refresh_token": kept_refresh, "key_statement_b64": "!!not base64" }),
    )
    .await;
    assert_code(
        st,
        &body,
        StatusCode::UNAUTHORIZED,
        "key-generation-superseded",
    );
    // A statement of another kind by the new key, over the same body.
    let (st, body) = post(
        &state,
        "/auth/session/refresh",
        None,
        &json!({
            "refresh_token": kept_refresh,
            "key_statement_b64": statement(
                &k1,
                AccountStatementKind::DeviceRevoke,
                &account.id,
                &session_upgrade_body(&kept_refresh),
                now_ms(),
            ),
        }),
    )
    .await;
    assert_code(
        st,
        &body,
        StatusCode::UNAUTHORIZED,
        "key-generation-superseded",
    );

    // A wrong clock on a device that DOES hold the new key: told the time.
    let (st, body) = refresh_with(
        &state,
        &account.id,
        &kept_refresh,
        Some((&k1, &kept_refresh, now_ms() - 600_000)),
    )
    .await;
    assert_code(st, &body, StatusCode::UNAUTHORIZED, "statement-stale");
    let server_time = json_of(&body)["server_time_ms"].as_i64().unwrap();
    assert!((server_time - now_ms()).abs() < 60_000);
    // A wrong clock on a device that holds only the old key: not told.
    let (st, body) = refresh_with(
        &state,
        &account.id,
        &kept_refresh,
        Some((&k0, &kept_refresh, now_ms() - 600_000)),
    )
    .await;
    assert_code(
        st,
        &body,
        StatusCode::UNAUTHORIZED,
        "key-generation-superseded",
    );

    // Signed again with the server's time, by the new key: upgraded.
    let (st, body) = refresh_with(
        &state,
        &account.id,
        &kept_refresh,
        Some((&k1, &kept_refresh, server_time)),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let upgraded = json_of(&body);
    assert_eq!(upgraded["key_generation"], 2);
    assert!(upgraded.get("fresh_grant").is_none());
    assert_eq!(probe(&state, &token(&upgraded)).await.0, StatusCode::OK);
    let claims = state
        .session_token_secret
        .verify(&token(&upgraded), now_ms())
        .unwrap();
    assert_eq!(
        claims.device_id, KEPT_DEVICE,
        "the pair names the same device"
    );
    assert_eq!(claims.key_generation, 2);
    // The upgraded refresh credential is an ordinary one from here on.
    let (st, body) = refresh_with(&state, &account.id, &refresh_token(&upgraded), None).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));

    // -- The stolen device ----------------------------------------------
    let stolen_refresh = refresh_token(&stolen);
    // It holds the old key only: superseded, never "removed" (it keeps
    // nothing a recall could not already be asked with).
    let (st, body) = refresh_with(
        &state,
        &account.id,
        &stolen_refresh,
        Some((&k0, &stolen_refresh, now_ms())),
    )
    .await;
    assert_code(
        st,
        &body,
        StatusCode::UNAUTHORIZED,
        "key-generation-superseded",
    );
    // Even holding the new key (it does not), its floor would refuse it.
    let (st, body) = refresh_with(
        &state,
        &account.id,
        &stolen_refresh,
        Some((&k1, &stolen_refresh, now_ms())),
    )
    .await;
    assert_code(st, &body, StatusCode::UNAUTHORIZED, "device-revoked");
}

/// A proof made under the key a reset has since displaced rotates nothing,
/// and after a reset no old device can upgrade (the new key is random to
/// them).
#[tokio::test]
async fn a_reset_ends_what_the_old_key_could_do() {
    let pool = db_or_skip!("a_reset_ends_what_the_old_key_could_do");
    let state = build_state(pool.clone(), roomy_rotation());
    let k0 = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &k0).await;
    let old_device = login(&state, &account.handle, b"pw", &OWNER_DEVICE)
        .await
        .unwrap();

    let k1 = fresh_key();
    takeover(&state, &account, b"pw", &fresh_seed(), &k1).await;
    assert_eq!(identity_of(&pool, &account.id).await.0, 2);

    // The old device: no statement it can sign upgrades it.
    let refresh = refresh_token(&old_device);
    for by in [&k0, &fresh_key()] {
        let (st, body) = refresh_with(
            &state,
            &account.id,
            &refresh,
            Some((by, &refresh, now_ms())),
        )
        .await;
        assert_code(
            st,
            &body,
            StatusCode::UNAUTHORIZED,
            "key-generation-superseded",
        );
    }

    // Someone signed in afresh, holding the OLD key: its proof names a key
    // that is no longer the account's.
    let again = login(&state, &account.handle, b"pw", &OWNER_DEVICE)
        .await
        .unwrap();
    let k2 = fresh_key();
    let (st, body) = rotate(&state, &again, &fresh_seed(), &k2, &proof(&k0, &k2), &[]).await;
    assert_code(st, &body, StatusCode::FORBIDDEN, "account-key-required");
    assert_eq!(
        identity_of(&pool, &account.id).await,
        (2, pubkey(&k1).to_vec())
    );

    // The holder of the CURRENT key rotates, and an expired proof does not.
    let now = now_ms() / 1000;
    let expired =
        b64u(&SupersessionProof::new(pubkey(&k1), pubkey(&k2), now - 120, now - 60).sign(&k1));
    let (st, body) = rotate(&state, &again, &fresh_seed(), &k2, &expired, &[]).await;
    assert_code(st, &body, StatusCode::FORBIDDEN, "account-key-required");
    let (st, body) = rotate(&state, &again, &fresh_seed(), &k2, &proof(&k1, &k2), &[]).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(
        identity_of(&pool, &account.id).await,
        (3, pubkey(&k2).to_vec())
    );
    // Two anchors now: k0's (from the reset) and k1's (from the rotation).
    let keys: Vec<Vec<u8>> = anchors(&pool, &account.id)
        .await
        .into_iter()
        .map(|a| a.1)
        .collect();
    assert_eq!(keys, vec![pubkey(&k0).to_vec(), pubkey(&k1).to_vec()]);
}

/// The default rotation budget is three a day, charged only to a caller who
/// brought the grant and the proof.
#[tokio::test]
async fn the_default_budget_is_three_rotations_a_day() {
    let pool = db_or_skip!("the_default_budget_is_three_rotations_a_day");
    let state = build_state(pool.clone(), roomy());
    let mut key = fresh_key();
    let account = new_account(&state, b"pw", &fresh_seed(), &key).await;
    for round in 0..4 {
        let session = login(&state, &account.handle, b"pw", &OWNER_DEVICE)
            .await
            .unwrap();
        let next = fresh_key();
        let (st, body) = rotate(
            &state,
            &session,
            &fresh_seed(),
            &next,
            &proof(&key, &next),
            &[],
        )
        .await;
        if round < 3 {
            assert_eq!(st, StatusCode::OK, "round {round}: {}", text(&body));
            key = next;
        } else {
            assert_code(
                st,
                &body,
                StatusCode::TOO_MANY_REQUESTS,
                "too-many-attempts",
            );
        }
    }
    assert_eq!(identity_of(&pool, &account.id).await.0, 4);
}

// ----------------------------------------------------------------
// Recall follow-up 1: a wrong clock is not a missing anchor
// ----------------------------------------------------------------

/// A statement that is right but for its time is answered `statement-stale`
/// with the server's time, on all three routes, to the anchor key's holder
/// only; nothing is counted or consumed; signed again with that time it
/// works.
#[tokio::test]
#[allow(clippy::too_many_lines)] // three routes, one scenario
async fn a_stale_recall_statement_is_told_the_time_and_costs_nothing() {
    let pool = db_or_skip!("a_stale_recall_statement_is_told_the_time_and_costs_nothing");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let stranger = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;
    let skew = 30 * 60_000; // the old phone's clock is half an hour slow

    let stale_code = |st: StatusCode, body: &Bytes| -> i64 {
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{}", text(body));
        assert_eq!(code(body), "statement-stale", "{}", text(body));
        let t = json_of(body)["server_time_ms"]
            .as_i64()
            .expect("server_time_ms");
        assert!((t - now_ms()).abs() < 60_000);
        t
    };

    // status.
    let status_at = |key: &SigningKey, at: i64| {
        let body = json!({
            "statement_b64": statement(key, AccountStatementKind::RecallStatus, &account.id, &[], at),
        });
        let state = state.clone();
        async move { post(&state, "/account/recall/status", None, &body).await }
    };
    for at in [now_ms() - skew, now_ms() + skew] {
        let (st, body) = status_at(&k0, at).await;
        stale_code(st, &body);
        // A stranger's stale statement is refused like any other.
        let (st, body) = status_at(&stranger, at).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(text(&body), REFUSED);
    }
    // An account with no anchor at all: a stale statement by its own
    // current key is refused, not told the time.
    let other_key = fresh_key();
    let other = new_account(&state, b"pw", &fresh_seed(), &other_key).await;
    let (st, body) = post(
        &state,
        "/account/recall/status",
        None,
        &json!({
            "statement_b64": statement(
                &other_key,
                AccountStatementKind::RecallStatus,
                &other.id,
                &[],
                now_ms() - skew,
            ),
        }),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(text(&body), REFUSED);

    // start: stale -> told the time, no attempt counted, no state stored.
    let start_at = |key: &SigningKey, at: i64| {
        let cl = ClientLogin::<LakkoCipherSuite>::start(&mut OsRng, b"right").unwrap();
        let login_request = cl.message.serialize().to_vec();
        let body = json!({
            "account_id_b64": b64u(&account.id),
            "anchor_seq": 1,
            "statement_b64": statement(
                key,
                AccountStatementKind::RecallStart,
                &account.id,
                &recall_start_body(1, &login_request),
                at,
            ),
            "login_request": B64.encode(&login_request),
            "password_request": reg_start(b"new").1,
            "recovery_request": reg_start(b"seed").1,
        });
        let state = state.clone();
        async move { post(&state, "/account/recall/start", None, &body).await }
    };
    let (st, body) = start_at(&k0, now_ms() - skew).await;
    stale_code(st, &body);
    let (st, body) = start_at(&stranger, now_ms() - skew).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(text(&body), REFUSED);
    assert_eq!(
        anchors(&pool, &account.id).await[0].4,
        0,
        "no attempt counted"
    );
    assert_eq!(count(&pool, "recall_states", &account.id).await, 0);

    // finish: open a real attempt, then send its finish with a stale
    // statement. Told the time; the attempt is still there.
    let seed1 = fresh_seed();
    let k1 = fresh_key();
    let attempt = recall_start(&state, &k0, &account.id, 1, b"right", b"new pw", &seed1)
        .await
        .unwrap();
    let honest = finish_body(
        attempt,
        &k0,
        &account.id,
        1,
        b"right",
        b"new pw",
        &seed1,
        &k1,
    )
    .unwrap();
    let nonce: [u8; 32] = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT server_nonce FROM recall_states WHERE account_id = ?",
    )
    .bind(&account.id[..])
    .fetch_one(&pool)
    .await
    .unwrap()
    .try_into()
    .unwrap();
    let finish_statement = |key: &SigningKey, at: i64| {
        let body = recall_body(
            &nonce,
            &pubkey(&k1),
            &B64.decode(honest["password_upload"].as_str().unwrap())
                .unwrap(),
            &B64.decode(honest["recovery_upload"].as_str().unwrap())
                .unwrap(),
        );
        statement(key, AccountStatementKind::Recall, &account.id, &body, at)
    };
    let mut outdated = honest.clone();
    outdated["statement_b64"] = json!(finish_statement(&k0, now_ms() - skew));
    let (st, body) = post(&state, "/account/recall/finish", None, &outdated).await;
    let server_time = stale_code(st, &body);
    let mut strangers = honest.clone();
    strangers["statement_b64"] = json!(finish_statement(&stranger, now_ms() - skew));
    let (st, body) = post(&state, "/account/recall/finish", None, &strangers).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(text(&body), REFUSED);
    assert_eq!(
        count(&pool, "recall_states", &account.id).await,
        1,
        "not consumed"
    );
    assert_eq!(identity_of(&pool, &account.id).await.0, 2);

    // Signed again with the server's time: the recall goes through.
    let mut resigned = honest.clone();
    resigned["statement_b64"] = json!(finish_statement(&k0, server_time));
    let (st, body) = post(&state, "/account/recall/finish", None, &resigned).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    assert_eq!(
        identity_of(&pool, &account.id).await,
        (3, pubkey(&k1).to_vec())
    );
}

// ----------------------------------------------------------------
// Recall follow-up 2: a lost answer can be asked again
// ----------------------------------------------------------------

/// The identical finish is answered again with the same attestation and a
/// working session at the same generation, changing nothing; any different
/// finish for the attempt is refused; and once the account has moved on, or
/// the attempt's lifetime has passed, so is the identical one.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn a_lost_recall_answer_can_be_asked_again() {
    let pool = db_or_skip!("a_lost_recall_answer_can_be_asked_again");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;

    let seed1 = fresh_seed();
    let k1 = fresh_key();
    let attempt = recall_start(&state, &k0, &account.id, 1, b"right", b"new pw", &seed1)
        .await
        .unwrap();
    let request = finish_body(
        attempt,
        &k0,
        &account.id,
        1,
        b"right",
        b"new pw",
        &seed1,
        &k1,
    )
    .unwrap();

    // The first finish: its answer is "lost".
    let (st, body) = post(&state, "/account/recall/finish", None, &request).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let first = json_of(&body);
    assert_eq!(first["key_generation"], 3);
    let after_first = (
        identity_of(&pool, &account.id).await,
        password_registration(&pool, &account.id).await,
        anchors(&pool, &account.id).await.len(),
    );
    assert_eq!(count(&pool, "recall_outcomes", &account.id).await, 1);
    assert_eq!(count(&pool, "recall_states", &account.id).await, 0);

    // The same request, twice more: the same outcome each time.
    for _ in 0..2 {
        let (st, body) = post(&state, "/account/recall/finish", None, &request).await;
        assert_eq!(st, StatusCode::OK, "{}", text(&body));
        let again = json_of(&body);
        assert_eq!(again["key_generation"], 3);
        assert_eq!(again["key_reset_attest_b64"], first["key_reset_attest_b64"]);
        assert!(again["fresh_grant"].is_string());
        assert_eq!(probe(&state, &token(&again)).await.0, StatusCode::OK);
        let claims = state
            .session_token_secret
            .verify(&token(&again), now_ms())
            .unwrap();
        assert_eq!(claims.device_id, NEW_DEVICE);
        // The refresh credential it carries is good too.
        let (st, _) = refresh_with(&state, &account.id, &refresh_token(&again), None).await;
        assert_eq!(st, StatusCode::OK);
    }
    assert_eq!(
        (
            identity_of(&pool, &account.id).await,
            password_registration(&pool, &account.id).await,
            anchors(&pool, &account.id).await.len(),
        ),
        after_first,
        "a retry changes nothing"
    );
    // The first answer's session still works: nothing was revoked either.
    assert_eq!(probe(&state, &token(&first)).await.0, StatusCode::OK);

    // Anything else about that attempt is refused: each field in turn.
    let refused = |body: Value| {
        let state = state.clone();
        async move {
            let (st, bytes) = post(&state, "/account/recall/finish", None, &body).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED, "{}", text(&bytes));
            assert_eq!(text(&bytes), REFUSED);
        }
    };
    let mut other = request.clone();
    other["device_id_b64"] = json!(b64u(&ATTACKER_DEVICE));
    refused(other).await;
    let mut other = request.clone();
    other.as_object_mut().unwrap().remove("device_id_b64");
    refused(other).await;
    let mut other = request.clone();
    swap_new_key(&mut other);
    refused(other).await;
    let mut other = request.clone();
    other["anchor_seq"] = json!(2);
    refused(other).await;
    let mut other = request.clone();
    other["login_finish"] = json!(unrelated_finalization());
    refused(other).await;
    let mut other = request.clone();
    other["password_upload"] = json!(registration_upload(
        &state,
        &account.id,
        KIND_PASSWORD,
        b"attacker"
    ));
    refused(other).await;
    let mut other = request.clone();
    other["recovery_upload"] = json!(registration_upload(
        &state,
        &account.id,
        KIND_RECOVERY,
        b"attacker"
    ));
    refused(other).await;
    // The same claims re-signed (another nonce): a different statement.
    let mut other = request.clone();
    other["statement_b64"] = json!(statement(
        &k0,
        AccountStatementKind::RecallStatus,
        &account.id,
        &[],
        now_ms()
    ));
    refused(other).await;
    // Another account's id with this attempt's session.
    let stranger_account = new_account(&state, b"pw", &fresh_seed(), &fresh_key()).await;
    let mut other = request.clone();
    other["account_id_b64"] = json!(b64u(&stranger_account.id));
    // With the new key's proof for that account, so the body is well formed.
    other["identity_proof_b64"] = json!(identity_proof(&stranger_account.id, &k1));
    refused(other).await;

    // The account moves on (the recalled owner resets): the old request is
    // dead, it cannot bring back a session of the standing it created.
    let session = login(&state, &account.handle, b"new pw", &NEW_DEVICE)
        .await
        .expect("the recall set this password");
    let (st, body) = reset_keys(&state, &session, &fresh_seed(), &fresh_key()).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    refused(request.clone()).await;
    assert_eq!(identity_of(&pool, &account.id).await.0, 4);
}

/// An outcome is honoured only for what was left of the attempt's ten
/// minutes.
#[tokio::test]
async fn a_recall_outcome_expires_with_its_attempt() {
    let pool = db_or_skip!("a_recall_outcome_expires_with_its_attempt");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;
    let seed1 = fresh_seed();
    let k1 = fresh_key();
    let before = now_ms();
    let attempt = recall_start(&state, &k0, &account.id, 1, b"right", b"new pw", &seed1)
        .await
        .unwrap();
    let request = finish_body(
        attempt,
        &k0,
        &account.id,
        1,
        b"right",
        b"new pw",
        &seed1,
        &k1,
    )
    .unwrap();
    let (st, _) = post(&state, "/account/recall/finish", None, &request).await;
    assert_eq!(st, StatusCode::OK);

    // The record expires when the attempt's state would have.
    let expires: i64 =
        sqlx::query_scalar("SELECT expires_at_ms FROM recall_outcomes WHERE account_id = ?")
            .bind(&account.id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    let ttl = crate::recall::RECALL_STATE_TTL_MS;
    assert!(
        expires >= before + ttl && expires <= now_ms() + ttl,
        "{expires}"
    );

    sqlx::query("UPDATE recall_outcomes SET expires_at_ms = ? WHERE account_id = ?")
        .bind(now_ms() - 1)
        .bind(&account.id[..])
        .execute(&pool)
        .await
        .unwrap();
    let (st, body) = post(&state, "/account/recall/finish", None, &request).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(text(&body), REFUSED);
    assert_eq!(identity_of(&pool, &account.id).await.0, 3);
}

/// A wrong password at finish leaves no outcome: there is nothing to answer
/// again, and the retry is refused like any second finish.
#[tokio::test]
async fn a_failed_finish_records_no_outcome() {
    let pool = db_or_skip!("a_failed_finish_records_no_outcome");
    let state = build_state(pool.clone(), roomy());
    let k0 = fresh_key();
    let account = new_account(&state, b"right", &fresh_seed(), &k0).await;
    takeover(&state, &account, b"right", &fresh_seed(), &fresh_key()).await;
    let seed1 = fresh_seed();
    let k1 = fresh_key();
    let attempt = recall_start(&state, &k0, &account.id, 1, b"right", b"new pw", &seed1)
        .await
        .unwrap();
    let mut request = finish_body(
        attempt,
        &k0,
        &account.id,
        1,
        b"right",
        b"new pw",
        &seed1,
        &k1,
    )
    .unwrap();
    request["login_finish"] = json!(unrelated_finalization());
    let (st, body) = post(&state, "/account/recall/finish", None, &request).await;
    assert_code(st, &body, StatusCode::UNAUTHORIZED, "wrong-password");
    assert_eq!(count(&pool, "recall_outcomes", &account.id).await, 0);
    let (st, body) = post(&state, "/account/recall/finish", None, &request).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(text(&body), REFUSED);
}
