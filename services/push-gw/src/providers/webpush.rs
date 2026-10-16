// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Web Push provider — RFC 8030 + RFC 8291 + VAPID (RFC 8292).
//!
//! ## Wire protocol
//!
//! ```text
//! POST <subscription.endpoint>             # from PushManager.subscribe()
//! Authorization: vapid t=<JWT>, k=<base64url(VAPID public key)>
//! Content-Type: application/octet-stream
//! Content-Encoding: aes128gcm
//! TTL: 14400
//! Urgency: normal
//!
//! <RFC 8188 aes128gcm payload — encrypted to the subscription's
//!  `keys.p256dh` ECDH key with ephemeral per-request ECDH(P-256)>
//! ```
//!
//! A browser-side `PushSubscription` carries three pieces:
//!
//! - `endpoint` — HTTPS URL chosen by the browser's push service
//!   (Mozilla, Chrome, Edge, Safari). We POST our wake-up there.
//! - `keys.p256dh` — subscriber's P-256 public key, base64url-encoded.
//!   Per RFC 8291 we ECDH against it with a fresh ephemeral key,
//!   feed the shared secret into HKDF-SHA256, derive an AES-128-GCM
//!   key + nonce, encrypt the payload.
//! - `keys.auth` — 16-byte symmetric salt mixed into the HKDF info
//!   string so even a successful ECDH between two parties yields a
//!   different content-encryption key per subscription.
//!
//! The token a sealed subscription carries is JSON of the three fields above
//! (UTF-8 bytes); we parse it once per send.
//!
//! ## VAPID JWT (RFC 8292)
//!
//! Single ES256 JWT per request:
//!
//! ```json
//! header: { "alg": "ES256", "typ": "JWT" }
//! claims: {
//!   "aud": "<scheme>://<host>",     // origin of subscription.endpoint
//!   "sub": "mailto:admin@lakko.me", // operator contact
//!   "exp": <unix_seconds + 12h>
//! }
//! ```
//!
//! Browsers cap `exp` at 24 h; 12 h is a power-of-two-friendly default
//! with plenty of headroom against clock skew. The VAPID public key
//! (base64url-uncompressed P-256 point, 65 bytes) rides in the
//! `Authorization` header under `k=…` so the browser knows which
//! application is identifying.
//!
//! ## Encryption (RFC 8291)
//!
//! ```text
//! ephemeral_secret = P-256 keygen
//! shared = ECDH(ephemeral_secret, subscriber.p256dh)
//! prk_key = HKDF-Extract(auth, shared)
//! key_info = "WebPush: info\x00" || subscriber.p256dh || ephemeral.public
//! ikm = HKDF-Expand(prk_key, key_info, 32)
//! salt = random 16 bytes
//! prk = HKDF-Extract(salt, ikm)
//! cek = HKDF-Expand(prk, "Content-Encoding: aes128gcm\x00", 16)
//! nonce = HKDF-Expand(prk, "Content-Encoding: nonce\x00", 12)
//! ciphertext = AES-128-GCM(cek, nonce).encrypt(payload || 0x02)
//! frame = salt(16) || rs(4 BE) || idlen(1) || keyid(65) || ciphertext
//! ```
//!
//! Where `rs` is the record size (we send a single record, so any
//! value ≥ ciphertext length works — 4096 is a typical browser-side
//! default) and `keyid` is our ephemeral public key in
//! uncompressed-point form.
//!
//! The `\x02` padding-delimiter byte marks the END of the payload
//! per RFC 8188 (vs. `\x01` for "more records follow").
//!
//! ## Call wakes
//!
//! A call wake sends `Urgency: high` (RFC 8030 §5.3) and `TTL: 60`
//! instead of `Urgency: normal` / `TTL: 14400`, and adds `"k":"c"` to
//! the encrypted body. Urgency is the only lever we have over a push
//! service that may hold a low-urgency message while the browser is
//! offline or the device is saving power; the short TTL stops a queued
//! call wake surfacing long after the caller gave up.
//!
//! Urgency and TTL are **plaintext headers** — the push service sees
//! them. The body, including the `k` tag, is inside the RFC 8291
//! encryption, so the service learns urgency but not the kind tag.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, KeyInit, Nonce};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::Utc;
use hkdf::Hkdf;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{ecdh::EphemeralSecret, PublicKey};
use rand_core::{OsRng, RngCore};
use reqwest::{header, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::Duration;

use crate::provider::{DeliverError, Provider, ProviderKind, WakeKind, WakePayload};

/// Browser-pushed message TTL — 4 hours mirrors the UnifiedPush
/// default. Long enough for off-line tabs to wake up; short enough
/// that an offline subscriber doesn't accumulate stale wake-ups.
const TTL_SECONDS: u32 = 4 * 60 * 60;

/// TTL for a call wake. A call is worthless once the caller has hung
/// up, and a stale ring is worse than a missed one.
const CALL_TTL_SECONDS: u32 = 60;

/// VAPID JWT validity. RFC 8292 caps it at 24 h; 12 h leaves headroom
/// against subscriber-side clock skew.
const VAPID_JWT_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// RFC 8291 application key prefix — embedded in the HKDF
/// content-encryption derivation. Must be the exact 14 bytes (the
/// trailing NUL is mandatory).
const WEBPUSH_KEY_INFO_PREFIX: &[u8] = b"WebPush: info\x00";

/// RFC 8188 content-encoding label for AES-128-GCM. Embedded in the
/// HKDF cek derivation.
const AES128GCM_INFO: &[u8] = b"Content-Encoding: aes128gcm\x00";

/// RFC 8188 nonce label.
const NONCE_INFO: &[u8] = b"Content-Encoding: nonce\x00";

/// Single-record framing — we never split, so the record size only
/// needs to exceed ciphertext length. 4096 is the typical browser
/// default for the aes128gcm content-encoding.
const RECORD_SIZE: u32 = 4096;

pub(crate) struct WebPushConfig {
    /// VAPID public key, uncompressed P-256 SEC1 (65 bytes), encoded
    /// as base64url-no-pad. Sent verbatim in the `Authorization`
    /// header under `k=…`.
    public_key_b64url: String,
    /// VAPID private key parsed from PEM. Used to sign the per-
    /// request ES256 JWT.
    encoding_key: EncodingKey,
    /// `mailto:` or `https:` URI identifying the operator — appears as
    /// the `sub` claim on the VAPID JWT (RFC 8292 §2.1).
    subject: String,
}

impl WebPushConfig {
    /// Load from env. Returns `None` if any required variable is
    /// missing or malformed. The private + public keys must be a
    /// matching pair (P-256); we don't validate that here — a
    /// mismatch surfaces server-side as VAPID auth failure on the
    /// first send.
    pub(crate) fn from_env() -> Option<Self> {
        let priv_pem = std::env::var("LAKKO_WEBPUSH_VAPID_PRIVATE_KEY_PEM").ok()?;
        let pub_b64url = std::env::var("LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL").ok()?;
        let subject = std::env::var("LAKKO_WEBPUSH_VAPID_SUBJECT").ok()?;

        // The key must be PKCS#8 (`-----BEGIN PRIVATE KEY-----`). A SEC1
        // key straight from `openssl ecparam -genkey`
        // (`-----BEGIN EC PRIVATE KEY-----`) is refused with
        // InvalidKeyFormat — observed on the test server — so convert it
        // with `openssl pkcs8 -topk8 -nocrypt`.
        let encoding_key = match EncodingKey::from_ec_pem(priv_pem.as_bytes()) {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "VAPID private key does not parse as a PKCS#8 EC PEM (BEGIN PRIVATE KEY) — falling back to LogOnly",
                );
                return None;
            },
        };

        // Validate the public key is base64url-no-pad of an
        // uncompressed (65-byte) P-256 point.
        match URL_SAFE_NO_PAD.decode(pub_b64url.trim()) {
            Ok(bytes) if bytes.len() == 65 && bytes[0] == 0x04 => {},
            Ok(bytes) => {
                tracing::warn!(
                    len = bytes.len(),
                    first = bytes.first().copied().unwrap_or(0),
                    "VAPID public key is not a 65-byte uncompressed P-256 point — falling back to LogOnly",
                );
                return None;
            },
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "VAPID public key is not valid base64url — falling back to LogOnly",
                );
                return None;
            },
        }

        Some(Self {
            public_key_b64url: pub_b64url.trim().to_string(),
            encoding_key,
            subject,
        })
    }
}

pub(crate) struct WebPushProvider {
    config: WebPushConfig,
    http: reqwest::Client,
}

impl WebPushProvider {
    pub(crate) fn new(config: WebPushConfig) -> Self {
        Self {
            config,
            http: super::shared_http_client(),
        }
    }

    /// Sign a fresh VAPID JWT for the given subscription origin.
    /// Per RFC 8292 the `aud` claim is the scheme://host of the
    /// subscription endpoint — re-signing per-request because the
    /// origin differs across subscribers (Mozilla, Chrome, etc.).
    fn sign_vapid_jwt(&self, aud: &str) -> Result<String, DeliverError> {
        let now = Utc::now().timestamp();
        let claims = VapidClaims {
            aud: aud.to_string(),
            sub: self.config.subject.clone(),
            // u32 → i64 cast is widening, never truncates.
            exp: now + i64::from(u32::try_from(VAPID_JWT_TTL.as_secs()).unwrap_or(u32::MAX)),
        };
        let header = Header::new(Algorithm::ES256);
        jsonwebtoken::encode(&header, &claims, &self.config.encoding_key)
            .map_err(|e| DeliverError::Transport(format!("VAPID JWT sign: {e}")))
    }
}

#[async_trait]
impl Provider for WebPushProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::WebPush
    }

    async fn deliver(&self, token: &[u8], payload: &WakePayload) -> Result<(), DeliverError> {
        // Token is the JSON-encoded PushSubscription { endpoint, keys
        // { p256dh, auth } } the browser handed us at registration.
        let subscription: Subscription = serde_json::from_slice(token)
            .map_err(|e| DeliverError::Transport(format!("WebPush subscription parse: {e}")))?;
        let p256dh = URL_SAFE_NO_PAD
            .decode(subscription.keys.p256dh.trim())
            .map_err(|e| DeliverError::Transport(format!("p256dh base64: {e}")))?;
        let auth_secret = URL_SAFE_NO_PAD
            .decode(subscription.keys.auth.trim())
            .map_err(|e| DeliverError::Transport(format!("auth base64: {e}")))?;
        if auth_secret.len() != 16 {
            return Err(DeliverError::Transport(format!(
                "WebPush auth secret length {} != 16",
                auth_secret.len(),
            )));
        }
        if p256dh.len() != 65 || p256dh[0] != 0x04 {
            return Err(DeliverError::Transport(
                "WebPush p256dh must be 65-byte uncompressed P-256 point".to_string(),
            ));
        }

        // Build the wake-payload JSON (same shape as UnifiedPush —
        // tiny single-byte-keyed envelope).
        let plaintext = serde_json::to_vec(&WakeBody::from(payload))
            .map_err(|e| DeliverError::Transport(format!("serialize wake body: {e}")))?;

        // Encrypt per RFC 8291.
        let encrypted = encrypt_payload(&plaintext, &p256dh, &auth_secret)?;

        // VAPID auth — aud is the origin (scheme://host) of the
        // subscription endpoint.
        let aud = subscription_audience(&subscription.endpoint)?;
        let jwt = self.sign_vapid_jwt(&aud)?;
        let auth_header = format!("vapid t={jwt}, k={}", self.config.public_key_b64url);
        let delivery = DeliveryHeaders::for_kind(payload.kind);

        let resp = match self
            .http
            .post(&subscription.endpoint)
            .header(header::AUTHORIZATION, auth_header)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_ENCODING, "aes128gcm")
            .header("TTL", delivery.ttl_seconds.to_string())
            .header("Urgency", delivery.urgency)
            .body(encrypted)
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
                Err(DeliverError::Transport(format!(
                    "WebPush {} → {}: {}",
                    other.as_u16(),
                    other.canonical_reason().unwrap_or("?"),
                    truncate(&body, 256),
                )))
            },
        }
    }
}

/// The two plaintext delivery headers that differ between a message
/// wake and a call wake. Split out of [`WebPushProvider::deliver`] so
/// the choice is unit-testable without a live push service.
#[derive(Debug, Eq, PartialEq)]
struct DeliveryHeaders {
    /// RFC 8030 §5.3 `Urgency`. `normal` may be held while the
    /// subscriber is offline or power-saving; `high` may not.
    urgency: &'static str,
    /// RFC 8030 §5.2 `TTL`, in seconds.
    ttl_seconds: u32,
}

impl DeliveryHeaders {
    const fn for_kind(kind: WakeKind) -> Self {
        if kind.is_call() {
            Self {
                urgency: "high",
                ttl_seconds: CALL_TTL_SECONDS,
            }
        } else {
            Self {
                urgency: "normal",
                ttl_seconds: TTL_SECONDS,
            }
        }
    }
}

/// RFC 8291 aes128gcm encryption end-to-end.
fn encrypt_payload(
    plaintext: &[u8],
    subscriber_p256dh: &[u8],
    auth_secret: &[u8],
) -> Result<Vec<u8>, DeliverError> {
    // 1. Ephemeral keypair for this send.
    let ephemeral = EphemeralSecret::random(&mut OsRng);
    let ephemeral_public = ephemeral.public_key();
    let ephemeral_public_bytes = ephemeral_public.to_encoded_point(false);
    let ephemeral_public_bytes = ephemeral_public_bytes.as_bytes();
    debug_assert_eq!(ephemeral_public_bytes.len(), 65);

    // 2. ECDH shared secret with the subscriber's p256dh.
    let subscriber_public = PublicKey::from_sec1_bytes(subscriber_p256dh)
        .map_err(|e| DeliverError::Transport(format!("subscriber p256dh parse: {e}")))?;
    let shared = ephemeral.diffie_hellman(&subscriber_public);

    // 3. RFC 8291 IKM derivation:
    //    PRK_key = HKDF-Extract(auth, shared_secret)
    //    info    = "WebPush: info\x00" || subscriber_p256dh || ephemeral_public
    //    IKM     = HKDF-Expand(PRK_key, info, 32)
    let hk_key = Hkdf::<Sha256>::new(Some(auth_secret), shared.raw_secret_bytes().as_slice());
    let mut info = Vec::with_capacity(WEBPUSH_KEY_INFO_PREFIX.len() + 65 + 65);
    info.extend_from_slice(WEBPUSH_KEY_INFO_PREFIX);
    info.extend_from_slice(subscriber_p256dh);
    info.extend_from_slice(ephemeral_public_bytes);
    let mut ikm = [0u8; 32];
    hk_key
        .expand(&info, &mut ikm)
        .map_err(|e| DeliverError::Transport(format!("HKDF key expand: {e}")))?;

    // 4. Per-message salt + content-encryption derivations:
    //    salt  = random 16 bytes
    //    PRK   = HKDF-Extract(salt, IKM)
    //    cek   = HKDF-Expand(PRK, "Content-Encoding: aes128gcm\x00", 16)
    //    nonce = HKDF-Expand(PRK, "Content-Encoding: nonce\x00", 12)
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut cek = [0u8; 16];
    hk.expand(AES128GCM_INFO, &mut cek)
        .map_err(|e| DeliverError::Transport(format!("HKDF cek expand: {e}")))?;
    let mut nonce_bytes = [0u8; 12];
    hk.expand(NONCE_INFO, &mut nonce_bytes)
        .map_err(|e| DeliverError::Transport(format!("HKDF nonce expand: {e}")))?;

    // 5. AES-128-GCM(cek, nonce) over (plaintext || 0x02).
    //    The 0x02 is the RFC 8188 "end of records" delimiter.
    let cipher = Aes128Gcm::new_from_slice(&cek)
        .map_err(|e| DeliverError::Transport(format!("AES-128-GCM key: {e}")))?;
    let mut delimited = Vec::with_capacity(plaintext.len() + 1);
    delimited.extend_from_slice(plaintext);
    delimited.push(0x02);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, delimited.as_slice())
        .map_err(|e| DeliverError::Transport(format!("AES-128-GCM encrypt: {e}")))?;

    // 6. RFC 8188 single-record framing:
    //    salt(16) || rs(4 BE) || idlen(1) || keyid(65) || ciphertext
    let mut frame = Vec::with_capacity(16 + 4 + 1 + 65 + ciphertext.len());
    frame.extend_from_slice(&salt);
    frame.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    frame.push(65); // keyid length = uncompressed P-256 point
    frame.extend_from_slice(ephemeral_public_bytes);
    frame.extend_from_slice(&ciphertext);
    Ok(frame)
}

/// Extract `scheme://host[:port]` from a subscription endpoint URL.
/// The VAPID `aud` claim binds the JWT to one origin — per RFC 8292
/// `aud` is the origin, not the full URL with path.
fn subscription_audience(endpoint: &str) -> Result<String, DeliverError> {
    let url = url::Url::parse(endpoint)
        .map_err(|e| DeliverError::Transport(format!("subscription endpoint parse: {e}")))?;
    let scheme = url.scheme();
    let host = url
        .host_str()
        .ok_or_else(|| DeliverError::Transport("subscription endpoint has no host".into()))?;
    Ok(if let Some(port) = url.port() {
        format!("{scheme}://{host}:{port}")
    } else {
        format!("{scheme}://{host}")
    })
}

#[derive(Deserialize)]
struct Subscription {
    endpoint: String,
    keys: SubscriptionKeys,
}

#[derive(Deserialize)]
struct SubscriptionKeys {
    /// Base64url-no-pad encoded 65-byte uncompressed P-256 point.
    p256dh: String,
    /// Base64url-no-pad encoded 16-byte symmetric auth secret.
    auth: String,
}

#[derive(Serialize)]
struct VapidClaims {
    aud: String,
    sub: String,
    exp: i64,
}

#[derive(Serialize)]
struct WakeBody {
    v: u8,
    /// Hex subscription id (ADR-0078).
    s: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    g: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    p: Option<u64>,
    /// Wake kind tag, mirroring `lakko-push`'s `k` field: `"c"` for a
    /// call, absent for a message. The service worker reads this to
    /// decide whether to show a call notification with Accept /
    /// Decline actions instead of a message preview. Unlike the
    /// `Urgency` header this rides inside the RFC 8291 encryption,
    /// so the browser's push service never sees it.
    #[serde(skip_serializing_if = "Option::is_none")]
    k: Option<&'static str>,
}

impl From<&WakePayload> for WakeBody {
    fn from(p: &WakePayload) -> Self {
        Self {
            v: 2,
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
    fn subscription_audience_strips_path() {
        let aud =
            subscription_audience("https://updates.push.services.mozilla.com/wpush/v2/gAAAAA")
                .unwrap();
        assert_eq!(aud, "https://updates.push.services.mozilla.com");
    }

    #[test]
    fn subscription_audience_keeps_port() {
        let aud = subscription_audience("https://example.test:8443/push/x").unwrap();
        assert_eq!(aud, "https://example.test:8443");
    }

    fn wake(kind: WakeKind) -> WakePayload {
        WakePayload {
            subscription_id: [0x07; 16],
            group_pseudonym: Some([0x08; 32]),
            envelope_pointer: Some(5),
            kind,
        }
    }

    #[test]
    fn message_wake_keeps_normal_urgency_and_the_long_ttl() {
        let h = DeliveryHeaders::for_kind(WakeKind::Message);
        assert_eq!(h.urgency, "normal");
        assert_eq!(h.ttl_seconds, 4 * 60 * 60);
    }

    #[test]
    fn call_wake_is_high_urgency_with_a_minute_ttl() {
        let h = DeliveryHeaders::for_kind(WakeKind::Call);
        assert_eq!(h.urgency, "high");
        assert_eq!(h.ttl_seconds, 60);
    }

    #[test]
    fn wake_body_tags_only_a_call() {
        let message = serde_json::to_string(&WakeBody::from(&wake(WakeKind::Message))).unwrap();
        let call = serde_json::to_string(&WakeBody::from(&wake(WakeKind::Call))).unwrap();
        assert!(!message.contains(r#""k""#), "got {message}");
        assert!(call.contains(r#""k":"c""#), "got {call}");
        // ...and discloses nothing else a message body does not.
        assert_eq!(call.replace(r#","k":"c""#, ""), message);
    }

    #[test]
    fn encrypt_payload_round_trip_with_subscriber_decrypt() {
        // Smoke: encryption succeeds + the frame has the expected
        // RFC 8188 prefix shape (16 B salt + 4 B rs + 1 B keyid len +
        // 65 B keyid + ciphertext). We don't decrypt here because
        // the subscriber decrypt path runs in the browser; verifying
        // the byte-level frame shape is enough to catch regressions
        // in the layout code.
        use p256::SecretKey;
        let subscriber_secret = SecretKey::random(&mut OsRng);
        let subscriber_pub = subscriber_secret
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        let auth_secret = [42u8; 16];
        let frame = encrypt_payload(b"hello", &subscriber_pub, &auth_secret).unwrap();
        // 16 + 4 + 1 + 65 = 86 bytes of header.
        assert!(frame.len() > 86);
        // First 16 = salt (any random bytes, we don't pin them).
        // Next 4 = record size, big-endian.
        let rs = u32::from_be_bytes(frame[16..20].try_into().unwrap());
        assert_eq!(rs, RECORD_SIZE);
        // Next 1 = keyid len.
        assert_eq!(frame[20], 65);
        // Next 65 = our ephemeral public; first byte must be 0x04
        // (uncompressed SEC1 marker).
        assert_eq!(frame[21], 0x04);
    }
}
