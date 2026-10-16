// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for sealed wakes and sealed push subscriptions
//! ([ADR-0078] section 3). Mirror of `crates/lakko-ffi-uniffi/src/push_wake.rs`.
//!
//! - `pushWakeHeaderValue(pushGwKey, secret, kind, timestampMs)` - what a
//!   member publishing to a group puts in `x-lakko-wake`: a wake sealed to
//!   push-gw's key, signed by the group's wake key, carrying the group's
//!   unlock key. Built from the group's push routing secret, which never
//!   leaves this call.
//! - `sealPushSubscription(secret, subId, tokens)` - this device's provider
//!   tokens (a [`PushTokenList`]) sealed under the group's unlock key as the
//!   row `subId`; what `POST /push/subscribe` takes beside
//!   `derivePushWakePublicKey` and the `push_gid`.
//! - `derivePushWakePublicKey` / `derivePushWakeUnlockKey` - the two
//!   derivations, exposed so every platform pins the same goldens. The unlock
//!   key is secret: no client needs it outside a test.
//!
//! A refusal throws an `Error` whose `pushWakeError` own property is the
//! frozen code (`badTokens`, `keyLength`, `subIdLength`, `badProvider`, ...).
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_contacts::push_routing::{
    derive_push_wake_public_key as core_wake_pk, derive_push_wake_unlock_key as core_unlock,
};
use lakko_contacts::push_wake::{
    self as core, PushProvider, PushTokenEntry, PushWakeKind, PUSH_SUBSCRIPTION_ID_LEN,
};
use wasm_bindgen::prelude::*;

use crate::contacts::coded_error;

/// A refusal on its way to JS: the frozen code and a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    code: &'static str,
    message: String,
}

impl From<core::PushWakeError> for Refusal {
    fn from(e: core::PushWakeError) -> Self {
        Self {
            code: e.code(),
            message: e.to_string(),
        }
    }
}

fn js<T>(r: Result<T, Refusal>) -> Result<T, JsValue> {
    r.map_err(|e| coded_error(&e.message, "pushWakeError", e.code))
}

fn key32(bytes: &[u8], what: &str) -> Result<[u8; 32], Refusal> {
    <[u8; 32]>::try_from(bytes).map_err(|_| Refusal {
        code: "keyLength",
        message: format!("{what} must be 32 bytes, got {}", bytes.len()),
    })
}

/// What a wake tells the woken devices about. The numeric values are the
/// frozen wire bytes.
#[wasm_bindgen(js_name = PushWakeKind)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmPushWakeKind {
    /// An ordinary message.
    Message = 1,
    /// A call signal: the woken device rings.
    Call = 2,
}

impl From<WasmPushWakeKind> for PushWakeKind {
    fn from(k: WasmPushWakeKind) -> Self {
        match k {
            WasmPushWakeKind::Message => Self::Message,
            WasmPushWakeKind::Call => Self::Call,
        }
    }
}

/// This device's provider tokens, built one entry at a time.
#[wasm_bindgen]
#[derive(Default)]
pub struct PushTokenList {
    entries: Vec<PushTokenEntry>,
}

#[wasm_bindgen]
impl PushTokenList {
    /// An empty list.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the token of `provider` (`"apns"`, `"fcm"`, `"web_push"`,
    /// `"unified_push"`, `"apns_voip"`).
    ///
    /// # Errors
    ///
    /// `badProvider` for any other name.
    pub fn add(&mut self, provider: &str, token: &[u8]) -> Result<(), JsValue> {
        let provider = js(
            PushProvider::from_wire_name(provider).ok_or_else(|| Refusal {
                code: "badProvider",
                message: format!("unknown push provider {provider:?}"),
            }),
        )?;
        self.entries.push(PushTokenEntry {
            provider,
            token: token.to_vec(),
        });
        Ok(())
    }

    /// How many entries the list holds.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn length(&self) -> usize {
        self.entries.len()
    }
}

/// The 32-byte public half of the group's wake signing key.
///
/// # Errors
///
/// `keyLength`.
#[wasm_bindgen(js_name = derivePushWakePublicKey)]
pub fn derive_push_wake_public_key(secret: &[u8]) -> Result<Vec<u8>, JsValue> {
    js(key32(secret, "push routing secret").map(|s| core_wake_pk(&s).to_vec()))
}

/// The 32-byte unlock key of the group. Secret: exposed for the golden tests
/// only.
///
/// # Errors
///
/// `keyLength`.
#[wasm_bindgen(js_name = derivePushWakeUnlockKey)]
pub fn derive_push_wake_unlock_key(secret: &[u8]) -> Result<Vec<u8>, JsValue> {
    js(key32(secret, "push routing secret").map(|s| core_unlock(&s).as_bytes().to_vec()))
}

/// The name of the header a sealed wake rides in.
#[wasm_bindgen(js_name = pushWakeHeaderName)]
#[must_use]
pub fn push_wake_header_name() -> String {
    "x-lakko-wake".to_string()
}

/// Length of a subscription id.
#[wasm_bindgen(js_name = pushSubscriptionIdLen)]
#[must_use]
#[allow(clippy::missing_const_for_fn)] // wasm-bindgen cannot export a const fn
pub fn push_subscription_id_len() -> usize {
    PUSH_SUBSCRIPTION_ID_LEN
}

/// The `x-lakko-wake` header value: the wake for the group whose routing
/// secret is `secret`, sealed to push-gw's 32-byte X25519 key, as base64url
/// without padding. `timestamp_ms` is this device's clock.
///
/// # Errors
///
/// `keyLength`.
#[wasm_bindgen(js_name = pushWakeHeaderValue)]
pub fn push_wake_header_value(
    push_gw_key: &[u8],
    secret: &[u8],
    kind: WasmPushWakeKind,
    timestamp_ms: f64,
) -> Result<String, JsValue> {
    js((|| {
        let gw = key32(push_gw_key, "push-gw key")?;
        let secret = key32(secret, "push routing secret")?;
        // A JS number of milliseconds; clamped into u64 range.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ts = if timestamp_ms.is_finite() && timestamp_ms > 0.0 {
            timestamp_ms as u64
        } else {
            0
        };
        let wire = core::seal_push_wake(&gw, &secret, kind.into(), ts)?;
        Ok(URL_SAFE_NO_PAD.encode(wire))
    })())
}

/// This device's provider tokens sealed as the subscription row `sub_id` of
/// the group whose routing secret is `secret`.
///
/// # Errors
///
/// `keyLength`, `subIdLength`, `badTokens`.
#[wasm_bindgen(js_name = sealPushSubscription)]
pub fn seal_push_subscription(
    secret: &[u8],
    sub_id: &[u8],
    tokens: &PushTokenList,
) -> Result<Vec<u8>, JsValue> {
    js((|| {
        let secret = key32(secret, "push routing secret")?;
        let sub: [u8; PUSH_SUBSCRIPTION_ID_LEN] = sub_id.try_into().map_err(|_| Refusal {
            code: "subIdLength",
            message: format!(
                "a subscription id must be {PUSH_SUBSCRIPTION_ID_LEN} bytes, got {}",
                sub_id.len()
            ),
        })?;
        Ok(core::seal_push_subscription_for_secret(
            &secret,
            &sub,
            &tokens.entries,
        )?)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::push_wake::{open_push_wake, PushGwKey};

    #[test]
    fn the_derivations_match_the_core_goldens() {
        let pk = derive_push_wake_public_key(&[0x42; 32]).unwrap();
        let hex = pk.iter().fold(String::new(), |mut out, b| {
            use std::fmt::Write as _;
            let _ = write!(out, "{b:02x}");
            out
        });
        assert_eq!(
            hex,
            "2153e575c8abc4a91af2b70258982068480edc0e0451f6b6bdf6c0692e5c8e56"
        );
    }

    #[test]
    fn a_header_value_opens_at_push_gw() {
        let gw = PushGwKey::generate();
        let header =
            push_wake_header_value(&gw.public(), &[0x42; 32], WasmPushWakeKind::Message, 5.0)
                .unwrap();
        let opened = open_push_wake(&gw, &URL_SAFE_NO_PAD.decode(header).unwrap()).unwrap();
        assert_eq!(opened.timestamp_ms, 5);
        assert_eq!(opened.kind, PushWakeKind::Message);
    }

    #[test]
    fn a_subscription_seals() {
        let mut list = PushTokenList::new();
        list.add("web_push", b"{}").unwrap();
        assert_eq!(list.length(), 1);
        let wire = seal_push_subscription(&[0x42; 32], &[1; 16], &list).unwrap();
        assert!(core::is_well_formed_subscription_len(wire.len()));
    }
}
