// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The passkey vault: the server side of ADR-0064 decision 6. (The routes
//! keep their `/vault` prefix from ADR-0049.)
//!
//! One opaque blob per passkey: the phrase entropy sealed by the client
//! under a key derived from that passkey's WebAuthn PRF output, wrapped
//! again under `LAKKO_VAULT_ENC_KEY`. The server has no verifier for it and
//! cannot open or search it; only the account reads it, and only the
//! passkey's PRF output opens it.
//!
//! - **`PUT /vault/passkey`** (bearer). Stores or replaces the blob for one
//!   of the account's registered passkeys.
//! - **`POST /vault/passkey/get`** (bearer). The blob for a credential id,
//!   or `null`.
//! - **`DELETE /vault/passkey`** (bearer). Removes one blob ("Stop restoring
//!   keys"). The passkey itself stays and still signs in.
//! - **`GET /vault/passkeys`** (bearer). The credential ids that have a
//!   blob.
//!
//! Deleting a passkey (`DELETE /webauthn/credentials/{id}`) deletes its blob
//! in the same transaction; deleting the account deletes every blob (foreign
//! key cascade).
//!
//! Without `LAKKO_VAULT_ENC_KEY` every route here answers `503`.
//!
//! The three-word check has no server side. A device that holds the
//! recovery phrase asks for three words at random positions and compares
//! them itself (`lakko_mnemonic::word_check`); nothing about the words is
//! stored here or sent here. The `/vault`, `/vault/enroll` and
//! `/vault/recover/*` routes and the `word_vaults` / `vault_opens` tables
//! that served the earlier server-side verifier are gone (migration 0029).

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// The exact size of a passkey-vault blob as the client seals it:
/// 12 (nonce) + 16 (phrase entropy) + 16 (tag). A blob of any other size is
/// not one a current client can open (a blob sealed for a 24-word phrase is
/// 60 bytes) and is refused at the door.
const PASSKEY_BLOB_LEN: usize = 44;

// ---------------------------------------------------------------------------
// Env-loaded encryption key
// ---------------------------------------------------------------------------

/// Load the at-rest wrapping key from `LAKKO_VAULT_ENC_KEY`.
///
/// `Ok(Some(_))` when the env var is present and decodes to exactly 32 bytes;
/// `Ok(None)` when it is unset (the `/vault/*` routes then return 503);
/// `Err` (loud-fail at startup) when it is present but malformed.
///
/// # Errors
///
/// Returns an error when the env var is set but is not valid base64 or does
/// not decode to exactly 32 bytes.
pub(crate) fn load_vault_enc_key() -> anyhow::Result<Option<[u8; 32]>> {
    let raw = match std::env::var("LAKKO_VAULT_ENC_KEY") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return Ok(None),
    };
    let bytes = STANDARD
        .decode(raw.trim())
        .map_err(|e| anyhow::anyhow!("LAKKO_VAULT_ENC_KEY is not valid base64: {e}"))?;
    let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "LAKKO_VAULT_ENC_KEY must decode to exactly 32 bytes (got {})",
            bytes.len()
        )
    })?;
    Ok(Some(key))
}

fn require_key(state: &AppState) -> Result<[u8; 32], ApiError> {
    state
        .vault_enc_key
        .as_ref()
        .map(|k| **k)
        .ok_or(ApiError::Disabled)
}

// ---------------------------------------------------------------------------
// Wire types and handlers
// ---------------------------------------------------------------------------

/// Upper bound on a WebAuthn credential id.
const MAX_CREDENTIAL_ID_LEN: usize = 1024;

/// `PUT /vault/passkey` request.
#[derive(Deserialize)]
pub(crate) struct PasskeyPutReq {
    pub credential_id_b64: String,
    pub sealed_entropy_b64: String,
}

/// `POST /vault/passkey/get` and `DELETE /vault/passkey` request.
#[derive(Deserialize)]
pub(crate) struct PasskeyIdReq {
    pub credential_id_b64: String,
}

/// `POST /vault/passkey/get` response.
#[derive(Serialize)]
pub(crate) struct PasskeyGetRes {
    pub sealed_entropy_b64: Option<String>,
}

/// `GET /vault/passkeys` response.
#[derive(Serialize)]
pub(crate) struct PasskeyListRes {
    /// The credential ids (base64url without padding, the form
    /// `GET /webauthn/credentials` uses) that have a blob, oldest first.
    pub credential_ids_b64: Vec<String>,
}

fn decode_credential_id(b64: &str) -> Result<Vec<u8>, ApiError> {
    let credential_id = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("credential_id_b64 not URL-safe base64"))?;
    if credential_id.is_empty() || credential_id.len() > MAX_CREDENTIAL_ID_LEN {
        return Err(ApiError::BadInput("credential_id has an impossible length"));
    }
    Ok(credential_id)
}

/// `PUT /vault/passkey` — store (or replace) the entropy this account
/// sealed under one of its passkeys' PRF output. Bearer-auth; the blob is
/// opaque and only the credential's PRF output opens it.
///
/// The credential must be one of the account's registered passkeys, so a
/// blob always has a passkey whose deletion removes it.
#[axum::debug_handler]
pub(crate) async fn passkey_put(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<PasskeyPutReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let key = require_key(&state)?;
    let credential_id = decode_credential_id(&req.credential_id_b64)?;
    let sealed = STANDARD
        .decode(&req.sealed_entropy_b64)
        .map_err(|_| ApiError::BadInput("sealed_entropy_b64 not base64"))?;
    if sealed.len() != PASSKEY_BLOB_LEN {
        return Err(ApiError::BadInput(
            "sealed_entropy must be the 44-byte blob of a 12-word phrase",
        ));
    }
    let body_enc = lakko_crypto::aead::seal_with_key(&key, &sealed).map_err(|e| {
        tracing::error!(error = %e, "passkey-vault: body seal failed");
        ApiError::Internal
    })?;

    let mut tx = state.db.begin().await.map_err(ApiError::Db)?;
    // Locking read: a concurrent `DELETE /webauthn/credentials/{id}` either
    // commits first (no row here, refused) or waits and then removes the
    // blob written below.
    let registered: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM webauthn_credentials WHERE credential_id = ? AND account_id = ? FOR UPDATE",
    )
    .bind(credential_id.as_slice())
    .bind(auth.0.as_slice())
    .fetch_optional(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    if registered.is_none() {
        return Err(ApiError::BadInput(
            "credential_id is not one of this account's passkeys",
        ));
    }
    sqlx::query(
        "INSERT INTO passkey_vaults (account_id, credential_id, body_enc) VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE body_enc = VALUES(body_enc)",
    )
    .bind(auth.0.as_slice())
    .bind(credential_id.as_slice())
    .bind(body_enc.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;
    tracing::info!(account_id = %URL_SAFE_NO_PAD.encode(auth.0), "passkey-vault: stored");
    Ok(Json(serde_json::json!({ "stored": true })))
}

/// `POST /vault/passkey/get` — the blob for one of this account's
/// credentials, or `null`. Bearer-auth (the passkey login already issued the
/// token; only the account itself can read its own blobs).
#[axum::debug_handler]
pub(crate) async fn passkey_get(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<PasskeyIdReq>,
) -> Result<Json<PasskeyGetRes>, ApiError> {
    let key = require_key(&state)?;
    let credential_id = decode_credential_id(&req.credential_id_b64)?;
    let row: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT body_enc FROM passkey_vaults WHERE account_id = ? AND credential_id = ?",
    )
    .bind(auth.0.as_slice())
    .bind(credential_id.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    let Some((body_enc,)) = row else {
        return Ok(Json(PasskeyGetRes {
            sealed_entropy_b64: None,
        }));
    };
    let sealed = lakko_crypto::aead::open_with_key(&key, &body_enc).map_err(|e| {
        tracing::error!(error = %e, "passkey-vault: body open failed");
        ApiError::Internal
    })?;
    Ok(Json(PasskeyGetRes {
        sealed_entropy_b64: Some(STANDARD.encode(sealed)),
    }))
}

/// `DELETE /vault/passkey` — remove the blob of one credential ("Stop
/// restoring keys"). The passkey stays registered and still signs in.
/// Idempotent: `deleted` says whether there was a blob.
#[axum::debug_handler]
pub(crate) async fn passkey_delete(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<PasskeyIdReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_key(&state)?;
    let credential_id = decode_credential_id(&req.credential_id_b64)?;
    let deleted =
        sqlx::query("DELETE FROM passkey_vaults WHERE account_id = ? AND credential_id = ?")
            .bind(auth.0.as_slice())
            .bind(credential_id.as_slice())
            .execute(&state.db)
            .await
            .map_err(ApiError::Db)?
            .rows_affected()
            > 0;
    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(auth.0),
        deleted,
        "passkey-vault: blob removed"
    );
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

/// `GET /vault/passkeys` — which of this account's passkeys carry a blob.
#[axum::debug_handler]
pub(crate) async fn passkey_list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<PasskeyListRes>, ApiError> {
    require_key(&state)?;
    let rows: Vec<(Vec<u8>,)> = sqlx::query_as(
        "SELECT credential_id FROM passkey_vaults WHERE account_id = ? \
         ORDER BY created_at, credential_id",
    )
    .bind(auth.0.as_slice())
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(Json(PasskeyListRes {
        credential_ids_b64: rows
            .into_iter()
            .map(|(id,)| URL_SAFE_NO_PAD.encode(id))
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_passkey_blob_length_is_what_the_client_seals() {
        let m = lakko_mnemonic::Mnemonic::generate();
        let blob = lakko_mnemonic::vault::seal_entropy_under_secret(&m, &[7u8; 32]).unwrap();
        assert_eq!(blob.len(), PASSKEY_BLOB_LEN);
    }

    #[test]
    fn load_key_rejects_wrong_length() {
        let decoded = STANDARD.decode(STANDARD.encode([0u8; 31])).unwrap();
        let res: Result<[u8; 32], _> = decoded.as_slice().try_into();
        assert!(res.is_err());
    }
}

/// DB-backed tests of the passkey vault through
/// the production router. They run only with `LAKKO_AUTH_TEST_DATABASE_URL`
/// set and skip (pass) without it.
/// DB-backed tests of the passkey vault through
/// the production router. They run only with `LAKKO_AUTH_TEST_DATABASE_URL`
/// set and skip (pass) without it.
#[cfg(test)]
mod vault_e2e_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, StatusCode};
    use http_body_util::BodyExt as _;
    use lakko_mnemonic::vault;
    use lakko_mnemonic::Mnemonic;
    use sqlx::mysql::MySqlPoolOptions;
    use sqlx::MySqlPool;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt as _;
    use url::Url;
    use uuid::Uuid;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_authenticator_rs::WebauthnAuthenticator;
    use webauthn_rs::prelude::{
        Passkey, PublicKeyCredential, RequestChallengeResponse, Webauthn, WebauthnBuilder,
    };

    const VAULT_KEY: [u8; 32] = [0x6Bu8; 32];
    const RP_ID: &str = "lakko.app";
    const RP_ORIGIN: &str = "https://lakko.app";
    const RP_NAME: &str = "Lakko";

    fn rp_origin() -> Url {
        Url::parse(RP_ORIGIN).expect("static origin")
    }

    fn high_level() -> Webauthn {
        WebauthnBuilder::new(RP_ID, &rp_origin())
            .expect("builder")
            .rp_name(RP_NAME)
            .build()
            .expect("build")
    }

    fn build_state(
        pool: MySqlPool,
        with_key: bool,
    ) -> (crate::AppState, lakko_auth_token::TokenSecret) {
        use opaque_ke::ServerSetup;
        use rand::rngs::OsRng;
        let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
        let opaque_setup = ServerSetup::<crate::opaque::LakkoCipherSuite>::new(&mut OsRng);
        let action_core = crate::webauthn_action::build_action_core(RP_NAME, RP_ID, &rp_origin());
        // These tests send many anonymous requests from one (unidentified)
        // address; the per-address bucket is covered by `limits`' own tests.
        let limits = crate::limits::Limits::defaults().with_ip_window(
            crate::limits::Category::Attempt,
            crate::limits::Window::new(10_000, 60),
        );
        let state = crate::AppState {
            db: pool,
            opaque_setup: Arc::new(opaque_setup),
            session_token_secret: Arc::new(token_secret.clone()),
            session_token_ttl_ms: 86_400_000,
            refresh_token_ttl_ms: 30 * 86_400_000,
            fresh_grant_ttl_ms: 600_000,
            webauthn: Some(Arc::new(high_level())),
            webauthn_action_core: Some(Arc::new(action_core)),
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
            turn: None,
            totp_enc_key: None,
            vault_enc_key: with_key.then(|| Arc::new(VAULT_KEY)),
            throttle: crate::throttle::ThrottleConfig::disabled(),
            backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
            anon_tokens: None,
            handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
            recovery_email: None,
            limits: Arc::new(limits),
        };
        (state, token_secret)
    }

    async fn send(state: &crate::AppState, req: Request<Body>) -> (StatusCode, Bytes) {
        let resp = crate::build_app(state.clone())
            .oneshot(req)
            .await
            .expect("router oneshot");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, bytes)
    }

    fn now_ms() -> i64 {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(i64::MAX)
    }

    fn bearer_for(token_secret: &lakko_auth_token::TokenSecret, account_id: &[u8; 16]) -> String {
        format!(
            "Bearer {}",
            token_secret
                .issue(account_id, 1, &[0u8; 32], 0, now_ms() + 3_600_000)
                .expect("mint token")
        )
    }

    async fn json_req(
        state: &crate::AppState,
        method: Method,
        uri: &str,
        bearer: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, Bytes) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(b) = bearer {
            req = req.header(header::AUTHORIZATION, b);
        }
        let req = match body {
            Some(v) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&v).unwrap()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        send(state, req).await
    }

    /// `DELETE /account` with, when given, the fresh-auth grant in its
    /// `x-lakko-fresh-grant` header.
    async fn delete_account(
        state: &crate::AppState,
        bearer: &str,
        fresh_grant: Option<&str>,
    ) -> (StatusCode, Bytes) {
        let mut req = Request::builder()
            .method(Method::DELETE)
            .uri("/account")
            .header(header::AUTHORIZATION, bearer);
        if let Some(g) = fresh_grant {
            req = req.header(crate::session::FRESH_GRANT_HEADER, g);
        }
        send(state, req.body(Body::empty()).unwrap()).await
    }

    /// A request that must answer `expected`; returns the parsed JSON body
    /// (`null` for an empty or non-JSON body).
    async fn call(
        state: &crate::AppState,
        method: Method,
        uri: &str,
        bearer: Option<&str>,
        body: Option<serde_json::Value>,
        expected: StatusCode,
    ) -> serde_json::Value {
        let (st, bytes) = json_req(state, method.clone(), uri, bearer, body).await;
        assert_eq!(
            st,
            expected,
            "{method} {uri}: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    /// The test database, migrated; `None` (skip) without the env var.
    async fn pool(test: &str) -> Option<MySqlPool> {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping {test}: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return None;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect");
        crate::MIGRATIONS.run(&pool).await.expect("migrate");
        Some(pool)
    }

    /// A fresh account and a session for it.
    struct Account {
        id: [u8; 16],
        bearer: String,
    }

    async fn new_account(pool: &MySqlPool, secret: &lakko_auth_token::TokenSecret) -> Account {
        let id: [u8; 16] = *Uuid::now_v7().as_bytes();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(id.as_slice())
            .execute(pool)
            .await
            .expect("insert account");
        Account {
            id,
            bearer: bearer_for(secret, &id),
        }
    }

    async fn drop_account(pool: &MySqlPool, id: &[u8; 16]) {
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(id.as_slice())
            .execute(pool)
            .await
            .unwrap();
    }

    /// Register a fresh passkey with the software authenticator and persist
    /// it exactly as `webauthn::register_finish` does.
    async fn register_and_store(
        pool: &MySqlPool,
        account_id: &[u8; 16],
        wa: &mut WebauthnAuthenticator<SoftPasskey>,
    ) -> Passkey {
        let hl = high_level();
        let (ccr, reg_state) = hl
            .start_passkey_registration(Uuid::from_bytes(*account_id), "tester", "Tester", None)
            .expect("reg start");
        let rpkc = wa.do_registration(rp_origin(), ccr).expect("soft register");
        let passkey = hl
            .finish_passkey_registration(&rpkc, &reg_state)
            .expect("reg finish");
        sqlx::query(
            "INSERT INTO webauthn_credentials (credential_id, account_id, passkey_bytes, nickname) \
             VALUES (?, ?, ?, '')",
        )
        .bind(passkey.cred_id().as_ref())
        .bind(account_id.as_slice())
        .bind(serde_json::to_vec(&passkey).expect("serialize passkey"))
        .execute(pool)
        .await
        .expect("insert credential");
        passkey
    }

    /// Run the `/auth/action/{start,finish}` ceremony so the next gated call
    /// bound to `action_hash` finds its grant.
    async fn grant_action(
        state: &crate::AppState,
        bearer: &str,
        wa: &mut WebauthnAuthenticator<SoftPasskey>,
        action_hash: [u8; 32],
    ) {
        let action_hash_b64 = URL_SAFE_NO_PAD.encode(action_hash);
        let v = call(
            state,
            Method::POST,
            "/auth/action/start",
            Some(bearer),
            Some(serde_json::json!({ "action_hash_b64": action_hash_b64 })),
            StatusCode::OK,
        )
        .await;
        let rcr: RequestChallengeResponse =
            serde_json::from_value(v["options"].clone()).expect("parse options");
        let pkc: PublicKeyCredential = wa
            .do_authentication(rp_origin(), rcr)
            .expect("soft assertion");
        call(
            state,
            Method::POST,
            "/auth/action/finish",
            Some(bearer),
            Some(serde_json::json!({
                "state_id": v["state_id"],
                "action_hash_b64": action_hash_b64,
                "credential": pkc,
            })),
            StatusCode::OK,
        )
        .await;
    }

    async fn stored_ids(state: &crate::AppState, bearer: &str) -> Vec<String> {
        let v = call(
            state,
            Method::GET,
            "/vault/passkeys",
            Some(bearer),
            None,
            StatusCode::OK,
        )
        .await;
        let mut ids: Vec<String> = serde_json::from_value(v["credential_ids_b64"].clone()).unwrap();
        ids.sort();
        ids
    }

    async fn blob_rows(pool: &MySqlPool, account_id: &[u8; 16]) -> i64 {
        let (n,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM passkey_vaults WHERE account_id = ?")
                .bind(account_id.as_slice())
                .fetch_one(pool)
                .await
                .unwrap();
        n
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines, clippy::similar_names)]
    async fn passkey_blobs_are_stored_listed_removed_and_follow_their_passkey() {
        let Some(pool) =
            pool("passkey_blobs_are_stored_listed_removed_and_follow_their_passkey").await
        else {
            return;
        };
        let (state, secret) = build_state(pool.clone(), true);
        let acct = new_account(&pool, &secret).await;
        let other = new_account(&pool, &secret).await;
        let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let pk_a = register_and_store(&pool, &acct.id, &mut wa).await;
        let pk_b = register_and_store(&pool, &acct.id, &mut wa).await;
        let cred_a = URL_SAFE_NO_PAD.encode(pk_a.cred_id().as_ref());
        let cred_b = URL_SAFE_NO_PAD.encode(pk_b.cred_id().as_ref());

        let m = Mnemonic::generate();
        let prf_a = [0x33u8; 32];
        let blob_a = vault::seal_entropy_under_secret(&m, &prf_a).unwrap();
        let blob_b = vault::seal_entropy_under_secret(&m, &[0x44u8; 32]).unwrap();
        let put = |cred: String, blob: Vec<u8>| {
            serde_json::json!({
                "credential_id_b64": cred,
                "sealed_entropy_b64": STANDARD.encode(blob),
            })
        };

        assert!(stored_ids(&state, &acct.bearer).await.is_empty());
        let v = call(
            &state,
            Method::POST,
            "/vault/passkey/get",
            Some(&acct.bearer),
            Some(serde_json::json!({ "credential_id_b64": cred_a })),
            StatusCode::OK,
        )
        .await;
        assert!(v["sealed_entropy_b64"].is_null());

        // Refused: a credential that is not one of the account's passkeys,
        // another account's passkey, and a blob of the 24-word size.
        for (bearer, body) in [
            (
                &acct.bearer,
                put(URL_SAFE_NO_PAD.encode([0xC1u8; 40]), blob_a.clone()),
            ),
            (&other.bearer, put(cred_a.clone(), blob_a.clone())),
            (&acct.bearer, put(cred_a.clone(), vec![0u8; 60])),
        ] {
            call(
                &state,
                Method::PUT,
                "/vault/passkey",
                Some(bearer),
                Some(body),
                StatusCode::BAD_REQUEST,
            )
            .await;
        }
        assert_eq!(blob_rows(&pool, &acct.id).await, 0);
        assert_eq!(blob_rows(&pool, &other.id).await, 0);

        for (cred, blob) in [(&cred_a, &blob_a), (&cred_b, &blob_b)] {
            call(
                &state,
                Method::PUT,
                "/vault/passkey",
                Some(&acct.bearer),
                Some(put(cred.clone(), blob.clone())),
                StatusCode::OK,
            )
            .await;
        }
        let mut both = vec![cred_a.clone(), cred_b.clone()];
        both.sort();
        assert_eq!(stored_ids(&state, &acct.bearer).await, both);
        // Another account sees none of them and cannot read or remove them.
        assert!(stored_ids(&state, &other.bearer).await.is_empty());
        let v = call(
            &state,
            Method::POST,
            "/vault/passkey/get",
            Some(&other.bearer),
            Some(serde_json::json!({ "credential_id_b64": cred_a })),
            StatusCode::OK,
        )
        .await;
        assert!(v["sealed_entropy_b64"].is_null());
        let v = call(
            &state,
            Method::DELETE,
            "/vault/passkey",
            Some(&other.bearer),
            Some(serde_json::json!({ "credential_id_b64": cred_a })),
            StatusCode::OK,
        )
        .await;
        assert_eq!(v["deleted"], false);

        // Served back verbatim; the PRF output opens it.
        let v = call(
            &state,
            Method::POST,
            "/vault/passkey/get",
            Some(&acct.bearer),
            Some(serde_json::json!({ "credential_id_b64": cred_a })),
            StatusCode::OK,
        )
        .await;
        let served = STANDARD
            .decode(v["sealed_entropy_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(served, blob_a);
        assert_eq!(
            vault::open_entropy_under_secret(&prf_a, &served)
                .unwrap()
                .phrase(),
            m.phrase()
        );

        // "Stop restoring keys": the blob goes, the passkey stays.
        let v = call(
            &state,
            Method::DELETE,
            "/vault/passkey",
            Some(&acct.bearer),
            Some(serde_json::json!({ "credential_id_b64": cred_a })),
            StatusCode::OK,
        )
        .await;
        assert_eq!(v["deleted"], true);
        let v = call(
            &state,
            Method::DELETE,
            "/vault/passkey",
            Some(&acct.bearer),
            Some(serde_json::json!({ "credential_id_b64": cred_a })),
            StatusCode::OK,
        )
        .await;
        assert_eq!(v["deleted"], false);
        assert_eq!(stored_ids(&state, &acct.bearer).await, vec![cred_b.clone()]);
        let creds = call(
            &state,
            Method::GET,
            "/webauthn/credentials",
            Some(&acct.bearer),
            None,
            StatusCode::OK,
        )
        .await;
        assert_eq!(creds["credentials"].as_array().unwrap().len(), 2);

        // Deleting passkey B deletes its blob with it.
        grant_action(
            &state,
            &acct.bearer,
            &mut wa,
            crate::webauthn_action::delete_credential_action_hash(pk_b.cred_id().as_ref()),
        )
        .await;
        call(
            &state,
            Method::DELETE,
            &format!("/webauthn/credentials/{cred_b}"),
            Some(&acct.bearer),
            None,
            StatusCode::NO_CONTENT,
        )
        .await;
        assert!(stored_ids(&state, &acct.bearer).await.is_empty());
        assert_eq!(blob_rows(&pool, &acct.id).await, 0);
        // ... and a blob can no longer be stored for it.
        call(
            &state,
            Method::PUT,
            "/vault/passkey",
            Some(&acct.bearer),
            Some(put(cred_b.clone(), blob_b.clone())),
            StatusCode::BAD_REQUEST,
        )
        .await;

        // Deleting the account deletes every blob.
        call(
            &state,
            Method::PUT,
            "/vault/passkey",
            Some(&acct.bearer),
            Some(put(cred_a.clone(), blob_a.clone())),
            StatusCode::OK,
        )
        .await;
        assert_eq!(blob_rows(&pool, &acct.id).await, 1);
        grant_action(
            &state,
            &acct.bearer,
            &mut wa,
            crate::webauthn_action::delete_account_action_hash(&acct.id),
        )
        .await;
        // The account has a passkey, so the action grant alone deletes it
        // (ADR-0065, amended 2026-10-06): no fresh-auth header is sent.
        let (st, bytes) = delete_account(&state, &acct.bearer, None).await;
        assert_eq!(
            st,
            StatusCode::NO_CONTENT,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(blob_rows(&pool, &acct.id).await, 0);

        drop_account(&pool, &other.id).await;
    }

    /// ADR-0064 decision 6 on a device that knows nothing: no cached account
    /// id, no username typed. The discoverable passkey sign-in names the
    /// account from the authenticator's user handle, and the session it
    /// issues reads the blob that passkey's PRF output opens.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn a_device_with_no_cached_account_signs_in_with_a_passkey_and_gets_its_blob() {
        let Some(pool) =
            pool("a_device_with_no_cached_account_signs_in_with_a_passkey_and_gets_its_blob").await
        else {
            return;
        };
        let (state, secret) = build_state(pool.clone(), true);
        let acct = new_account(&pool, &secret).await;
        let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let passkey = register_and_store(&pool, &acct.id, &mut wa).await;
        let cred_b64 = URL_SAFE_NO_PAD.encode(passkey.cred_id().as_ref());

        // The signed-in device opted in: it stored the blob.
        let m = Mnemonic::generate();
        let prf = [0x5Cu8; 32];
        let blob = vault::seal_entropy_under_secret(&m, &prf).unwrap();
        call(
            &state,
            Method::PUT,
            "/vault/passkey",
            Some(&acct.bearer),
            Some(serde_json::json!({
                "credential_id_b64": cred_b64,
                "sealed_entropy_b64": STANDARD.encode(&blob),
            })),
            StatusCode::OK,
        )
        .await;

        // The fresh device: an anonymous start that names no account.
        let v = call(
            &state,
            Method::POST,
            "/webauthn/auth/start",
            None,
            Some(serde_json::json!({})),
            StatusCode::OK,
        )
        .await;
        assert_eq!(v["discoverable"], true);
        let mut options = v["options"].clone();
        assert_eq!(
            options["publicKey"]["allowCredentials"]
                .as_array()
                .map_or(0, Vec::len),
            0,
            "the server names no credential"
        );
        // The software authenticator has no resident-key store, so the test
        // plays the part of one: it picks the credential itself and reports
        // the user handle the credential was created for, as a platform
        // authenticator does. Neither value is covered by the server's
        // challenge state.
        options["publicKey"]["allowCredentials"] =
            serde_json::json!([{ "type": "public-key", "id": cred_b64 }]);
        let rcr: RequestChallengeResponse = serde_json::from_value(options).expect("parse options");
        let pkc: PublicKeyCredential = wa
            .do_authentication(rp_origin(), rcr)
            .expect("soft assertion");
        let mut credential = serde_json::to_value(&pkc).unwrap();
        credential["response"]["userHandle"] = serde_json::json!(URL_SAFE_NO_PAD.encode(acct.id));

        let session = call(
            &state,
            Method::POST,
            "/webauthn/auth/finish",
            None,
            Some(serde_json::json!({ "state_id": v["state_id"], "credential": credential })),
            StatusCode::OK,
        )
        .await;
        assert_eq!(session["account_id_b64"], URL_SAFE_NO_PAD.encode(acct.id));
        let bearer = format!("Bearer {}", session["session_token"].as_str().unwrap());

        // With that session and the credential id the assertion carried, the
        // device fetches the blob and opens it with the PRF output.
        let used_cred = credential["rawId"].as_str().unwrap().to_string();
        assert_eq!(used_cred, cred_b64);
        let got = call(
            &state,
            Method::POST,
            "/vault/passkey/get",
            Some(&bearer),
            Some(serde_json::json!({ "credential_id_b64": used_cred })),
            StatusCode::OK,
        )
        .await;
        let served = STANDARD
            .decode(got["sealed_entropy_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            vault::open_entropy_under_secret(&prf, &served)
                .unwrap()
                .phrase(),
            m.phrase()
        );

        drop_account(&pool, &acct.id).await;
    }

    /// One passkey sign-in over the discoverable route, the test standing in
    /// for a resident-key store as in the test above; returns the whole
    /// finish answer.
    async fn passkey_login(
        state: &crate::AppState,
        account_id: &[u8; 16],
        passkey: &Passkey,
        wa: &mut WebauthnAuthenticator<SoftPasskey>,
    ) -> serde_json::Value {
        let v = call(
            state,
            Method::POST,
            "/webauthn/auth/start",
            None,
            Some(serde_json::json!({})),
            StatusCode::OK,
        )
        .await;
        let mut options = v["options"].clone();
        options["publicKey"]["allowCredentials"] = serde_json::json!([{
            "type": "public-key",
            "id": URL_SAFE_NO_PAD.encode(passkey.cred_id().as_ref()),
        }]);
        let rcr: RequestChallengeResponse = serde_json::from_value(options).expect("parse options");
        let pkc: PublicKeyCredential = wa
            .do_authentication(rp_origin(), rcr)
            .expect("soft assertion");
        let mut credential = serde_json::to_value(&pkc).unwrap();
        credential["response"]["userHandle"] =
            serde_json::json!(URL_SAFE_NO_PAD.encode(account_id));
        call(
            state,
            Method::POST,
            "/webauthn/auth/finish",
            None,
            Some(serde_json::json!({ "state_id": v["state_id"], "credential": credential })),
            StatusCode::OK,
        )
        .await
    }

    /// ADR-0065 amended 2026-10-06: a passkey sign-in mints a fresh-auth
    /// grant, like the password login. That grant lets a device in (twice
    /// within its lifetime: it is not spent). It does NOT delete an account
    /// that has a passkey: that takes the ADR-0014 passkey action grant, and
    /// only that. Without the action grant the delete is refused whatever
    /// the `x-lakko-fresh-grant` header holds; with it, the header is not
    /// read, so an unusable one does not refuse the delete.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn a_passkey_sign_in_mints_a_fresh_grant_that_adds_a_device_and_deletes_the_account() {
        let Some(pool) = pool(
            "a_passkey_sign_in_mints_a_fresh_grant_that_adds_a_device_and_deletes_the_account",
        )
        .await
        else {
            return;
        };
        let (state, secret) = build_state(pool.clone(), true);
        let acct = new_account(&pool, &secret).await;
        let other = new_account(&pool, &secret).await;
        let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let passkey = register_and_store(&pool, &acct.id, &mut wa).await;

        let login = passkey_login(&state, &acct.id, &passkey, &mut wa).await;
        let bearer = format!("Bearer {}", login["session_token"].as_str().unwrap());
        let grant = login["fresh_grant"]
            .as_str()
            .expect("a passkey sign-in mints a grant");
        assert!(grant.starts_with("k1."), "{grant}");
        let ttl = login["fresh_grant_expires_at_ms"].as_i64().unwrap() - now_ms();
        assert!(ttl > 0 && ttl <= state.fresh_grant_ttl_ms, "{ttl}");
        let verified = secret.verify_fresh_grant(grant, now_ms()).unwrap();
        assert_eq!(verified.account_id, acct.id);
        assert_eq!(
            verified.key_generation,
            login["key_generation"].as_u64().unwrap()
        );

        // Add a device, twice, with the one grant; another account's session
        // cannot use it.
        let mint = |bearer: String, grant: Option<String>| {
            let state = state.clone();
            async move {
                json_req(
                    &state,
                    Method::POST,
                    "/device/authorize/start",
                    Some(&bearer),
                    Some(serde_json::json!({ "fresh_grant": grant })),
                )
                .await
                .0
            }
        };
        assert_eq!(mint(bearer.clone(), None).await, StatusCode::FORBIDDEN);
        assert_eq!(
            mint(other.bearer.clone(), Some(grant.to_string())).await,
            StatusCode::FORBIDDEN
        );
        for _ in 0..2 {
            assert_eq!(
                mint(bearer.clone(), Some(grant.to_string())).await,
                StatusCode::OK
            );
        }

        // Delete: the account has a passkey, so the action grant is the
        // proof. Without it the delete is refused with or without a valid
        // fresh-auth header, and the answer is the action refusal, not
        // `fresh-auth-required`.
        let code = |bytes: &Bytes| -> String { String::from_utf8_lossy(bytes).into_owned() };
        let exists = || async {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM accounts WHERE id = ?")
                .bind(acct.id.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        for header in [None, Some(grant), Some("k1.not-a-grant")] {
            let (st, bytes) = delete_account(&state, &bearer, header).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "header {header:?}");
            assert_eq!(
                code(&bytes),
                "action-authorization-required",
                "header {header:?}"
            );
        }
        assert_eq!(exists().await, 1);
        // An action grant bound to ANOTHER action is not the delete's.
        grant_action(&state, &bearer, &mut wa, [0x5Au8; 32]).await;
        let (st, bytes) = delete_account(&state, &bearer, Some(grant)).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert_eq!(code(&bytes), "action-authorization-required");
        assert_eq!(exists().await, 1);
        // With the delete-account action grant: done, and an unusable
        // fresh-auth header does not refuse it (the header is not read).
        grant_action(
            &state,
            &bearer,
            &mut wa,
            crate::webauthn_action::delete_account_action_hash(&acct.id),
        )
        .await;
        let (st, bytes) = delete_account(&state, &bearer, Some("k1.not-a-grant")).await;
        assert_eq!(st, StatusCode::NO_CONTENT, "{}", code(&bytes));
        assert_eq!(exists().await, 0);

        drop_account(&pool, &other.id).await;
    }
}
