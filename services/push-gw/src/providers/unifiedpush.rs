// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UnifiedPush provider — plain POST of the wake-payload to a
//! subscriber-chosen distributor endpoint.
//!
//! [UnifiedPush](https://unifiedpush.org/) is a vendor-neutral push
//! protocol used by de-Googled Android (LineageOS, /e/, GrapheneOS)
//! and increasingly by EU-privacy-conscious mainline Android. The
//! subscriber picks a "distributor" app (NextPush, ntfy, gotify, …),
//! which gives the application an HTTPS endpoint URL. The application
//! POSTs raw bytes to that URL; the distributor pushes them to the
//! device over its own transport (typically a long-lived TCP/QUIC
//! tunnel).
//!
//! ## Wire protocol
//!
//! From the application's side it's just:
//!
//! ```text
//! POST <subscriber_endpoint>
//! Content-Type: application/octet-stream
//! TTL: <seconds>
//!
//! <raw bytes — opaque to the distributor>
//! ```
//!
//! There is **no application-level authentication** — the endpoint
//! URL itself is the capability. Leaking it = anyone can spam the
//! subscriber's device. UnifiedPush distributors enforce per-endpoint
//! rate limits to bound that damage; clients rotate endpoints
//! periodically if needed.
//!
//! ## Status mapping
//!
//! - `200` / `201` / `202` — accepted. We treat the union as success
//!   because different distributors disagree on which "accepted"
//!   code to return.
//! - `404` / `410` — the endpoint has been revoked by the
//!   distributor (subscriber uninstalled the app, distributor
//!   evicted the registration). Surface as
//!   [`DeliverError::TokenInvalid`] so the registry cleans the row.
//! - `429` — rate-limited; transient. Phase-2 retry queue will back
//!   off.
//! - Everything else — [`DeliverError::Transport`] with the body for
//!   diagnostics.
//!
//! ## Env-gating
//!
//! UnifiedPush has no shared credentials, so it's always available
//! — the resolver always wires this provider, no env-var probe.

use async_trait::async_trait;
use reqwest::StatusCode;

use crate::provider::{DeliverError, Provider, ProviderKind, WakeKind, WakePayload};

/// UnifiedPush wake-payload TTL in seconds. The distributor is
/// allowed to drop the message if it can't deliver within the TTL —
/// 4 hours is generous enough for typical battery-saver
/// throttling windows but short enough that an offline device
/// doesn't accumulate a backlog when it comes back online days
/// later (the Lakko client will catch up via the DS journal in any
/// case).
const TTL_SECONDS: u32 = 4 * 60 * 60;

/// TTL for a call wake. UnifiedPush has no priority knob — the TTL is
/// the only delivery lever a distributor honours — so a call gets the
/// same one-minute window the other three providers use, which at
/// least stops a queued wake ringing a phone after the caller gave
/// up. A de-Googled Android device must be able to ring like any
/// other, so the kind tag is carried here too.
const CALL_TTL_SECONDS: u32 = 60;

/// UnifiedPush provider. Stateless — the subscriber's endpoint URL
/// (stored in `push_tokens.token` as UTF-8 bytes) is the only
/// per-device routing information.
pub(crate) struct UnifiedPushProvider {
    http: reqwest::Client,
}

impl UnifiedPushProvider {
    pub(crate) fn new() -> Self {
        Self {
            http: super::shared_http_client(),
        }
    }
}

/// How long a distributor may keep retrying this wake.
const fn ttl_seconds_for(kind: WakeKind) -> u32 {
    if kind.is_call() {
        CALL_TTL_SECONDS
    } else {
        TTL_SECONDS
    }
}

#[async_trait]
impl Provider for UnifiedPushProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::UnifiedPush
    }

    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError> {
        let endpoint = std::str::from_utf8(token)
            .map_err(|_| DeliverError::Transport("token is not valid UTF-8".to_string()))?;
        if !endpoint.starts_with("https://") {
            return Err(DeliverError::Transport(format!(
                "endpoint must be https://, got {}",
                &endpoint[..endpoint.len().min(16)],
            )));
        }
        // Wake payload is a tiny JSON envelope — the recipient's
        // client decodes it inside the in-process `lakko-push`
        // module, fetches ciphertext from the DS, and builds the
        // OS-level notification on-device. Distributors never see
        // plaintext.
        let body = serde_json::to_vec(&wire::WakeBody::from(payload))
            .map_err(|e| DeliverError::Transport(format!("serialize wake body: {e}")))?;

        let resp = match self
            .http
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .header("TTL", ttl_seconds_for(payload.kind).to_string())
            .body(body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(DeliverError::Transport(e.to_string())),
        };

        match resp.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::ACCEPTED => Ok(()),
            StatusCode::NOT_FOUND | StatusCode::GONE => Err(DeliverError::TokenInvalid),
            other => {
                let body = resp.text().await.unwrap_or_default();
                let truncated = if body.len() > 256 {
                    format!("{}…", &body[..256])
                } else {
                    body
                };
                Err(DeliverError::Transport(format!(
                    "UnifiedPush {} → {}: {}",
                    other.as_u16(),
                    other.canonical_reason().unwrap_or("?"),
                    truncated,
                )))
            },
        }
    }
}

/// Internal wire-shape helpers — kept private so the trait surface
/// stays decoupled from the JSON we happen to send today. If the
/// `lakko-push` crate's wake envelope ever migrates to CBOR or a
/// custom binary format, only this module changes.
mod wire {
    use serde::Serialize;

    use crate::provider::WakePayload;

    /// Body shape mirrors `lakko-push`'s `Wakeup` JSON envelope.
    /// Distributors and recipient devices both parse it as opaque
    /// bytes; the receiver decodes via the same crate.
    #[derive(Serialize)]
    pub(super) struct WakeBody {
        /// Schema-version byte. Legacy clients drop unknown
        /// versions and fall back to long-poll catch-up.
        pub(super) v: u8,
        /// Hex-encoded device pseudonym — the receiver matches
        /// against its own known pseudonym to detect mis-routes.
        pub(super) d: String,
        /// Optional hex-encoded group pseudonym — lets the client
        /// jump straight to the right journal cursor.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(super) g: Option<String>,
        /// Optional envelope pointer — the DS cursor past which the
        /// client should fetch.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(super) p: Option<u64>,
        /// Wake kind tag, mirroring `lakko-push`'s `k` field: `"c"`
        /// for a call, absent for a message. Lets the woken client
        /// ring instead of rendering a message preview.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(super) k: Option<&'static str>,
    }

    impl From<&WakePayload> for WakeBody {
        fn from(p: &WakePayload) -> Self {
            Self {
                v: 1,
                d: hex::encode(p.device_pseudonym),
                g: p.group_pseudonym.map(hex::encode),
                p: p.envelope_pointer,
                k: p.kind.wire_tag(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_https_endpoint() {
        let provider = UnifiedPushProvider::new();
        let payload = WakePayload {
            device_pseudonym: [0; 32],
            group_pseudonym: None,
            envelope_pointer: None,
            kind: WakeKind::Message,
        };
        let err = provider
            .deliver(b"http://insecure.example.com/upx", &payload)
            .await
            .unwrap_err();
        let DeliverError::Transport(msg) = err else {
            panic!("expected Transport, got {err:?}");
        };
        assert!(msg.contains("https://"));
    }

    #[tokio::test]
    async fn rejects_non_utf8_token() {
        let provider = UnifiedPushProvider::new();
        let payload = WakePayload {
            device_pseudonym: [0; 32],
            group_pseudonym: None,
            envelope_pointer: None,
            kind: WakeKind::Message,
        };
        // Invalid UTF-8 (lone 0xC0 byte).
        let err = provider.deliver(&[0xC0], &payload).await.unwrap_err();
        let DeliverError::Transport(msg) = err else {
            panic!("expected Transport, got {err:?}");
        };
        assert!(msg.contains("UTF-8"));
    }

    #[test]
    fn a_call_wake_shortens_the_ttl_and_carries_the_kind_tag() {
        assert_eq!(ttl_seconds_for(super::WakeKind::Message), 4 * 60 * 60);
        assert_eq!(ttl_seconds_for(super::WakeKind::Call), 60);

        let body = |kind| {
            serde_json::to_string(&wire::WakeBody::from(&WakePayload {
                device_pseudonym: [0x03; 32],
                group_pseudonym: Some([0x04; 32]),
                envelope_pointer: Some(11),
                kind,
            }))
            .unwrap()
        };
        let message = body(super::WakeKind::Message);
        let call = body(super::WakeKind::Call);
        assert!(!message.contains(r#""k""#), "got {message}");
        assert!(call.contains(r#""k":"c""#), "got {call}");
        assert_eq!(call.replace(r#","k":"c""#, ""), message);
    }
}
