// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Device requests — ADR-0049 section 2, the camera-less another-device
//! route, as amended 2026-09-28 (the device link).
//!
//! The QR pairing flow carries the fresh device's pairing payload to an
//! already-signed-in device by having that device scan a screen. This module
//! relays the same payload without a camera, inside a SPAKE2 exchange
//! (`lakko-device-link`): the signed-in device (the **old** device) chooses a
//! six-digit code and shows it, the person types it on the **new** device,
//! and the devices agree on a key from it. The new device's payload travels
//! sealed under that key with a key confirmation; the old device pairs only
//! if the confirmation verifies.
//!
//! The server relays opaque values it cannot open and never sees the code.
//! What it enforces is **one exchange per request**: the old device's message
//! is written once, the new device's sealed payload once, the outcome once.
//! A relay (or this server, turned malicious) therefore gets one online guess
//! at the code per request the person's device answers.
//!
//! Routes, in the order a request lives through them:
//!
//! 1. **`POST /device/request`** — anonymous, the new device. Names the
//!    account by the **login handle** of the username the person typed
//!    (ADR-0056), resolved by [`crate::login_handle::resolve`]. Charged to the
//!    handle's prompt counter and the per-address bucket (ADR-0054 S4, `429`
//!    `device-request-limited`). One open request per account; a newer one
//!    replaces the older. A handle nobody holds resolves to its stable
//!    stand-in and is stored the same way: no device answers a stand-in, so
//!    every route below answers it exactly as it answers a real request no
//!    device has answered yet, until it expires. Without the handle pepper:
//!    `503 login-by-name-disabled`.
//! 2. **`GET /device/requests`** — bearer, the old device. The open request
//!    for the caller's account with every value posted so far.
//! 3. **`POST /device/requests/{id}/pake`** — bearer, the old device posts
//!    its SPAKE2 message (`pake_b`), once (`409
//!    device-request-already-answered` for a second device).
//! 4. **`POST /device/request/status`** — anonymous, the new device polls:
//!    still open, `pake_b` once posted, the outcome and `confirm_b` once
//!    decided. The request id is a 128-bit secret.
//! 5. **`POST /device/request/payload`** — anonymous, the new device posts
//!    `pake_a`, the sealed payload and `confirm_a`, once (`409
//!    device-request-not-ready` before `pake_b`, `409
//!    device-request-already-used` after a first post). The request id rides
//!    in the body, like the status route, so it never lands in an access log.
//! 6. **`POST /device/requests/{id}/result`** — bearer, the old device
//!    records `accepted` (with `confirm_b`) or `wrong_code`, once. The row
//!    then stays until it expires so the new device can read the outcome.
//! 7. **`DELETE /device/requests/{id}`** — bearer. Decline ("It is not me").
//!
//! Every value is opaque here (invariant #2): SPAKE2 messages, a MAC and an
//! AEAD ciphertext under a key only the two devices hold. No new social-graph
//! edge is materialised (invariant #5).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::Extension;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};

use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

const REQUEST_ID_LEN: usize = 16;

/// A SPAKE2 message is 33 bytes today; the column and this cap leave room
/// without the server knowing the shape.
const MAX_PAKE_LEN: usize = 64;

/// A key-confirmation MAC (HMAC-SHA-256).
const CONFIRM_LEN: usize = 32;

/// The sealed pairing payload: a payload of at most 2048 bytes plus a tag.
const MAX_SEALED_LEN: usize = 4096;

/// How long a request stays open. Long enough to walk to the other laptop and
/// type six digits.
const REQUEST_TTL_SECS: i64 = 600;

const OUTCOME_ACCEPTED: u8 = 1;
const OUTCOME_WRONG_CODE: u8 = 2;

/// Body code: no open request with that id (for this account, on the bearer
/// routes).
pub(crate) const CODE_NOT_FOUND: &str = "device-request-not-found";
/// Body code: another device of the account already answered this request.
pub(crate) const CODE_ALREADY_ANSWERED: &str = "device-request-already-answered";
/// Body code: the step before this one has not happened yet.
pub(crate) const CODE_NOT_READY: &str = "device-request-not-ready";
/// Body code: this step already happened; a request gets one exchange.
pub(crate) const CODE_ALREADY_USED: &str = "device-request-already-used";

fn refusal(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

/// `POST /device/request` body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateReq {
    /// The 32-byte login handle of the account, base64url without padding.
    pub handle: String,
}

/// `POST /device/request` response.
#[derive(Serialize)]
pub(crate) struct CreateRes {
    pub request_id_b64: String,
    pub expires_in_secs: i64,
}

/// `POST /device/request/status` body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatusReq {
    pub request_id_b64: String,
}

/// `POST /device/request/status` response.
#[derive(Serialize)]
pub(crate) struct StatusRes {
    /// The request exists and has not expired. `false` after a decline or
    /// expiry (indistinguishable by design).
    pub pending: bool,
    /// The old device's SPAKE2 message once posted, standard base64.
    pub pake_b_b64: Option<String>,
    /// `"accepted"` or `"wrong_code"` once the old device decided.
    pub outcome: Option<&'static str>,
    /// The old device's key confirmation when accepted, standard base64.
    pub confirm_b_b64: Option<String>,
}

/// One open request as the old device sees it.
#[derive(Serialize)]
pub(crate) struct PendingRequest {
    pub request_id_b64: String,
    pub created_at_ms: i64,
    pub pake_b_b64: Option<String>,
    pub pake_a_b64: Option<String>,
    pub sealed_payload_b64: Option<String>,
    pub confirm_a_b64: Option<String>,
    pub outcome: Option<&'static str>,
}

/// `GET /device/requests` response.
#[derive(Serialize)]
pub(crate) struct ListRes {
    pub request: Option<PendingRequest>,
}

/// `POST /device/requests/{id}/pake` body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnswerReq {
    pub pake_b_b64: String,
}

/// `POST /device/request/payload` body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // the wire names say what is base64
pub(crate) struct PayloadReq {
    pub request_id_b64: String,
    pub pake_a_b64: String,
    pub sealed_payload_b64: String,
    pub confirm_a_b64: String,
}

/// `POST /device/requests/{id}/result` body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResultReq {
    /// `"accepted"` or `"wrong_code"`.
    pub outcome: String,
    /// Required with `accepted`, refused with `wrong_code`.
    pub confirm_b_b64: Option<String>,
}

const fn outcome_name(v: u8) -> Option<&'static str> {
    match v {
        OUTCOME_ACCEPTED => Some("accepted"),
        OUTCOME_WRONG_CODE => Some("wrong_code"),
        _ => None,
    }
}

fn decode_blob(b64: &str, min: usize, max: usize, what: &'static str) -> Result<Vec<u8>, ApiError> {
    let bytes = STANDARD.decode(b64).map_err(|_| ApiError::BadInput(what))?;
    if bytes.len() < min || bytes.len() > max {
        return Err(ApiError::BadInput(what));
    }
    Ok(bytes)
}

/// `POST /device/request` — a fresh device asks to be confirmed.
#[axum::debug_handler]
pub(crate) async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreateReq>,
) -> Result<Json<CreateRes>, ApiError> {
    let pepper = crate::login_handle::pepper(&state)?;
    let handle = crate::login_handle::decode_handle(&req.handle)?;
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
    // is stored the same way and answered by nobody (ADR-0056).
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

    // A newer request replaces the older one outright: every value of the
    // older exchange is cleared with it.
    sqlx::query(
        "INSERT INTO device_requests (account_id, request_id, expires_at) \
         VALUES (?, ?, NOW(3) + INTERVAL ? SECOND) \
         ON DUPLICATE KEY UPDATE request_id = VALUES(request_id), \
         pake_b = NULL, pake_a = NULL, sealed_payload = NULL, confirm_a = NULL, \
         outcome = NULL, confirm_b = NULL, created_at = NOW(3), \
         expires_at = VALUES(expires_at)",
    )
    .bind(account_id.as_slice())
    .bind(request_id.as_slice())
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

/// `(pake_b, outcome, confirm_b)` as the status route reads them.
type StatusRow = (Option<Vec<u8>>, Option<u8>, Option<Vec<u8>>);

/// `POST /device/request/status` — where the request stands.
#[axum::debug_handler]
pub(crate) async fn status(
    State(state): State<AppState>,
    Json(req): Json<StatusReq>,
) -> Result<Json<StatusRes>, ApiError> {
    let request_id = decode_request_id(&req.request_id_b64)?;
    let row: Option<StatusRow> = sqlx::query_as(
        "SELECT pake_b, outcome, confirm_b FROM device_requests \
         WHERE request_id = ? AND expires_at > NOW(3)",
    )
    .bind(request_id.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(Json(row.map_or(
        StatusRes {
            pending: false,
            pake_b_b64: None,
            outcome: None,
            confirm_b_b64: None,
        },
        |(pake_b, outcome, confirm_b)| StatusRes {
            pending: true,
            pake_b_b64: pake_b.map(|b| STANDARD.encode(b)),
            outcome: outcome.and_then(outcome_name),
            confirm_b_b64: confirm_b.map(|b| STANDARD.encode(b)),
        },
    )))
}

/// `POST /device/request/payload` — the new device's half of the exchange.
#[axum::debug_handler]
pub(crate) async fn payload(
    State(state): State<AppState>,
    Json(req): Json<PayloadReq>,
) -> Result<Response, ApiError> {
    let request_id = decode_request_id(&req.request_id_b64)?;
    let pake_a = decode_blob(&req.pake_a_b64, 1, MAX_PAKE_LEN, "pake_a_b64")?;
    let sealed = decode_blob(
        &req.sealed_payload_b64,
        1,
        MAX_SEALED_LEN,
        "sealed_payload_b64",
    )?;
    let confirm_a = decode_blob(
        &req.confirm_a_b64,
        CONFIRM_LEN,
        CONFIRM_LEN,
        "confirm_a_b64",
    )?;
    // Written only after the old device answered and only once: the single
    // guess this request gets.
    let written = sqlx::query(
        "UPDATE device_requests SET pake_a = ?, sealed_payload = ?, confirm_a = ? \
         WHERE request_id = ? AND expires_at > NOW(3) \
         AND pake_b IS NOT NULL AND sealed_payload IS NULL",
    )
    .bind(pake_a.as_slice())
    .bind(sealed.as_slice())
    .bind(confirm_a.as_slice())
    .bind(request_id.as_slice())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?
    .rows_affected();
    if written == 1 {
        return Ok(Json(serde_json::json!({})).into_response());
    }
    // Classify the refusal. The status route already says as much, so this
    // tells an anonymous caller nothing new; a stand-in request, never
    // answered, reads `not-ready` exactly like a real one not yet answered.
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT pake_b IS NOT NULL, sealed_payload IS NOT NULL FROM device_requests \
         WHERE request_id = ? AND expires_at > NOW(3)",
    )
    .bind(request_id.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(match row {
        None => refusal(StatusCode::NOT_FOUND, CODE_NOT_FOUND),
        Some((0, _)) => refusal(StatusCode::CONFLICT, CODE_NOT_READY),
        Some(_) => refusal(StatusCode::CONFLICT, CODE_ALREADY_USED),
    })
}

/// `GET /device/requests` — the open request for this account.
#[axum::debug_handler]
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<ListRes>, ApiError> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Vec<u8>,
        sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<u8>,
    )> = sqlx::query_as(
        "SELECT request_id, created_at, pake_b, pake_a, sealed_payload, confirm_a, outcome \
         FROM device_requests WHERE account_id = ? AND expires_at > NOW(3)",
    )
    .bind(auth.0.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(Json(ListRes {
        request: row.map(
            |(id, created, pake_b, pake_a, sealed, confirm_a, outcome)| PendingRequest {
                request_id_b64: URL_SAFE_NO_PAD.encode(id),
                created_at_ms: created.timestamp_millis(),
                pake_b_b64: pake_b.map(|b| STANDARD.encode(b)),
                pake_a_b64: pake_a.map(|b| STANDARD.encode(b)),
                sealed_payload_b64: sealed.map(|b| STANDARD.encode(b)),
                confirm_a_b64: confirm_a.map(|b| STANDARD.encode(b)),
                outcome: outcome.and_then(outcome_name),
            },
        ),
    }))
}

/// `POST /device/requests/{id}/pake` — the old device answers, once.
#[axum::debug_handler]
pub(crate) async fn answer(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Path(request_id_b64): Path<String>,
    Json(req): Json<AnswerReq>,
) -> Result<Response, ApiError> {
    let request_id = decode_request_id(&request_id_b64)?;
    let pake_b = decode_blob(&req.pake_b_b64, 1, MAX_PAKE_LEN, "pake_b_b64")?;
    let written = sqlx::query(
        "UPDATE device_requests SET pake_b = ? \
         WHERE account_id = ? AND request_id = ? AND expires_at > NOW(3) AND pake_b IS NULL",
    )
    .bind(pake_b.as_slice())
    .bind(auth.0.as_slice())
    .bind(request_id.as_slice())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?
    .rows_affected();
    if written == 1 {
        tracing::info!(
            account_id = %URL_SAFE_NO_PAD.encode(auth.0),
            "device-request: answered"
        );
        return Ok(Json(serde_json::json!({})).into_response());
    }
    let exists = live_row_for(&state, &auth.0, &request_id).await?;
    Ok(if exists {
        refusal(StatusCode::CONFLICT, CODE_ALREADY_ANSWERED)
    } else {
        refusal(StatusCode::NOT_FOUND, CODE_NOT_FOUND)
    })
}

/// `POST /device/requests/{id}/result` — the old device records the outcome,
/// once.
#[axum::debug_handler]
pub(crate) async fn result(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Path(request_id_b64): Path<String>,
    Json(req): Json<ResultReq>,
) -> Result<Response, ApiError> {
    let request_id = decode_request_id(&request_id_b64)?;
    let (outcome, confirm_b) = match (req.outcome.as_str(), req.confirm_b_b64.as_deref()) {
        ("accepted", Some(c)) => (
            OUTCOME_ACCEPTED,
            Some(decode_blob(c, CONFIRM_LEN, CONFIRM_LEN, "confirm_b_b64")?),
        ),
        ("wrong_code", None) => (OUTCOME_WRONG_CODE, None),
        _ => {
            return Err(ApiError::BadInput(
                "outcome must be accepted with confirm_b_b64, or wrong_code without",
            ))
        },
    };
    let written = sqlx::query(
        "UPDATE device_requests SET outcome = ?, confirm_b = ? \
         WHERE account_id = ? AND request_id = ? AND expires_at > NOW(3) \
         AND sealed_payload IS NOT NULL AND outcome IS NULL",
    )
    .bind(outcome)
    .bind(confirm_b.as_deref())
    .bind(auth.0.as_slice())
    .bind(request_id.as_slice())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?
    .rows_affected();
    if written == 1 {
        tracing::info!(
            account_id = %URL_SAFE_NO_PAD.encode(auth.0),
            outcome = outcome_name(outcome).unwrap_or("?"),
            "device-request: decided"
        );
        return Ok(Json(serde_json::json!({})).into_response());
    }
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT sealed_payload IS NOT NULL, outcome IS NOT NULL FROM device_requests \
         WHERE account_id = ? AND request_id = ? AND expires_at > NOW(3)",
    )
    .bind(auth.0.as_slice())
    .bind(request_id.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(match row {
        None => refusal(StatusCode::NOT_FOUND, CODE_NOT_FOUND),
        Some((0, _)) => refusal(StatusCode::CONFLICT, CODE_NOT_READY),
        Some(_) => refusal(StatusCode::CONFLICT, CODE_ALREADY_USED),
    })
}

/// `DELETE /device/requests/{request_id}` — decline.
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

async fn live_row_for(
    state: &AppState,
    account_id: &[u8; 16],
    request_id: &[u8; REQUEST_ID_LEN],
) -> Result<bool, ApiError> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM device_requests \
         WHERE account_id = ? AND request_id = ? AND expires_at > NOW(3)",
    )
    .bind(account_id.as_slice())
    .bind(request_id.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(row.is_some())
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

    #[test]
    fn blob_decode_enforces_bounds() {
        assert!(decode_blob(&STANDARD.encode([0u8; 32]), 32, 32, "x").is_ok());
        assert!(decode_blob(&STANDARD.encode([0u8; 31]), 32, 32, "x").is_err());
        assert!(decode_blob(&STANDARD.encode([0u8; 65]), 1, MAX_PAKE_LEN, "x").is_err());
        assert!(decode_blob("", 1, MAX_PAKE_LEN, "x").is_err());
        assert!(decode_blob("!!", 1, MAX_PAKE_LEN, "x").is_err());
    }

    #[test]
    fn outcome_names_are_stable() {
        assert_eq!(outcome_name(OUTCOME_ACCEPTED), Some("accepted"));
        assert_eq!(outcome_name(OUTCOME_WRONG_CODE), Some("wrong_code"));
        assert_eq!(outcome_name(0), None);
    }
}

/// Database-backed walk of the device link over the relay (ADR-0049 section
/// 2, amended 2026-09-28): create by handle, answer, status, payload, list,
/// result, status; the one-exchange rules; a wrong code; decline; an unknown
/// handle answering identically; the per-handle limit; the old bodies and the
/// dormant pepper. Runs only with `LAKKO_AUTH_TEST_DATABASE_URL` set.
#[cfg(test)]
mod device_request_e2e_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, StatusCode};
    use http_body_util::BodyExt as _;
    use lakko_device_link::{initiate, LinkCode, LinkContext, Responder};
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
    ) -> (StatusCode, serde_json::Value) {
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
        let (st, bytes) = send(state, req).await;
        let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (st, v)
    }

    async fn status_of(state: &crate::AppState, id: &str) -> serde_json::Value {
        let (st, v) = json_req(
            state,
            Method::POST,
            "/device/request/status",
            None,
            Some(serde_json::json!({ "request_id_b64": id })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        v
    }

    fn b64(bytes: &[u8]) -> String {
        STANDARD.encode(bytes)
    }

    fn unb64(v: &serde_json::Value) -> Vec<u8> {
        STANDARD.decode(v.as_str().unwrap()).unwrap()
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn device_link_over_the_relay() {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping device_link_over_the_relay: LAKKO_AUTH_TEST_DATABASE_URL unset");
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
        let mut rng = OsRng;
        let payload = vec![0x5Au8; 300];

        // Nothing open.
        let (st, v) = json_req(&state, Method::GET, "/device/requests", Some(&bearer), None).await;
        assert_eq!(st, StatusCode::OK);
        assert!(v["request"].is_null());

        // 1. The new device asks.
        let (st, v) = json_req(
            &state,
            Method::POST,
            "/device/request",
            None,
            Some(serde_json::json!({ "handle": handle_b64 })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let id = v["request_id_b64"].as_str().unwrap().to_string();
        let id_bytes = URL_SAFE_NO_PAD.decode(&id).unwrap();
        let s = status_of(&state, &id).await;
        assert_eq!(s["pending"], true);
        assert!(s["pake_b_b64"].is_null() && s["outcome"].is_null());

        // The payload cannot come before the old device answered.
        let early = serde_json::json!({
            "request_id_b64": id,
            "pake_a_b64": b64(&[0x41; 33]),
            "sealed_payload_b64": b64(&[1; 64]),
            "confirm_a_b64": b64(&[2; 32]),
        });
        let (st, v) = json_req(
            &state,
            Method::POST,
            "/device/request/payload",
            None,
            Some(early.clone()),
        )
        .await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::CONFLICT, Some(CODE_NOT_READY))
        );

        // 2-3. The old device sees it and answers with its SPAKE2 message.
        let (st, v) = json_req(&state, Method::GET, "/device/requests", Some(&bearer), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["request"]["request_id_b64"], id);
        assert!(v["request"]["pake_b_b64"].is_null());
        let ctx = LinkContext::new(&id_bytes, &handle).unwrap();
        let (responder, code) = Responder::start(ctx.clone(), &mut rng);
        let pake_uri = format!("/device/requests/{id}/pake");
        let (st, _) = json_req(
            &state,
            Method::POST,
            &pake_uri,
            Some(&bearer),
            Some(serde_json::json!({ "pake_b_b64": b64(responder.pake_message()) })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        // A second answer (another device of the account) is refused.
        let (st, v) = json_req(
            &state,
            Method::POST,
            &pake_uri,
            Some(&bearer),
            Some(serde_json::json!({ "pake_b_b64": b64(&[0x42; 33]) })),
        )
        .await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::CONFLICT, Some(CODE_ALREADY_ANSWERED))
        );

        // 4-5. The new device reads pake_b, the person types the code.
        let s = status_of(&state, &id).await;
        let pake_b = unb64(&s["pake_b_b64"]);
        let typed = LinkCode::parse(&code.grouped()).unwrap();
        let (sealed, check) = initiate(&ctx, &typed, &pake_b, &payload, &mut rng).unwrap();
        let body = serde_json::json!({
            "request_id_b64": id,
            "pake_a_b64": b64(&sealed.pake_a),
            "sealed_payload_b64": b64(&sealed.sealed_payload),
            "confirm_a_b64": b64(&sealed.confirm_a),
        });
        let (st, _) = json_req(
            &state,
            Method::POST,
            "/device/request/payload",
            None,
            Some(body.clone()),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        // One guess per request.
        let (st, v) = json_req(
            &state,
            Method::POST,
            "/device/request/payload",
            None,
            Some(body),
        )
        .await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::CONFLICT, Some(CODE_ALREADY_USED))
        );

        // 6. The old device reads the payload, verifies and records the outcome.
        let (_, v) = json_req(&state, Method::GET, "/device/requests", Some(&bearer), None).await;
        let r = &v["request"];
        let accepted = responder
            .accept(
                &unb64(&r["pake_a_b64"]),
                &unb64(&r["sealed_payload_b64"]),
                &unb64(&r["confirm_a_b64"]),
            )
            .unwrap();
        assert_eq!(accepted.payload, payload);
        let result_uri = format!("/device/requests/{id}/result");
        let (st, _) = json_req(
            &state,
            Method::POST,
            &result_uri,
            Some(&bearer),
            Some(serde_json::json!({ "outcome": "wrong_code", "confirm_b_b64": b64(&[0; 32]) })),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::BAD_REQUEST,
            "wrong_code carries no confirm_b"
        );
        let (st, _) = json_req(
            &state,
            Method::POST,
            &result_uri,
            Some(&bearer),
            Some(serde_json::json!({
                "outcome": "accepted",
                "confirm_b_b64": b64(&accepted.confirm_b),
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, v) = json_req(
            &state,
            Method::POST,
            &result_uri,
            Some(&bearer),
            Some(serde_json::json!({ "outcome": "wrong_code" })),
        )
        .await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::CONFLICT, Some(CODE_ALREADY_USED))
        );

        // The new device reads the outcome and checks the old device's
        // confirmation.
        let s = status_of(&state, &id).await;
        assert_eq!(s["outcome"], "accepted");
        check.verify(&unb64(&s["confirm_b_b64"])).unwrap();

        // A second request replaces the first; a wrong code is recorded.
        let (_, v) = json_req(
            &state,
            Method::POST,
            "/device/request",
            None,
            Some(serde_json::json!({ "handle": handle_b64 })),
        )
        .await;
        let id2 = v["request_id_b64"].as_str().unwrap().to_string();
        assert_eq!(status_of(&state, &id).await["pending"], false, "replaced");
        let ctx2 = LinkContext::new(&URL_SAFE_NO_PAD.decode(&id2).unwrap(), &handle).unwrap();
        let (responder, code) = Responder::start(ctx2.clone(), &mut rng);
        let (st, _) = json_req(
            &state,
            Method::POST,
            &format!("/device/requests/{id2}/pake"),
            Some(&bearer),
            Some(serde_json::json!({ "pake_b_b64": b64(responder.pake_message()) })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let wrong = if code.as_str() == "000000" {
            "000001"
        } else {
            "000000"
        };
        let (sealed, _) = initiate(
            &ctx2,
            &LinkCode::parse(wrong).unwrap(),
            responder.pake_message(),
            &payload,
            &mut rng,
        )
        .unwrap();
        let (st, _) = json_req(
            &state,
            Method::POST,
            "/device/request/payload",
            None,
            Some(serde_json::json!({
                "request_id_b64": id2,
                "pake_a_b64": b64(&sealed.pake_a),
                "sealed_payload_b64": b64(&sealed.sealed_payload),
                "confirm_a_b64": b64(&sealed.confirm_a),
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            responder.accept(&sealed.pake_a, &sealed.sealed_payload, &sealed.confirm_a),
            Err(lakko_device_link::LinkError::WrongCode)
        );
        let (st, _) = json_req(
            &state,
            Method::POST,
            &format!("/device/requests/{id2}/result"),
            Some(&bearer),
            Some(serde_json::json!({ "outcome": "wrong_code" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let s = status_of(&state, &id2).await;
        assert_eq!(
            (s["pending"].as_bool(), s["outcome"].as_str()),
            (Some(true), Some("wrong_code"))
        );
        assert!(s["confirm_b_b64"].is_null());

        // Decline.
        let (_, v) = json_req(
            &state,
            Method::POST,
            "/device/request",
            None,
            Some(serde_json::json!({ "handle": handle_b64 })),
        )
        .await;
        let id3 = v["request_id_b64"].as_str().unwrap().to_string();
        let (st, _) = json_req(
            &state,
            Method::DELETE,
            &format!("/device/requests/{id3}"),
            Some(&bearer),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(status_of(&state, &id3).await["pending"], false);
        let (st, v) = json_req(
            &state,
            Method::POST,
            &format!("/device/requests/{id3}/pake"),
            Some(&bearer),
            Some(serde_json::json!({ "pake_b_b64": b64(&[0x42; 33]) })),
        )
        .await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::NOT_FOUND, Some(CODE_NOT_FOUND))
        );

        // A name nobody holds: every anonymous answer matches a real request
        // no device has answered yet.
        let (ghost_handle, ghost) = crate::login_handle::random_handle();
        let (st, v) = json_req(
            &state,
            Method::POST,
            "/device/request",
            None,
            Some(serde_json::json!({ "handle": ghost })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let ghost_id = v["request_id_b64"].as_str().unwrap().to_string();
        let s = status_of(&state, &ghost_id).await;
        assert_eq!(s["pending"], true);
        assert!(s["pake_b_b64"].is_null());
        let mut early_ghost = early.clone();
        early_ghost["request_id_b64"] = serde_json::json!(ghost_id);
        let (st, v) = json_req(
            &state,
            Method::POST,
            "/device/request/payload",
            None,
            Some(early_ghost),
        )
        .await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::CONFLICT, Some(CODE_NOT_READY))
        );
        let stand_in =
            crate::login_handle::resolve(&pool, &crate::login_handle::TEST_PEPPER, &ghost_handle)
                .await
                .unwrap();
        assert!(!stand_in.bound);
        let is_account: Option<i64> = sqlx::query_scalar("SELECT 1 FROM accounts WHERE id = ?")
            .bind(stand_in.account_id.as_slice())
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(is_account.is_none(), "the stand-in is not an account");

        // Per-target limit: known and unknown refused alike at the same count.
        let limit = state
            .limits
            .name_window(crate::limits::Scope::DeviceRequestTarget)
            .limit;
        let used_by_account = 3; // three creates above
        let post = |h: String| {
            let state = state.clone();
            async move {
                json_req(
                    &state,
                    Method::POST,
                    "/device/request",
                    None,
                    Some(serde_json::json!({ "handle": h })),
                )
                .await
            }
        };
        for _ in used_by_account..limit {
            assert_eq!(post(handle_b64.clone()).await.0, StatusCode::OK);
        }
        for _ in 1..limit {
            assert_eq!(post(ghost.clone()).await.0, StatusCode::OK);
        }
        let mut refusals = Vec::new();
        for h in [&handle_b64, &ghost] {
            let (st, v) = post(h.clone()).await;
            assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(v["error"], crate::limits::CODE_DEVICE_REQUEST_LIMITED);
            refusals.push(v["error"].clone());
        }
        assert_eq!(refusals[0], refusals[1]);

        // The old bodies are refused outright (flag day): a pairing payload
        // on create, an account id instead of a handle.
        let (other_handle, other) = crate::login_handle::random_handle();
        let _ = other_handle;
        for body in [
            serde_json::json!({ "handle": other, "pairing_payload_b64": b64(&payload) }),
            serde_json::json!({ "account_id_b64": URL_SAFE_NO_PAD.encode(account_id) }),
        ] {
            let (st, _) = json_req(&state, Method::POST, "/device/request", None, Some(body)).await;
            assert!(
                st.is_client_error() && st != StatusCode::TOO_MANY_REQUESTS,
                "{st}"
            );
        }

        // Without the handle pepper the route is dormant.
        let mut dormant = state.clone();
        dormant.handle_pepper = None;
        let (st, v) = json_req(
            &dormant,
            Method::POST,
            "/device/request",
            None,
            Some(serde_json::json!({ "handle": handle_b64 })),
        )
        .await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
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
