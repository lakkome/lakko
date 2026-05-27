// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! JWKS cache + admission verifier.
//!
//! The SFU pulls the auth service's
//! `/.well-known/jwks.json` once at startup and on a
//! configurable interval thereafter (default 5 min). Every
//! WebSocket admission verifies the client-supplied call-JWT
//! against the cached key set.
//!
//! The cache is **fail-fast on startup, best-effort on
//! refresh**: if the very first fetch errors, the SFU panics
//! during boot (better than admitting calls with a stale or
//! missing key). Subsequent refresh failures log a warning
//! but keep the previous snapshot live; this keeps brief auth-
//! service hiccups from cascading into call-admission
//! outages.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lakko_call_jwt::{CallJwtClaims, Jwks, JwtError};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::Config;

/// JWKS-related failure modes the public API exposes.
#[derive(Debug, Error)]
pub enum JwksError {
    /// The HTTP fetch could not be issued or returned a
    /// non-success status.
    #[error("jwks fetch failed: {0}")]
    Fetch(String),

    /// The response body could not be decoded as a JWKS doc.
    #[error("jwks decode failed: {0}")]
    Decode(String),

    /// The JWT failed verification against the current cache.
    /// Includes the underlying lakko-call-jwt error variant
    /// so callers can branch on (expired vs signature vs format).
    #[error("jwt verification failed: {0}")]
    Verify(JwtError),

    /// No JWK in the cache matched the JWT's `kid`. Either
    /// the auth service rotated and we have not refreshed
    /// yet, or the client is presenting a token from a fork.
    #[error("no JWK matches kid {kid:?}")]
    UnknownKid {
        /// kid the JWT claimed.
        kid: String,
    },
}

impl From<JwtError> for JwksError {
    fn from(value: JwtError) -> Self {
        Self::Verify(value)
    }
}

/// Cached JWKS doc — wraps a parsed [`Jwks`] in an Arc for
/// cheap snapshot reads, plus a refreshed-at timestamp for
/// future staleness alerts.
#[derive(Clone)]
pub struct JwksCache {
    inner: Arc<RwLock<Snapshot>>,
}

struct Snapshot {
    keys: Jwks,
    refreshed_at: SystemTime,
}

impl JwksCache {
    /// Construct directly from an already-loaded `Jwks`.
    /// Production code never calls this — it goes through
    /// [`Self::fetch_initial`] which hits the HTTPS endpoint.
    /// Integration tests use this constructor so they can
    /// pre-load a JWKS minted from a known test key.
    #[doc(hidden)]
    #[must_use]
    pub fn from_jwks_for_test(jwks: Jwks) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Snapshot {
                keys: jwks,
                refreshed_at: SystemTime::now(),
            })),
        }
    }

    /// Pull the JWKS once and store it. Returns `Err` on
    /// the very first failure — the caller (main.rs) panics,
    /// which is exactly what we want at boot.
    pub async fn fetch_initial(cfg: &Config) -> Result<Self, JwksError> {
        let keys = fetch_jwks(&cfg.jwks_url).await?;
        info!(url = %cfg.jwks_url, key_count = keys.keys.len(), "jwks initial fetch ok");
        Ok(Self {
            inner: Arc::new(RwLock::new(Snapshot {
                keys,
                refreshed_at: SystemTime::now(),
            })),
        })
    }

    /// Background-refresh task. Periodic; failures are
    /// logged and otherwise swallowed so a transient auth
    /// outage does not knock down the SFU.
    pub fn spawn_refresher(self, cfg: Config) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(cfg.jwks_refresh);
            // First tick fires immediately — skip; we
            // already fetched at startup.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match fetch_jwks(&cfg.jwks_url).await {
                    Ok(keys) => {
                        let mut guard = self.inner.write().await;
                        guard.keys = keys;
                        guard.refreshed_at = SystemTime::now();
                    },
                    Err(e) => {
                        warn!(error = %e, "jwks refresh failed; keeping previous snapshot");
                    },
                }
            }
        })
    }

    /// Verify a JWT against the currently-cached key set.
    /// Looks up the matching JWK by `kid`, then delegates to
    /// `lakko_call_jwt::verify_jwt`.
    pub async fn verify(&self, jwt: &str) -> Result<CallJwtClaims, JwksError> {
        // Parse the header without verifying so we can pick
        // the right key.
        let (header, _claims) = lakko_call_jwt::parse_unverified(jwt)?;
        let guard = self.inner.read().await;
        let jwk = guard
            .keys
            .keys
            .iter()
            .find(|k| k.kid == header.kid)
            .ok_or_else(|| JwksError::UnknownKid {
                kid: header.kid.clone(),
            })?;
        let vk = jwk.to_verifying_key()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map_or(0, |d| d.as_secs());
        let claims = lakko_call_jwt::verify_jwt(&vk, &jwk.kid, jwt, now)?;
        Ok(claims)
    }

    /// Read-only snapshot of when the cache was last
    /// refreshed. Useful for the `/ready` probe.
    pub async fn refreshed_at(&self) -> SystemTime {
        self.inner.read().await.refreshed_at
    }

    /// How long since the last successful refresh.
    pub async fn age(&self) -> Duration {
        let t = self.refreshed_at().await;
        SystemTime::now()
            .duration_since(t)
            .unwrap_or(Duration::ZERO)
    }
}

/// One-shot HTTPS GET → JSON-decode → [`Jwks`].
async fn fetch_jwks(url: &str) -> Result<Jwks, JwksError> {
    let resp = reqwest::Client::builder()
        .build()
        .map_err(|e| JwksError::Fetch(e.to_string()))?
        .get(url)
        .send()
        .await
        .map_err(|e| JwksError::Fetch(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(JwksError::Fetch(format!("HTTP {}", resp.status().as_u16())));
    }
    let body: Jwks = resp
        .json()
        .await
        .map_err(|e| JwksError::Decode(e.to_string()))?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use lakko_call_jwt::{
        kid_for, mint_jwt, CallJwtClaims, CallRole, CallSigningKey, Jwk, JWT_AUDIENCE, JWT_ISSUER,
        JWT_TTL_SECS,
    };

    /// Construct a JwksCache directly without an HTTP fetch
    /// — for unit testing the verify path.
    fn cache_from(keys: Jwks) -> JwksCache {
        JwksCache {
            inner: Arc::new(RwLock::new(Snapshot {
                keys,
                refreshed_at: SystemTime::now(),
            })),
        }
    }

    fn mint_test_token(seed: [u8; 32], expired: bool) -> (String, Jwks) {
        let key = CallSigningKey::from_seed(&seed);
        let kid = kid_for(&key.verifying);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = CallJwtClaims {
            iss: JWT_ISSUER.into(),
            aud: JWT_AUDIENCE.into(),
            sub: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
            call_id: "0123456789abcdef0123456789abcdef".into(),
            role: CallRole::Publisher,
            iat: if expired {
                now - JWT_TTL_SECS - 60
            } else {
                now
            },
            exp: if expired {
                now - 60
            } else {
                now + JWT_TTL_SECS
            },
        };
        let jwt = mint_jwt(&key, &claims).unwrap();
        let jwk = Jwk::from_signing_key(&key);
        assert_eq!(jwk.kid, kid);
        (jwt, Jwks { keys: vec![jwk] })
    }

    #[tokio::test]
    async fn verify_happy_path() {
        let (jwt, jwks) = mint_test_token([0x42; 32], false);
        let cache = cache_from(jwks);
        let claims = cache.verify(&jwt).await.unwrap();
        assert_eq!(claims.role, CallRole::Publisher);
    }

    #[tokio::test]
    async fn verify_rejects_expired() {
        let (jwt, jwks) = mint_test_token([0x43; 32], true);
        let cache = cache_from(jwks);
        let err = cache.verify(&jwt).await.unwrap_err();
        assert!(
            matches!(err, JwksError::Verify(JwtError::Expired)),
            "expected Expired, got {err:?}"
        );
    }

    #[tokio::test]
    async fn verify_rejects_unknown_kid() {
        let (jwt, _correct_jwks) = mint_test_token([0x44; 32], false);
        // Mint a different keypair, cache its JWK only — the
        // jwt's kid will not match.
        let other = CallSigningKey::from_seed(&[0x99; 32]);
        let cache = cache_from(Jwks {
            keys: vec![Jwk::from_signing_key(&other)],
        });
        let err = cache.verify(&jwt).await.unwrap_err();
        assert!(matches!(err, JwksError::UnknownKid { .. }));
    }

    #[tokio::test]
    async fn verify_rejects_tampered_signature() {
        let (jwt, jwks) = mint_test_token([0x45; 32], false);
        // Replace the signature segment entirely with a
        // valid-length, valid-base64url but cryptographically
        // wrong signature. 64 zero bytes encodes cleanly,
        // decodes as a valid `Signature` length, and is
        // overwhelmingly unlikely to verify against the
        // payload. The point of the test is the rejection,
        // not the specific JwtError variant — dalek may
        // surface this as `Signature` or as a length /
        // format mismatch depending on internal validation
        // order. Either way `Verify(_)` covers it.
        let parts: Vec<&str> = jwt.splitn(3, '.').collect();
        let bogus_sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 64]);
        let bad = format!("{}.{}.{bogus_sig}", parts[0], parts[1]);
        let cache = cache_from(jwks);
        let err = cache.verify(&bad).await.unwrap_err();
        assert!(
            matches!(err, JwksError::Verify(_)),
            "expected Verify(_), got {err:?}"
        );
    }
}
