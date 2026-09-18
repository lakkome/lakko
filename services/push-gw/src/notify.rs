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
//!
//! ## What a wake carries
//!
//! A wake is a pointer and nothing else: a 32-byte pseudonym, an
//! optional journal cursor, and — since calls — a kind tag saying
//! whether the woken device should ring or show a message. The
//! service never sees, and never forwards, a sender, a message body,
//! a caller identity, a call id, or a media kind; all of that stays
//! inside the E2EE journal row the device fetches for itself after
//! waking (invariants #2 and #5). Adding a call kind does not change
//! that: it adds one bit of visible metadata and no new content. See
//! [`crate::provider::WakeKind`] for the disclosure argument.
//!
//! ## Which of a device's tokens a wake reaches
//!
//! An iOS device holds **two** APNs tokens — an alert token
//! ([`ProviderKind::Apns`]) and a call token
//! ([`ProviderKind::ApnsVoip`]) — and they are not interchangeable.
//! The rule, in three sentences:
//!
//!   1. **A call wake goes to the device's call token when it has one,
//!      and to its alert token otherwise — never to both.** The call
//!      token is what rings a terminated phone; the alert token is the
//!      fallback for a device that has not registered one (an older
//!      build, a user who declined, a deployment with no VoIP
//!      credential). Sending to both would ring the phone through
//!      CallKit *and* post a notification for the same call.
//!   2. **A message wake never goes to a call token.** Apple terminates
//!      an app that takes a VoIP push without reporting a call, so a
//!      message delivered there is a crash rather than a mis-styled
//!      notification.
//!   3. **Every non-Apple token is untouched by both.** FCM, Web Push
//!      and UnifiedPush each carry one token per device and receive
//!      every wake they did before.
//!
//! [`delivers_to`] is that rule and the only place it is written as
//! code; both `/push/notify` and `/push/notify-group` route through it.
//! Note rule 1 makes the decision depend on the device's *whole*
//! registered set, not on the row in hand — which is why the fan-out
//! collects each device's kinds before delivering anything.

use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::provider::{DeliverError, ProviderKind, WakeKind, WakePayload};
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

/// The one rule deciding which of a device's tokens a wake reaches.
///
/// `registered` is every [`ProviderKind`] that device has a token
/// under; `provider` is the row being considered. See this module's
/// docs for the reasoning behind each arm.
///
/// Pure and total, so the rule is pinned by tests rather than by
/// whichever query happened to return the rows in which order.
pub(crate) fn delivers_to(
    wake: WakeKind,
    provider: ProviderKind,
    registered: &[ProviderKind],
) -> bool {
    if provider.is_call_token() {
        // Rule 2: a call token only ever carries a call.
        return wake.is_call();
    }
    if provider == ProviderKind::Apns && wake.is_call() {
        // Rule 1: the alert token is the call fallback, used only when
        // this device registered no call token.
        return !registered.iter().any(|k| k.is_call_token());
    }
    // Rule 3: everything else is unchanged by calls existing.
    true
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
        // The per-device route has no call kind: a call signal rides
        // the chat group's journal, so it always fans out through
        // `/push/notify-group`.
        kind: WakeKind::Message,
    };

    // Every kind this device is registered under, so the selection rule
    // can see the whole set rather than one row at a time.
    let registered: Vec<ProviderKind> = rows
        .iter()
        .filter_map(|(provider_db, _)| ProviderKind::from_db(*provider_db))
        .collect();

    let mut deliveries = Vec::with_capacity(rows.len());
    for (provider_db, token) in rows {
        let Some(kind) = ProviderKind::from_db(provider_db) else {
            tracing::warn!(provider_db, "unknown provider in DB row — skipping");
            continue;
        };
        if !delivers_to(payload.kind, kind, &registered) {
            // A message wake never reaches a call token.
            continue;
        }
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
    /// Optional — what the woken device is being told about.
    ///
    /// Absent (or `"message"`) is the pre-call behaviour, unchanged.
    /// `"call"` marks the wake as a call signal: the providers raise
    /// their urgency headers and shorten their expiry, and the
    /// delivered payload carries the `k` tag the background handler
    /// needs to ring rather than render a message preview. An
    /// unrecognised value is rejected by the extractor rather than
    /// quietly demoted to a message — a call that silently arrives
    /// as a notification is the failure this field exists to stop.
    ///
    /// It changes nothing about *who* may notify: the PoW gate and
    /// the subscription join are exactly as they were.
    #[serde(default)]
    pub(crate) kind: WakeKind,
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
///
/// The optional `kind` field marks a wake as a call signal, which
/// raises the per-provider urgency headers and tags the delivered
/// payload. It does not change who may call this endpoint: the PoW
/// gate and the `push_group_subscriptions` join are untouched, so a
/// caller who could already fan a message wake to a group can now
/// also fan a call wake to it — no new authority, and the existing
/// ringing-bound is the client's 45-second window, not this gate.
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

    // The JOIN produces one row per token, and the same device can
    // carry tokens for several providers. A first pass collects each
    // device's full registered set, because the selection rule for a
    // call wake depends on it: the alert token is the fallback only
    // when the device holds no call token, and that is not knowable
    // from the row in hand.
    let mut registered: HashMap<Vec<u8>, Vec<ProviderKind>> = HashMap::new();
    for (device_bytes, provider_db, _) in &rows {
        if let Some(kind) = ProviderKind::from_db(*provider_db) {
            registered
                .entry(device_bytes.clone())
                .or_default()
                .push(kind);
        }
    }

    // `devices_woken` counts distinct `device_id` values actually
    // delivered to — a device whose only token the rule skipped was
    // not woken, and saying otherwise would hide the skip from the
    // operator-side metric.
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
        let device_kinds = registered.get(&device_bytes).map_or(&[][..], Vec::as_slice);
        if !delivers_to(payload.kind, kind, device_kinds) {
            tracing::debug!(
                provider = kind.as_str(),
                wake_kind = payload.kind.as_str(),
                device_prefix = %hex::encode(&device_arr[..4]),
                "token not eligible for this wake kind — skipping",
            );
            continue;
        }
        // Wake payload — same shape `/push/notify` uses. Note
        // `device_pseudonym` is the receiver, not the
        // publisher; the receiver's client looks at this to
        // confirm the wake is for the right local account.
        let wake_payload = WakePayload {
            device_pseudonym: device_arr,
            group_pseudonym: Some(group),
            envelope_pointer: payload.envelope_pointer,
            kind: payload.kind,
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

#[cfg(test)]
mod tests {
    use super::{delivers_to, NotifyGroupRequest, ProviderKind, WakeKind};

    /// Every kind that is delivered to for `wake`, given a device
    /// registered under `registered`. Reads like the fan-out loop so a
    /// test states an outcome rather than a sequence of predicates.
    fn reached(wake: WakeKind, registered: &[ProviderKind]) -> Vec<ProviderKind> {
        registered
            .iter()
            .copied()
            .filter(|k| delivers_to(wake, *k, registered))
            .collect()
    }

    const ALERT: ProviderKind = ProviderKind::Apns;
    const CALL: ProviderKind = ProviderKind::ApnsVoip;

    #[test]
    fn a_call_reaches_the_call_token_and_not_the_alert_one() {
        // The whole point: a phone that registered a call token rings
        // through CallKit, and does NOT also get a notification for
        // the same call.
        assert_eq!(reached(WakeKind::Call, &[ALERT, CALL]), vec![CALL]);
        // Registration order is not a factor.
        assert_eq!(reached(WakeKind::Call, &[CALL, ALERT]), vec![CALL]);
    }

    #[test]
    fn a_call_falls_back_to_the_alert_token_when_there_is_no_call_token() {
        // An older build, a user who declined, or a deployment with no
        // VoIP credential. The call still has to arrive somehow.
        assert_eq!(reached(WakeKind::Call, &[ALERT]), vec![ALERT]);
    }

    #[test]
    fn a_message_never_reaches_a_call_token() {
        // Apple terminates an app that takes a VoIP push without
        // reporting a call, so this is a crash, not a styling choice.
        assert_eq!(reached(WakeKind::Message, &[ALERT, CALL]), vec![ALERT]);
        assert_eq!(reached(WakeKind::Message, &[CALL]), vec![]);
    }

    #[test]
    fn a_call_token_alone_still_receives_the_call() {
        assert_eq!(reached(WakeKind::Call, &[CALL]), vec![CALL]);
    }

    #[test]
    fn the_non_apple_providers_are_untouched_by_either_kind() {
        let others = [
            ProviderKind::Fcm,
            ProviderKind::WebPush,
            ProviderKind::UnifiedPush,
        ];
        for wake in [WakeKind::Message, WakeKind::Call] {
            for provider in others {
                // Alone, and alongside both Apple tokens — neither the
                // presence of a call token nor the wake kind may gate
                // a provider that has only one token per device.
                assert!(delivers_to(wake, provider, &[provider]));
                assert!(delivers_to(wake, provider, &[provider, ALERT, CALL]));
            }
        }
    }

    #[test]
    fn a_wake_never_reaches_both_apple_tokens() {
        // The "never both" half of the rule, stated directly so it
        // cannot be lost to a refactor of the individual arms.
        for wake in [WakeKind::Message, WakeKind::Call] {
            let apple: Vec<ProviderKind> = reached(wake, &[ALERT, CALL])
                .into_iter()
                .filter(|k| matches!(k, ProviderKind::Apns | ProviderKind::ApnsVoip))
                .collect();
            assert!(apple.len() <= 1, "{wake:?} reached {apple:?}");
        }
    }

    #[test]
    fn a_device_with_no_tokens_is_reached_by_nothing() {
        assert_eq!(reached(WakeKind::Call, &[]), vec![]);
        assert_eq!(reached(WakeKind::Message, &[]), vec![]);
    }

    fn parse(kind_json: &str) -> Result<NotifyGroupRequest, serde_json::Error> {
        let body = format!(
            r#"{{"pow":{{"seed":"AA","nonce":"BB","solution":"CC"}},
                 "group_pseudonym":"{}","envelope_pointer":7{kind_json}}}"#,
            "ab".repeat(32),
        );
        serde_json::from_str(&body)
    }

    #[test]
    fn a_request_without_a_kind_is_a_message_wake() {
        // The pre-call request shape must keep working byte-for-byte:
        // every client that has not been rebuilt still sends this.
        let req = parse("").unwrap();
        assert_eq!(req.kind, WakeKind::Message);
    }

    #[test]
    fn an_explicit_message_kind_is_accepted() {
        let req = parse(r#","kind":"message""#).unwrap();
        assert_eq!(req.kind, WakeKind::Message);
    }

    #[test]
    fn a_call_kind_is_accepted() {
        let req = parse(r#","kind":"call""#).unwrap();
        assert_eq!(req.kind, WakeKind::Call);
    }

    #[test]
    fn an_unknown_kind_is_refused_not_demoted() {
        // Fail closed. Silently treating an unrecognised kind as a
        // message would turn a future call variant into a
        // notification that never rings — the exact failure this
        // field exists to remove.
        for bad in [
            r#","kind":"ring""#,
            r#","kind":"Call""#,
            r#","kind":"voip""#,
            r#","kind":1"#,
        ] {
            assert!(parse(bad).is_err(), "expected refusal for {bad}");
        }
    }
}
