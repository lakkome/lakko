// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0065 S3 — the key-reset half of the identity directory.
//!
//! A reset (`POST /account/keys/reset/finish` on `services/auth`) bumps the
//! account's key generation and hands the resetting device a signed
//! [`KeyResetAttestation`]. Three things in this service hang on it:
//!
//! - **The pinned attest key** (`LAKKO_DISCOVERY_ATTEST_KEY`, base64 of auth's
//!   32-byte Ed25519 attest verifying key — the same key auth publishes at
//!   `GET /.well-known/device-attest-jwks.json`). Pinned at deploy time, as
//!   the transparency service pins it for the genesis gate and the clients
//!   pin it for device removal (ADR-0015); never fetched. Unset: an upload
//!   that carries an attestation answers `503 key-reset-disabled`, and since
//!   ADR-0065 retired every unattested key change, no key can change at all
//!   without a supersession proof — fail-closed, logged loudly at startup.
//! - **The attestation check** ([`verify_upload_attestation`]): signature
//!   under the pinned key, `account_ref` of the session account, the uploaded
//!   key. The generation comparison is the caller's, inside the row lock.
//! - **The generation floor** ([`floor_refuses`]): the two identity writes
//!   refuse a session token whose generation is below the row's.
//!
//! Every refusal here is a JSON `{"error": <code>}` body, the one shape a
//! client branches on (ADR-0065 sections 3.5 and 4.1).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_attest::{
    key_reset_account_ref, verify_key_reset_binding, KeyResetAttestation, KEY_RESET_ATTESTATION_LEN,
};
use lakko_crypto::signing::VerifyingKey;
use serde_json::json;
use sqlx::MySqlPool;

/// The environment variable the auth attest verifying key is pinned from.
pub(crate) const ATTEST_KEY_ENV: &str = "LAKKO_DISCOVERY_ATTEST_KEY";

/// `401` body of a session token issued under a key generation below the
/// row's (ADR-0065 section 4.1): this device's keys were reset elsewhere.
pub(crate) const CODE_KEY_GENERATION_SUPERSEDED: &str = "key-generation-superseded";

/// `409` body of a key change that carries neither a supersession proof nor
/// a key-reset attestation (ADR-0065 section 4.1).
pub(crate) const CODE_NEEDS_ATTESTATION: &str = "identity-change-needs-attestation";

/// `409` body of an attested key change whose generation is not strictly
/// above the row's: a replay of an applied reset, or a reset an even later
/// one has overtaken.
pub(crate) const CODE_GENERATION_NOT_NEWER: &str = "key-generation-not-newer";

/// `400` body of an attestation that does not verify: wrong length or
/// version, a signature not under the pinned key, another account's
/// `account_ref`, or a `new_signature_pubkey` that is not the uploaded key.
pub(crate) const CODE_ATTESTATION_INVALID: &str = "key-reset-attestation-invalid";

/// `503` body of an attested upload when no attest key is pinned.
pub(crate) const CODE_KEY_RESET_DISABLED: &str = "key-reset-disabled";

/// `429` body of an attested key change inside the ADR-0017 cool-down
/// window (kept as defence in depth on attested changes, ADR-0065 4.1).
pub(crate) const CODE_COOLING_DOWN: &str = "identity-changed-too-recently";

/// An error of the two identity writes: the plain-text refusals those routes
/// always had, or one of the JSON `{"error": code}` codes above. Kept small
/// (not a built `Response`) so it rides in a `Result` without clippy's
/// large-error objection.
#[derive(Debug)]
pub(crate) enum RouteError {
    /// `(status, message)` as the rest of the service answers.
    Text(StatusCode, String),
    /// `status` with the JSON body `{"error": code}`.
    Code(StatusCode, &'static str),
}

impl From<(StatusCode, String)> for RouteError {
    fn from((status, message): (StatusCode, String)) -> Self {
        Self::Text(status, message)
    }
}

impl IntoResponse for RouteError {
    fn into_response(self) -> Response {
        match self {
            Self::Text(status, message) => (status, message).into_response(),
            Self::Code(status, code) => (status, Json(json!({ "error": code }))).into_response(),
        }
    }
}

/// A JSON `{"error": code}` error with `status`.
pub(crate) const fn json_error(status: StatusCode, code: &'static str) -> RouteError {
    RouteError::Code(status, code)
}

/// Read `LAKKO_DISCOVERY_ATTEST_KEY` through `lookup`: base64 of a 32-byte
/// Ed25519 verifying key, or unset / blank for `None`.
///
/// # Errors
///
/// A message naming the variable when it is set but is not a valid key: a
/// malformed pin refuses startup rather than silently refusing every reset.
pub(crate) fn attest_key_from_lookup(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<VerifyingKey>, String> {
    let Some(raw) = lookup(ATTEST_KEY_ENV).map(|v| v.trim().to_owned()) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let bytes = B64
        .decode(raw.as_bytes())
        .map_err(|_| format!("{ATTEST_KEY_ENV} is not valid base64"))?;
    VerifyingKey::from_bytes(&bytes)
        .map(Some)
        .map_err(|_| format!("{ATTEST_KEY_ENV} is not a valid 32-byte Ed25519 public key"))
}

/// Decode and verify the `key_reset_attest` field of an identity upload
/// against the pinned key, the session account and the uploaded key.
///
/// `None` in, `None` out (the upload carries no attestation). Otherwise the
/// attestation must verify under `attest_key` (`503 key-reset-disabled` when
/// none is pinned), name `key_reset_account_ref(account_id)` and name
/// `signature_pubkey` — every failure is `400 key-reset-attestation-invalid`
/// (a stale-but-valid attestation is not refused here; the generation is
/// compared against the row under its lock). Returns the verified claims and
/// the raw bytes to store.
///
/// # Errors
///
/// The error described above.
pub(crate) fn verify_upload_attestation(
    field: Option<&str>,
    attest_key: Option<&VerifyingKey>,
    account_id: &[u8; 16],
    signature_pubkey: &[u8; 32],
) -> Result<Option<(KeyResetAttestation, Vec<u8>)>, RouteError> {
    let Some(b64) = field else {
        return Ok(None);
    };
    let bytes = B64.decode(b64).map_err(|e| {
        RouteError::Text(
            StatusCode::BAD_REQUEST,
            format!("invalid key_reset_attest base64: {e}"),
        )
    })?;
    if bytes.len() != KEY_RESET_ATTESTATION_LEN {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            CODE_ATTESTATION_INVALID,
        ));
    }
    let Some(key) = attest_key else {
        return Err(json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            CODE_KEY_RESET_DISABLED,
        ));
    };
    let expected_journal = lakko_attest::device_group_journal_id(account_id);
    let claims = verify_key_reset_binding(
        &bytes,
        key,
        &key_reset_account_ref(account_id),
        &expected_journal,
        signature_pubkey,
    )
    .map_err(|e| {
        tracing::warn!(error = %e, "identity upload: key-reset attestation refused");
        json_error(StatusCode::BAD_REQUEST, CODE_ATTESTATION_INVALID)
    })?;
    Ok(Some((claims, bytes)))
}

/// The ADR-0065 generation floor: `true` when a session token issued under
/// `token_generation` is below the account's stored `key_generation`. An
/// account with no identity bundle has no floor (a first publish). A token
/// of a higher generation than the row is accepted (the attested upload has
/// not landed yet) and raises nothing.
///
/// # Errors
///
/// A `500` on a database failure.
pub(crate) async fn floor_refuses(
    db: &MySqlPool,
    account_id: &[u8; 16],
    token_generation: u64,
) -> Result<bool, RouteError> {
    let stored: Option<u64> =
        sqlx::query_scalar("SELECT key_generation FROM identity_bundles WHERE account_id = ?")
            .bind(&account_id[..])
            .fetch_optional(db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "read identity key_generation for the floor");
                RouteError::Text(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;
    Ok(floor_decision(stored, token_generation))
}

/// Pure half of [`floor_refuses`]: refuse only when a row exists and the
/// token's generation is strictly below it.
pub(crate) const fn floor_decision(stored: Option<u64>, token_generation: u64) -> bool {
    match stored {
        Some(row) => token_generation < row,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    const ACCOUNT: [u8; 16] = [0xA5; 16];
    const KEY: [u8; 32] = [0x5A; 32];

    fn attest_key() -> SigningKey {
        SigningKey::from_seed(&[0x31; 32])
    }

    fn attestation(generation: u64, pubkey: [u8; 32]) -> String {
        B64.encode(
            KeyResetAttestation::for_account(&ACCOUNT, generation, pubkey, 1_000)
                .sign(&attest_key()),
        )
    }

    #[test]
    fn the_attest_key_is_base64_or_unset() {
        let env = |v: Option<&'static str>| move |_: &str| v.map(str::to_owned);
        assert!(attest_key_from_lookup(env(None)).unwrap().is_none());
        assert!(attest_key_from_lookup(env(Some("  "))).unwrap().is_none());
        let pinned = B64.encode(attest_key().verifying_key().to_bytes());
        let leaked: &'static str = Box::leak(pinned.into_boxed_str());
        assert_eq!(
            attest_key_from_lookup(env(Some(leaked))).unwrap(),
            Some(attest_key().verifying_key())
        );
        assert!(attest_key_from_lookup(env(Some("not base64!"))).is_err());
        let short: &'static str = Box::leak(B64.encode([1u8; 16]).into_boxed_str());
        assert!(attest_key_from_lookup(env(Some(short))).is_err());
    }

    #[test]
    fn floor_decision_refuses_only_a_lower_generation() {
        assert!(!floor_decision(None, 1));
        assert!(!floor_decision(Some(1), 1));
        assert!(!floor_decision(Some(1), 2), "a higher token raises nothing");
        assert!(floor_decision(Some(2), 1));
        assert!(floor_decision(Some(u64::MAX), u64::MAX - 1));
    }

    #[test]
    fn an_absent_field_is_no_attestation() {
        let vk = attest_key().verifying_key();
        assert!(verify_upload_attestation(None, Some(&vk), &ACCOUNT, &KEY)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_valid_attestation_verifies_and_yields_its_bytes() {
        let vk = attest_key().verifying_key();
        let field = attestation(2, KEY);
        let (claims, bytes) = verify_upload_attestation(Some(&field), Some(&vk), &ACCOUNT, &KEY)
            .unwrap()
            .unwrap();
        assert_eq!(claims.key_generation, 2);
        assert_eq!(bytes, B64.decode(field).unwrap());
    }

    #[test]
    fn no_pinned_key_is_503_on_the_attested_arm_only() {
        let field = attestation(2, KEY);
        let err = verify_upload_attestation(Some(&field), None, &ACCOUNT, &KEY).unwrap_err();
        assert_eq!(
            err.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(verify_upload_attestation(None, None, &ACCOUNT, &KEY)
            .unwrap()
            .is_none());
    }

    #[test]
    fn every_binding_failure_is_400() {
        let vk = attest_key().verifying_key();
        // Wrong key on the signature.
        let forged = B64.encode(
            KeyResetAttestation::for_account(&ACCOUNT, 2, KEY, 1_000)
                .sign(&SigningKey::from_seed(&[0x32; 32])),
        );
        for (name, field, account, key) in [
            ("wrong signing key", forged, ACCOUNT, KEY),
            ("another account", attestation(2, KEY), [0xA6; 16], KEY),
            ("another pubkey", attestation(2, KEY), ACCOUNT, [0x5B; 32]),
            (
                "wrong length",
                B64.encode([0u8; KEY_RESET_ATTESTATION_LEN - 1]),
                ACCOUNT,
                KEY,
            ),
        ] {
            let err =
                verify_upload_attestation(Some(&field), Some(&vk), &account, &key).unwrap_err();
            assert_eq!(
                err.into_response().status(),
                StatusCode::BAD_REQUEST,
                "{name}"
            );
        }
        // Not base64 at all.
        let err = verify_upload_attestation(Some("!!"), Some(&vk), &ACCOUNT, &KEY).unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }
}

/// ADR-0065 S3 route tests, driven through the real router. DB-gated on
/// `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skip, not fail, when unset).
#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::anon_token_route_tests::{state, test_pool};
    use crate::{build_app, AppState};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use lakko_crypto::signing::SigningKey;
    use tower::ServiceExt as _;

    fn attest_key() -> SigningKey {
        SigningKey::from_seed(&[0x65; 32])
    }

    /// A fresh account id per test so parallel tests never share a row.
    fn fresh_account(tag: u8) -> [u8; 16] {
        let mut out = [tag; 16];
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        out[8..].copy_from_slice(&now.to_be_bytes());
        out
    }

    fn attestation(account: &[u8; 16], generation: u64, pubkey: [u8; 32]) -> String {
        B64.encode(
            KeyResetAttestation::for_account(account, generation, pubkey, 1_000)
                .sign(&attest_key()),
        )
    }

    struct Client {
        app: axum::Router,
        state: AppState,
    }

    impl Client {
        fn new(db: MySqlPool, pinned: bool, cooldown_secs: i64) -> Self {
            let mut state = state(db, false);
            state.key_reset_attest_key = pinned.then(|| attest_key().verifying_key());
            state.fresh_identity_cooldown_secs = cooldown_secs;
            Self {
                app: build_app(state.clone()),
                state,
            }
        }

        fn token(&self, account: &[u8; 16], generation: u64) -> String {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            self.state
                .session_token_secret
                .issue(account, generation, now + 60_000)
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
                .header("x-real-ip", "192.0.2.65")
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

        /// `POST /discovery/identity` with `extra` fields merged in.
        async fn publish(
            &self,
            account: &[u8; 16],
            generation: u64,
            key: [u8; 32],
            extra: serde_json::Value,
        ) -> (StatusCode, String) {
            let mut body = serde_json::json!({
                "account_id": B64.encode(account),
                "signature_pubkey": B64.encode(key),
                "x25519_pubkey": B64.encode([0x33u8; 32]),
            });
            if let (Some(dst), Some(src)) = (body.as_object_mut(), extra.as_object()) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
            self.call(
                "POST",
                "/discovery/identity",
                Some(&self.token(account, generation)),
                body,
            )
            .await
        }

        async fn publish_keypackage(
            &self,
            account: &[u8; 16],
            generation: u64,
            device: u8,
        ) -> (StatusCode, String) {
            self.call(
                "POST",
                "/discovery/keypackage",
                Some(&self.token(account, generation)),
                serde_json::json!({
                    "account_id": B64.encode(account),
                    "device_id": B64.encode([device; 32]),
                    "keypackage": B64.encode([device; 64]),
                }),
            )
            .await
        }

        async fn fetch(&self, account: &[u8; 16]) -> serde_json::Value {
            use base64::engine::general_purpose::URL_SAFE_NO_PAD;
            let (status, text) = self
                .call(
                    "GET",
                    &format!("/discovery/identity/{}", URL_SAFE_NO_PAD.encode(account)),
                    None,
                    serde_json::Value::Null,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{text}");
            serde_json::from_str(&text).unwrap()
        }
    }

    fn error_code(body: &str) -> String {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    async fn keypackage_rows(pool: &MySqlPool, account: &[u8; 16]) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM keypackages WHERE account_id = ?")
            .bind(&account[..])
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn stored_attestation(pool: &MySqlPool, account: &[u8; 16]) -> Option<Vec<u8>> {
        sqlx::query_scalar("SELECT key_reset_attest FROM identity_bundles WHERE account_id = ?")
            .bind(&account[..])
            .fetch_one(pool)
            .await
            .unwrap()
    }

    const K1: [u8; 32] = [0x11; 32];
    const K2: [u8; 32] = [0x22; 32];
    const K3: [u8; 32] = [0x33; 32];
    const K4: [u8; 32] = [0x44; 32];

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn an_attested_reset_rewrites_the_directory_and_the_old_generation_is_gated() {
        let Some(pool) = test_pool("an_attested_reset_rewrites_the_directory").await else {
            return;
        };
        let c = Client::new(pool.clone(), true, 0);
        let acct = fresh_account(0x65);

        // Genesis publish at generation 1, two devices in the directory.
        let (status, text) = c.publish(&acct, 1, K1, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(c.fetch(&acct).await["key_generation"], 1);
        for device in [0xD1, 0xD2] {
            let (status, text) = c.publish_keypackage(&acct, 1, device).await;
            assert_eq!(status, StatusCode::OK, "{text}");
        }
        assert_eq!(keypackage_rows(&pool, &acct).await, 2);

        // The retired heal: an unattested, unproven key change is refused,
        // and nothing moved.
        let (status, text) = c.publish(&acct, 1, K2, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        assert_eq!(error_code(&text), CODE_NEEDS_ATTESTATION);
        assert_eq!(c.fetch(&acct).await["signature_pubkey"], B64.encode(K1));

        // The attested reset: generation 2, the uploaded key, the session
        // account. Accepted once; the generation is stored, the attestation
        // is stored, every KeyPackage row is gone.
        let att2 = attestation(&acct, 2, K2);
        let (status, text) = c
            .publish(
                &acct,
                2,
                K2,
                serde_json::json!({ "key_reset_attest": att2 }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let fetched = c.fetch(&acct).await;
        assert_eq!(fetched["signature_pubkey"], B64.encode(K2));
        assert_eq!(fetched["key_generation"], 2);
        assert_eq!(
            stored_attestation(&pool, &acct).await,
            Some(B64.decode(&att2).unwrap())
        );
        assert_eq!(keypackage_rows(&pool, &acct).await, 0);

        // The resetting device re-publishes its KeyPackage under the new
        // generation; a same-key re-upload (with or without the attestation
        // riding along, as a retry after a crash does) is routine and does
        // not delete it again.
        let (status, text) = c.publish_keypackage(&acct, 2, 0xD1).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (status, text) = c.publish(&acct, 2, K2, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (status, text) = c
            .publish(
                &acct,
                2,
                K2,
                serde_json::json!({ "key_reset_attest": att2 }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(keypackage_rows(&pool, &acct).await, 1);
        assert_eq!(c.fetch(&acct).await["key_generation"], 2);

        // The floor: a generation-1 token is refused on both identity writes
        // with the one code a client acts on, before the change rule runs
        // (the publish-back of K1 is a 401, not a 409).
        for (status, text) in [
            c.publish(&acct, 1, K2, serde_json::json!({})).await,
            c.publish(&acct, 1, K1, serde_json::json!({})).await,
            c.publish_keypackage(&acct, 1, 0xD2).await,
        ] {
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");
            assert_eq!(error_code(&text), CODE_KEY_GENERATION_SUPERSEDED);
        }
        assert_eq!(keypackage_rows(&pool, &acct).await, 1);
        // A publish-back of the previous key with a current token and no
        // proof is the retired heal again.
        let (status, text) = c.publish(&acct, 2, K1, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        assert_eq!(error_code(&text), CODE_NEEDS_ATTESTATION);

        // A replay of the generation-2 attestation under another key does not
        // bind; a fresh attestation of the SAME generation for another key is
        // not newer.
        let (status, text) = c
            .publish(
                &acct,
                2,
                K3,
                serde_json::json!({ "key_reset_attest": att2 }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert_eq!(error_code(&text), CODE_ATTESTATION_INVALID);
        let (status, text) = c
            .publish(
                &acct,
                2,
                K3,
                serde_json::json!({ "key_reset_attest": attestation(&acct, 2, K3) }),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        assert_eq!(error_code(&text), CODE_GENERATION_NOT_NEWER);
        assert_eq!(c.fetch(&acct).await["signature_pubkey"], B64.encode(K2));

        // A later reset (generation 3) lands; the generation-2 attestation is
        // then a replay of an overtaken reset even for the key it names.
        let (status, text) = c
            .publish(
                &acct,
                3,
                K3,
                serde_json::json!({ "key_reset_attest": attestation(&acct, 3, K3) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(c.fetch(&acct).await["key_generation"], 3);
        let (status, text) = c
            .publish(
                &acct,
                3,
                K2,
                serde_json::json!({ "key_reset_attest": att2 }),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        assert_eq!(error_code(&text), CODE_GENERATION_NOT_NEWER);

        // A token of a HIGHER generation than the row is accepted and raises
        // nothing: the row's generation moves only with an attestation.
        let (status, text) = c.publish(&acct, 5, K3, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(c.fetch(&acct).await["key_generation"], 3);

        // The supersession-proof path is unchanged: a key change carrying a
        // proof (opaque here) applies with no attestation and leaves the
        // generation alone.
        let (status, text) = c
            .publish(
                &acct,
                3,
                K4,
                serde_json::json!({ "supersession_proof": B64.encode([0x99u8; 145]) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let fetched = c.fetch(&acct).await;
        assert_eq!(fetched["signature_pubkey"], B64.encode(K4));
        assert_eq!(fetched["key_generation"], 3);
        assert_eq!(fetched["supersession_proof"], B64.encode([0x99u8; 145]));

        sqlx::query("DELETE FROM identity_bundles WHERE account_id = ?")
            .bind(&acct[..])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM keypackages WHERE account_id = ?")
            .bind(&acct[..])
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn without_a_pinned_key_the_attested_arm_is_503_and_no_key_changes() {
        let Some(pool) = test_pool("without_a_pinned_key_the_attested_arm_is_503").await else {
            return;
        };
        let c = Client::new(pool.clone(), false, 0);
        let acct = fresh_account(0x66);
        let (status, text) = c.publish(&acct, 1, K1, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (status, text) = c
            .publish(
                &acct,
                2,
                K2,
                serde_json::json!({ "key_reset_attest": attestation(&acct, 2, K2) }),
            )
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
        assert_eq!(error_code(&text), CODE_KEY_RESET_DISABLED);
        let (status, text) = c.publish(&acct, 2, K2, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        assert_eq!(error_code(&text), CODE_NEEDS_ATTESTATION);
        assert_eq!(c.fetch(&acct).await["signature_pubkey"], B64.encode(K1));
        // A same-key re-upload is not the attested arm.
        let (status, text) = c.publish(&acct, 1, K1, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        sqlx::query("DELETE FROM identity_bundles WHERE account_id = ?")
            .bind(&acct[..])
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_cooldown_gates_a_second_attested_reset_only() {
        let Some(pool) = test_pool("the_cooldown_gates_a_second_attested_reset_only").await else {
            return;
        };
        let c = Client::new(pool.clone(), true, 3600);
        let acct = fresh_account(0x67);
        let (status, text) = c.publish(&acct, 1, K1, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (status, text) = c
            .publish(
                &acct,
                2,
                K2,
                serde_json::json!({ "key_reset_attest": attestation(&acct, 2, K2) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        // A second reset inside the window: refused, the generation stays.
        let (status, text) = c
            .publish(
                &acct,
                3,
                K3,
                serde_json::json!({ "key_reset_attest": attestation(&acct, 3, K3) }),
            )
            .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{text}");
        assert_eq!(error_code(&text), CODE_COOLING_DOWN);
        assert_eq!(c.fetch(&acct).await["key_generation"], 2);
        // A same-key retry and a proven rotation are never gated.
        let (status, text) = c.publish(&acct, 2, K2, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (status, text) = c
            .publish(
                &acct,
                2,
                K4,
                serde_json::json!({ "supersession_proof": B64.encode([0x98u8; 145]) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        sqlx::query("DELETE FROM identity_bundles WHERE account_id = ?")
            .bind(&acct[..])
            .execute(&pool)
            .await
            .unwrap();
    }
}
