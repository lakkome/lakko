// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! APNs provider — HTTP/2 mutable-alert push to Apple's `api.push.apple.com`.
//!
//! ## Wire protocol (Apple HTTP/2 token-based, 2016+)
//!
//! ```text
//! POST /3/device/<hex(device_token)>
//! Host: api.push.apple.com           # or sandbox.push.apple.com
//! Authorization: bearer <ES256 JWT>
//! apns-topic: me.lakko.dev           # bundle id
//! apns-push-type: alert              # mutable-alert push that fires the NSE
//! apns-priority: 5                    # power-aware
//! apns-expiration: 0                  # immediate, no store-and-forward
//! content-type: application/json
//!
//! {"aps":{"alert":{"title":"Lakko","body":"New message"},
//!         "mutable-content":1,"thread-id":"<g>"},
//!  "d":"<device pseudonym>","g":"<group pseudonym>","p":<cursor>}
//! ```
//!
//! ## Call wakes
//!
//! A [`WakeKind::Call`] wake swaps two headers and adds one payload
//! key; everything else — the topic, the push type, the metadata
//! shape — is unchanged:
//!
//! ```text
//! apns-priority: 10                   # deliver now, do not defer
//! apns-expiration: <now + 60>         # a call is worthless late
//!
//! {"aps":{…,"alert":{…,"body":"Incoming call"}}, …, "k":"c"}
//! ```
//!
//! `apns-expiration` is an **absolute** UNIX timestamp, not a TTL, so
//! the call value is computed per send. A minute is deliberately
//! shorter than the 45-second ring window plus slack: a wake that
//! arrives after the caller gave up would ring a phone for a call
//! nobody is on, which is worse than not ringing at all.
//!
//! ## The two flavours: `alert` and `voip`
//!
//! This module serves BOTH Apple token classes, because the wire
//! protocol, the ES256 JWT machinery and the status mapping are
//! identical for them. [`ApnsPushKind`] is the whole difference:
//!
//! | | `Alert` | `Voip` |
//! |---|---|---|
//! | Provider kind | [`ProviderKind::Apns`] | [`ProviderKind::ApnsVoip`] |
//! | `apns-push-type` | `alert` | `voip` |
//! | `apns-topic` | `<bundle id>` | `<bundle id>.voip` |
//! | Credential | `LAKKO_APNS_*` | `LAKKO_APNS_VOIP_*` |
//! | Body | `aps` alert the NSE rewrites | bare metadata, no `aps` |
//! | Accepts | any wake | a call wake **only** |
//!
//! The `voip` push type is what actually lets a **terminated** app be
//! launched and report a call to CallKit before its handler returns —
//! the alert path cannot do that, and it is the reason section 2.1 of
//! `docs/ux/calls.md` ("the phone rings even with the app closed") is
//! reachable at all. The alert flavour stays the correct type for
//! message wakes and remains the fallback for a device that registered
//! no call token.
//!
//! **The credential is genuinely separate.** Apple issues VoIP push
//! authority under its own artifact — historically a VoIP Services
//! certificate, and under token-based auth a `.p8` key whose associated
//! topic is `<bundle id>.voip`. Lakko is token-based throughout, so the
//! VoIP flavour reads a `.p8` of its own from `LAKKO_APNS_VOIP_*`
//! rather than borrowing the alert one: an operator who has only the
//! alert credential must not silently start sending VoIP pushes APNs
//! will reject, and one whose two credentials really are the same file
//! says so by pointing both variables at it.
//!
//! **A VoIP push carries no `aps` alert.** PushKit hands the whole
//! dictionary to the app and displays nothing, so an `alert` body there
//! would be dead weight; the payload is the same `s` / `g` / `p` / `k`
//! metadata the alert body carries, and nothing more.
//!
//! Lakko uses the **mutable-alert** model, NOT a silent `content-available`
//! background push: the body carries a generic placeholder alert plus
//! `mutable-content: 1`, which fires the recipient's
//! `UNNotificationServiceExtension` BEFORE display. The NSE fetches ciphertext
//! from `services/ds`, decrypts locally, and rewrites the visible
//! `UNNotificationContent` on-device — Apple never sees plaintext. If the NSE
//! is unavailable / times out, the user still sees the generic placeholder
//! alert ("New message" — the same fallback the foreground app uses).
//! `apns-push-type` MUST be `alert` for this: a `background` push must omit
//! the alert and carry `content-available: 1`, which would not fire the NSE
//! (and Apple may reject the alert+`background` combination outright). Wake
//! payload metadata (subscription id, `push_gid`, `envelope_pointer`) rides as custom
//! keys (`s`/`g`/`p`) alongside `aps` so the extension can decide which cursor
//! to fetch without parsing the whole DS journal.
//!
//! ## ES256 JWT cache
//!
//! Apple caps provider-token validity at 60 minutes and recommends
//! refresh in 20-60. We cache the signed JWT in
//! [`parking_lot::RwLock<Option<CachedJwt>>`] with a 50-minute
//! window — generous headroom against clock skew, single-writer
//! lock contention is negligible (tens of nanoseconds per send).
//!
//! Token signing uses the .p8 PKCS#8 private key Apple issues:
//!
//! - `kid` = `LAKKO_APNS_KEY_ID` (10-char Apple-assigned key id)
//! - `iss` = `LAKKO_APNS_TEAM_ID` (10-char Apple team id)
//! - `iat` = unix seconds at sign time
//!
//! ## Status mapping (Apple's APNS reason codes)
//!
//! - `200` — accepted by APNs.
//! - `400 BadDeviceToken` / `BadCollapseId` / `DeviceTokenNotForTopic`
//!   → [`DeliverError::TokenInvalid`] — registry drops the row.
//! - `403 InvalidProviderToken` / `MissingProviderToken` →
//!   [`DeliverError::Transport`] — usually a JWT staleness bug, the
//!   next send refreshes.
//! - `410 Unregistered` → [`DeliverError::TokenInvalid`].
//! - `429 TooManyRequests` → [`DeliverError::Transport`].
//! - Any other 4xx/5xx → [`DeliverError::Transport`] with the reason
//!   verbatim for diagnostics.

use async_trait::async_trait;
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use parking_lot::RwLock;
use reqwest::{header, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

use crate::provider::{DeliverError, Provider, ProviderKind, WakeKind, WakePayload};

/// JWT lifetime — sign new tokens 50 minutes apart so we always have
/// ≥ 10 minutes of margin against Apple's 60-minute ceiling.
const JWT_REFRESH_AFTER: Duration = Duration::from_secs(50 * 60);

/// How long a call wake stays worth delivering, in seconds past the
/// send. Shorter than the 45-second ring window plus slack, because a
/// call wake that arrives after the caller hung up rings a phone for
/// nothing.
const CALL_EXPIRATION_SECS: i64 = 60;

/// Apple endpoint flavour. The same .p8 key signs JWTs for either,
/// but the host differs: production for App Store builds, sandbox
/// for Xcode Debug-config + TestFlight-signed builds.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ApnsEndpoint {
    Production,
    Sandbox,
}

impl ApnsEndpoint {
    fn host(self) -> &'static str {
        match self {
            ApnsEndpoint::Production => "https://api.push.apple.com",
            ApnsEndpoint::Sandbox => "https://api.sandbox.push.apple.com",
        }
    }
}

/// Which Apple token class a provider instance serves. See the module
/// docs for the table of everything that differs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApnsPushKind {
    /// The alert token — mutable-alert message wakes, and the call
    /// fallback for a device holding no call token.
    Alert,
    /// The PushKit call token — `apns-push-type: voip`, call wakes only.
    Voip,
}

impl ApnsPushKind {
    /// Value of the `apns-push-type` header.
    const fn push_type(self) -> &'static str {
        match self {
            ApnsPushKind::Alert => "alert",
            ApnsPushKind::Voip => "voip",
        }
    }

    /// Which registry row this flavour delivers to.
    const fn provider_kind(self) -> ProviderKind {
        match self {
            ApnsPushKind::Alert => ProviderKind::Apns,
            ApnsPushKind::Voip => ProviderKind::ApnsVoip,
        }
    }

    /// Env-variable prefix the credential is read from.
    const fn env_prefix(self) -> &'static str {
        match self {
            ApnsPushKind::Alert => "LAKKO_APNS_",
            ApnsPushKind::Voip => "LAKKO_APNS_VOIP_",
        }
    }
}

/// Suffix APNs requires on the topic of a VoIP push. Apple addresses a
/// PushKit token under `<bundle id>.voip`; the plain bundle id is
/// rejected with `TopicDisallowed`, so a topic missing it is a
/// misconfiguration worth catching at startup rather than on the first
/// call nobody hears.
const VOIP_TOPIC_SUFFIX: &str = ".voip";

/// Env-loaded APNs config. Built once at service startup; returns
/// `None` if any required variable is missing — the resolver falls
/// back to `LogOnlyProvider` for that kind in that case.
pub(crate) struct ApnsConfig {
    /// Raw bytes of the .p8 PKCS#8 PEM file Apple issues. Passed to
    /// `jsonwebtoken::EncodingKey::from_ec_pem` once per JWT refresh
    /// (not per send).
    key_pem: Vec<u8>,
    /// 10-char key id (`<prefix>KEY_ID`).
    key_id: String,
    /// 10-char team id (`<prefix>TEAM_ID`).
    team_id: String,
    /// Bundle id, e.g. `me.lakko.dev` — or `me.lakko.dev.voip` for the
    /// VoIP flavour (`<prefix>TOPIC`).
    topic: String,
    endpoint: ApnsEndpoint,
    push_kind: ApnsPushKind,
}

impl ApnsConfig {
    /// Read the alert credential from `LAKKO_APNS_*`.
    pub(crate) fn from_env() -> Option<Self> {
        Self::build(
            ApnsPushKind::Alert,
            &|name| std::env::var(name).ok(),
            &|path| std::fs::read(path),
        )
    }

    /// Read the call credential from `LAKKO_APNS_VOIP_*`.
    ///
    /// Deliberately a second set of variables rather than a reuse of
    /// the alert ones — see the module docs on why the credential is
    /// separate.
    pub(crate) fn voip_from_env() -> Option<Self> {
        Self::build(
            ApnsPushKind::Voip,
            &|name| std::env::var(name).ok(),
            &|path| std::fs::read(path),
        )
    }

    /// The whole config load, with the environment and the filesystem
    /// injected so every refusal is reachable from a test.
    ///
    /// `None` if any required variable is missing, the .p8 file cannot
    /// be read, the key does not parse as an ES256 PEM, or (VoIP only)
    /// the topic lacks Apple's required suffix. Each cause is logged
    /// via `tracing::warn!` so an operator sees what is wrong at
    /// startup instead of at the first send.
    fn build(
        push_kind: ApnsPushKind,
        var: &dyn Fn(&str) -> Option<String>,
        read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    ) -> Option<Self> {
        let prefix = push_kind.env_prefix();
        let name = |suffix: &str| format!("{prefix}{suffix}");
        let required = |suffix: &str| -> Option<String> {
            let full = name(suffix);
            let value = var(&full).filter(|v| !v.trim().is_empty());
            if value.is_none() {
                tracing::warn!(
                    variable = %full,
                    push_type = push_kind.push_type(),
                    "APNs credential variable missing or empty — falling back to LogOnly",
                );
            }
            value
        };

        let key_path = required("KEY_PATH")?;
        let key_id = required("KEY_ID")?;
        let team_id = required("TEAM_ID")?;
        let topic = required("TOPIC")?;
        let endpoint = match var(&name("ENDPOINT")).as_deref() {
            Some("sandbox") => ApnsEndpoint::Sandbox,
            // Production is the safe default — operator must
            // opt-in to sandbox.
            _ => ApnsEndpoint::Production,
        };

        // A VoIP push to the bare bundle id is refused by APNs, which
        // means a phone that never rings and a log full of
        // `TopicDisallowed`. Catch it here instead.
        if push_kind == ApnsPushKind::Voip && !topic.ends_with(VOIP_TOPIC_SUFFIX) {
            tracing::warn!(
                variable = %name("TOPIC"),
                "VoIP topic must end in {VOIP_TOPIC_SUFFIX} (<bundle id>{VOIP_TOPIC_SUFFIX}) \
                 — falling back to LogOnly",
            );
            return None;
        }

        let key_pem = match read(&key_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(
                    path = %key_path,
                    push_type = push_kind.push_type(),
                    error = %e,
                    "APNs .p8 key file unreadable — falling back to LogOnly",
                );
                return None;
            },
        };

        // Validate the key parses BEFORE returning so a malformed
        // .p8 surfaces at startup, not at first send.
        if let Err(e) = EncodingKey::from_ec_pem(&key_pem) {
            tracing::warn!(
                path = %key_path,
                push_type = push_kind.push_type(),
                error = %e,
                "APNs .p8 key does not parse as ES256 PEM — falling back to LogOnly",
            );
            return None;
        }

        Some(Self {
            key_pem,
            key_id,
            team_id,
            topic,
            endpoint,
            push_kind,
        })
    }
}

#[derive(Clone)]
struct CachedJwt {
    token: String,
    /// Wall-clock instant past which we MUST sign a new JWT.
    expires_at: SystemTime,
}

pub(crate) struct ApnsProvider {
    config: ApnsConfig,
    http: reqwest::Client,
    jwt_cache: RwLock<Option<CachedJwt>>,
}

impl ApnsProvider {
    pub(crate) fn new(config: ApnsConfig) -> Self {
        Self {
            config,
            http: super::shared_http_client(),
            jwt_cache: RwLock::new(None),
        }
    }

    /// Return a cached JWT or sign a fresh one. Holds the read-lock
    /// only long enough to check expiry; the write-path is taken at
    /// most once per [`JWT_REFRESH_AFTER`] window.
    fn jwt(&self) -> Result<String, DeliverError> {
        let now = SystemTime::now();
        // Fast path — read-lock, check expiry, return clone.
        if let Some(cached) = self.jwt_cache.read().as_ref() {
            if cached.expires_at > now {
                return Ok(cached.token.clone());
            }
        }
        // Slow path — write-lock + sign. The double-check pattern
        // (re-read after taking the write lock) prevents a thundering
        // herd at expiry: only the first writer signs, the rest find
        // a fresh token and return.
        let mut slot = self.jwt_cache.write();
        if let Some(cached) = slot.as_ref() {
            if cached.expires_at > now {
                return Ok(cached.token.clone());
            }
        }

        let key = EncodingKey::from_ec_pem(&self.config.key_pem)
            .map_err(|e| DeliverError::Transport(format!("APNs .p8 EncodingKey: {e}")))?;
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.config.key_id.clone());
        let claims = JwtClaims {
            iss: self.config.team_id.clone(),
            iat: Utc::now().timestamp(),
        };
        let token = jsonwebtoken::encode(&header, &claims, &key)
            .map_err(|e| DeliverError::Transport(format!("APNs JWT sign: {e}")))?;
        *slot = Some(CachedJwt {
            token: token.clone(),
            expires_at: now + JWT_REFRESH_AFTER,
        });
        Ok(token)
    }
}

#[async_trait]
impl Provider for ApnsProvider {
    fn kind(&self) -> ProviderKind {
        self.config.push_kind.provider_kind()
    }

    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError> {
        if token.is_empty() {
            return Err(DeliverError::TokenInvalid);
        }
        // Defence in depth behind `notify::delivers_to`, which is what
        // actually routes. iOS terminates an app that takes a VoIP push
        // without reporting a call, so a message arriving here would
        // crash the recipient rather than look wrong — a refusal plus a
        // loud line is the only safe response to a routing bug.
        if self.config.push_kind == ApnsPushKind::Voip && !payload.kind.is_call() {
            tracing::error!(
                wake_kind = payload.kind.as_str(),
                "refusing a non-call wake addressed to a VoIP token — routing bug",
            );
            return Err(DeliverError::Transport(
                "VoIP push type carries call wakes only".to_string(),
            ));
        }
        let device_token_hex = hex::encode(token);
        let url = format!(
            "{}/3/device/{}",
            self.config.endpoint.host(),
            device_token_hex,
        );

        let jwt = self.jwt()?;
        let body = match self.config.push_kind {
            ApnsPushKind::Alert => serde_json::to_vec(&ApsBody::from_payload(payload)),
            // PushKit displays nothing, so a VoIP push carries the bare
            // metadata and no `aps` alert at all.
            ApnsPushKind::Voip => serde_json::to_vec(&VoipBody::from_payload(payload)),
        }
        .map_err(|e| DeliverError::Transport(format!("APNs serialize body: {e}")))?;

        let urgency = ApnsUrgency::for_kind(payload.kind, Utc::now().timestamp());

        let resp = match self
            .http
            .post(&url)
            .header(header::AUTHORIZATION, format!("bearer {jwt}"))
            .header("apns-topic", &self.config.topic)
            // `alert` for the message path and the call fallback;
            // `voip` for the PushKit token — see the module docs.
            .header("apns-push-type", self.config.push_kind.push_type())
            .header("apns-priority", urgency.priority)
            .header("apns-expiration", &urgency.expiration)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(DeliverError::Transport(e.to_string())),
        };

        let status = resp.status();
        if status == StatusCode::OK {
            return Ok(());
        }

        // Parse the JSON error reason so we can map specific codes
        // to TokenInvalid vs Transport. Apple sends
        // `{"reason":"BadDeviceToken"}` on every 4xx.
        let body_text = resp.text().await.unwrap_or_default();
        let reason = serde_json::from_str::<ApnsErrorBody>(&body_text)
            .ok()
            .map(|b| b.reason);
        match (status, reason.as_deref()) {
            (StatusCode::BAD_REQUEST, Some("BadDeviceToken" | "DeviceTokenNotForTopic"))
            | (StatusCode::GONE, _) => Err(DeliverError::TokenInvalid),
            _ => Err(DeliverError::Transport(format!(
                "APNs {} → {}: {}",
                status.as_u16(),
                reason.as_deref().unwrap_or("?"),
                truncate(&body_text, 256),
            ))),
        }
    }
}

/// The two delivery headers that differ between a message wake and a
/// call wake. Split out of [`ApnsProvider::deliver`] so the choice is
/// unit-testable without an Apple endpoint — `deliver` itself cannot
/// be exercised offline.
#[derive(Debug, Eq, PartialEq)]
struct ApnsUrgency {
    /// `apns-priority`: `5` is power-aware (Apple may defer it), `10`
    /// is immediate.
    priority: &'static str,
    /// `apns-expiration`: an **absolute** UNIX timestamp, or `"0"`
    /// meaning "deliver once now, never store and forward".
    expiration: String,
}

impl ApnsUrgency {
    fn for_kind(kind: WakeKind, now_unix: i64) -> Self {
        if kind.is_call() {
            Self {
                priority: "10",
                expiration: now_unix.saturating_add(CALL_EXPIRATION_SECS).to_string(),
            }
        } else {
            Self {
                priority: "5",
                expiration: "0".to_string(),
            }
        }
    }
}

#[derive(Serialize)]
struct JwtClaims {
    iss: String,
    iat: i64,
}

#[derive(Deserialize)]
struct ApnsErrorBody {
    reason: String,
}

/// JSON body shape. Phase 1.20c2: shipped as an **alert push
/// with `mutable-content: 1`** so iOS invokes the recipient's
/// `UNNotificationServiceExtension` (`LakkoNotificationService`
/// in `apps/ios/`) before displaying. The extension parses the
/// wake metadata, sets `threadIdentifier` to the group hex (so
/// iOS groups notifications per conversation on lock screen),
/// and — in a later 1.20c2b sub-sprint — fetches the ciphertext
/// from the DS via App-Group-shared MLS state and renders the
/// decrypted preview body.
///
/// **`thread-id`** is iOS's notification-grouping key. We
/// pre-populate it with `group_pseudonym` hex so the grouping
/// works even when the NSE is unavailable (extension disabled
/// in Settings, device under memory pressure, etc.) — the
/// extension is allowed to override `threadIdentifier` after
/// launch but the initial value is enough for the default
/// grouping behaviour.
///
/// **Body copy** is the generic placeholder the NSE rewrites
/// post-decrypt. When no NSE runs (or it times out within the
/// 30s budget), the user sees "New message" — exactly the same
/// fallback the foreground app code paths use.
#[derive(Serialize)]
struct ApsBody {
    aps: Aps,
    /// Hex subscription id (ADR-0078): the receiver handles the wake only
    /// when it holds this subscription for the `push_gid` in `g`.
    s: String,
    /// Hex group pseudonym (optional). Lets the receiver jump to
    /// the right journal without polling every group it belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    g: Option<String>,
    /// Envelope pointer (DS cursor) past which to fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    p: Option<u64>,
    /// Wake kind tag, mirroring `lakko-push`'s `k` field: `"c"` for a
    /// call, absent for a message. The NSE reads this to decide
    /// whether to ring rather than render a preview — it must be
    /// legible before any ciphertext is fetched. Absent keeps the
    /// message payload byte-identical to what shipped before calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    k: Option<&'static str>,
}

#[derive(Serialize)]
struct Aps {
    alert: ApsAlert,
    /// 1.20c2: signal iOS to invoke the
    /// `UNNotificationServiceExtension` before display.
    #[serde(rename = "mutable-content")]
    mutable_content: u8,
    /// 1.20c2: notification-grouping key. iOS uses this to
    /// stack per-conversation alerts on the lock screen even
    /// when the NSE isn't reachable.
    #[serde(rename = "thread-id", skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
}

#[derive(Serialize)]
struct ApsAlert {
    /// Bundle name — same string the OS shows on the lock
    /// screen for the app. Localised display name on the
    /// device overrides this anyway.
    title: &'static str,
    /// Placeholder body the NSE rewrites after decrypt.
    body: &'static str,
}

impl ApsBody {
    fn from_payload(p: &WakePayload) -> Self {
        Self {
            aps: Aps {
                alert: ApsAlert {
                    title: "Lakko",
                    // The placeholder the NSE rewrites after decrypt.
                    // A call wake says so: if the NSE cannot run, the
                    // user is told a call arrived rather than being
                    // shown "New message" for a ringing phone.
                    body: if p.kind.is_call() {
                        "Incoming call"
                    } else {
                        "New message"
                    },
                },
                mutable_content: 1,
                thread_id: p.group_pseudonym.map(hex::encode),
            },
            s: hex::encode(p.subscription_id),
            g: p.group_pseudonym.map(hex::encode),
            p: p.envelope_pointer,
            k: p.kind.wire_tag(),
        }
    }
}

/// Body of an `apns-push-type: voip` push.
///
/// PushKit hands the dictionary straight to
/// `pushRegistry(_:didReceiveIncomingPushWith:for:completion:)` and
/// displays nothing itself, so there is no `aps` alert and no
/// `mutable-content`: the four metadata keys are the whole payload,
/// and they are exactly the four [`ApsBody`] carries.
///
/// The receiving side reads `g`, `p` and `k`
/// (`VoipWakePayload.parse` in `apps/ios/Lakko/Calls/IncomingCallWake.swift`);
/// `s` (ADR-0078) lets the device confirm the wake is for a subscription
/// the signed-in account holds, the same check the alert path makes.
///
/// Opacity is unchanged from the alert body and from every wake before
/// calls existed: a subscription id, a group pseudonym, a journal cursor, and
/// a one-letter kind tag. No caller, no call id, no media kind — those
/// live in the E2EE journal row the woken device fetches for itself
/// (invariants #2 and #5).
#[derive(Serialize)]
struct VoipBody {
    /// Hex subscription id (ADR-0078): the receiver handles the wake only
    /// when it holds this subscription for the `push_gid` in `g`.
    s: String,
    /// Hex group pseudonym (optional): which journal to read.
    #[serde(skip_serializing_if = "Option::is_none")]
    g: Option<String>,
    /// Envelope pointer (DS cursor) past which to fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    p: Option<u64>,
    /// Kind tag — always `"c"` here, since a VoIP push carries nothing
    /// else, but emitted rather than assumed so the receiving parser
    /// reads one shape on both paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    k: Option<&'static str>,
}

impl VoipBody {
    fn from_payload(p: &WakePayload) -> Self {
        Self {
            s: hex::encode(p.subscription_id),
            g: p.group_pseudonym.map(hex::encode),
            p: p.envelope_pointer,
            k: p.kind.wire_tag(),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}…", &s[..max])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apns_endpoint_host_picks_per_variant() {
        assert!(ApnsEndpoint::Production.host().ends_with("push.apple.com"));
        assert!(ApnsEndpoint::Sandbox
            .host()
            .contains("sandbox.push.apple.com"));
    }

    fn wake(kind: WakeKind) -> WakePayload {
        WakePayload {
            subscription_id: [0xAB; 16],
            group_pseudonym: Some([0xCD; 32]),
            envelope_pointer: Some(42),
            kind,
        }
    }

    #[test]
    fn message_wake_keeps_the_power_aware_headers() {
        let u = ApnsUrgency::for_kind(WakeKind::Message, 1_700_000_000);
        assert_eq!(u.priority, "5");
        // "0" is Apple's "deliver once, now, never store" — NOT an
        // absolute timestamp, and unchanged from before calls existed.
        assert_eq!(u.expiration, "0");
    }

    #[test]
    fn call_wake_is_immediate_and_expires_in_a_minute() {
        let now = 1_700_000_000;
        let u = ApnsUrgency::for_kind(WakeKind::Call, now);
        assert_eq!(u.priority, "10");
        // `apns-expiration` is an absolute UNIX timestamp, so a call
        // must carry `now + 60` — a literal "60" would mean 1970.
        assert_eq!(u.expiration, (now + 60).to_string());
    }

    #[test]
    fn call_expiration_never_overflows_on_a_broken_clock() {
        let u = ApnsUrgency::for_kind(WakeKind::Call, i64::MAX);
        assert_eq!(u.expiration, i64::MAX.to_string());
    }

    #[test]
    fn aps_body_serializes_nse_alert_shape() {
        let body = ApsBody::from_payload(&wake(WakeKind::Message));
        let json = serde_json::to_string(&body).unwrap();
        // 1.20c2: alert push with `mutable-content: 1` so iOS
        // invokes the NSE; previous silent-push shape didn't.
        assert!(json.contains(r#""mutable-content":1"#));
        assert!(json.contains(r#""alert":"#));
        assert!(json.contains(r#""title":"Lakko""#));
        assert!(json.contains(r#""body":"New message""#));
        // Thread id is the group hex — drives iOS's lock-screen
        // grouping when the NSE isn't reachable.
        assert!(json.contains(r#""thread-id":"cdcdcd"#));
        // Per-row metadata uses short single-letter keys.
        assert!(json.contains(r#""s":""#));
        assert!(json.contains(r#""g":""#));
        assert!(json.contains(r#""p":42"#));
        // A message wake carries no kind tag at all — the shape is
        // byte-identical to what shipped before calls existed.
        assert!(!json.contains(r#""k""#), "got {json}");
    }

    #[test]
    fn aps_body_tags_a_call_and_says_so_in_the_fallback_alert() {
        let json = serde_json::to_string(&ApsBody::from_payload(&wake(WakeKind::Call))).unwrap();
        assert!(json.contains(r#""k":"c""#), "got {json}");
        assert!(json.contains(r#""body":"Incoming call""#), "got {json}");
    }

    #[test]
    fn a_call_body_discloses_nothing_a_message_body_does_not() {
        // The only differences must be the kind tag and the fallback
        // alert copy. No caller, no call id, no media kind.
        let call = serde_json::to_string(&ApsBody::from_payload(&wake(WakeKind::Call))).unwrap();
        let message =
            serde_json::to_string(&ApsBody::from_payload(&wake(WakeKind::Message))).unwrap();
        let normalised = call
            .replace(r#""body":"Incoming call""#, r#""body":"New message""#)
            .replace(r#","k":"c""#, "");
        assert_eq!(normalised, message);
    }

    #[test]
    fn aps_body_omits_thread_id_when_group_absent() {
        let body = ApsBody::from_payload(&WakePayload {
            subscription_id: [0u8; 16],
            group_pseudonym: None,
            envelope_pointer: None,
            kind: WakeKind::Message,
        });
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("thread-id"));
    }

    // ------------------------------------------------------------------
    // VoIP flavour
    // ------------------------------------------------------------------

    #[test]
    fn the_push_type_header_is_what_separates_the_two_flavours() {
        // The header value is the difference between a notification and
        // a phone that rings from a terminated app.
        assert_eq!(ApnsPushKind::Alert.push_type(), "alert");
        assert_eq!(ApnsPushKind::Voip.push_type(), "voip");
    }

    #[test]
    fn each_flavour_serves_its_own_registry_row() {
        assert_eq!(ApnsPushKind::Alert.provider_kind(), ProviderKind::Apns);
        assert_eq!(ApnsPushKind::Voip.provider_kind(), ProviderKind::ApnsVoip);
        assert!(ApnsPushKind::Voip.provider_kind().is_call_token());
        assert!(!ApnsPushKind::Alert.provider_kind().is_call_token());
    }

    #[test]
    fn each_flavour_reads_its_own_credential_variables() {
        // Separate credentials, so separate variables — a VoIP push
        // signed by the alert key against the alert topic is refused by
        // APNs, and silently borrowing one for the other would produce
        // exactly that.
        assert_eq!(ApnsPushKind::Alert.env_prefix(), "LAKKO_APNS_");
        assert_eq!(ApnsPushKind::Voip.env_prefix(), "LAKKO_APNS_VOIP_");
        assert_ne!(
            ApnsPushKind::Alert.env_prefix(),
            ApnsPushKind::Voip.env_prefix(),
        );
    }

    #[test]
    fn a_call_wake_is_immediate_on_the_voip_path_too() {
        // A VoIP push only ever carries a call, so it always takes the
        // priority-10 / short-expiry treatment.
        let now = 1_700_000_000;
        let u = ApnsUrgency::for_kind(WakeKind::Call, now);
        assert_eq!(u.priority, "10");
        assert_eq!(u.expiration, (now + CALL_EXPIRATION_SECS).to_string());
    }

    #[test]
    fn a_voip_body_carries_the_metadata_and_no_alert() {
        let json = serde_json::to_string(&VoipBody::from_payload(&wake(WakeKind::Call))).unwrap();
        // PushKit displays nothing, so none of the display machinery
        // belongs here.
        assert!(!json.contains("aps"), "got {json}");
        assert!(!json.contains("alert"), "got {json}");
        assert!(!json.contains("mutable-content"), "got {json}");
        assert!(!json.contains("thread-id"), "got {json}");
        // What the iOS PushKit handler actually reads.
        assert!(json.contains(r#""k":"c""#), "got {json}");
        assert!(json.contains(r#""g":""#), "got {json}");
        assert!(json.contains(r#""p":42"#), "got {json}");
        assert!(json.contains(r#""s":""#), "got {json}");
    }

    #[test]
    fn a_voip_body_discloses_nothing_the_alert_body_does_not() {
        // Opacity (invariants #2 and #5): the VoIP payload must be a
        // strict subset of the keys the alert payload already carried.
        // No caller, no call id, no media kind.
        let voip: serde_json::Value =
            serde_json::to_value(VoipBody::from_payload(&wake(WakeKind::Call))).unwrap();
        let alert: serde_json::Value =
            serde_json::to_value(ApsBody::from_payload(&wake(WakeKind::Call))).unwrap();
        let voip_obj = voip.as_object().unwrap();
        let alert_obj = alert.as_object().unwrap();
        for (key, value) in voip_obj {
            assert_eq!(
                alert_obj.get(key),
                Some(value),
                "voip key {key} is new or differs",
            );
        }
        // And it is only the metadata: `aps` is the alert body's extra.
        assert_eq!(voip_obj.len() + 1, alert_obj.len());
    }

    #[test]
    fn a_voip_body_omits_the_optional_fields_when_absent() {
        let json = serde_json::to_string(&VoipBody::from_payload(&WakePayload {
            subscription_id: [0u8; 16],
            group_pseudonym: None,
            envelope_pointer: None,
            kind: WakeKind::Call,
        }))
        .unwrap();
        assert!(!json.contains(r#""g""#), "got {json}");
        assert!(!json.contains(r#""p""#), "got {json}");
    }

    // ------------------------------------------------------------------
    // Credential loading — every refusal path
    // ------------------------------------------------------------------

    /// The four variables a flavour needs, with plausible values.
    fn full_env(prefix: &str) -> Vec<(String, String)> {
        let topic = if prefix.contains("VOIP") {
            "me.lakko.dev.voip"
        } else {
            "me.lakko.dev"
        };
        vec![
            (format!("{prefix}KEY_PATH"), "/etc/lakko/k.p8".to_string()),
            (format!("{prefix}KEY_ID"), "ABCDE12345".to_string()),
            (format!("{prefix}TEAM_ID"), "1A2B3C4D5E".to_string()),
            (format!("{prefix}TOPIC"), topic.to_string()),
        ]
    }

    /// Build with an injected environment and an injected reader, so no
    /// test touches the process environment or the filesystem.
    fn build_from(
        push_kind: ApnsPushKind,
        env: &[(String, String)],
        read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    ) -> Option<ApnsConfig> {
        ApnsConfig::build(
            push_kind,
            &|name| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
            read,
        )
    }

    /// As [`build_from`], with a reader returning a file that exists but
    /// is not an ES256 PEM — every caller asserts a refusal, and this
    /// keeps the reason for it inside the config loader.
    fn build_with(push_kind: ApnsPushKind, env: &[(String, String)]) -> Option<ApnsConfig> {
        build_from(push_kind, env, &|_| {
            Ok(b"-----BEGIN PRIVATE KEY-----\nnot-a-key\n-----END PRIVATE KEY-----\n".to_vec())
        })
    }

    /// A real PKCS#8 P-256 key, so the parse gate in `build` is
    /// genuinely cleared rather than skipped.
    fn test_key_pem() -> String {
        use p256::pkcs8::EncodePrivateKey as _;
        p256::SecretKey::random(&mut rand_core::OsRng)
            .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
            .expect("encode PKCS#8 PEM")
            .to_string()
    }

    /// A fully configured provider of either flavour.
    fn configured(push_kind: ApnsPushKind) -> ApnsProvider {
        let pem = test_key_pem();
        let env = full_env(push_kind.env_prefix());
        let cfg = build_from(push_kind, &env, &|_| Ok(pem.as_bytes().to_vec()))
            .expect("fully configured flavour");
        ApnsProvider::new(cfg)
    }

    #[test]
    fn a_missing_credential_variable_refuses_the_provider() {
        // Any one of the four missing means no provider, which the
        // resolver turns into a LogOnly fallback plus a startup warning
        // — never a half-configured sender.
        for push_kind in [ApnsPushKind::Alert, ApnsPushKind::Voip] {
            let full = full_env(push_kind.env_prefix());
            for skipped in 0..full.len() {
                let partial: Vec<(String, String)> = full
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != skipped)
                    .map(|(_, kv)| kv.clone())
                    .collect();
                assert!(
                    build_with(push_kind, &partial).is_none(),
                    "{} configured without {}",
                    push_kind.push_type(),
                    full[skipped].0,
                );
            }
        }
    }

    #[test]
    fn an_empty_credential_variable_counts_as_missing() {
        // A variable set to "" in an env file is a deployment that
        // meant to fill it in and did not; treating it as present
        // would sign JWTs with an empty key id.
        for push_kind in [ApnsPushKind::Alert, ApnsPushKind::Voip] {
            let mut env = full_env(push_kind.env_prefix());
            env[1].1 = "   ".to_string();
            assert!(build_with(push_kind, &env).is_none());
        }
    }

    #[test]
    fn the_voip_credential_is_not_satisfied_by_the_alert_one() {
        // The point of the separate prefix: an operator holding only
        // the alert credential gets a LogOnly VoIP provider, not VoIP
        // pushes APNs will reject.
        let alert_env = full_env(ApnsPushKind::Alert.env_prefix());
        assert!(build_with(ApnsPushKind::Voip, &alert_env).is_none());
    }

    #[test]
    fn a_voip_topic_without_the_suffix_is_refused() {
        // APNs answers `TopicDisallowed` for a VoIP push to the bare
        // bundle id, which is a phone that never rings. Catch it at
        // startup instead.
        let mut env = full_env(ApnsPushKind::Voip.env_prefix());
        env[3].1 = "me.lakko.dev".to_string();
        assert!(build_with(ApnsPushKind::Voip, &env).is_none());
        // ... and the alert flavour has no such requirement.
        let alert = full_env(ApnsPushKind::Alert.env_prefix());
        assert_eq!(alert[3].1, "me.lakko.dev");
    }

    #[test]
    fn a_key_that_is_not_an_es256_pem_is_refused() {
        // The injected reader returns a well-formed PEM envelope with
        // nonsense inside — a malformed .p8 must fail at startup, not
        // at the first send.
        for push_kind in [ApnsPushKind::Alert, ApnsPushKind::Voip] {
            let env = full_env(push_kind.env_prefix());
            assert!(build_with(push_kind, &env).is_none());
        }
    }

    #[test]
    fn an_unreadable_key_file_is_refused() {
        for push_kind in [ApnsPushKind::Alert, ApnsPushKind::Voip] {
            let env = full_env(push_kind.env_prefix());
            let cfg = build_from(push_kind, &env, &|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file",
                ))
            });
            assert!(cfg.is_none());
        }
    }

    #[test]
    fn a_fully_configured_flavour_is_accepted_with_its_own_topic() {
        let pem = test_key_pem();
        for push_kind in [ApnsPushKind::Alert, ApnsPushKind::Voip] {
            let env = full_env(push_kind.env_prefix());
            let cfg = build_from(push_kind, &env, &|_| Ok(pem.as_bytes().to_vec()))
                .expect("fully configured flavour");
            assert_eq!(cfg.push_kind, push_kind);
            assert_eq!(cfg.key_id, "ABCDE12345");
            assert_eq!(cfg.team_id, "1A2B3C4D5E");
            assert_eq!(
                cfg.topic.ends_with(VOIP_TOPIC_SUFFIX),
                push_kind == ApnsPushKind::Voip,
            );
        }
    }

    #[tokio::test]
    async fn a_voip_provider_refuses_a_message_wake_before_any_network_call() {
        // Defence in depth behind `notify::delivers_to`. A message
        // reaching a VoIP token crashes the recipient app, so the
        // refusal must not depend on the router being right.
        let provider = configured(ApnsPushKind::Voip);
        assert_eq!(provider.kind(), ProviderKind::ApnsVoip);
        let err = provider
            .deliver(b"token-bytes", &wake(WakeKind::Message))
            .await
            .expect_err("a message must never reach a VoIP token");
        assert!(matches!(err, DeliverError::Transport(_)));
    }

    #[tokio::test]
    async fn an_empty_token_is_invalid_on_both_flavours() {
        for push_kind in [ApnsPushKind::Alert, ApnsPushKind::Voip] {
            let err = configured(push_kind)
                .deliver(b"", &wake(WakeKind::Call))
                .await
                .expect_err("empty token");
            assert!(matches!(err, DeliverError::TokenInvalid));
        }
    }
}
