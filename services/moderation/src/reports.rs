// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `POST /moderation/report` and `GET /moderation/report/{receipt_id}`.
//!
//! The order of checks on a report: reporting configured (else 503
//! `reporting-unavailable`), the body is a well-formed report sealed to a
//! known list version and to at least one moderator on the current list
//! (else 400), then the token is spent (401 / 503 / 500 from the shared
//! gate), then the row is written. A report refused for its shape costs the
//! reporter no token.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_token::TokenContext;
use lakko_moderation::report::split_sealed;
use lakko_moderation::TargetKind;
use rand::RngCore as _;
use serde_json::json;

use crate::store::{NewReport, StoreError};
use crate::{error, AppState};

/// Days since the unix epoch, by the system clock.
fn today() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    u32::try_from(secs / 86_400).unwrap_or(u32::MAX)
}

/// Milliseconds since the unix epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `POST /moderation/report`.
pub async fn submit(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(list) = state.list.clone() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "reporting-unavailable");
    };
    let Ok((clear, sealed)) = split_sealed(&body) else {
        return error(StatusCode::BAD_REQUEST, "bad-report");
    };
    if clear.target.kind() == TargetKind::Blob {
        return error(StatusCode::BAD_REQUEST, "bad-target");
    }
    let Ok(parts) = lakko_moderation::seal::split(sealed) else {
        return error(StatusCode::BAD_REQUEST, "bad-report");
    };
    // A report sealed to a list this service does not know cannot be read
    // by anyone it knows of; one sealed to an older list stays readable by
    // the moderators that list named.
    if parts.list_version == 0 || parts.list_version > list.list.list_version {
        return error(StatusCode::BAD_REQUEST, "unknown-list-version");
    }
    let now = now_ms();
    if !parts
        .moderator_ids
        .iter()
        .any(|id| list.list.authorised_at(id, now).is_some())
    {
        return error(StatusCode::BAD_REQUEST, "no-current-moderator");
    }

    if let Err(rejection) = state
        .tokens
        .spend_header(&headers, TokenContext::Report)
        .await
    {
        return rejection.into_response();
    }

    let mut row = NewReport {
        receipt_id: [0u8; 16],
        target_kind: clear.target.kind().as_u8(),
        target: clear.target.bytes().to_vec(),
        reason: clear.reason.as_u8(),
        conversation_kind: clear.conversation_kind as u8,
        evidence_count: clear.evidence_count,
        list_version: parts.list_version,
        report: body.to_vec(),
        received_day: today(),
    };
    // A 128-bit collision is not expected; retry once rather than fail.
    for _ in 0..2 {
        rand::rngs::OsRng.fill_bytes(&mut row.receipt_id);
        match state.store.insert(row.clone()).await {
            Ok(()) => {
                tracing::info!(
                    reason = clear.reason.name(),
                    evidence_count = clear.evidence_count,
                    "report received"
                );
                return (
                    StatusCode::CREATED,
                    Json(json!({ "receipt_id": URL_SAFE_NO_PAD.encode(row.receipt_id) })),
                )
                    .into_response();
            },
            Err(StoreError::Collision) => {},
            Err(StoreError::Db(e)) => {
                tracing::error!(error = %e, "report store failed");
                return error(StatusCode::INTERNAL_SERVER_ERROR, "internal");
            },
        }
    }
    error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
}

/// `GET /moderation/report/{receipt_id}`.
pub async fn status(State(state): State<AppState>, Path(receipt_id): Path<String>) -> Response {
    let Some(id) = URL_SAFE_NO_PAD
        .decode(receipt_id.as_bytes())
        .ok()
        .and_then(|b| <[u8; 16]>::try_from(b.as_slice()).ok())
    else {
        return error(StatusCode::BAD_REQUEST, "bad-receipt");
    };
    match state.store.status(&id).await {
        Ok(Some(s)) => (StatusCode::OK, Json(json!({ "status": s.name() }))).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "unknown-receipt"),
        Err(e) => {
            tracing::error!(error = %e, "report status read failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        },
    }
}
