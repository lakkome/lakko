// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Device-authorization grants — recovery factor (a) (ADR-0018).
//!
//! An already-authenticated device authorizes a FRESH device (the one the
//! user just paired) to obtain a session token (ACCESS) without entering a
//! password or recovery phrase. The two halves are:
//!
//! - **`POST /device/authorize/start`** — bearer-authenticated (the
//!   existing device proves it already holds a session) AND a live
//!   fresh-auth grant in the JSON body, `{"fresh_grant": "..."}` (ADR-0065,
//!   amended 2026-10-06): the person signed in on this device with the
//!   password or a passkey within the grant's lifetime. Without one: `403
//!   fresh-auth-required`, nothing minted, no counter charged, so a stolen
//!   session or an unlocked phone cannot let a device in. The grant is not
//!   spent. Then limited per account and per address (ADR-0054 S4). Mints a
//!   random 32-byte `grant_token` bound to the session's `account_id`,
//!   stores only its SHA-256 hash, and returns the raw token to the
//!   existing device.
//! - **`POST /device/authorize/redeem`** — anonymous (the fresh device has
//!   no token yet); per-address limited. The body is the `grant_token` alone
//!   (ADR-0056): nothing a fresh device sends before it holds a session names
//!   the account. The server recomputes `SHA-256(grant_token)`, finds the
//!   grant by that hash (its own unique index, migration `0022`), and
//!   **atomically consumes** it — a `SELECT … FOR UPDATE` of the live row and
//!   a `DELETE` of exactly that row whose `rows_affected == 1`, in one
//!   transaction, so the consume is race-free and strictly single-use — then
//!   issues the session-token triple. The response's `account_id_b64` is
//!   where the fresh device learns its account id, after proving the grant.
//!   There is no per-name counter: the grant is 256 random bits, so a guess
//!   never lands, and keying a counter on the presented hash would charge a
//!   bucket nobody else ever shares (every guess is a new hash); the
//!   per-address bucket is the only useful limit.
//!
//! Why a server grant and not "trust the MLS Welcome": the DS is opaque and
//! the auth service is never in the MLS path, so only an
//! already-authenticated device can authorize the auth service to mint a
//! token for a new device. The grant is that authorization — short-lived,
//! single-use, rate-limited, account-bound.
//!
//! **Server opacity (invariant #2).** This module handles only the
//! ACCESS-side grant + the session token. The E2EE phrase seed (IDENTITY)
//! never reaches the server — the existing device seals it to the fresh
//! device's X25519 key over the rendezvous channel; the server sees only
//! opaque ciphertext at a pseudonym (the client-side half of ADR-0018).
//!
//! **Hash at rest (mirrors `webauthn_action_grants`, ADR-0014).** Only the
//! `SHA-256` of the random token is stored — not the raw token. The raw
//! token leaves the server exactly once (the `/start` response) and is
//! required again only at `/redeem`. SHA-256's preimage resistance means a
//! transient read of `device_grants` yields a hash that cannot be turned
//! back into a redeemable token.
//!
//! **Enumeration safety.** A `/redeem` that does not consume a live grant
//! returns the SAME generic `401 Unauthorized` an OPAQUE login returns for a
//! bad credential — it does not reveal whether the account exists, whether
//! the grant never existed, or whether it merely expired.

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Length of the raw random grant token (bytes). Matched in the
/// `device_grants.grant_hash` `BINARY(32)` column via SHA-256, not stored
/// raw.
const GRANT_TOKEN_LEN: usize = 32;

/// Length of the `account_id` (16-byte UUIDv7 raw form).
const ACCOUNT_ID_LEN: usize = 16;

/// Lifetime of a single-use device-authorization grant, in seconds. The
/// grant is meant to be redeemed by the fresh device shortly after the
/// existing device mints it (over the live pairing channel), so the window
/// is short; it is also strictly single-use (redeem deletes the row).
const DEVICE_GRANT_TTL_SECS: i64 = 600;

/// Generate a fresh random 32-byte grant token.
fn random_grant_token() -> [u8; GRANT_TOKEN_LEN] {
    let mut token = [0u8; GRANT_TOKEN_LEN];
    OsRng.fill_bytes(&mut token);
    token
}

/// Compute the at-rest grant hash, `SHA-256(grant_token)`. The raw token is
/// never stored; the redeem path recomputes this from the presented token
/// and matches it.
fn grant_hash(token: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(token);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Decode the URL-safe base64 32-byte raw grant token the fresh device
/// received over the rendezvous channel.
fn decode_grant_token(b64: &str) -> Result<[u8; GRANT_TOKEN_LEN], ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("grant_token_b64 not URL-safe base64"))?;
    if bytes.len() != GRANT_TOKEN_LEN {
        return Err(ApiError::BadInput("grant_token must decode to 32 bytes"));
    }
    let mut out = [0u8; GRANT_TOKEN_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// `POST /device/authorize/start` — mint a single-use device grant.
///
/// Bearer-authenticated (the already-logged-in existing device) and gated on
/// a fresh-auth grant: the body is `{"fresh_grant": "..."}`. A body that is
/// empty, not JSON, or carries no usable grant is answered `403
/// fresh-auth-required` before anything is charged or written. Then charged
/// to the account's grant counter (`429` `too-many-attempts`) and the
/// per-address bucket. The device grant is bound to the session's
/// `account_id`; only its SHA-256 hash is stored. The raw token is returned
/// to the existing device, which seals it to the fresh device over the
/// pairing channel.
#[axum::debug_handler]
pub(crate) async fn start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Extension(caller): Extension<crate::webauthn::AuthenticatedDevice>,
    body: axum::body::Bytes,
) -> Result<Json<DeviceAuthorizeStartRes>, ApiError> {
    let account_id = auth.0;
    // Read leniently: the route had no body before the grant, and whatever
    // arrives without a usable grant gets the one refusal.
    let req: DeviceAuthorizeStartReq = serde_json::from_slice(&body).unwrap_or_default();
    crate::session::require_fresh_grant(&state, &account_id, req.fresh_grant.as_deref()).await?;
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        crate::limits::Scope::DeviceGrantMint,
        &account_id,
    )
    .await?;

    let token = random_grant_token();
    let hash = grant_hash(&token);

    // Refresh `expires_at` if the same (astronomically-unlikely) hash
    // already exists for this account — mirrors `write_action_grant`.
    //
    // Inside the generation fence (ADR-0066 "T3 as built"): a device grant
    // is redeemable for a session, so one minted by a session that a key
    // reset or a recall has just displaced must not survive it.
    let mut tx =
        crate::session::begin_at_generation(&state.db, &account_id, caller.key_generation).await?;
    sqlx::query(
        "INSERT INTO device_grants (account_id, grant_hash, expires_at) \
         VALUES (?, ?, NOW(3) + INTERVAL ? SECOND) \
         ON DUPLICATE KEY UPDATE expires_at = VALUES(expires_at)",
    )
    .bind(account_id.as_slice())
    .bind(hash.as_slice())
    .bind(DEVICE_GRANT_TTL_SECS)
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;

    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        "device-authorize: grant minted",
    );

    Ok(Json(DeviceAuthorizeStartRes {
        grant_token_b64: URL_SAFE_NO_PAD.encode(token),
    }))
}

/// `POST /device/authorize/redeem` — consume a device grant for a session
/// token.
///
/// Anonymous (the fresh device has no token yet), per-address limited. The
/// fresh device submits the `grant_token` alone (received over the
/// rendezvous channel; ADR-0056). The server recomputes
/// `SHA-256(grant_token)`, finds the account through it and atomically
/// consumes the live grant. A missing / expired / unknown grant fails as an
/// enumeration-safe `401` — the same generic shape an OPAQUE bad-credential
/// login returns.
#[axum::debug_handler]
pub(crate) async fn redeem(
    State(state): State<AppState>,
    Json(req): Json<DeviceAuthorizeRedeemReq>,
) -> Result<Json<DeviceAuthorizeRedeemRes>, ApiError> {
    let token = decode_grant_token(&req.grant_token_b64)?;
    // Decoded before the single-use grant is consumed.
    let device_id = crate::session::decode_device_id(req.device_id_b64.as_deref())?;
    let hash = grant_hash(&token);

    let Some((account_id, key_generation)) = consume_grant(&state.db, &hash)
        .await
        .map_err(ApiError::Db)?
    else {
        // Enumeration-safe: do NOT leak whether the grant never existed or
        // merely expired. Same generic `401 invalid credentials` shape an
        // OPAQUE bad-credential login returns (opaque.rs
        // `login_finish_inner`).
        tracing::debug!("device-authorize: redeem rejected (no live grant)");
        return Err(ApiError::InvalidCredential);
    };

    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        "device-authorize: grant redeemed, issuing session token",
    );

    let session = crate::session::issue_pair_at(&state, &account_id, key_generation, &device_id)?;
    Ok(Json(DeviceAuthorizeRedeemRes {
        account_id_b64: URL_SAFE_NO_PAD.encode(account_id),
        session,
    }))
}

/// Consume the live grant whose hash is `hash` and return its account, or
/// `None` when there is none (unknown, expired, or already consumed).
///
/// MariaDB has no `DELETE … RETURNING` we rely on elsewhere (see
/// `opaque::take_login_state`), so this is a transaction: a locking read of
/// the live row, then a `DELETE` of exactly that `(account_id, grant_hash)`
/// whose `rows_affected == 1`. Two concurrent redeems of one token serialise
/// on the row lock; the second reads nothing once the first commits.
///
/// Also returns the account's key generation, read while the grant row's
/// lock is held (ADR-0066 "T3 as built"). A key reset and a recall delete the
/// account's device grants in the transaction that bumps the generation, so a
/// redeem that straddles one either finds no grant, or issues its pair at the
/// generation from before the change, which is refused on first use.
async fn consume_grant(
    pool: &sqlx::MySqlPool,
    hash: &[u8; 32],
) -> Result<Option<([u8; ACCOUNT_ID_LEN], u64)>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT account_id FROM device_grants \
         WHERE grant_hash = ? AND expires_at > NOW(3) FOR UPDATE",
    )
    .bind(hash.as_slice())
    .fetch_optional(&mut *tx)
    .await?;
    let Some(account_id) = row.and_then(|(v,)| <[u8; ACCOUNT_ID_LEN]>::try_from(v.as_slice()).ok())
    else {
        tx.commit().await?;
        return Ok(None);
    };
    let key_generation: Option<u64> =
        sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ?")
            .bind(account_id.as_slice())
            .fetch_optional(&mut *tx)
            .await?;
    let deleted = sqlx::query("DELETE FROM device_grants WHERE account_id = ? AND grant_hash = ?")
        .bind(account_id.as_slice())
        .bind(hash.as_slice())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(key_generation
        .filter(|_| deleted == 1)
        .map(|generation| (account_id, generation)))
}

// ----------------------------------------------------------------
// Wire shapes — /device/authorize/{start,redeem}
// ----------------------------------------------------------------

/// `POST /device/authorize/start` request body.
#[derive(Default, Deserialize)]
pub(crate) struct DeviceAuthorizeStartReq {
    /// The `k1` fresh-auth grant a password or passkey sign-in returned.
    #[serde(default)]
    pub fresh_grant: Option<String>,
}

/// `POST /device/authorize/start` response body.
#[derive(Serialize)]
pub(crate) struct DeviceAuthorizeStartRes {
    /// URL-safe base64 of the RAW 32-byte grant token — the only time it
    /// leaves the server. The existing device seals it to the fresh device
    /// over the pairing channel; the server stores only its SHA-256 hash.
    pub grant_token_b64: String,
}

/// `POST /device/authorize/redeem` request body (ADR-0056: the grant alone).
#[derive(Deserialize)]
pub(crate) struct DeviceAuthorizeRedeemReq {
    /// URL-safe base64 of the raw 32-byte grant token the fresh device
    /// received over the rendezvous channel.
    pub grant_token_b64: String,
    /// URL-safe base64 (no padding) of the 32-byte id of the device this
    /// session lands on: its conversation leaf signature key (ADR-0066
    /// section 0.4). Absent when the device has no keys yet; the session then
    /// names no device and the client calls `POST /auth/session/bind-device`
    /// once it has them.
    #[serde(default)]
    pub device_id_b64: Option<String>,
}

/// `POST /device/authorize/redeem` response body. Mirrors `AuthFinishRes`
/// (webauthn.rs) — the account id plus the same session pair every login
/// flow returns (ADR-0065).
#[derive(Serialize)]
pub(crate) struct DeviceAuthorizeRedeemRes {
    /// URL-safe base64 of the 16-byte account id. Since ADR-0056 this is
    /// where the fresh device first learns it.
    pub account_id_b64: String,
    /// `session_token`, `session_expires_at_ms`, `refresh_token`,
    /// `refresh_expires_at_ms`, `key_generation`.
    #[serde(flatten)]
    pub session: crate::session::SessionPair,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_hash_is_sha256_of_token() {
        let token = [0x42u8; GRANT_TOKEN_LEN];
        let expected = {
            let mut h = Sha256::new();
            h.update(token);
            let d = h.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&d);
            out
        };
        assert_eq!(grant_hash(&token), expected);
    }

    #[test]
    fn grant_hash_is_deterministic_and_distinct() {
        let a = grant_hash(&[0x01u8; GRANT_TOKEN_LEN]);
        let a2 = grant_hash(&[0x01u8; GRANT_TOKEN_LEN]);
        let b = grant_hash(&[0x02u8; GRANT_TOKEN_LEN]);
        assert_eq!(a, a2, "same token must hash identically");
        assert_ne!(a, b, "different tokens must hash differently");
    }

    #[test]
    fn random_tokens_differ() {
        // A collision here is astronomically unlikely; a constant token
        // would make a leaked /start response trivially replayable.
        assert_ne!(random_grant_token(), random_grant_token());
    }

    #[test]
    fn decode_grant_token_validates_length() {
        let ok = URL_SAFE_NO_PAD.encode([0u8; GRANT_TOKEN_LEN]);
        assert_eq!(decode_grant_token(&ok).expect("32 bytes").len(), 32);

        let short = URL_SAFE_NO_PAD.encode([0u8; 16]);
        assert!(matches!(
            decode_grant_token(&short),
            Err(ApiError::BadInput(_))
        ));
        assert!(matches!(
            decode_grant_token("!!!not-base64!!!"),
            Err(ApiError::BadInput(_))
        ));
    }
}

/// End-to-end HTTP tests of the device-authorization grant flow (ADR-0018,
/// factor (a)). Drives the production router + a live MariaDB: mint a grant
/// (bearer), redeem it (anonymous) for a session token that verifies, and
/// prove single-use / expiry / unknown-grant / missing-bearer rejections and
/// that a legacy `pow` field is ignored.
///
/// Gated on `LAKKO_AUTH_TEST_DATABASE_URL` — skips (does not fail) when no
/// test database is provided, so `cargo test` stays green in DB-less CI.
/// Run it with, e.g.:
/// `LAKKO_AUTH_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:13306/lakko_auth_test \`
/// `  cargo test -p lakko-svc-auth device_authorize`.
#[cfg(test)]
mod device_authorize_e2e_tests {
    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, StatusCode};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use http_body_util::BodyExt as _;
    use sqlx::mysql::MySqlPoolOptions;
    use sqlx::MySqlPool;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt as _;
    use uuid::Uuid;

    /// Fixed attestation seed (unused by this flow but required by AppState).
    const ATTEST_SEED: [u8; 32] = [0xCD; 32];

    fn build_state(pool: MySqlPool) -> (crate::AppState, lakko_auth_token::TokenSecret) {
        use opaque_ke::ServerSetup;
        use rand::rngs::OsRng;
        let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
        let opaque_setup = ServerSetup::<crate::opaque::LakkoCipherSuite>::new(&mut OsRng);
        let state = crate::AppState {
            db: pool,
            opaque_setup: Arc::new(opaque_setup),
            session_token_secret: Arc::new(token_secret.clone()),
            session_token_ttl_ms: 86_400_000,
            refresh_token_ttl_ms: 30 * 86_400_000,
            fresh_grant_ttl_ms: 600_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&ATTEST_SEED)),
            turn: None,
            totp_enc_key: None,
            vault_enc_key: None,
            throttle: crate::throttle::ThrottleConfig::disabled(),
            backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
            anon_tokens: None,
            handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
            limits: Arc::new(crate::limits::Limits::defaults()),
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

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn device_authorize_grant_round_trip() {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping device_authorize_grant_round_trip: LAKKO_AUTH_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");

        let (state, token_secret) = build_state(pool.clone());

        // Fresh isolated account; the bearer belongs to the EXISTING device.
        let account_id: [u8; 16] = Uuid::new_v4().into_bytes();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .expect("insert account");
        let bearer = format!(
            "Bearer {}",
            token_secret
                .issue(&account_id, 1, &[0u8; 32], 0, now_ms() + 3_600_000)
                .expect("mint session token")
        );
        let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);

        // (vi) /start WITHOUT a bearer must 401 (route is behind the
        // require_session middleware).
        let (st, _b) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/start")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({})).unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "device/authorize/start without a bearer must 401"
        );

        // A bearer alone (the old empty body) mints nothing: the fresh-auth
        // grant is required (ADR-0065, amended 2026-10-06).
        let (st, body) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/start")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({})).unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
            "fresh-auth-required"
        );
        let minted: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM device_grants WHERE account_id = ?")
                .bind(account_id.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(minted, 0, "a refused start mints nothing");

        // The person signed in on this device just now.
        let start_body = serde_json::to_vec(&serde_json::json!({
            "fresh_grant": token_secret
                .issue_fresh_grant(&account_id, 1, now_ms() + 60_000)
                .expect("mint fresh-auth grant"),
        }))
        .unwrap();

        // (i) /start (bearer + fresh-auth grant) mints a grant + returns the
        // raw token.
        let (st, body) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/start")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(start_body.clone()))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "device/authorize/start: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let grant_token_b64 = v["grant_token_b64"].as_str().unwrap().to_string();

        // The row stores the HASH, not the raw token.
        let raw_token = URL_SAFE_NO_PAD.decode(&grant_token_b64).unwrap();
        let stored_hash: Vec<u8> =
            sqlx::query_scalar("SELECT grant_hash FROM device_grants WHERE account_id = ?")
                .bind(account_id.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(stored_hash, raw_token, "the raw token must not be stored");
        assert_eq!(stored_hash, super::grant_hash(&raw_token).to_vec());

        // (v) /redeem still carrying a legacy `pow` field (ADR-0054 S4
        // removed it): the field is ignored — serde drops unknown fields — so
        // the request reaches the consume and a wrong token is the plain
        // generic 401, not a proof-of-work error. The real grant is untouched.
        let (st, body) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/redeem")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "pow": { "seed": "junk", "nonce": "junk", "solution": "junk" },
                        "grant_token_b64": URL_SAFE_NO_PAD.encode([0x55u8; 32]),
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert_eq!(
            &body[..],
            b"invalid credentials",
            "a credential miss, not a pow error"
        );
        let live: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM device_grants WHERE account_id = ?")
                .bind(account_id.as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(live, 1, "a missed redeem must not consume the grant");

        // (iv) /redeem with an UNKNOWN grant token must fail enumeration-safe
        // (401, generic).
        let bogus = URL_SAFE_NO_PAD.encode([0x99u8; 32]);
        let (st, _b) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/redeem")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "grant_token_b64": bogus,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "redeem with an unknown grant must 401 enumeration-safe"
        );

        // The old body (an account id and no grant) is refused outright
        // (ADR-0056: no dual path); nothing is consumed.
        let (st, _b) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/redeem")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "account_id_b64": account_id_b64,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert!(
            st.is_client_error() && st != StatusCode::UNAUTHORIZED,
            "got {st}"
        );

        // (i continued) /redeem the REAL grant (anonymous, the grant alone)
        // succeeds and returns the account id and a verifiable session token.
        let (st, body) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/redeem")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "grant_token_b64": grant_token_b64,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "device/authorize/redeem: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["account_id_b64"].as_str().unwrap(), account_id_b64);
        let session_token = v["session_token"].as_str().unwrap().to_string();
        // The issued token verifies for this account.
        let verified = token_secret
            .verify(&session_token, now_ms())
            .expect("issued session token must verify");
        assert_eq!(verified.account_id.as_slice(), account_id.as_slice());

        // (ii) single-use: a SECOND redeem of the same grant fails 401.
        let (st, _b) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/redeem")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "grant_token_b64": grant_token_b64,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "a second redeem of the same grant must 401 (single-use)"
        );

        // (iii) expired grant fails. Mint a fresh grant then force-expire it.
        let (st, body) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/start")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(start_body.clone()))
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "second /start must succeed");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let grant2_b64 = v["grant_token_b64"].as_str().unwrap().to_string();
        let grant2_hash = super::grant_hash(&URL_SAFE_NO_PAD.decode(&grant2_b64).unwrap());
        sqlx::query(
            "UPDATE device_grants SET expires_at = NOW(3) - INTERVAL 10 SECOND \
             WHERE account_id = ? AND grant_hash = ?",
        )
        .bind(account_id.as_slice())
        .bind(grant2_hash.as_slice())
        .execute(&pool)
        .await
        .unwrap();

        let (st, _b) = send(
            &state,
            Request::builder()
                .method(Method::POST)
                .uri("/device/authorize/redeem")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "grant_token_b64": grant2_b64,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "an expired grant must 401 (the WHERE expires_at>NOW gate)"
        );

        // Cleanup — cascade drops any leftover grants.
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .ok();
    }
}
