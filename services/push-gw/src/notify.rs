// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `POST /push/notify-group` — the group wake fan-out, internal only.
//!
//! A group wake is a consequence of a signed publish: the DS, after the
//! ADR-0053 proof gate passed and the row was appended, forwards the sealed
//! `x-lakko-wake` blob it received, unread, with the message id it assigned,
//! over the internal network, authenticated by a shared secret (see
//! [`crate::internal`]). No client calls push-gw to wake anyone.
//!
//! ## What happens to a forwarded wake (ADR-0078 section 3)
//!
//! 1. **Open.** The blob is opened with push-gw's wake key
//!    ([`crate::wake_key`]) and its signature verified under the wake key it
//!    names. A blob that does not open, or whose signature does not verify,
//!    is refused and nothing is read.
//! 2. **Guard.** Freshness, nonce dedup and the per-group cap
//!    ([`crate::wake_guard`]).
//! 3. **Select.** Only rows registered under that `push_gid` AND that wake
//!    key are read. A wake signed by any other key - somebody who knows the
//!    `push_gid` but not the routing secret - selects only rows registered
//!    under its own key, which can only be its own.
//! 4. **Unlock.** Each row's sealed token list is opened with the unlock key
//!    the wake carried. The key is held only for this request and never
//!    stored or logged. A row that does not open (sealed under another key)
//!    is skipped.
//! 5. **Deliver.** [`delivers_to`] decides which of the row's tokens a wake
//!    reaches. A token the provider reports invalid is removed by re-sealing
//!    the row without it (the unlock key is still in hand), or by deleting
//!    the row when nothing is left.
//!
//! There is deliberately no job queue: a failure is recorded in the response
//! and dropped. The DS does not read the response; an operator driving the
//! route by hand does.
//!
//! ## What a delivered wake carries
//!
//! A pointer and nothing else: the subscription id it was delivered for,
//! the group's `push_gid`, the DS message id, and - for a call - a kind tag.
//! Never a sender, a body, a call id or a media kind (invariants #2, #5).
//! See [`crate::provider::WakeKind`] for what the kind tag discloses.
//!
//! ## What push-gw still sees (class (b), ADR-0078 section 3)
//!
//! A live push-gw opens every row a wake names and calls the providers with
//! their tokens, so it sees which tokens are woken together and can
//! accumulate group co-membership over time. Nothing that delivers through
//! APNs / FCM / Web Push removes that; separating the push operator does.
//!
//! ## Which of a device's tokens a wake reaches
//!
//! An iOS device holds **two** APNs tokens — an alert token
//! ([`ProviderKind::Apns`]) and a call token
//! ([`ProviderKind::ApnsVoip`]) — and they are not interchangeable. A
//! subscription carries a device's whole token set, so the rule is decided
//! per row:
//!
//!   1. **A call wake goes to the device's call token when it has one,
//!      and to its alert token otherwise — never to both.**
//!   2. **A message wake never goes to a call token.** Apple terminates
//!      an app that takes a VoIP push without reporting a call.
//!   3. **Every non-Apple token is untouched by both.**
//!
//! [`delivers_to`] is that rule and the only place it is written as code.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use lakko_contacts::push_wake::{
    open_push_subscription, open_push_wake, seal_push_subscription, OpenedPushWake, PushTokenEntry,
    PushWakeError, PushWakeKind, PUSH_SUBSCRIPTION_ID_LEN, PUSH_WAKE_HEADER_LEN,
};
use serde::{Deserialize, Serialize};

use crate::metrics::{DeliveryResult, WakeOutcome};
use crate::provider::{DeliverError, ProviderKind, WakeKind, WakePayload};
use crate::wake_guard::GuardRefusal;
use crate::AppState;

type HandlerError = (StatusCode, String);

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
/// `registered` is every [`ProviderKind`] the subscription holds a token
/// under; `provider` is the token being considered. See this module's docs
/// for the reasoning behind each arm.
///
/// Pure and total, so the rule is pinned by tests rather than by the order
/// a token list was sealed in.
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

/// The body the DS sends. Every field is required and nothing else is
/// accepted: the only caller is another Lakko service, so a request that
/// does not match the one shape it sends is a bug to surface.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotifyGroupRequest {
    /// The sealed wake exactly as the publisher put it in `x-lakko-wake`
    /// (base64url without padding). The DS never opened it.
    pub(crate) wake: String,
    /// The DS message id of the row just appended, so the woken client can
    /// jump to it. Assigned by the DS after the publisher sealed its wake,
    /// so it travels beside the blob rather than inside.
    pub(crate) envelope_pointer: u64,
}

#[derive(Serialize)]
pub(crate) struct NotifyGroupResponse {
    /// Subscription rows at least one delivery was attempted for.
    pub(crate) subscriptions_woken: usize,
    /// Rows registered under the wake's key whose token list did not open
    /// under the unlock key the wake carried.
    pub(crate) unopened: usize,
    /// Every provider delivery attempted.
    pub(crate) deliveries: Vec<DeliveryOutcome>,
}

const fn kind_of(kind: PushWakeKind) -> WakeKind {
    match kind {
        PushWakeKind::Message => WakeKind::Message,
        PushWakeKind::Call => WakeKind::Call,
    }
}

fn refused(status: StatusCode, why: &'static str) -> HandlerError {
    // The cause alone: never a key, a push_gid or a nonce.
    tracing::info!(why, "wake refused");
    (status, why.to_string())
}

/// The outcomes of one wake's deliveries as `provider:status=count` pairs in
/// a fixed order (`apns:ok=1,webpush:ok=1`), or `none`. Provider names and
/// the three status words only: no token, no detail.
fn delivery_summary(deliveries: &[DeliveryOutcome]) -> String {
    let mut counts: std::collections::BTreeMap<(&'static str, &'static str), usize> =
        std::collections::BTreeMap::new();
    for d in deliveries {
        *counts.entry((d.provider.as_str(), d.status)).or_insert(0) += 1;
    }
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .iter()
        .map(|((provider, status), n)| format!("{provider}:{status}={n}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `POST /push/notify-group` (internal listener) — open a forwarded wake
/// and fan it out to the subscriptions registered under its key.
///
/// Refusals: `400` a blob of the wrong shape; `403` a blob that does not
/// open or whose signature does not verify, or a stale timestamp; `409` a
/// replayed nonce; `429` the group's wake budget spent.
pub(crate) async fn notify_group(
    State(state): State<AppState>,
    Json(req): Json<NotifyGroupRequest>,
) -> Result<Json<NotifyGroupResponse>, HandlerError> {
    let wake = open_and_admit(&state, &req.wake)?;

    let rows: Vec<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT sub_id, sealed FROM push_subscriptions WHERE push_gid = ? AND wake_pk = ?",
    )
    .bind(&wake.push_gid[..])
    .bind(&wake.wake_public_key[..])
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "lookup push_subscriptions");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    let kind = kind_of(wake.kind);
    let unlock = wake.unlock_key().as_bytes();
    let mut woken = 0usize;
    let mut unopened = 0usize;
    let mut deliveries = Vec::new();

    for (sub_bytes, sealed) in rows {
        let Ok(sub_id) = <[u8; PUSH_SUBSCRIPTION_ID_LEN]>::try_from(sub_bytes.as_slice()) else {
            tracing::warn!("subscription row with a wrong-length id; skipping");
            continue;
        };
        let Ok(entries) = open_push_subscription(unlock, &wake.push_gid, &sub_id, &sealed) else {
            unopened += 1;
            continue;
        };
        let registered: Vec<ProviderKind> = entries
            .iter()
            .filter_map(|e| ProviderKind::from_db(e.provider.to_byte()))
            .collect();
        let mut invalid: Vec<u8> = Vec::new();
        let mut attempted = false;
        for entry in &entries {
            let Some(provider) = ProviderKind::from_db(entry.provider.to_byte()) else {
                continue;
            };
            if !delivers_to(kind, provider, &registered) {
                continue;
            }
            let payload = WakePayload {
                subscription_id: sub_id,
                group_pseudonym: Some(wake.push_gid),
                envelope_pointer: Some(req.envelope_pointer),
                kind,
            };
            let outcome = dispatch_one(&state, provider, &entry.token, &payload).await;
            if outcome.status == "token_invalid" {
                invalid.push(entry.provider.to_byte());
            }
            deliveries.push(outcome);
            attempted = true;
        }
        if attempted {
            woken += 1;
        }
        if !invalid.is_empty() {
            drop_invalid_tokens(
                &state,
                unlock,
                &wake.push_gid,
                &sub_id,
                &sealed,
                entries,
                &invalid,
            )
            .await;
        }
    }

    // One line per accepted wake: how many rows it reached and what each
    // provider answered, counts only. A failure's detail can carry the
    // provider URL, and APNs puts the device token in its path, so it stays
    // out of the log.
    let ring = kind.is_call();
    tracing::info!(
        ring,
        woken,
        unopened,
        deliveries = %delivery_summary(&deliveries),
        "wake fanned out"
    );

    Ok(Json(NotifyGroupResponse {
        subscriptions_woken: woken,
        unopened,
        deliveries,
    }))
}

/// Decode, open and admit one forwarded wake: the shape, the seal, the
/// signature, the clock, the nonce and the group's budget.
fn open_and_admit(state: &AppState, header: &str) -> Result<OpenedPushWake, HandlerError> {
    let wake = admit(state, header).map_err(|(outcome, error)| {
        crate::metrics::wake(outcome);
        error
    })?;
    crate::metrics::wake(WakeOutcome::Accepted);
    Ok(wake)
}

/// [`open_and_admit`] without the count: each refusal carries the outcome
/// it is counted under.
fn admit(state: &AppState, header: &str) -> Result<OpenedPushWake, (WakeOutcome, HandlerError)> {
    let malformed = || {
        (
            WakeOutcome::Malformed,
            refused(StatusCode::BAD_REQUEST, "malformed wake"),
        )
    };
    if header.len() != PUSH_WAKE_HEADER_LEN {
        return Err(malformed());
    }
    let blob = B64URL.decode(header).map_err(|_| malformed())?;
    let wake = open_push_wake(&state.wake_key, &blob).map_err(|e| match e {
        PushWakeError::BadLength | PushWakeError::UnknownVersion(_) => malformed(),
        _ => (
            WakeOutcome::NotOpened,
            refused(StatusCode::FORBIDDEN, "wake refused"),
        ),
    })?;
    state
        .wake_guard
        .admit(
            wake.timestamp_ms,
            &wake.nonce,
            &wake.push_gid,
            unix_ms_now(),
            Instant::now(),
        )
        .map_err(|r| match r {
            GuardRefusal::Stale => (
                WakeOutcome::Stale,
                refused(StatusCode::FORBIDDEN, "wake refused"),
            ),
            GuardRefusal::Replay => (
                WakeOutcome::Replayed,
                refused(StatusCode::CONFLICT, "wake replayed"),
            ),
            GuardRefusal::NonceSetFull => (
                WakeOutcome::NonceSetFull,
                refused(StatusCode::CONFLICT, "wake replayed"),
            ),
            GuardRefusal::Capped => (
                WakeOutcome::Capped,
                refused(StatusCode::TOO_MANY_REQUESTS, "wake capped"),
            ),
        })?;
    Ok(wake)
}

/// Remove the tokens a provider rejected from one subscription row: re-seal
/// it without them under the unlock key still in hand, or delete it when
/// nothing is left. The write is conditional on the row still holding the
/// bytes it was read with, so a device that re-registered meanwhile is not
/// overwritten. Failures are logged and dropped.
async fn drop_invalid_tokens(
    state: &AppState,
    unlock: &[u8; 32],
    push_gid: &[u8; 32],
    sub_id: &[u8; PUSH_SUBSCRIPTION_ID_LEN],
    read_sealed: &[u8],
    entries: Vec<PushTokenEntry>,
    invalid: &[u8],
) {
    let remaining: Vec<PushTokenEntry> = entries
        .into_iter()
        .filter(|e| !invalid.contains(&e.provider.to_byte()))
        .collect();
    let result = if remaining.is_empty() {
        sqlx::query("DELETE FROM push_subscriptions WHERE sub_id = ? AND sealed = ?")
            .bind(&sub_id[..])
            .bind(read_sealed)
            .execute(&state.db)
            .await
    } else {
        let Ok(resealed) = seal_push_subscription(unlock, push_gid, sub_id, &remaining) else {
            tracing::warn!("re-sealing a subscription without its invalid token failed");
            return;
        };
        sqlx::query("UPDATE push_subscriptions SET sealed = ? WHERE sub_id = ? AND sealed = ?")
            .bind(&resealed)
            .bind(&sub_id[..])
            .bind(read_sealed)
            .execute(&state.db)
            .await
    };
    match result {
        Ok(r) if r.rows_affected() > 0 => {
            tracing::info!(
                removed_tokens = invalid.len(),
                row_deleted = remaining.is_empty(),
                "provider-rejected token removed from a subscription",
            );
        },
        Ok(_) => {},
        Err(e) => tracing::warn!(error = %e, "removing a rejected token failed"),
    }
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
        crate::metrics::delivery(kind, DeliveryResult::NotConfigured);
        return DeliveryOutcome {
            provider: kind,
            status: "transport",
            detail: Some(format!("no provider configured for {}", kind.as_str())),
        };
    };
    let started = Instant::now();
    let result = provider.deliver(token, payload).await;
    let log_only = provider.is_log_only();
    if !log_only {
        crate::metrics::provider_send(kind, started.elapsed());
    }
    let (outcome, counted) = match result {
        Ok(()) => (
            DeliveryOutcome {
                provider: kind,
                status: "ok",
                detail: None,
            },
            if log_only {
                DeliveryResult::LogOnly
            } else {
                DeliveryResult::Ok
            },
        ),
        Err(DeliverError::TokenInvalid) => (
            DeliveryOutcome {
                provider: kind,
                status: "token_invalid",
                detail: None,
            },
            DeliveryResult::TokenInvalid,
        ),
        Err(DeliverError::Transport(msg)) => (
            DeliveryOutcome {
                provider: kind,
                status: "transport",
                detail: Some(msg),
            },
            DeliveryResult::Transport,
        ),
    };
    crate::metrics::delivery(kind, counted);
    outcome
}

#[cfg(test)]
mod tests {
    use super::{
        delivers_to, delivery_summary, DeliveryOutcome, NotifyGroupRequest, ProviderKind,
        PushWakeKind, WakeKind,
    };

    fn outcome(provider: ProviderKind, status: &'static str) -> DeliveryOutcome {
        DeliveryOutcome {
            provider,
            status,
            detail: Some("https://api.sandbox.push.apple.com/3/device/abc".to_string()),
        }
    }

    #[test]
    fn the_delivery_summary_counts_provider_and_status_and_never_the_detail() {
        let summary = delivery_summary(&[
            outcome(ProviderKind::WebPush, "ok"),
            outcome(ProviderKind::Apns, "ok"),
            outcome(ProviderKind::Apns, "token_invalid"),
            outcome(ProviderKind::Apns, "ok"),
        ]);
        assert_eq!(summary, "apns:ok=2,apns:token_invalid=1,webpush:ok=1");
        assert!(!summary.contains("device"));
    }

    #[test]
    fn a_wake_that_reached_nothing_says_none() {
        assert_eq!(delivery_summary(&[]), "none");
    }

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

    fn parse(body: &str) -> Result<NotifyGroupRequest, serde_json::Error> {
        serde_json::from_str(body)
    }

    #[test]
    fn the_request_is_the_blob_and_the_pointer() {
        let req = parse(r#"{"wake":"AAAA","envelope_pointer":7}"#).unwrap();
        assert_eq!(req.envelope_pointer, 7);
        assert_eq!(req.wake, "AAAA");
    }

    #[test]
    fn the_old_shapes_are_refused() {
        // The ADR-0054 shape named the push_gid and the kind in the clear;
        // since ADR-0078 both are inside the sealed blob.
        let old = format!(
            r#"{{"push_gid":"{}","envelope_pointer":7,"kind":"message"}}"#,
            "ab".repeat(32),
        );
        assert!(parse(&old).is_err());
        assert!(parse(r#"{"wake":"AAAA","envelope_pointer":7,"kind":"call"}"#).is_err());
        assert!(parse(r#"{"wake":"AAAA"}"#).is_err());
        assert!(parse(r#"{"envelope_pointer":7}"#).is_err());
    }

    #[test]
    fn both_wake_kinds_map() {
        assert_eq!(super::kind_of(PushWakeKind::Message), WakeKind::Message);
        assert_eq!(super::kind_of(PushWakeKind::Call), WakeKind::Call);
    }
}
