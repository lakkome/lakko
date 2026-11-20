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

/// The platform a token belongs to. Its byte is the
/// `lakko_contacts::push_wake::PushProvider` byte a sealed subscription
/// carries (the former `push_tokens.provider` column values, ADR-0078).
/// Wire-format-stable: never renumber.
///
/// ## Two token classes, not five platforms
///
/// Four of the five variants are platforms. [`ProviderKind::ApnsVoip`]
/// is not: it is a **second token for a device that already holds an
/// [`ProviderKind::Apns`] one**, issued by a different system facility
/// (`PKPushRegistry` rather than `registerForRemoteNotifications`),
/// addressed with a different topic and a different `apns-push-type`,
/// and signed by a different credential. Neither token can deliver the
/// other's pushes, so they are two rows of one device rather than one
/// row overwritten.
///
/// That split is what makes a terminated iPhone ring at all: only an
/// `apns-push-type: voip` push may launch a terminated app and let it
/// report a call to CallKit before its handler returns
/// (`docs/ux/calls.md` section 2.1).
///
/// Which class a given wake reaches is decided by
/// [`crate::notify::delivers_to`]; the rule is written out in that
/// module's docs.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderKind {
    /// Apple Push Notification service (iOS / iPadOS / macOS / watchOS).
    /// The **alert** token — mutable-alert message wakes, decrypted by
    /// the notification-service extension.
    Apns,
    /// Firebase Cloud Messaging (Android with Google services).
    Fcm,
    /// Web Push (RFC 8030 + RFC 8291), VAPID-authenticated.
    WebPush,
    /// UnifiedPush (self-hosted, for CN / RU and de-Googled Android).
    UnifiedPush,
    /// Apple PushKit — the **call** token of an iOS device.
    ///
    /// Registered from `PKPushRegistry`, delivered to with
    /// `apns-push-type: voip` on the `<bundle id>.voip` topic under a
    /// credential of its own. Only ever carries a [`WakeKind::Call`]
    /// wake: Apple terminates an app that takes a VoIP push without
    /// reporting a call, so sending a message wake here would be a
    /// crash, not a mis-styled notification.
    ApnsVoip,
}

impl ProviderKind {
    /// The byte a sealed subscription carries for this provider.
    ///
    /// **Never renumber.** A new kind takes the next free integer and
    /// nothing else moves.
    #[cfg(test)]
    pub(crate) const fn as_db(self) -> u8 {
        match self {
            ProviderKind::Apns => 1,
            ProviderKind::Fcm => 2,
            ProviderKind::WebPush => 3,
            ProviderKind::UnifiedPush => 4,
            ProviderKind::ApnsVoip => 5,
        }
    }

    /// Parse a provider byte from an opened sealed subscription.
    pub(crate) const fn from_db(value: u8) -> Option<Self> {
        match value {
            1 => Some(ProviderKind::Apns),
            2 => Some(ProviderKind::Fcm),
            3 => Some(ProviderKind::WebPush),
            4 => Some(ProviderKind::UnifiedPush),
            5 => Some(ProviderKind::ApnsVoip),
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
            ProviderKind::ApnsVoip => "apns_voip",
        }
    }

    /// `true` for a token that exists only to make a device ring.
    ///
    /// A call token is never an alternative route for a message wake —
    /// see [`crate::notify::delivers_to`].
    pub(crate) const fn is_call_token(self) -> bool {
        matches!(self, ProviderKind::ApnsVoip)
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
    /// The 16-byte id of the subscription row this delivery comes from
    /// (ADR-0078). The receiving device handles the wake only when it
    /// holds that subscription for the `push_gid`; it replaces the device
    /// pseudonym, which no longer exists.
    pub(crate) subscription_id: [u8; 16],
    /// Which group's queue advanced (its ADR-0042 `push_gid`). Lets the
    /// client jump straight to the right journal cursor instead of polling
    /// all of its groups. `None` only in provider unit tests.
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
    /// the bytes a sealed subscription carried for it (ADR-0078) — the
    /// provider knows how to interpret them.
    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError>;

    /// `true` for a stand-in that contacts no remote service
    /// ([`LogOnlyProvider`]), so its deliveries are counted as `log_only`
    /// rather than `ok` and are not timed (ADR-0096).
    fn is_log_only(&self) -> bool {
        false
    }
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

    fn is_log_only(&self) -> bool {
        true
    }

    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError> {
        // ADR-0078: no token, subscription id, push_gid or prefix of one in
        // a log line - only what kind of delivery would have happened.
        tracing::info!(
            provider = self.kind.as_str(),
            wake_kind = payload.kind.as_str(),
            token_len = token.len(),
            has_group = payload.group_pseudonym.is_some(),
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
pub(crate) mod tests {
    use super::*;

    /// Every kind, so a new variant has to be added here rather than
    /// quietly skipping the round-trip and wire-name assertions.
    pub(crate) const ALL_KINDS: [ProviderKind; 5] = [
        ProviderKind::Apns,
        ProviderKind::Fcm,
        ProviderKind::WebPush,
        ProviderKind::UnifiedPush,
        ProviderKind::ApnsVoip,
    ];

    #[test]
    fn provider_kind_db_values_are_stable() {
        // The provider bytes are frozen in every sealed subscription.
        // Renumbering any of them re-points every stored row at the
        // wrong platform, so they are pinned literally.
        assert_eq!(ProviderKind::Apns.as_db(), 1);
        assert_eq!(ProviderKind::Fcm.as_db(), 2);
        assert_eq!(ProviderKind::WebPush.as_db(), 3);
        assert_eq!(ProviderKind::UnifiedPush.as_db(), 4);
        // The call token took the next free integer and moved nothing.
        assert_eq!(ProviderKind::ApnsVoip.as_db(), 5);
        for kind in ALL_KINDS {
            assert_eq!(ProviderKind::from_db(kind.as_db()), Some(kind));
        }
        assert_eq!(ProviderKind::from_db(0), None);
        assert_eq!(ProviderKind::from_db(6), None);
        assert_eq!(ProviderKind::from_db(99), None);
    }

    #[test]
    fn only_the_voip_token_is_a_call_token() {
        for kind in ALL_KINDS {
            assert_eq!(
                kind.is_call_token(),
                kind == ProviderKind::ApnsVoip,
                "{} misclassified",
                kind.as_str(),
            );
        }
    }

    #[test]
    fn provider_wire_names_are_the_ones_the_clients_send() {
        // These strings are hand-mirrored in `PushClient.Provider`
        // (iOS), `PushClient.kt` (Android) and `push/client.ts` (web).
        // A rename here is a silent 400 on every register from a
        // client that was not rebuilt.
        for (kind, wire) in [
            (ProviderKind::Apns, "apns"),
            (ProviderKind::Fcm, "fcm"),
            (ProviderKind::WebPush, "web_push"),
            (ProviderKind::UnifiedPush, "unified_push"),
            (ProviderKind::ApnsVoip, "apns_voip"),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{wire}\""));
            assert_eq!(
                serde_json::from_str::<ProviderKind>(&format!("\"{wire}\"")).unwrap(),
                kind,
            );
        }
    }

    #[tokio::test]
    async fn log_only_provider_always_succeeds() {
        let p = LogOnlyProvider {
            kind: ProviderKind::Apns,
        };
        let payload = WakePayload {
            subscription_id: [0xAB; 16],
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
            subscription_id: [0; 16],
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
            subscription_id: [0; 16],
            group_pseudonym: None,
            envelope_pointer: None,
            kind: WakeKind::Message,
        };
        let err = p.deliver(b"x", &payload).await.unwrap_err();
        assert!(matches!(err, DeliverError::Transport(_)));
    }
}
