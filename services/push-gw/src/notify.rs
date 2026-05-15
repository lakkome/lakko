// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `POST /push/notify` — wake-up dispatcher.
//!
//! Looks up every registered token for `device_pseudonym`, picks the
//! matching `Provider` from `AppState.providers`, and fires off a
//! delivery in parallel. The response reports per-provider outcomes.
//!
//! Phase 1.6 deliberately does NOT introduce a job queue: failures
//! are recorded in the response and the caller can decide whether to
//! retry. Phase 2 adds a durable retry queue with backoff.
//!
//! `TokenInvalid` results trigger an immediate cleanup of the offending
//! row from `push_tokens` — Apple/Google generate a steady drift of
//! invalid tokens (app uninstalls, backup restores, etc.) and the
//! cleanest place to garbage-collect is at the moment we see the
//! provider reject one.

use std::collections::HashSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::provider::{DeliverError, ProviderKind, WakePayload};
use crate::AppState;

const PSEUDONYM_LEN: usize = 32;

/// Decode a 64-char lowercase hex string into a 32-byte pseudonym.
fn decode_pseudonym_hex(hex_str: &str) -> Result<[u8; PSEUDONYM_LEN], (StatusCode, String)> {
    if hex_str.len() != PSEUDONYM_LEN * 2 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("pseudonym must be {} hex chars", PSEUDONYM_LEN * 2),
        ));
    }
    if !hex_str
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "pseudonym must be lowercase hex [0-9a-f]".to_string(),
        ));
    }
    let mut out = [0u8; PSEUDONYM_LEN];
    hex::decode_to_slice(hex_str, &mut out)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("hex decode: {e}")))?;
    Ok(out)
}

#[derive(Deserialize)]
pub(crate) struct NotifyRequest {
    pub(crate) pow: crate::pow::PowProof,
    pub(crate) device_pseudonym: String,
    /// Optional — if known, lets the recipient client jump to the
    /// right journal cursor instead of polling all of its groups.
    #[serde(default)]
    pub(crate) group_pseudonym: Option<String>,
    /// Optional — server-assigned cursor in `services/ds`.
    #[serde(default)]
    pub(crate) envelope_pointer: Option<u64>,
}

#[derive(Serialize)]
pub(crate) struct NotifyResponse {
    /// Number of provider rows looked up for this device.
    pub(crate) tokens_found: usize,
    /// Per-provider outcomes; useful for the caller to see which legs
    /// of the fan-out actually completed.
    pub(crate) deliveries: Vec<DeliveryOutcome>,
}

#[derive(Serialize)]
pub(crate) struct DeliveryOutcome {
    pub(crate) provider: ProviderKind,
    /// "ok" / "token_invalid" / "transport".
    pub(crate) status: &'static str,
    /// Free-form, present only for failures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

/// Strip a token row that the platform rejected. Errors here are
/// non-fatal — we just log and move on.
async fn drop_invalid_token(state: &AppState, device: &[u8; PSEUDONYM_LEN], provider_db: u8) {
    let result = sqlx::query("DELETE FROM push_tokens WHERE device_id = ? AND provider = ?")
        .bind(&device[..])
        .bind(provider_db)
        .execute(&state.db)
        .await;
    match result {
        Ok(r) if r.rows_affected() > 0 => {
            tracing::info!(
                provider = provider_db,
                device_prefix = %hex::encode(&device[..4]),
                "removed token after provider reported it invalid",
            );
        },
        Ok(_) => {},
        Err(e) => tracing::warn!(error = %e, "drop_invalid_token: delete failed"),
    }
}

/// `POST /push/notify` — wake every registered device endpoint for
/// this pseudonym.
pub(crate) async fn notify(
    State(state): State<AppState>,
    Json(payload): Json<NotifyRequest>,
) -> Result<Json<NotifyResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let device = decode_pseudonym_hex(&payload.device_pseudonym)?;
    let group = payload
        .group_pseudonym
        .as_deref()
        .map(decode_pseudonym_hex)
        .transpose()?;

    // Look up every (provider, token) row for this device.
    let rows: Vec<(u8, Vec<u8>)> =
        sqlx::query_as("SELECT provider, token FROM push_tokens WHERE device_id = ?")
            .bind(&device[..])
            .fetch_all(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "lookup push_tokens");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    let payload = WakePayload {
        device_pseudonym: device,
        group_pseudonym: group,
        envelope_pointer: payload.envelope_pointer,
    };

    let mut deliveries = Vec::with_capacity(rows.len());
    for (provider_db, token) in rows {
        let Some(kind) = ProviderKind::from_db(provider_db) else {
            tracing::warn!(provider_db, "unknown provider in DB row — skipping");
            continue;
        };
        let outcome = dispatch_one(&state, kind, &token, &payload).await;
        deliveries.push(outcome);

        // If the provider declared the token invalid, drop the row
        // synchronously — Apple/Google produce a steady drift and the
        // notify path is the only place we get a signal.
        if matches!(deliveries.last().map(|d| d.status), Some("token_invalid")) {
            drop_invalid_token(&state, &device, provider_db).await;
        }
    }

    let tokens_found = deliveries.len();
    Ok(Json(NotifyResponse {
        tokens_found,
        deliveries,
    }))
}

// ============================================================================
// /push/notify-group (Phase 1.20e — fan-out)
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct NotifyGroupRequest {
    pub(crate) pow: crate::pow::PowProof,
    /// 32-byte group pseudonym, lowercase hex. The publisher
    /// just wrote a message into the DS journal at this key;
    /// we fan a wake out to every device subscribed.
    pub(crate) group_pseudonym: String,
    /// Optional — server-assigned cursor in `services/ds` so
    /// the woken client can jump to the exact row.
    #[serde(default)]
    pub(crate) envelope_pointer: Option<u64>,
}

#[derive(Serialize)]
pub(crate) struct NotifyGroupResponse {
    /// Number of `(device_id, token, provider)` rows fanned to.
    /// `0` is a legitimate outcome — no device in the group has
    /// push registered yet.
    pub(crate) devices_woken: usize,
    /// Number of provider deliveries actually attempted (some
    /// devices may carry tokens for multiple providers).
    pub(crate) deliveries: Vec<DeliveryOutcome>,
}

/// `POST /push/notify-group` — fan a wake out to every device
/// subscribed to this group. Phase 1.20e: the publisher's client
/// calls this immediately after a successful
/// `DsClient::publishGroup` so peers' devices wake and pull the
/// new message.
///
/// Privacy posture: the push-gw learns `(device_id, group_id)`
/// via the subscription table; same metadata DS already has via
/// the cursor-fetch pattern. Documented in
/// `migrations/0002_push_group_subscriptions.sql`.
pub(crate) async fn notify_group(
    State(state): State<AppState>,
    Json(payload): Json<NotifyGroupRequest>,
) -> Result<Json<NotifyGroupResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

    let group = decode_pseudonym_hex(&payload.group_pseudonym)?;

    // Join `push_group_subscriptions` on `push_tokens` so we
    // get every `(device, provider, token)` triple in one
    // query. The same `(device, group)` row may match multiple
    // tokens per device (web + mobile); each is delivered to
    // independently.
    let rows: Vec<(Vec<u8>, u8, Vec<u8>)> = sqlx::query_as(
        "SELECT t.device_id, t.provider, t.token \
         FROM push_group_subscriptions s \
         JOIN push_tokens t ON s.device_id = t.device_id \
         WHERE s.group_id = ?",
    )
    .bind(&group[..])
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "lookup push_group_subscriptions JOIN push_tokens");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    // Deduplicate by `(device_id, provider)` — the JOIN
    // produces one row per token, and the same device can carry
    // tokens for multiple providers; both get fanned to, which
    // is the right behaviour. We just count `devices_woken` as
    // distinct `device_id` values for the operator-side metric.
    let mut woken: HashSet<Vec<u8>> = HashSet::new();
    let mut deliveries = Vec::with_capacity(rows.len());

    for (device_bytes, provider_db, token) in rows {
        let Ok(device_arr): Result<[u8; PSEUDONYM_LEN], _> =
            <[u8; PSEUDONYM_LEN]>::try_from(device_bytes.as_slice())
        else {
            tracing::warn!(
                len = device_bytes.len(),
                "subscriptions row has wrong-length device_id — skipping",
            );
            continue;
        };
        let Some(kind) = ProviderKind::from_db(provider_db) else {
            tracing::warn!(provider_db, "unknown provider in DB row — skipping");
            continue;
        };
        // Wake payload — same shape `/push/notify` uses. Note
        // `device_pseudonym` is the receiver, not the
        // publisher; the receiver's client looks at this to
        // confirm the wake is for the right local account.
        let wake_payload = WakePayload {
            device_pseudonym: device_arr,
            group_pseudonym: Some(group),
            envelope_pointer: payload.envelope_pointer,
        };

        let outcome = dispatch_one(&state, kind, &token, &wake_payload).await;
        if matches!(outcome.status, "token_invalid") {
            drop_invalid_token(&state, &device_arr, provider_db).await;
        }
        deliveries.push(outcome);
        woken.insert(device_arr.to_vec());
    }

    Ok(Json(NotifyGroupResponse {
        devices_woken: woken.len(),
        deliveries,
    }))
}

async fn dispatch_one(
    state: &AppState,
    kind: ProviderKind,
    token: &[u8],
    payload: &WakePayload,
) -> DeliveryOutcome {
    let Some(provider) = state.providers.get(&kind) else {
        // No provider configured for this kind. This is a config gap,
        // not a per-request failure — surface it but don't blow up.
        tracing::warn!(
            provider = kind.as_str(),
            "no provider configured for this kind — skipping",
        );
        return DeliveryOutcome {
            provider: kind,
            status: "transport",
            detail: Some(format!("no provider configured for {}", kind.as_str())),
        };
    };
    match provider.deliver(token, payload).await {
        Ok(()) => DeliveryOutcome {
            provider: kind,
            status: "ok",
            detail: None,
        },
        Err(DeliverError::TokenInvalid) => DeliveryOutcome {
            provider: kind,
            status: "token_invalid",
            detail: None,
        },
        Err(DeliverError::Transport(msg)) => DeliveryOutcome {
            provider: kind,
            status: "transport",
            detail: Some(msg),
        },
    }
}
