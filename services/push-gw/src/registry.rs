// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sealed subscriptions ([ADR-0078] section 3).
//!
//! A subscription row is `(sub_id, push_gid, wake_pk, sealed)`:
//!
//! - `sub_id` - 16 random bytes the device keeps per (its own) subscription,
//!   so it can replace or remove its row. Knowing it is the capability; no
//!   two of a device's subscriptions share one.
//! - `push_gid` - the ADR-0042 routing id of the group.
//! - `wake_pk` - the public half of the group's wake key. A wake is
//!   delivered only to rows registered under the key that signed it.
//! - `sealed` - the device's provider tokens, AEAD-sealed under the group's
//!   unlock key (`lakko_contacts::push_wake::seal_push_subscription`). The
//!   unlock key reaches push-gw only inside a sealed wake and is never
//!   stored, so this table at rest holds no token in the clear, nothing
//!   equal across two groups, and no device pseudonym.
//!
//! No timestamp is stored: a device re-registers all of its subscriptions
//! together when its provider token changes, and equal timestamps on rows of
//! different groups would tie those rows to one device.
//!
//! Every route here is anonymous (a session would name the account behind
//! a subscription) and spends one `push-register` anonymous token, checked by
//! the router's gate (`x-lakko-anon-token`) before the handler runs.
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use lakko_contacts::push_wake::{is_well_formed_subscription_len, PUSH_SUBSCRIPTION_ID_LEN};
use serde::{Deserialize, Serialize};

use crate::AppState;

type HandlerError = (StatusCode, String);

fn bad(what: &str) -> HandlerError {
    (StatusCode::BAD_REQUEST, format!("malformed {what}"))
}

fn db_error(what: &'static str) -> impl Fn(sqlx::Error) -> HandlerError {
    move |e: sqlx::Error| {
        tracing::error!(error = %e, what, "subscription store");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    }
}

/// Decode a base64url-no-pad field of exactly `N` bytes.
fn fixed<const N: usize>(b64: &str, what: &str) -> Result<[u8; N], HandlerError> {
    let bytes = B64URL.decode(b64).map_err(|_| bad(what))?;
    bytes.try_into().map_err(|_| bad(what))
}

// ============================================================================
// /push/subscribe
// ============================================================================

/// `POST /push/subscribe`. Every field is required and nothing else is
/// accepted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubscribeRequest {
    /// 16 bytes, base64url without padding.
    pub(crate) sub_id: String,
    /// The group's `push_gid`, 32 bytes, base64url without padding.
    pub(crate) push_gid: String,
    /// The group's wake public key, 32 bytes, base64url without padding.
    pub(crate) wake_pk: String,
    /// The sealed token list, base64url without padding.
    pub(crate) sealed: String,
}

#[derive(Serialize)]
pub(crate) struct OkResponse {
    pub(crate) ok: bool,
}

/// `POST /push/subscribe` — store (or replace) the subscription row
/// `sub_id`. Idempotent: re-registering the same id replaces its row, which
/// is how a device publishes a new provider token.
///
/// push-gw cannot open `sealed` here (it has no unlock key) and does not
/// try; it checks only that the length is one a sealed subscription can
/// have, so it never stores a row no wake could open.
pub(crate) async fn subscribe(
    State(state): State<AppState>,
    Json(req): Json<SubscribeRequest>,
) -> Result<Json<OkResponse>, HandlerError> {
    let sub_id: [u8; PUSH_SUBSCRIPTION_ID_LEN] = fixed(&req.sub_id, "sub_id")?;
    let push_gid: [u8; 32] = fixed(&req.push_gid, "push_gid")?;
    let wake_pk: [u8; 32] = fixed(&req.wake_pk, "wake_pk")?;
    let sealed = B64URL.decode(&req.sealed).map_err(|_| bad("sealed"))?;
    if !is_well_formed_subscription_len(sealed.len()) {
        return Err(bad("sealed"));
    }

    sqlx::query(
        "INSERT INTO push_subscriptions (sub_id, push_gid, wake_pk, sealed) VALUES (?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE push_gid = VALUES(push_gid), wake_pk = VALUES(wake_pk), \
         sealed = VALUES(sealed)",
    )
    .bind(&sub_id[..])
    .bind(&push_gid[..])
    .bind(&wake_pk[..])
    .bind(&sealed)
    .execute(&state.db)
    .await
    .map_err(db_error("upsert push_subscriptions"))?;

    tracing::debug!("push subscription stored");
    Ok(Json(OkResponse { ok: true }))
}

// ============================================================================
// /push/unsubscribe
// ============================================================================

/// `POST /push/unsubscribe`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnsubscribeRequest {
    /// 16 bytes, base64url without padding.
    pub(crate) sub_id: String,
}

#[derive(Serialize)]
pub(crate) struct UnsubscribeResponse {
    pub(crate) ok: bool,
    /// `true` if a row was deleted; `false` if there was none (idempotent:
    /// a device may call it on sign-out without checking first).
    pub(crate) removed: bool,
}

/// `POST /push/unsubscribe` — drop the subscription row `sub_id`.
pub(crate) async fn unsubscribe(
    State(state): State<AppState>,
    Json(req): Json<UnsubscribeRequest>,
) -> Result<Json<UnsubscribeResponse>, HandlerError> {
    let sub_id: [u8; PUSH_SUBSCRIPTION_ID_LEN] = fixed(&req.sub_id, "sub_id")?;
    let result = sqlx::query("DELETE FROM push_subscriptions WHERE sub_id = ?")
        .bind(&sub_id[..])
        .execute(&state.db)
        .await
        .map_err(db_error("delete push_subscriptions"))?;
    Ok(Json(UnsubscribeResponse {
        ok: true,
        removed: result.rows_affected() > 0,
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        B64URL.encode(bytes)
    }

    fn body(sub: usize, gid: usize, pk: usize, sealed: usize) -> String {
        format!(
            r#"{{"sub_id":"{}","push_gid":"{}","wake_pk":"{}","sealed":"{}"}}"#,
            b64(&vec![1; sub]),
            b64(&vec![2; gid]),
            b64(&vec![3; pk]),
            b64(&vec![4; sealed]),
        )
    }

    #[test]
    fn a_subscribe_body_has_exactly_the_four_fields() {
        let ok: SubscribeRequest = serde_json::from_str(&body(16, 32, 32, 285)).unwrap();
        assert!(fixed::<16>(&ok.sub_id, "sub_id").is_ok());
        // The pre-ADR-0078 shapes are refused.
        for old in [
            r#"{"device_pseudonym":"ab","provider":"fcm","token":"AA"}"#,
            r#"{"device_pseudonym":"ab","group_pseudonym":"cd"}"#,
        ] {
            assert!(serde_json::from_str::<SubscribeRequest>(old).is_err());
        }
        let extra = body(16, 32, 32, 285).replace('}', r#","device_pseudonym":"x"}"#);
        assert!(serde_json::from_str::<SubscribeRequest>(&extra).is_err());
    }

    #[test]
    fn wrong_length_fields_are_refused() {
        assert!(fixed::<16>(&b64(&[0; 15]), "sub_id").is_err());
        assert!(fixed::<16>(&b64(&[0; 17]), "sub_id").is_err());
        assert!(fixed::<32>(&b64(&[0; 31]), "push_gid").is_err());
        assert!(fixed::<32>("not base64!", "push_gid").is_err());
        // Padded and standard-alphabet spellings are refused.
        assert!(fixed::<16>(&(b64(&[0; 16]) + "=="), "sub_id").is_err());
    }

    #[test]
    fn only_a_sealed_subscription_length_is_stored() {
        use lakko_contacts::push_wake::{MAX_PUSH_SUBSCRIPTION_LEN, MIN_PUSH_SUBSCRIPTION_LEN};
        assert!(is_well_formed_subscription_len(MIN_PUSH_SUBSCRIPTION_LEN));
        assert!(is_well_formed_subscription_len(MAX_PUSH_SUBSCRIPTION_LEN));
        assert!(!is_well_formed_subscription_len(
            MIN_PUSH_SUBSCRIPTION_LEN - 1
        ));
        assert!(!is_well_formed_subscription_len(
            MIN_PUSH_SUBSCRIPTION_LEN + 1
        ));
        assert!(!is_well_formed_subscription_len(
            MAX_PUSH_SUBSCRIPTION_LEN + 256
        ));
    }

    #[test]
    fn an_unsubscribe_body_is_the_sub_id_alone() {
        assert!(serde_json::from_str::<UnsubscribeRequest>(&format!(
            r#"{{"sub_id":"{}"}}"#,
            b64(&[1; 16])
        ))
        .is_ok());
        assert!(serde_json::from_str::<UnsubscribeRequest>(
            r#"{"device_pseudonym":"ab","provider":"fcm"}"#
        )
        .is_err());
    }
}
