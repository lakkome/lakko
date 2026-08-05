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
//! payload metadata (`group_pseudonym`, `envelope_pointer`) rides as custom
//! keys (`d`/`g`/`p`) alongside `aps` so the extension can decide which cursor
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

use crate::provider::{DeliverError, Provider, ProviderKind, WakePayload};

/// JWT lifetime — sign new tokens 50 minutes apart so we always have
/// ≥ 10 minutes of margin against Apple's 60-minute ceiling.
const JWT_REFRESH_AFTER: Duration = Duration::from_secs(50 * 60);

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

/// Env-loaded APNs config. Built once at service startup; returns
/// `None` if any required variable is missing — the resolver falls
/// back to `LogOnlyProvider` for the APNs kind in that case.
pub(crate) struct ApnsConfig {
    /// Raw bytes of the .p8 PKCS#8 PEM file Apple issues. Passed to
    /// `jsonwebtoken::EncodingKey::from_ec_pem` once per JWT refresh
    /// (not per send).
    key_pem: Vec<u8>,
    /// 10-char key id (`LAKKO_APNS_KEY_ID`).
    key_id: String,
    /// 10-char team id (`LAKKO_APNS_TEAM_ID`).
    team_id: String,
    /// Bundle id, e.g. `me.lakko.dev` (`LAKKO_APNS_TOPIC`).
    topic: String,
    endpoint: ApnsEndpoint,
}

impl ApnsConfig {
    /// Read `LAKKO_APNS_*` env vars. `None` if any are missing or
    /// the .p8 file can't be read. Logs the cause via `tracing::warn!`
    /// so operators see what's wrong on startup.
    pub(crate) fn from_env() -> Option<Self> {
        let key_path = std::env::var("LAKKO_APNS_KEY_PATH").ok()?;
        let key_id = std::env::var("LAKKO_APNS_KEY_ID").ok()?;
        let team_id = std::env::var("LAKKO_APNS_TEAM_ID").ok()?;
        let topic = std::env::var("LAKKO_APNS_TOPIC").ok()?;
        let endpoint = match std::env::var("LAKKO_APNS_ENDPOINT").as_deref() {
            Ok("sandbox") => ApnsEndpoint::Sandbox,
            // Production is the safe default — operator must
            // opt-in to sandbox.
            _ => ApnsEndpoint::Production,
        };

        let key_pem = match std::fs::read(&key_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(
                    path = %key_path,
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
        ProviderKind::Apns
    }

    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError> {
        if token.is_empty() {
            return Err(DeliverError::TokenInvalid);
        }
        let device_token_hex = hex::encode(token);
        let url = format!(
            "{}/3/device/{}",
            self.config.endpoint.host(),
            device_token_hex,
        );

        let jwt = self.jwt()?;
        let body = serde_json::to_vec(&ApsBody::from_payload(payload))
            .map_err(|e| DeliverError::Transport(format!("APNs serialize aps body: {e}")))?;

        let resp = match self
            .http
            .post(&url)
            .header(header::AUTHORIZATION, format!("bearer {jwt}"))
            .header("apns-topic", &self.config.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "5")
            .header("apns-expiration", "0")
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
    /// Hex device pseudonym — receiver matches against its own.
    d: String,
    /// Hex group pseudonym (optional). Lets the receiver jump to
    /// the right journal without polling every group it belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    g: Option<String>,
    /// Envelope pointer (DS cursor) past which to fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    p: Option<u64>,
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
                    body: "New message",
                },
                mutable_content: 1,
                thread_id: p.group_pseudonym.map(hex::encode),
            },
            d: hex::encode(p.device_pseudonym),
            g: p.group_pseudonym.map(hex::encode),
            p: p.envelope_pointer,
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

    #[test]
    fn aps_body_serializes_nse_alert_shape() {
        let body = ApsBody::from_payload(&WakePayload {
            device_pseudonym: [0xAB; 32],
            group_pseudonym: Some([0xCD; 32]),
            envelope_pointer: Some(42),
        });
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
        assert!(json.contains(r#""d":""#));
        assert!(json.contains(r#""g":""#));
        assert!(json.contains(r#""p":42"#));
    }

    #[test]
    fn aps_body_omits_thread_id_when_group_absent() {
        let body = ApsBody::from_payload(&WakePayload {
            device_pseudonym: [0u8; 32],
            group_pseudonym: None,
            envelope_pointer: None,
        });
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("thread-id"));
    }
}
