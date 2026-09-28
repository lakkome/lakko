// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Device requests — ADR-0049 section 2, the camera-less another-device
//! route.
//!
//! The QR pairing flow carries the fresh device's pairing payload (its
//! KeyPackage + the rendezvous pseudonym) to an already-signed-in device by
//! having that device scan a screen. This module is a second carrier for
//! the same bytes: the fresh device posts them here, the signed-in device
//! polls, shows a comparison code both sides derive from the payload, and
//! on Confirm runs the unchanged ADR-0018 add-device flow against the
//! payload it fetched.
//!
//! - **`POST /device/request`** — anonymous. Names the account by the
//!   **login handle** of the username the person typed (ADR-0056; the
//!   ADR-0054 S5 handle, resolved by [`crate::login_handle::resolve`]), never
//!   by account id. Charged to the handle's prompt counter and the
//!   per-address bucket (ADR-0054 S4, `429` `device-request-limited`: every
//!   request puts a prompt on that account's signed-in devices, and a newer
//!   one replaces the older). Stores the payload for the resolved account
//!   (one open request per account) and returns a random `request_id`. A
//!   handle nobody holds resolves to its stable stand-in account id and the
//!   request is stored against it exactly the same way: no device polls a
//!   stand-in, so the request simply expires, and neither this answer nor
//!   `/device/request/status` says whether the name exists. Without the
//!   handle pepper the route answers `503 login-by-name-disabled`.
//! - **`POST /device/request/status`** — anonymous, unlimited (the id is a
//!   128-bit secret; the answer is one bit). Lets the fresh device notice
//!   a decline.
//! - **`GET /device/requests`** — bearer. The pending request for the
//!   caller's account, or `null`.
//! - **`DELETE /device/requests/{request_id}`** — bearer. Decline, or
//!   consume after a confirmed pairing.
//!
//! **Comparison code.** The server relays the payload and could substitute
//! its own device's; what stops that is the six-digit code each side
//! derives from the payload it holds and the user compares. The code is
//! computed client-side on both devices; the server never sees it.
//!
//! The payload is opaque here (invariant #2) and is exactly what the
//! Delivery Service would see at rendezvous time, so no new social-graph
//! edge is materialised (invariant #5).

use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};

use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

const REQUEST_ID_LEN: usize = 16;

/// A pairing payload is a protobuf carrying one KeyPackage and a 32-byte
/// pseudonym; a few hundred bytes today. The cap keeps the column honest.
const MAX_PAYLOAD_LEN: usize = 2048;

/// How long a request stays open. Long enough to walk to the other laptop.
const REQUEST_TTL_SECS: i64 = 600;

/// `POST /device/request` body.
#[derive(Deserialize)]
pub(crate) struct CreateReq {
    /// The 32-byte login handle of the account, base64url without padding.
    pub handle: String,
    /// Standard base64 of the pairing payload bytes.
    pub pairing_payload_b64: String,
}

/// `POST /device/request` response.
#[derive(Serialize)]
pub(crate) struct CreateRes {
    pub request_id_b64: String,
    pub expires_in_secs: i64,
}

/// `POST /device/request/status` body.
#[derive(Deserialize)]
pub(crate) struct StatusReq {
    pub request_id_b64: String,
}

/// `POST /device/request/status` response.
#[derive(Serialize)]
pub(crate) struct StatusRes {
    pub pending: bool,
}

/// One pending request as the signed-in device sees it.
#[derive(Serialize)]
pub(crate) struct PendingRequest {
    pub request_id_b64: String,
    pub pairing_payload_b64: String,
    pub created_at_ms: i64,
}

/// `GET /device/requests` response.
#[derive(Serialize)]
pub(crate) struct ListRes {
    pub request: Option<PendingRequest>,
}

/// `POST /device/request` — a fresh device asks to be confirmed.
#[axum::debug_handler]
pub(crate) async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreateReq>,
) -> Result<Json<CreateRes>, ApiError> {
    let pepper = crate::login_handle::pepper(&state)?;
    let handle = crate::login_handle::decode_handle(&req.handle)?;
    let payload = STANDARD
        .decode(&req.pairing_payload_b64)
        .map_err(|_| ApiError::BadInput("pairing_payload_b64 not base64"))?;
    if payload.is_empty() || payload.len() > MAX_PAYLOAD_LEN {
        return Err(ApiError::BadInput(
            "pairing payload has an impossible length",
        ));
    }
    // Counted against the named handle before anything is looked up, so an
    // unknown name trips the limit exactly like a known one.
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        crate::limits::Scope::DeviceRequestTarget,
        &handle,
    )
    .await?;
    // A bound handle names its account; an unbound one its stand-in, which
    // is stored the same way and polled by nobody (ADR-0056).
    let account_id = crate::login_handle::resolve(&state.db, &pepper, &handle)
        .await?
        .account_id;
    let mut request_id = [0u8; REQUEST_ID_LEN];
    OsRng.fill_bytes(&mut request_id);

    // Sweep expired rows opportunistically; the table is tiny.
    sqlx::query("DELETE FROM device_requests WHERE expires_at < NOW(3)")
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;

    sqlx::query(
        "INSERT INTO device_requests (account_id, request_id, pairing_payload, expires_at) \
         VALUES (?, ?, ?, NOW(3) + INTERVAL ? SECOND) \
         ON DUPLICATE KEY UPDATE request_id = VALUES(request_id), \
         pairing_payload = VALUES(pairing_payload), created_at = NOW(3), \
         expires_at = VALUES(expires_at)",
    )
    .bind(account_id.as_slice())
    .bind(request_id.as_slice())
    .bind(payload.as_slice())
    .bind(REQUEST_TTL_SECS)
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;
    // No account id in the log: for an unknown name it is a stand-in, and
    // logging which one would let the logs tell the two apart.
    tracing::info!("device-request: posted");

    Ok(Json(CreateRes {
        request_id_b64: URL_SAFE_NO_PAD.encode(request_id),
        expires_in_secs: REQUEST_TTL_SECS,
    }))
}

/// `POST /device/request/status` — is the request still open?
#[axum::debug_handler]
pub(crate) async fn status(
    State(state): State<AppState>,
    Json(req): Json<StatusReq>,
) -> Result<Json<StatusRes>, ApiError> {
    let request_id = decode_request_id(&req.request_id_b64)?;
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM device_requests WHERE request_id = ? AND expires_at > NOW(3)",
    )
    .bind(request_id.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(Json(StatusRes {
        pending: row.is_some(),
    }))
}

/// `GET /device/requests` — the pending request for this account.
#[axum::debug_handler]
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<ListRes>, ApiError> {
    let row: Option<(
        Vec<u8>,
        Vec<u8>,
        sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
    )> = sqlx::query_as(
        "SELECT request_id, pairing_payload, created_at FROM device_requests \
             WHERE account_id = ? AND expires_at > NOW(3)",
    )
    .bind(auth.0.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(Json(ListRes {
        request: row.map(|(id, payload, created)| PendingRequest {
            request_id_b64: URL_SAFE_NO_PAD.encode(id),
            pairing_payload_b64: STANDARD.encode(payload),
            created_at_ms: created.timestamp_millis(),
        }),
    }))
}

/// `DELETE /device/requests/{request_id}` — decline or consume.
#[axum::debug_handler]
pub(crate) async fn delete(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Path(request_id_b64): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let request_id = decode_request_id(&request_id_b64)?;
    let removed =
        sqlx::query("DELETE FROM device_requests WHERE account_id = ? AND request_id = ?")
            .bind(auth.0.as_slice())
            .bind(request_id.as_slice())
            .execute(&state.db)
            .await
            .map_err(ApiError::Db)?
            .rows_affected();
    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(auth.0),
        removed,
        "device-request: closed"
    );
    Ok(Json(serde_json::json!({ "removed": removed == 1 })))
}

fn decode_request_id(b64: &str) -> Result<[u8; REQUEST_ID_LEN], ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("request_id_b64 not URL-safe base64"))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::BadInput("request_id must decode to 16 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_decode_rejects_wrong_length() {
        assert!(decode_request_id(&URL_SAFE_NO_PAD.encode([1u8; 16])).is_ok());
        assert!(decode_request_id(&URL_SAFE_NO_PAD.encode([1u8; 15])).is_err());
        assert!(decode_request_id("not base64!").is_err());
    }
}

/// Database-backed round trip by login handle (ADR-0056): post, list,
/// status, delete, status again; an unknown handle answers identically and
/// reaches nobody; the per-handle limit; the old body and the dormant pepper.
/// Runs only with `LAKKO_AUTH_TEST_DATABASE_URL` set.
#[cfg(test)]
mod device_request_e2e_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, StatusCode};
    use http_body_util::BodyExt as _;
    use opaque_ke::ServerSetup;
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt as _;
    use uuid::Uuid;

    async fn send(state: &crate::AppState, req: Request<Body>) -> (StatusCode, Bytes) {
        let resp = crate::build_app(state.clone())
            .oneshot(req)
            .await
            .expect("router oneshot");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, bytes)
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

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn device_request_round_trip() {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping device_request_round_trip: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect");
        crate::MIGRATIONS.run(&pool).await.expect("migrate");

        let account_id: [u8; 16] = *Uuid::now_v7().as_bytes();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .expect("insert account");
        // The account's login handle (ADR-0054 S5); the fresh device names
        // the account by it (ADR-0056).
        let (handle, handle_b64) = crate::login_handle::random_handle();
        crate::login_handle::bind_for_test(&pool, &account_id, &handle).await;

        let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
        let state = crate::AppState {
            db: pool.clone(),
            opaque_setup: Arc::new(ServerSetup::<crate::opaque::LakkoCipherSuite>::new(
                &mut OsRng,
            )),
            session_token_secret: Arc::new(token_secret.clone()),
            session_token_ttl_ms: 86_400_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
            turn: None,
            totp_enc_key: None,
            recovery_contact_pepper: None,
            vault_enc_key: None,
            otp_sender: Arc::new(crate::otp_sender::LogOnlyOtpSender),
            throttle: crate::throttle::ThrottleConfig::disabled(),
            backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
            anon_tokens: None,
            // Room for every post below on the one shared test address; the
            // per-target counter is what this test exercises.
            handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
            limits: Arc::new(crate::limits::Limits::defaults().with_ip_window(
                crate::limits::Category::DeviceRequest,
                crate::limits::Window::new(100, 3600),
            )),
        };
        let now_ms = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(i64::MAX);
        let bearer = format!(
            "Bearer {}",
            token_secret
                .issue(&account_id, now_ms + 3_600_000)
                .expect("token")
        );

        // Nothing pending.
        let (st, body) =
            json_req(&state, Method::GET, "/device/requests", Some(&bearer), None).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["request"].is_null());

        // Post a request.
        let payload = vec![0x5Au8; 300];
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/device/request",
            None,
            Some(serde_json::json!({
                "handle": handle_b64,
                "pairing_payload_b64": STANDARD.encode(&payload),
            })),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let request_id_b64 = v["request_id_b64"].as_str().unwrap().to_string();

        // A name nobody holds answers identically (200 + an id) and is stored
        // against its stand-in, which no device polls.
        let (ghost_handle, ghost) = crate::login_handle::random_handle();
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/device/request",
            None,
            Some(serde_json::json!({
                "handle": ghost,
                "pairing_payload_b64": STANDARD.encode(&payload),
            })),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let ghost_id = v["request_id_b64"].as_str().unwrap().to_string();
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/device/request/status",
            None,
            Some(serde_json::json!({ "request_id_b64": ghost_id })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["pending"], true,
            "an unknown name's request looks open, exactly like a real one"
        );
        // It reached nobody: the only row it wrote is under the stand-in,
        // which is not an account.
        let stand_in =
            crate::login_handle::resolve(&pool, &crate::login_handle::TEST_PEPPER, &ghost_handle)
                .await
                .unwrap();
        assert!(!stand_in.bound);
        let ghost_rows: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT r.account_id FROM device_requests r \
             WHERE r.request_id = ?",
        )
        .bind(URL_SAFE_NO_PAD.decode(&ghost_id).unwrap())
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(ghost_rows, vec![(stand_in.account_id.to_vec(),)]);
        let is_account: Option<i64> = sqlx::query_scalar("SELECT 1 FROM accounts WHERE id = ?")
            .bind(stand_in.account_id.as_slice())
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(is_account.is_none(), "the stand-in is not an account");

        // The signed-in device sees its own request, payload intact.
        let (st, body) =
            json_req(&state, Method::GET, "/device/requests", Some(&bearer), None).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["request"]["request_id_b64"], request_id_b64);
        assert_eq!(
            STANDARD
                .decode(v["request"]["pairing_payload_b64"].as_str().unwrap())
                .unwrap(),
            payload
        );

        // Pending from the fresh device's point of view.
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/device/request/status",
            None,
            Some(serde_json::json!({ "request_id_b64": request_id_b64 })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["pending"], true);

        // Decline.
        let (st, body) = json_req(
            &state,
            Method::DELETE,
            &format!("/device/requests/{request_id_b64}"),
            Some(&bearer),
            None,
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let (st, body) = json_req(
            &state,
            Method::POST,
            "/device/request/status",
            None,
            Some(serde_json::json!({ "request_id_b64": request_id_b64 })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["pending"], false);

        // Per-target limit: the account and the ghost have one post each in
        // the window; both trip at the same count with the same 429 body.
        let limit = state
            .limits
            .name_window(crate::limits::Scope::DeviceRequestTarget)
            .limit;
        let post = |id: String| {
            let state = state.clone();
            let payload = payload.clone();
            async move {
                json_req(
                    &state,
                    Method::POST,
                    "/device/request",
                    None,
                    Some(serde_json::json!({
                        "handle": id,
                        "pairing_payload_b64": STANDARD.encode(&payload),
                    })),
                )
                .await
            }
        };
        for id in [&handle_b64, &ghost] {
            for _ in 1..limit {
                let (st, _) = post(id.clone()).await;
                assert_eq!(st, StatusCode::OK, "under the limit");
            }
        }
        let mut refusals = Vec::new();
        for id in [&handle_b64, &ghost] {
            let (st, body) = post(id.clone()).await;
            assert_eq!(
                st,
                StatusCode::TOO_MANY_REQUESTS,
                "per-target limit reached"
            );
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["error"], crate::limits::CODE_DEVICE_REQUEST_LIMITED);
            refusals.push(v["error"].clone());
        }
        assert_eq!(refusals[0], refusals[1], "known and unknown refused alike");

        // The old body (an account id, no handle) is refused outright.
        let (st, _) = json_req(
            &state,
            Method::POST,
            "/device/request",
            None,
            Some(serde_json::json!({
                "account_id_b64": URL_SAFE_NO_PAD.encode(account_id),
                "pairing_payload_b64": STANDARD.encode(&payload),
            })),
        )
        .await;
        assert!(st.is_client_error() && st != StatusCode::TOO_MANY_REQUESTS);

        // Without the handle pepper the route is dormant, like every name route.
        let mut dormant = state.clone();
        dormant.handle_pepper = None;
        let (st, body) = json_req(
            &dormant,
            Method::POST,
            "/device/request",
            None,
            Some(serde_json::json!({
                "handle": handle_b64,
                "pairing_payload_b64": STANDARD.encode(&payload),
            })),
        )
        .await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "login-by-name-disabled");

        sqlx::query("DELETE FROM device_requests WHERE account_id IN (?, ?)")
            .bind(account_id.as_slice())
            .bind(stand_in.account_id.as_slice())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .unwrap();
    }
}
