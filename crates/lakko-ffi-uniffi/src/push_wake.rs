// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for sealed wakes and sealed push subscriptions
//! ([ADR-0078] section 3). Mirror of `crates/lakko-ffi-wasm/src/push.rs`'s
//! ADR-0078 half.
//!
//! - [`push_wake_header_value`] - what a member publishing to a group puts
//!   in `x-lakko-wake`: a wake sealed to push-gw's key, signed by the
//!   group's wake key, carrying the group's unlock key. Built from the
//!   group's push routing secret, which never leaves this call.
//! - [`seal_push_subscription`] - the device's provider tokens sealed under
//!   the group's unlock key, as the row `sub_id`; what `POST /push/subscribe`
//!   takes beside [`derive_push_wake_public_key`] and the `push_gid`.
//! - [`derive_push_wake_public_key`] / [`derive_push_wake_unlock_key`] - the
//!   two derivations, exposed so every platform pins the same goldens. The
//!   unlock key is secret: no client needs it outside a test.
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_contacts::push_routing::{
    derive_push_wake_public_key as core_wake_pk, derive_push_wake_unlock_key as core_unlock,
};
use lakko_contacts::push_wake::{
    self as core, PushProvider, PushTokenEntry, PushWakeError, PushWakeKind,
    PUSH_SUBSCRIPTION_ID_LEN,
};

use crate::FfiError;

/// The request header a sealed wake rides in.
const WAKE_HEADER: &str = "x-lakko-wake";

fn wake_err(e: PushWakeError) -> FfiError {
    FfiError::PushWake(format!("{}: {e}", e.code()))
}

fn key32(bytes: &[u8], what: &str) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        FfiError::PushWake(format!(
            "keyLength: {what} must be 32 bytes, got {}",
            bytes.len()
        ))
    })
}

/// What a wake tells the woken devices about (ADR-0078).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiPushWakeKind {
    /// An ordinary message.
    Message,
    /// A call signal: the woken device rings.
    Call,
}

impl From<FfiPushWakeKind> for PushWakeKind {
    fn from(k: FfiPushWakeKind) -> Self {
        match k {
            FfiPushWakeKind::Message => Self::Message,
            FfiPushWakeKind::Call => Self::Call,
        }
    }
}

/// The push provider a token belongs to.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiPushProvider {
    /// APNs, the alert token.
    Apns,
    /// Firebase Cloud Messaging.
    Fcm,
    /// Web Push.
    WebPush,
    /// UnifiedPush.
    UnifiedPush,
    /// APNs PushKit, the call token.
    ApnsVoip,
}

impl From<FfiPushProvider> for PushProvider {
    fn from(p: FfiPushProvider) -> Self {
        match p {
            FfiPushProvider::Apns => Self::Apns,
            FfiPushProvider::Fcm => Self::Fcm,
            FfiPushProvider::WebPush => Self::WebPush,
            FfiPushProvider::UnifiedPush => Self::UnifiedPush,
            FfiPushProvider::ApnsVoip => Self::ApnsVoip,
        }
    }
}

/// One provider token of this device.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiPushTokenEntry {
    /// Which provider the token is for.
    pub provider: FfiPushProvider,
    /// The provider-opaque token bytes.
    pub token: Vec<u8>,
}

/// The 32-byte public half of the group's wake signing key: what a
/// subscription registers at push-gw (`wake_pk_b64`).
///
/// # Errors
///
/// [`FfiError::PushWake`] `keyLength`.
#[uniffi::export]
pub fn derive_push_wake_public_key(secret: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(core_wake_pk(&key32(&secret, "push routing secret")?).to_vec())
}

/// The 32-byte unlock key of the group. Secret: exposed for the golden tests
/// only; [`seal_push_subscription`] and [`push_wake_header_value`] derive it
/// themselves.
///
/// # Errors
///
/// [`FfiError::PushWake`] `keyLength`.
#[uniffi::export]
pub fn derive_push_wake_unlock_key(secret: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(core_unlock(&key32(&secret, "push routing secret")?)
        .as_bytes()
        .to_vec())
}

/// The name of the header a sealed wake rides in.
#[uniffi::export]
pub fn push_wake_header_name() -> String {
    WAKE_HEADER.to_string()
}

/// Length of a subscription id (a device keeps one per subscription).
#[uniffi::export]
pub fn push_subscription_id_len() -> u32 {
    // 16, a constant.
    #[allow(clippy::cast_possible_truncation)]
    let len = PUSH_SUBSCRIPTION_ID_LEN as u32;
    len
}

/// The sealed wake bytes for the group whose routing secret is `secret`,
/// sealed to push-gw's 32-byte X25519 key `push_gw_key`. `timestamp_ms` is
/// this device's clock.
///
/// # Errors
///
/// [`FfiError::PushWake`] `keyLength`.
#[uniffi::export]
pub fn seal_push_wake(
    push_gw_key: Vec<u8>,
    secret: Vec<u8>,
    kind: FfiPushWakeKind,
    timestamp_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    core::seal_push_wake(
        &key32(&push_gw_key, "push-gw key")?,
        &key32(&secret, "push routing secret")?,
        kind.into(),
        timestamp_ms,
    )
    .map_err(wake_err)
}

/// The `x-lakko-wake` header value (base64url without padding of
/// [`seal_push_wake`]).
///
/// # Errors
///
/// As [`seal_push_wake`].
#[uniffi::export]
pub fn push_wake_header_value(
    push_gw_key: Vec<u8>,
    secret: Vec<u8>,
    kind: FfiPushWakeKind,
    timestamp_ms: u64,
) -> Result<String, FfiError> {
    Ok(URL_SAFE_NO_PAD.encode(seal_push_wake(push_gw_key, secret, kind, timestamp_ms)?))
}

/// The device's provider tokens sealed as the subscription row `sub_id` of
/// the group whose routing secret is `secret`.
///
/// # Errors
///
/// [`FfiError::PushWake`] `keyLength`, `subIdLength`, or `badTokens` (no
/// entry, a repeated provider, an empty or over-long token).
#[uniffi::export]
pub fn seal_push_subscription(
    secret: Vec<u8>,
    sub_id: Vec<u8>,
    entries: Vec<FfiPushTokenEntry>,
) -> Result<Vec<u8>, FfiError> {
    let sub_id: [u8; PUSH_SUBSCRIPTION_ID_LEN] = sub_id.as_slice().try_into().map_err(|_| {
        FfiError::PushWake(format!(
            "subIdLength: a subscription id must be {PUSH_SUBSCRIPTION_ID_LEN} bytes, got {}",
            sub_id.len()
        ))
    })?;
    let entries: Vec<PushTokenEntry> = entries
        .into_iter()
        .map(|e| PushTokenEntry {
            provider: e.provider.into(),
            token: e.token,
        })
        .collect();
    core::seal_push_subscription_for_secret(
        &key32(&secret, "push routing secret")?,
        &sub_id,
        &entries,
    )
    .map_err(wake_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::push_wake::{open_push_subscription, open_push_wake, PushGwKey};

    const SECRET: [u8; 32] = [0x42; 32];

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    #[test]
    fn the_derivations_match_the_core_goldens() {
        assert_eq!(
            hex(&derive_push_wake_public_key(SECRET.to_vec()).unwrap()),
            "2153e575c8abc4a91af2b70258982068480edc0e0451f6b6bdf6c0692e5c8e56"
        );
        assert_eq!(
            hex(&derive_push_wake_unlock_key(SECRET.to_vec()).unwrap()),
            "d76a3ce72be511a512c6afce89447fa73d9595c5b0fe0942780137a7d1f4cd0a"
        );
        assert!(derive_push_wake_public_key(vec![0; 31]).is_err());
    }

    #[test]
    fn a_header_value_opens_at_push_gw() {
        let gw = PushGwKey::generate();
        let header = push_wake_header_value(
            gw.public().to_vec(),
            SECRET.to_vec(),
            FfiPushWakeKind::Call,
            9,
        )
        .unwrap();
        assert_eq!(header.len(), core::PUSH_WAKE_HEADER_LEN);
        let wire = URL_SAFE_NO_PAD.decode(header).unwrap();
        let opened = open_push_wake(&gw, &wire).unwrap();
        assert_eq!(opened.kind, PushWakeKind::Call);
        assert_eq!(opened.timestamp_ms, 9);
        assert_eq!(push_wake_header_name(), "x-lakko-wake");
    }

    #[test]
    fn a_sealed_subscription_opens_under_the_unlock_key() {
        let sub = vec![3u8; 16];
        let wire = seal_push_subscription(
            SECRET.to_vec(),
            sub.clone(),
            vec![FfiPushTokenEntry {
                provider: FfiPushProvider::Fcm,
                token: b"tok".to_vec(),
            }],
        )
        .unwrap();
        let unlock: [u8; 32] = derive_push_wake_unlock_key(SECRET.to_vec())
            .unwrap()
            .try_into()
            .unwrap();
        let entries = open_push_subscription(
            &unlock,
            &lakko_contacts::derive_push_gid(&SECRET),
            &[3u8; 16],
            &wire,
        )
        .unwrap();
        assert_eq!(entries[0].provider, PushProvider::Fcm);
        assert!(seal_push_subscription(SECRET.to_vec(), vec![1; 15], vec![]).is_err());
        let err = seal_push_subscription(SECRET.to_vec(), sub, vec![]).unwrap_err();
        assert!(err.to_string().starts_with("badTokens:"));
        assert_eq!(push_subscription_id_len(), 16);
    }
}
