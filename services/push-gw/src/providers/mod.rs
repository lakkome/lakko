// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Real push provider implementations (Phase 1.6b).
//!
//! Each sub-module implements the [`Provider`](crate::provider::Provider)
//! trait against one platform's wire protocol:
//!
//! - [`apns`] — Apple Push Notification service (iOS / iPadOS / macOS /
//!   watchOS). ES256-signed JWT provider tokens, HTTP/2 POST to
//!   `api.push.apple.com`. Silent-push (`content-available: 1`) only —
//!   the recipient client builds the user-visible notification after
//!   decrypting locally.
//! - [`fcm`] — Firebase Cloud Messaging (Android with Google Play
//!   Services). Service-account OAuth2 token exchange via Google's
//!   token endpoint, then HTTP v1 send (`/v1/projects/<pid>/messages:send`).
//! - [`webpush`] — Web Push / RFC 8030 + RFC 8291 with VAPID
//!   (RFC 8292). Per-request ECDH(P-256) with the subscriber's
//!   `p256dh` key → HKDF-SHA256 → AES-128-GCM payload encryption with
//!   the `aes128gcm` content-encoding (RFC 8188).
//! - [`unifiedpush`] — UnifiedPush / NextPush / ntfy. Plain POST of
//!   the wake-payload to the subscriber-chosen distributor endpoint.
//!   No auth at the protocol layer — the endpoint URL itself is the
//!   capability.
//!
//! All four providers share one [`reqwest::Client`] built by
//! [`shared_http_client`] so the HTTP/2 connection pool + DNS cache
//! are reused across send sites. Each provider also handles its own
//! token-cache lifecycle internally (APNs JWT refresh ≤ 1 h, FCM
//! OAuth2 access token refresh < 1 h, Web Push VAPID JWT per-request
//! signed on the fly).
//!
//! ## Env-gating
//!
//! Every real provider is constructed only when ALL of its required
//! env vars resolve to a usable value. Missing or malformed config
//! falls back to [`crate::provider::LogOnlyProvider`] for that kind,
//! with a `tracing::warn!` line so operators see the gap on startup.
//! This keeps the service launchable in dev without any of the four
//! external accounts configured.

pub(crate) mod apns;
pub(crate) mod fcm;
pub(crate) mod unifiedpush;
pub(crate) mod webpush;

use std::time::Duration;

/// Build the shared `reqwest::Client` every real provider uses.
///
/// Pinned settings:
///
/// - 10-second connect timeout — provider DNS + TLS shouldn't take
///   longer; we'd rather surface a transient `Transport` error than
///   block the dispatch task.
/// - 30-second per-request timeout — covers the full HTTP/2 round-trip
///   to APNs / FCM / a slow Web-Push subscriber. We don't keep retry
///   state at this layer (Phase 2 will), so a long-hanging request
///   only loses one wake-up.
/// - HTTP/2 preferred (the `reqwest` http2 feature is on); HTTP/1.1
///   stays available as a fallback for misbehaving subscribers.
/// - rustls (no system OpenSSL) — same root-store choice as
///   `lakko-transport::http`.
pub(crate) fn shared_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("lakko-svc-push-gw/", env!("CARGO_PKG_VERSION"),))
        .build()
        .expect("reqwest::Client build with workspace-pinned features")
}
