// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0101 S2 — the continuity binding of an account that reset its keys.
//!
//! After an ADR-0073 key reset, the resetting device announces a second
//! mailbox (the continuity mailbox) for a window, in a 105-byte binding
//! signed by its new identity key (`lakko_contacts::continuity`). This
//! module stores and serves it:
//!
//! - `PUT /discovery/identity/continuity` (bearer session, under the
//!   ADR-0073 generation floor): body `{"continuity_binding_b64": <standard
//!   base64>}`. The binding is verified against the identity key the
//!   account's row holds NOW, at the server's clock; an invalid one is `400
//!   {"error":"continuity-binding-invalid"}`. A later upload replaces it.
//! - `GET /discovery/identity/{account_id}` serves it as
//!   `continuity_binding_b64` while unexpired (see
//!   [`crate::identity_bundle::fetch`] and [`served_binding`]).
//! - An hourly sweep nulls expired bindings ([`spawn_sweeper`]).
//! - A key change on `POST /discovery/identity` clears it (a binding names
//!   the key that signed it).
//!
//! Discovery parses the binding only to verify it; the bytes stored and
//! served are the uploaded ones (invariant #2: a public artefact signed by a
//! public key). No log line names the account or the binding.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;

use crate::key_reset::{json_error, RouteError, CODE_KEY_GENERATION_SUPERSEDED};
use crate::AppState;

/// `400` body of a binding that does not decode or verify against the
/// account's current identity key at the server's clock.
pub(crate) const CODE_CONTINUITY_BINDING_INVALID: &str = "continuity-binding-invalid";

/// `404` body of a binding upload for an account with no identity bundle
/// (nothing to verify it against).
pub(crate) const CODE_IDENTITY_NOT_FOUND: &str = "identity-bundle-not-found";

/// Upper bound on the base64 text, checked before decoding. The binding is
/// 105 bytes (140 characters of padded base64).
const MAX_BINDING_B64_LEN: usize = 256;

/// How often the sweep runs.
const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

/// Upper bound on the rows one sweep statement clears, so a large backlog is
/// cleared in short statements.
const SWEEP_BATCH: u32 = 10_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutRequest {
    /// Standard base64 of the 105-byte binding.
    continuity_binding_b64: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PutResponse {
    ok: bool,
}

/// The server's clock in unix milliseconds.
pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// What the fetch serves: the stored binding while `now_ms` is before its
/// expiry, nothing otherwise (expired, or no binding).
pub(crate) fn served_binding(
    binding: Option<Vec<u8>>,
    expires_at_ms: Option<u64>,
    now_ms: u64,
) -> Option<String> {
    match (binding, expires_at_ms) {
        (Some(bytes), Some(expiry)) if now_ms < expiry => Some(B64.encode(bytes)),
        _ => None,
    }
}

fn db_error(e: &sqlx::Error) -> RouteError {
    tracing::error!(error = %e, "continuity binding store");
    RouteError::Text(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal db error".to_string(),
    )
}

/// `PUT /discovery/identity/continuity`.
///
/// # Errors
///
/// `400 continuity-binding-invalid`, `401 key-generation-superseded`, `404
/// identity-bundle-not-found`, `500` on a database failure.
pub(crate) async fn put(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    axum::Extension(generation): axum::Extension<crate::auth::SessionKeyGeneration>,
    Json(body): Json<PutRequest>,
) -> Result<Json<PutResponse>, RouteError> {
    let invalid = || json_error(StatusCode::BAD_REQUEST, CODE_CONTINUITY_BINDING_INVALID);
    if body.continuity_binding_b64.len() > MAX_BINDING_B64_LEN {
        return Err(invalid());
    }
    let bytes = B64
        .decode(body.continuity_binding_b64.as_bytes())
        .map_err(|_| invalid())?;
    let binding = lakko_contacts::ContinuityBinding::decode(&bytes).map_err(|_| invalid())?;

    let mut tx = state.db.begin().await.map_err(|e| db_error(&e))?;
    let row: Option<(Vec<u8>, u64)> = sqlx::query_as(
        "SELECT signature_pubkey, key_generation FROM identity_bundles \
         WHERE account_id = ? FOR UPDATE",
    )
    .bind(&authed.0[..])
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| db_error(&e))?;
    let Some((signature_pubkey, stored_generation)) = row else {
        return Err(json_error(StatusCode::NOT_FOUND, CODE_IDENTITY_NOT_FOUND));
    };
    if crate::key_reset::floor_decision(Some(stored_generation), generation.0) {
        tracing::warn!("continuity binding refused: session key generation superseded");
        return Err(json_error(
            StatusCode::UNAUTHORIZED,
            CODE_KEY_GENERATION_SUPERSEDED,
        ));
    }
    let identity_pk: [u8; 32] = signature_pubkey.as_slice().try_into().map_err(|_| {
        tracing::error!("identity_bundles row has an unexpected key length");
        RouteError::Text(
            StatusCode::INTERNAL_SERVER_ERROR,
            "stored bundle has unexpected key lengths".to_string(),
        )
    })?;
    if let Err(e) = binding.verify(&identity_pk, now_unix_ms()) {
        tracing::info!(reason = e.code(), "continuity binding refused");
        return Err(invalid());
    }
    sqlx::query(
        "UPDATE identity_bundles SET continuity_binding = ?, continuity_expires_at_ms = ? \
         WHERE account_id = ?",
    )
    .bind(&bytes[..])
    .bind(binding.expires_at_ms())
    .bind(&authed.0[..])
    .execute(&mut *tx)
    .await
    .map_err(|e| db_error(&e))?;
    tx.commit().await.map_err(|e| db_error(&e))?;
    tracing::info!("continuity binding stored");
    Ok(Json(PutResponse { ok: true }))
}

/// Null every binding whose expiry is at or before `now_ms`. Returns the
/// number of rows cleared.
///
/// # Errors
///
/// A database failure.
pub(crate) async fn sweep_expired(db: &MySqlPool, now_ms: u64) -> Result<u64, sqlx::Error> {
    let mut cleared = 0u64;
    loop {
        let done = sqlx::query(
            "UPDATE identity_bundles \
             SET continuity_binding = NULL, continuity_expires_at_ms = NULL \
             WHERE continuity_expires_at_ms IS NOT NULL AND continuity_expires_at_ms <= ? \
             LIMIT ?",
        )
        .bind(now_ms)
        .bind(SWEEP_BATCH)
        .execute(db)
        .await?
        .rows_affected();
        cleared += done;
        if done < u64::from(SWEEP_BATCH) {
            return Ok(cleared);
        }
    }
}

/// Spawn the hourly sweep of expired continuity bindings (heartbeat
/// `discovery_continuity_binding_sweep`).
pub(crate) fn spawn_sweeper(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    lakko_metrics::job_registered(crate::metrics::jobs::CONTINUITY_BINDING_SWEEP);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SWEEP_INTERVAL);
        loop {
            interval.tick().await;
            let result = sweep_expired(&pool, now_unix_ms()).await;
            crate::metrics::job_run(crate::metrics::jobs::CONTINUITY_BINDING_SWEEP, &result);
            match result {
                Ok(0) => {},
                Ok(cleared) => tracing::info!(cleared, "swept expired continuity bindings"),
                Err(e) => tracing::warn!(error = %e, "continuity binding sweep failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_is_served_only_before_its_expiry() {
        assert_eq!(
            served_binding(Some(vec![1, 2]), Some(10), 9).as_deref(),
            Some("AQI=")
        );
        assert_eq!(served_binding(Some(vec![1, 2]), Some(10), 10), None);
        assert_eq!(served_binding(Some(vec![1, 2]), Some(10), 11), None);
        assert_eq!(served_binding(None, Some(10), 1), None);
        assert_eq!(served_binding(Some(vec![1]), None, 1), None);
    }
}

/// Route tests, driven through the real router. DB-gated on
/// `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skip, not fail, when unset).
#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::anon_token_route_tests::{state, test_pool};
    use crate::build_app;
    use axum::body::Body;
    use axum::http::Request;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use http_body_util::BodyExt as _;
    use lakko_attest::KeyResetAttestation;
    use lakko_contacts::{ContinuityBinding, CONTINUITY_WINDOW_MS};
    use lakko_crypto::signing::SigningKey;
    use tower::ServiceExt as _;

    fn attest_key() -> SigningKey {
        SigningKey::from_seed(&[0x66; 32])
    }

    fn fresh_account(tag: u8) -> [u8; 16] {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut out = [tag; 16];
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        out[4..12].copy_from_slice(&now.to_be_bytes());
        out[12..].copy_from_slice(
            &u32::try_from(NEXT.fetch_add(1, Ordering::Relaxed) % u64::from(u32::MAX))
                .unwrap_or(0)
                .to_be_bytes(),
        );
        out
    }

    struct Client {
        app: axum::Router,
        state: AppState,
    }

    impl Client {
        fn new(db: MySqlPool) -> Self {
            let mut state = state(db, false);
            state.key_reset_attest_key = Some(attest_key().verifying_key());
            state.fresh_identity_cooldown_secs = 0;
            Self {
                app: build_app(state.clone()),
                state,
            }
        }

        fn token(&self, account: &[u8; 16], generation: u64) -> String {
            let now = i64::try_from(now_unix_ms()).unwrap_or(i64::MAX);
            self.state
                .session_token_secret
                .issue(account, generation, &[0u8; 32], 0, now + 60_000)
                .unwrap()
        }

        async fn call(
            &self,
            method: &str,
            uri: &str,
            token: Option<&str>,
            body: serde_json::Value,
        ) -> (StatusCode, String) {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header("x-real-ip", "192.0.2.66")
                .header("content-type", "application/json");
            if let Some(t) = token {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            let resp = self
                .app
                .clone()
                .oneshot(b.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }

        async fn publish(
            &self,
            account: &[u8; 16],
            generation: u64,
            seed: u8,
            attest: Option<u64>,
        ) -> (StatusCode, String) {
            let signing = SigningKey::from_seed(&[seed; 32]);
            let key = signing.verifying_key().to_bytes();
            let mut body = serde_json::json!({
                "account_id": B64.encode(account),
                "signature_pubkey": B64.encode(key),
                "x25519_pubkey": B64.encode([0x33u8; 32]),
                "x25519_binding": B64.encode(lakko_contacts::sign_x25519_binding(&signing, &[0x33u8; 32])),
            });
            if let Some(g) = attest {
                body["key_reset_attest"] = serde_json::Value::String(B64.encode(
                    KeyResetAttestation::for_account(account, g, key, 1_000).sign(&attest_key()),
                ));
            }
            self.call(
                "POST",
                "/discovery/identity",
                Some(&self.token(account, generation)),
                body,
            )
            .await
        }

        async fn put_binding(
            &self,
            account: &[u8; 16],
            generation: u64,
            binding: &[u8],
        ) -> (StatusCode, String) {
            self.call(
                "PUT",
                "/discovery/identity/continuity",
                Some(&self.token(account, generation)),
                serde_json::json!({ "continuity_binding_b64": B64.encode(binding) }),
            )
            .await
        }

        async fn served(&self, account: &[u8; 16]) -> Option<String> {
            let (status, text) = self
                .call(
                    "GET",
                    &format!("/discovery/identity/{}", URL_SAFE_NO_PAD.encode(account)),
                    None,
                    serde_json::Value::Null,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{text}");
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            v["continuity_binding_b64"].as_str().map(str::to_owned)
        }
    }

    fn error_code(body: &str) -> String {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    fn binding(seed: u8, expires_at_ms: u64) -> Vec<u8> {
        ContinuityBinding::for_account_seed(&[seed; 32], expires_at_ms)
            .encode()
            .to_vec()
    }

    #[tokio::test]
    async fn a_verified_binding_is_stored_served_and_cleared_by_a_key_change() {
        let Some(pool) = test_pool("continuity_binding_store").await else {
            return;
        };
        let c = Client::new(pool.clone());
        let account = fresh_account(0xC1);
        let now = now_unix_ms();

        // No bundle yet: nothing to verify against.
        let (s, b) = c
            .put_binding(&account, 1, &binding(0x51, now + CONTINUITY_WINDOW_MS))
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
        assert_eq!(error_code(&b), CODE_IDENTITY_NOT_FOUND);

        let (s, b) = c.publish(&account, 1, 0x51, None).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(c.served(&account).await, None);

        // Refusals: another key's binding, expired, too far ahead, garbage,
        // and an over-long body.
        for bad in [
            binding(0x52, now + CONTINUITY_WINDOW_MS),
            binding(0x51, now.saturating_sub(1)),
            binding(0x51, now + 2 * CONTINUITY_WINDOW_MS),
            vec![0x01; 105],
            vec![0x02; 105],
            vec![0x01; 10],
        ] {
            let (s, b) = c.put_binding(&account, 1, &bad).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
            assert_eq!(error_code(&b), CODE_CONTINUITY_BINDING_INVALID);
        }
        let (s, b) = c
            .call(
                "PUT",
                "/discovery/identity/continuity",
                Some(&c.token(&account, 1)),
                serde_json::json!({ "continuity_binding_b64": "A".repeat(400) }),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
        assert_eq!(c.served(&account).await, None);

        // Unauthenticated.
        let (s, _) = c
            .call(
                "PUT",
                "/discovery/identity/continuity",
                None,
                serde_json::json!({ "continuity_binding_b64": B64.encode(binding(0x51, now + 1000)) }),
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);

        // A valid one is stored and served verbatim.
        let good = binding(0x51, now + CONTINUITY_WINDOW_MS);
        let (s, b) = c.put_binding(&account, 1, &good).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(c.served(&account).await, Some(B64.encode(&good)));

        // A same-key re-upload of the bundle keeps it.
        let (s, b) = c.publish(&account, 1, 0x51, None).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(c.served(&account).await, Some(B64.encode(&good)));

        // An attested key change clears it.
        let (s, b) = c.publish(&account, 2, 0x53, Some(2)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(c.served(&account).await, None);

        // The generation floor: a token of generation 1 is superseded now.
        let (s, b) = c
            .put_binding(&account, 1, &binding(0x53, now + CONTINUITY_WINDOW_MS))
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{b}");
        assert_eq!(error_code(&b), CODE_KEY_GENERATION_SUPERSEDED);
        // The old key's binding is refused under the new generation too.
        let (s, b) = c.put_binding(&account, 2, &good).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
        let fresh = binding(0x53, now + CONTINUITY_WINDOW_MS);
        let (s, b) = c.put_binding(&account, 2, &fresh).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(c.served(&account).await, Some(B64.encode(&fresh)));
    }

    #[tokio::test]
    async fn an_expired_binding_is_not_served_and_the_sweep_clears_it() {
        let Some(pool) = test_pool("continuity_binding_sweep").await else {
            return;
        };
        let c = Client::new(pool.clone());
        let account = fresh_account(0xC2);
        let (s, b) = c.publish(&account, 1, 0x54, None).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let now = now_unix_ms();
        let good = binding(0x54, now + CONTINUITY_WINDOW_MS);
        let (s, b) = c.put_binding(&account, 1, &good).await;
        assert_eq!(s, StatusCode::OK, "{b}");

        // Move the stored expiry into the past: no longer served.
        sqlx::query(
            "UPDATE identity_bundles SET continuity_expires_at_ms = ? WHERE account_id = ?",
        )
        .bind(now - 1)
        .bind(&account[..])
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(c.served(&account).await, None);

        assert!(sweep_expired(&pool, now_unix_ms()).await.unwrap() >= 1);
        let row: (Option<Vec<u8>>, Option<u64>) = sqlx::query_as(
            "SELECT continuity_binding, continuity_expires_at_ms FROM identity_bundles \
             WHERE account_id = ?",
        )
        .bind(&account[..])
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, (None, None));
    }
}
