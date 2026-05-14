// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! FCM provider — Firebase Cloud Messaging HTTP v1 API.
//!
//! ## Wire protocol (Google HTTP v1, post-2024)
//!
//! Google retired the "legacy server-key" API on June 2024; HTTP v1
//! requires per-request OAuth2 bearer tokens minted from a service
//! account. Two-step flow:
//!
//! **Step 1 — mint an access token (≤ 1 h cache)**
//!
//! ```text
//! POST https://oauth2.googleapis.com/token
//! Content-Type: application/x-www-form-urlencoded
//!
//! grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer
//! &assertion=<RS256 JWT signed by service-account private_key>
//! ```
//!
//! The JWT claims look like:
//!
//! ```json
//! { "iss":  "<sa-client-email>",
//!   "scope":"https://www.googleapis.com/auth/firebase.messaging",
//!   "aud":  "https://oauth2.googleapis.com/token",
//!   "iat":  <unix_seconds>,
//!   "exp":  <unix_seconds + 3600> }
//! ```
//!
//! **Step 2 — send the wake-up**
//!
//! ```text
//! POST https://fcm.googleapis.com/v1/projects/<project_id>/messages:send
//! Authorization: Bearer <access_token>
//! Content-Type: application/json
//!
//! { "message": {
//!     "token": "<device_token>",
//!     "data":  { "d": "<hex device pseudonym>",
//!                "g": "<hex group pseudonym>",
//!                "p": "<envelope pointer as decimal string>" } } }
//! ```
//!
//! `data`-only messages reach the client's
//! `FirebaseMessagingService.onMessageReceived(RemoteMessage)`; the
//! client then decodes the wake-metadata, fetches ciphertext from
//! `services/ds`, decrypts locally, and builds the user-visible
//! notification on-device via `NotificationManagerCompat`. Google
//! never sees plaintext.
//!
//! ## Status mapping
//!
//! - `200` — delivered to FCM.
//! - `400` with `error.details[].errorCode = "INVALID_ARGUMENT"`
//!   referencing "registration" → [`DeliverError::TokenInvalid`].
//! - `404` with `error.details[].errorCode = "UNREGISTERED"` →
//!   [`DeliverError::TokenInvalid`].
//! - `403` (project mismatch) / `429` (rate limit) / any other 4xx /
//!   5xx → [`DeliverError::Transport`] with the raw body for
//!   diagnostics.

use async_trait::async_trait;
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use parking_lot::RwLock;
use reqwest::{header, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use crate::provider::{DeliverError, Provider, ProviderKind, WakePayload};

/// Refresh the OAuth2 token a minute before the server-stated TTL.
/// Google's tokens nominally live 60 minutes; subtracting 60 s gives
/// margin against clock skew + a slow first send right at the edge.
const ACCESS_TOKEN_SAFETY_WINDOW: Duration = Duration::from_secs(60);

/// JWT validity for the OAuth2 assertion — Google accepts up to 1 h.
/// We use the maximum because the JWT is single-use; longer claims
/// don't help us but they don't hurt either.
const ASSERTION_TTL_SECONDS: i64 = 3600;

/// Loaded service-account JSON. Built once at startup.
pub(crate) struct FcmConfig {
    project_id: String,
    /// Google service-account `client_email`.
    client_email: String,
    /// Raw PEM bytes of the `private_key` JSON field (RS256 / RSA-2048).
    private_key_pem: Vec<u8>,
}

impl FcmConfig {
    /// Read `LAKKO_FCM_SA_PATH` and parse the service-account JSON.
    /// Returns `None` on missing env var, unreadable file, or
    /// malformed JSON. Logs the cause so operators see what's wrong.
    pub(crate) fn from_env() -> Option<Self> {
        let path = std::env::var("LAKKO_FCM_SA_PATH").ok()?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "FCM service-account JSON unreadable — falling back to LogOnly",
                );
                return None;
            },
        };
        let parsed: ServiceAccountJson = match serde_json::from_slice(&bytes) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "FCM service-account JSON malformed — falling back to LogOnly",
                );
                return None;
            },
        };

        // Validate the RSA key parses BEFORE returning so a bad SA
        // surfaces at startup.
        let key_pem = parsed.private_key.into_bytes();
        if let Err(e) = EncodingKey::from_rsa_pem(&key_pem) {
            tracing::warn!(
                path = %path,
                error = %e,
                "FCM service-account private_key does not parse as RSA PEM — falling back to LogOnly",
            );
            return None;
        }

        Some(Self {
            project_id: parsed.project_id,
            client_email: parsed.client_email,
            private_key_pem: key_pem,
        })
    }
}

/// Subset of the Google service-account JSON we care about. Google
/// adds new fields over time; serde's default behaviour
/// (`deny_unknown_fields` off) makes that a forward-compat no-op.
#[derive(Deserialize)]
struct ServiceAccountJson {
    project_id: String,
    client_email: String,
    private_key: String,
}

#[derive(Clone)]
struct CachedAccessToken {
    bearer: String,
    /// Wall-clock instant past which we MUST mint a new token.
    expires_at: SystemTime,
}

pub(crate) struct FcmProvider {
    config: FcmConfig,
    http: reqwest::Client,
    access_token: RwLock<Option<CachedAccessToken>>,
}

impl FcmProvider {
    pub(crate) fn new(config: FcmConfig) -> Self {
        Self {
            config,
            http: super::shared_http_client(),
            access_token: RwLock::new(None),
        }
    }

    /// Return a cached OAuth2 access token or mint a fresh one.
    /// Same double-check pattern as `ApnsProvider::jwt` — fast read
    /// path, write-lock + verify on slow path.
    async fn access_token(&self) -> Result<String, DeliverError> {
        let now = SystemTime::now();
        if let Some(cached) = self.access_token.read().as_ref() {
            if cached.expires_at > now {
                return Ok(cached.bearer.clone());
            }
        }

        // Mint a new token. We do the OAuth2 round-trip outside the
        // write-lock — that's a network call that could take
        // hundreds of ms; blocking other deliver()s on it would
        // serialise the whole provider. Brief race window where
        // two threads both mint is fine; whichever lands last wins
        // and the other token is GC'd.
        let bearer = self.mint_access_token().await?;
        let expires_at = SystemTime::now() + Duration::from_secs(3600) - ACCESS_TOKEN_SAFETY_WINDOW;
        *self.access_token.write() = Some(CachedAccessToken {
            bearer: bearer.clone(),
            expires_at,
        });
        Ok(bearer)
    }

    /// Build an RS256 JWT, exchange it at the Google token endpoint,
    /// return the access_token string.
    async fn mint_access_token(&self) -> Result<String, DeliverError> {
        let key = EncodingKey::from_rsa_pem(&self.config.private_key_pem)
            .map_err(|e| DeliverError::Transport(format!("FCM RS256 key parse: {e}")))?;
        let now = Utc::now().timestamp();
        let claims = AssertionClaims {
            iss: self.config.client_email.clone(),
            scope: "https://www.googleapis.com/auth/firebase.messaging".to_string(),
            aud: "https://oauth2.googleapis.com/token".to_string(),
            iat: now,
            exp: now + ASSERTION_TTL_SECONDS,
        };
        let assertion = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key)
            .map_err(|e| DeliverError::Transport(format!("FCM JWT sign: {e}")))?;

        let form: HashMap<&str, &str> = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", assertion.as_str()),
        ]
        .into_iter()
        .collect();

        let resp = match self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&form)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(DeliverError::Transport(format!("oauth2 send: {e}"))),
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(DeliverError::Transport(format!(
                "oauth2 {}: {}",
                status.as_u16(),
                truncate(&body, 256),
            )));
        }
        let parsed: OAuth2TokenResponse = match resp.json().await {
            Ok(p) => p,
            Err(e) => return Err(DeliverError::Transport(format!("oauth2 parse: {e}"))),
        };
        Ok(parsed.access_token)
    }
}

#[async_trait]
impl Provider for FcmProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Fcm
    }

    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError> {
        if token.is_empty() {
            return Err(DeliverError::TokenInvalid);
        }
        let device_token = std::str::from_utf8(token)
            .map_err(|_| DeliverError::Transport("FCM token is not valid UTF-8".to_string()))?;

        let bearer = self.access_token().await?;

        let url = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.config.project_id,
        );
        let body = SendBody {
            message: Message {
                token: device_token.to_string(),
                data: WakeData::from_payload(payload),
            },
        };

        let resp = match self
            .http
            .post(&url)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .json(&body)
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
        let body_text = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<FcmErrorBody>(&body_text).ok();
        let fcm_code = parsed
            .as_ref()
            .and_then(|b| b.error.details.first().and_then(|d| d.error_code.clone()));

        match (status, fcm_code.as_deref()) {
            (StatusCode::NOT_FOUND, _) | (_, Some("UNREGISTERED")) => {
                Err(DeliverError::TokenInvalid)
            },
            (StatusCode::BAD_REQUEST, Some("INVALID_ARGUMENT"))
                if body_text.to_ascii_lowercase().contains("registration") =>
            {
                Err(DeliverError::TokenInvalid)
            },
            _ => Err(DeliverError::Transport(format!(
                "FCM {} → {}: {}",
                status.as_u16(),
                fcm_code.as_deref().unwrap_or("?"),
                truncate(&body_text, 256),
            ))),
        }
    }
}

#[derive(Serialize)]
struct AssertionClaims {
    iss: String,
    scope: String,
    aud: String,
    iat: i64,
    exp: i64,
}

#[derive(Deserialize)]
struct OAuth2TokenResponse {
    access_token: String,
    /// Google nominally returns `expires_in` here (seconds, typically
    /// 3599). We don't use it — the 1-hour assumption with a
    /// safety-window subtract is enough granularity for our purposes,
    /// and decoupling from the field lets us survive a server-side
    /// rename without code changes.
    #[serde(default, rename = "expires_in")]
    _expires_in_seconds: Option<u64>,
}

#[derive(Serialize)]
struct SendBody {
    message: Message,
}

#[derive(Serialize)]
struct Message {
    token: String,
    /// FCM HTTP v1 requires every `data` value to be a string. We
    /// stringify the integer envelope pointer accordingly.
    data: WakeData,
}

#[derive(Serialize)]
struct WakeData {
    /// Hex device pseudonym.
    d: String,
    /// Hex group pseudonym (optional). Empty string when absent —
    /// FCM rejects `null` data values.
    #[serde(skip_serializing_if = "Option::is_none")]
    g: Option<String>,
    /// Envelope pointer as decimal string (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    p: Option<String>,
}

impl WakeData {
    fn from_payload(p: &WakePayload) -> Self {
        Self {
            d: hex::encode(p.device_pseudonym),
            g: p.group_pseudonym.map(hex::encode),
            p: p.envelope_pointer.map(|n| n.to_string()),
        }
    }
}

#[derive(Deserialize)]
struct FcmErrorBody {
    error: FcmError,
}

#[derive(Deserialize, Default)]
struct FcmError {
    #[serde(default)]
    details: Vec<FcmErrorDetail>,
}

#[derive(Deserialize)]
struct FcmErrorDetail {
    #[serde(rename = "errorCode")]
    error_code: Option<String>,
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
    fn wake_data_serialises_with_string_pointer() {
        // FCM HTTP v1 rejects non-string `data` values. The integer
        // envelope pointer must marshal as a decimal string.
        let data = WakeData::from_payload(&WakePayload {
            device_pseudonym: [0x01; 32],
            group_pseudonym: Some([0x02; 32]),
            envelope_pointer: Some(99),
        });
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains(r#""p":"99""#), "got {json}");
    }

    #[test]
    fn wake_data_omits_optional_fields_when_absent() {
        let data = WakeData::from_payload(&WakePayload {
            device_pseudonym: [0; 32],
            group_pseudonym: None,
            envelope_pointer: None,
        });
        let json = serde_json::to_string(&data).unwrap();
        assert!(!json.contains(r#""g""#));
        assert!(!json.contains(r#""p""#));
    }

    #[test]
    fn fcm_error_body_parses_unregistered() {
        // Real Google response shape for an unregistered device.
        let body = r#"{
            "error": {
                "code": 404,
                "message": "Requested entity was not found.",
                "status": "NOT_FOUND",
                "details": [{
                    "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                    "errorCode": "UNREGISTERED"
                }]
            }
        }"#;
        let parsed: FcmErrorBody = serde_json::from_str(body).unwrap();
        let code = parsed
            .error
            .details
            .first()
            .and_then(|d| d.error_code.clone());
        assert_eq!(code.as_deref(), Some("UNREGISTERED"));
    }
}
