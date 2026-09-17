// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Push provider abstraction.
//!
//! Each platform (APNs, FCM, Web Push, UnifiedPush) speaks a different
//! protocol but the high-level operation is the same: "deliver this
//! tiny opaque payload to the device that owns this token". The
//! [`Provider`] trait is the single point all dispatch goes through;
//! adding a real APNs / FCM / Web Push impl is a Phase 1.6b drop-in
//! once developer accounts and signing keys are in place.
//!
//! ## Phase 1.6 scaffolding
//!
//! Real platform integrations require:
//!
//!   * Apple Developer account + .p8 key + team / key / topic ids
//!   * Google Cloud project + service account JSON
//!   * VAPID keypair for Web Push (RFC 8292)
//!   * UnifiedPush self-host setup (NextPush / ntfy)
//!
//! All four are operational tasks, not code. So Phase 1.6 ships:
//!
//!   * The full registry + dispatch wiring
//!   * One real implementation: [`LogOnlyProvider`], which records the
//!     intent into `tracing` instead of contacting a remote service.
//!     Used in dev and tests, and in production until real keys land.
//!
//! Phase 1.6b will add `ApnsProvider`, `FcmProvider`, `WebPushProvider`
//! alongside `LogOnlyProvider` — chosen at startup based on env
//! configuration. No registry / handler changes needed.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Mapping between the integer column value in `push_tokens.provider`
/// and the platform name. Wire-format-stable: never renumber.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderKind {
    /// Apple Push Notification service (iOS / iPadOS / macOS / watchOS).
    Apns,
    /// Firebase Cloud Messaging (Android with Google services).
    Fcm,
    /// Web Push (RFC 8030 + RFC 8291), VAPID-authenticated.
    WebPush,
    /// UnifiedPush (self-hosted, for CN / RU and de-Googled Android).
    UnifiedPush,
}

impl ProviderKind {
    pub(crate) const fn as_db(self) -> u8 {
        match self {
            ProviderKind::Apns => 1,
            ProviderKind::Fcm => 2,
            ProviderKind::WebPush => 3,
            ProviderKind::UnifiedPush => 4,
        }
    }

    pub(crate) const fn from_db(value: u8) -> Option<Self> {
        match value {
            1 => Some(ProviderKind::Apns),
            2 => Some(ProviderKind::Fcm),
            3 => Some(ProviderKind::WebPush),
            4 => Some(ProviderKind::UnifiedPush),
            _ => None,
        }
    }

    /// Lowercase canonical name for log lines.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Apns => "apns",
            ProviderKind::Fcm => "fcm",
            ProviderKind::WebPush => "webpush",
            ProviderKind::UnifiedPush => "unifiedpush",
        }
    }
}

/// What kind of thing the woken device is being told about.
///
/// A call offer travels as an ordinary MLS application message on the
/// chat group's DS journal, so at the pointer level a call wake and a
/// message wake are byte-identical. This tag is the only thing that
/// separates them, and it exists for two reasons that both need it
/// *before* any ciphertext is fetched:
///
///   * **Delivery.** A call is worth spending battery on. Each
///     provider has its own knob (APNs `apns-priority: 10` + a short
///     `apns-expiration`, FCM `android.priority: "high"`, Web Push
///     `Urgency: high`) that stops the wake being delayed, batched or
///     coalesced the way a message wake legitimately may be.
///   * **Behaviour.** The woken background handler must report the
///     call to the OS within its budget — the iOS system call screen,
///     the Android full-screen call notification — rather than render
///     a message preview. It cannot decide that from a cursor.
///
/// ## Disclosure
///
/// The tag is visible to Apple / Google / Mozilla. They learn exactly
/// one extra bit: *this wake is a call*. They do **not** learn who is
/// calling, the call id, or whether it is voice or video — the wake
/// still carries only a pseudonym and a cursor, and every call detail
/// stays inside the E2EE journal row the device fetches after waking
/// (invariants #2 and #5). The bit is unavoidable: the raised
/// priority and shortened expiry a ringing call needs are themselves
/// visible to the provider, so hiding the tag would buy nothing.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WakeKind {
    /// An ordinary message wake. The default, and byte-for-byte the
    /// behaviour that shipped before calls existed.
    #[default]
    Message,
    /// A call signal (offer / accept / decline / hang-up).
    Call,
}

impl WakeKind {
    /// `true` for a call wake. Reads better than a `matches!` at each
    /// of the four provider call sites.
    pub(crate) const fn is_call(self) -> bool {
        matches!(self, WakeKind::Call)
    }

    /// Single-letter tag placed in the delivered payload body,
    /// mirroring `lakko-push`'s `k` field so a client that decodes
    /// either shape branches on the same letter.
    ///
    /// `None` for a message: omitting the field keeps today's
    /// message payload byte-identical, so a client built before calls
    /// existed sees no change at all, and "no `k`" means "message".
    pub(crate) const fn wire_tag(self) -> Option<&'static str> {
        match self {
            WakeKind::Message => None,
            WakeKind::Call => Some("c"),
        }
    }

    /// Lowercase canonical name for log lines.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            WakeKind::Message => "message",
            WakeKind::Call => "call",
        }
    }
}

/// What we ask a provider to deliver. Strictly metadata pointers — no
/// plaintext, no preview. The recipient device wakes up, fetches
/// ciphertext from `services/ds`, decrypts locally and builds the
/// OS-level notification on-device.
#[derive(Clone, Debug)]
pub(crate) struct WakePayload {
    /// 32-byte device pseudonym — the addressee.
    pub(crate) device_pseudonym: [u8; 32],
    /// Optional: which group's queue advanced. Lets the client jump
    /// straight to the right journal cursor instead of polling all
    /// of its groups.
    pub(crate) group_pseudonym: Option<[u8; 32]>,
    /// Optional: server-assigned cursor id past which the client
    /// should fetch (`?after=<envelope_pointer>`).
    pub(crate) envelope_pointer: Option<u64>,
    /// Message or call. Drives per-provider urgency headers and the
    /// payload's kind tag; see [`WakeKind`] for what it discloses.
    pub(crate) kind: WakeKind,
}

/// Errors a provider can return. Most distinguishable failure modes
/// matter to the caller — `TokenInvalid` triggers a token cleanup,
/// other errors are transient.
///
/// Phase 1.6 only ships [`LogOnlyProvider`] which always succeeds, so
/// the failure variants are not constructed in the production binary
/// today. They're constructed by tests (see this module) and will be
/// constructed by the real `ApnsProvider` / `FcmProvider` in 1.6b.
#[derive(Error, Debug)]
#[allow(dead_code)] // see doc-comment above
pub(crate) enum DeliverError {
    /// The token was rejected by the provider — clean it up so we
    /// don't keep retrying. Examples: APNs `BadDeviceToken`, FCM
    /// `UNREGISTERED`.
    #[error("provider rejected the token as invalid")]
    TokenInvalid,
    /// Transient provider error. Caller may retry later (Phase 2 will
    /// add a job queue).
    #[error("provider transport error: {0}")]
    Transport(String),
}

/// One push provider. Implementations are constructed once at service
/// start and stored in `AppState`.
#[async_trait]
pub(crate) trait Provider: Send + Sync {
    /// Which `ProviderKind` this implementation handles. Used by tests
    /// to assert wiring; production routing goes through the
    /// `HashMap<ProviderKind, Box<dyn Provider>>` key directly.
    #[allow(dead_code)]
    fn kind(&self) -> ProviderKind;

    /// Deliver a wake-up to the device that owns `token`. `token` is
    /// the bytes we previously stored in `push_tokens.token` — the
    /// provider knows how to interpret them.
    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError>;
}

// ============================================================================
// LogOnlyProvider — no-op stub for dev / tests / pre-keys production
// ============================================================================

/// A provider that doesn't actually contact any remote service —
/// records the intent into `tracing` and returns success.
///
/// Used in:
///   * Unit tests (no live providers needed)
///   * Local dev (no Apple / Google accounts needed)
///   * Production until Phase 1.6b ships real provider impls
///
/// Production deployments configure which `ProviderKind` values map to
/// `LogOnlyProvider` via env (see `main.rs::resolve_providers`). When
/// a real `ApnsProvider` arrives, it replaces the LogOnly entry for
/// `ProviderKind::Apns` with no other code changes.
pub(crate) struct LogOnlyProvider {
    pub(crate) kind: ProviderKind,
}

#[async_trait]
impl Provider for LogOnlyProvider {
    fn kind(&self) -> ProviderKind {
        self.kind
    }

    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError> {
        // Never log the full token at info level — even though it's
        // not PII per se, treating it as sensitive is a good habit.
        // Length + first 4 bytes is enough for ops debugging.
        let token_prefix = hex::encode(&token[..token.len().min(4)]);
        tracing::info!(
            provider = self.kind.as_str(),
            wake_kind = payload.kind.as_str(),
            token_len = token.len(),
            token_prefix = %token_prefix,
            device_prefix = %hex::encode(&payload.device_pseudonym[..4]),
            has_group = payload.group_pseudonym.is_some(),
            envelope_pointer = ?payload.envelope_pointer,
            "[stub] would deliver wake-up",
        );
        Ok(())
    }
}

// ============================================================================
// Failing providers — used in tests for the dispatch error paths.
// `#[cfg(test)]`-gated so they don't bloat production binaries.
// ============================================================================

#[cfg(test)]
pub(crate) struct AlwaysInvalidProvider {
    pub(crate) kind: ProviderKind,
}

#[cfg(test)]
#[async_trait]
impl Provider for AlwaysInvalidProvider {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    async fn deliver(&self, _token: &[u8], _payload: &WakePayload) -> Result<(), DeliverError> {
        Err(DeliverError::TokenInvalid)
    }
}

#[cfg(test)]
pub(crate) struct AlwaysTransportFailProvider {
    pub(crate) kind: ProviderKind,
}

#[cfg(test)]
#[async_trait]
impl Provider for AlwaysTransportFailProvider {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    async fn deliver(&self, _token: &[u8], _payload: &WakePayload) -> Result<(), DeliverError> {
        Err(DeliverError::Transport(
            "simulated 500 from upstream".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_kind_db_values_are_stable() {
        assert_eq!(ProviderKind::Apns.as_db(), 1);
        assert_eq!(ProviderKind::Fcm.as_db(), 2);
        assert_eq!(ProviderKind::WebPush.as_db(), 3);
        assert_eq!(ProviderKind::UnifiedPush.as_db(), 4);
        for kind in [
            ProviderKind::Apns,
            ProviderKind::Fcm,
            ProviderKind::WebPush,
            ProviderKind::UnifiedPush,
        ] {
            assert_eq!(ProviderKind::from_db(kind.as_db()), Some(kind));
        }
        assert_eq!(ProviderKind::from_db(0), None);
        assert_eq!(ProviderKind::from_db(99), None);
    }

    #[tokio::test]
    async fn log_only_provider_always_succeeds() {
        let p = LogOnlyProvider {
            kind: ProviderKind::Apns,
        };
        let payload = WakePayload {
            device_pseudonym: [0xAB; 32],
            group_pseudonym: Some([0xCD; 32]),
            envelope_pointer: Some(42),
            kind: WakeKind::Message,
        };
        p.deliver(b"fake-apns-token", &payload).await.unwrap();
    }

    #[tokio::test]
    async fn always_invalid_provider_yields_token_invalid() {
        let p = AlwaysInvalidProvider {
            kind: ProviderKind::Fcm,
        };
        let payload = WakePayload {
            device_pseudonym: [0; 32],
            group_pseudonym: None,
            envelope_pointer: None,
            kind: WakeKind::Message,
        };
        let err = p.deliver(b"x", &payload).await.unwrap_err();
        assert!(matches!(err, DeliverError::TokenInvalid));
    }

    #[tokio::test]
    async fn always_transport_fail_provider_yields_transport_err() {
        let p = AlwaysTransportFailProvider {
            kind: ProviderKind::WebPush,
        };
        let payload = WakePayload {
            device_pseudonym: [0; 32],
            group_pseudonym: None,
            envelope_pointer: None,
            kind: WakeKind::Message,
        };
        let err = p.deliver(b"x", &payload).await.unwrap_err();
        assert!(matches!(err, DeliverError::Transport(_)));
    }
}
