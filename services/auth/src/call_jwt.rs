// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call-JWT HTTP handlers (Phase 3b4) — companion to
//! 3b1a's SFU scaffolding.
//!
//! The crypto + wire-format logic lives in
//! [`lakko_call_jwt`]; this module is the **server-side
//! integration** — the keypair persistence + the two HTTP
//! handlers (`POST /call/jwt` + `GET /.well-known/jwks.json`).
//!
//! Audit anchor: invariants C-15..C-23 in
//! [`docs/security/calls.md`](../../../docs/security/calls.md)
//! cite this module's tests for the issuance shape.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use lakko_call_jwt::{
    mint_jwt, CallJwtClaims, CallRole, CallSigningKey, Jwk, Jwks, JWT_AUDIENCE, JWT_ISSUER,
    JWT_TTL_SECS,
};
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;

use crate::webauthn::AuthenticatedAccount;
use crate::AppState;

/// Load (or generate + persist on first run) the Ed25519
/// keypair the call-JWT issuer uses. Same pattern as
/// `opaque::load_or_init_server_setup`: single-row table,
/// `INSERT IGNORE` on first run, read back.
///
/// # Errors
///
/// Propagates sqlx errors. Returns `anyhow::Error` since
/// startup-side; the caller (`main::AppState` builder)
/// surfaces it as a fatal bootstrap error.
pub(crate) async fn load_or_init_call_signing_key(
    pool: &MySqlPool,
) -> anyhow::Result<CallSigningKey> {
    use anyhow::Context;
    use rand::RngCore;

    // Try to read an existing row.
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT seed FROM call_jwt_signing_key WHERE id = 1")
            .fetch_optional(pool)
            .await
            .context("read call_jwt_signing_key")?;
    if let Some((seed_bytes,)) = row {
        if seed_bytes.len() != 32 {
            anyhow::bail!(
                "call_jwt_signing_key.seed length {} != 32 — DB corruption?",
                seed_bytes.len()
            );
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&seed_bytes);
        return Ok(CallSigningKey::from_seed(&seed));
    }

    // No row — generate a fresh seed + insert. `INSERT
    // IGNORE` so two racing startups don't both insert.
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    sqlx::query("INSERT IGNORE INTO call_jwt_signing_key (id, seed) VALUES (1, ?)")
        .bind(&seed[..])
        .execute(pool)
        .await
        .context("insert call_jwt_signing_key")?;

    // Re-read so a racing insert wins gracefully.
    let (seed_bytes,): (Vec<u8>,) =
        sqlx::query_as("SELECT seed FROM call_jwt_signing_key WHERE id = 1")
            .fetch_one(pool)
            .await
            .context("re-read call_jwt_signing_key after insert")?;
    if seed_bytes.len() != 32 {
        anyhow::bail!(
            "call_jwt_signing_key.seed length {} != 32 after re-read",
            seed_bytes.len()
        );
    }
    let mut final_seed = [0u8; 32];
    final_seed.copy_from_slice(&seed_bytes);
    Ok(CallSigningKey::from_seed(&final_seed))
}

// ----------------------------------------------------------------
// HTTP handlers
// ----------------------------------------------------------------

/// `POST /call/jwt` request body.
#[derive(Debug, Deserialize)]
pub(crate) struct IssueCallJwtReq {
    /// 16-byte random call_id, lower-case hex (32 chars).
    /// The caller (Lakko client) mints this with
    /// `SecRandomCopyBytes` / `SecureRandom.nextBytes` /
    /// `crypto.getRandomValues` before opening the call.
    pub call_id: String,
    /// Requested role. Defaults to `participant` when omitted.
    #[serde(default = "default_role")]
    pub role: CallRole,
}

fn default_role() -> CallRole {
    CallRole::Participant
}

/// `POST /call/jwt` response body. `Deserialize` is for
/// the integration test that parses the handler's output;
/// no other consumer reads this shape in-process.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct IssueCallJwtRes {
    /// JWS Compact representation of the token.
    pub jwt: String,
    /// `exp` field for client-side caching / refresh
    /// scheduling. Duplicates the value embedded in the
    /// JWT for convenience — clients should NOT trust this
    /// (the JWT itself is the source of truth) but it
    /// saves a parse on the happy path.
    pub expires_at_unix_secs: u64,
}

/// Issue a call JWT for the authenticated account + the
/// requested call_id / role. Bearer-auth required (the
/// `webauthn::require_session` middleware stamps the
/// `AuthenticatedAccount`).
#[axum::debug_handler]
pub(crate) async fn issue_call_jwt(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<IssueCallJwtReq>,
) -> Result<Json<IssueCallJwtRes>, (StatusCode, &'static str)> {
    // Strict 32-char hex; we don't accept upper-case or
    // separators. Matches what the clients emit.
    if req.call_id.len() != 32 {
        return Err((StatusCode::BAD_REQUEST, "call_id must be 32 hex chars"));
    }
    if !req.call_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err((StatusCode::BAD_REQUEST, "call_id must be hex"));
    }
    let call_id_lower = req.call_id.to_ascii_lowercase();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "clock before epoch"))?
        .as_secs();

    let mut account_hex = String::with_capacity(32);
    for b in &auth.0 {
        let _ = write!(account_hex, "{b:02x}");
    }

    let claims = CallJwtClaims {
        iss: JWT_ISSUER.into(),
        aud: JWT_AUDIENCE.into(),
        sub: account_hex,
        call_id: call_id_lower,
        role: req.role,
        iat: now,
        exp: now + JWT_TTL_SECS,
    };

    let jwt = mint_jwt(&state.call_jwt_signing, &claims).map_err(|e| {
        tracing::error!(error = %e, "mint_jwt failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "mint failed")
    })?;

    Ok(Json(IssueCallJwtRes {
        jwt,
        expires_at_unix_secs: claims.exp,
    }))
}

/// Anonymous handler — publishes the current public key
/// as a JWKS document. SFU process fetches this at startup
/// + refreshes every 24 h per ADR-0008.
#[axum::debug_handler]
pub(crate) async fn jwks_handler(State(state): State<AppState>) -> Json<Jwks> {
    let key = &state.call_jwt_signing;
    Json(Jwks {
        keys: vec![Jwk::from_signing_key(key)],
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::body::Body;
    use axum::http::{header, Method, Request, StatusCode};
    use axum::routing::{get, post};
    use axum::Router;
    use http_body_util::BodyExt as _;
    use lakko_auth_token::TokenSecret;
    use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
    use tower::ServiceExt as _;

    use super::*;
    use lakko_call_jwt::{parse_unverified, verify_jwt, verify_with_jwks, Jwks};

    /// Smoke test that the auth-service-side wiring (claim
    /// shape, hex normalization, role default) round-trips
    /// through the `lakko_call_jwt` mint + verify. The full
    /// crypto-side property tests live in that crate.
    #[test]
    fn auth_side_issuance_round_trips() {
        let key = CallSigningKey::from_seed(&[0xAB; 32]);
        let now = 1_700_000_000;
        let claims = CallJwtClaims {
            iss: JWT_ISSUER.into(),
            aud: JWT_AUDIENCE.into(),
            sub: "00112233445566778899aabbccddeeff".into(),
            call_id: "0102030405060708090a0b0c0d0e0f10".into(),
            role: CallRole::Participant,
            iat: now,
            exp: now + JWT_TTL_SECS,
        };
        let jwt = mint_jwt(&key, &claims).unwrap();
        let (_header, parsed_unverified) = parse_unverified(&jwt).unwrap();
        assert_eq!(parsed_unverified, claims);
        let parsed = verify_jwt(&key.verifying, &key.kid, &jwt, now + 1).unwrap();
        assert_eq!(parsed, claims);
    }

    // ====================================================
    // HTTP integration tests — drive the actual axum router
    // through `tower::ServiceExt::oneshot`. Covers the
    // middleware → handler → JSON pipeline that the
    // primitive-level tests in `lakko-call-jwt` and the
    // smoke test above don't exercise.
    //
    // The handlers don't touch the DB (only
    // `load_or_init_call_signing_key` does, and that runs
    // at bootstrap), so we wire a lazy MySQL pool that
    // never opens a connection. The same pattern would
    // work to test the WebAuthn / OPAQUE routes if we
    // teach those tests to inject the surrounding state.
    // ====================================================

    /// Test-only AppState builder. The DB pool is lazy and
    /// never actually opens a connection — fine because the
    /// call_jwt handlers don't query it (the keypair was
    /// already loaded at bootstrap in production; here we
    /// inject a fixture key directly).
    ///
    /// `token_secret` is shared by reference so successive
    /// builds (one per `oneshot` consumer) accept tokens
    /// minted under the same secret.
    fn test_app_state(token_secret: &TokenSecret) -> AppState {
        use opaque_ke::ServerSetup;
        use rand::rngs::OsRng;
        let signing = Arc::new(CallSigningKey::from_seed(&[0xAB; 32]));
        // OPAQUE ServerSetup is not used by the call_jwt
        // routes but is non-optional on AppState. Mint a
        // throwaway one from OsRng (same pattern as the
        // production bootstrap path).
        let opaque_setup = ServerSetup::<crate::opaque::LakkoCipherSuite>::new(&mut OsRng);
        let pool = MySqlPoolOptions::new()
            .max_connections(1)
            .connect_lazy_with(
                MySqlConnectOptions::new()
                .host("127.0.0.1")
                .port(0) // never actually dialed
                .username("test")
                .database("test"),
            );
        AppState {
            db: pool,
            opaque_setup: Arc::new(opaque_setup),
            session_token_secret: Arc::new(token_secret.clone()),
            session_token_ttl_ms: 86_400_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: signing.clone(),
            attest_key: signing,
            turn: None,
        }
    }

    /// Convenience — builds a fresh `(token_secret, AppState)`
    /// pair for tests that don't need to share the secret
    /// with anything external.
    fn fresh_app_state() -> (AppState, TokenSecret) {
        let token_secret = TokenSecret::new(vec![0u8; 64]).expect("64-byte HMAC key");
        let state = test_app_state(&token_secret);
        (state, token_secret)
    }

    /// Build a router with ONLY the call-JWT routes wired
    /// up — same middleware chain as production, but no
    /// other routes mixed in (keeps the tests focused).
    fn test_call_jwt_router(state: AppState) -> Router {
        let call_jwt_routes = Router::new()
            .route("/call/jwt", post(issue_call_jwt))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::webauthn::require_session,
            ));
        Router::new()
            .route("/.well-known/jwks.json", get(jwks_handler))
            .merge(call_jwt_routes)
            .with_state(state)
    }

    /// Issue a session token valid for one hour, ready for
    /// the `Authorization: Bearer …` header.
    fn issue_test_session_token(secret: &TokenSecret, account_id: &[u8; 16]) -> String {
        let now_ms: i64 = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        secret
            .issue(account_id, now_ms + 3_600_000)
            .expect("issue session token")
    }

    /// Happy path: valid bearer token, valid call_id →
    /// 200 OK + a JWT that verifies under the same
    /// signing key the AppState was built with. The JWT's
    /// `sub` matches the bearer token's account_id (the
    /// middleware → handler boundary stamps it from the
    /// token, NOT from the request body).
    #[tokio::test]
    async fn issue_call_jwt_happy_path_round_trips() {
        let (state, token_secret) = fresh_app_state();
        let signing = state.call_jwt_signing.clone();
        let app = test_call_jwt_router(state);

        let account_id: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let session_token = issue_test_session_token(&token_secret, &account_id);

        let body = serde_json::json!({
            "call_id": "0102030405060708090a0b0c0d0e0f10",
            "role": "participant",
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/call/jwt")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: IssueCallJwtRes = serde_json::from_slice(&bytes).expect("parse response");

        // Verify the JWT under the same signing key. The
        // middleware → handler path ran for real; if the
        // signature verifies, the whole pipeline worked.
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = verify_jwt(&signing.verifying, &signing.kid, &parsed.jwt, now_secs)
            .expect("verify minted JWT");
        // The `sub` must be the bearer-token account_id, NOT
        // anything from the request body. This is the
        // C-21 invariant.
        let mut expected_sub = String::new();
        for b in &account_id {
            use std::fmt::Write as _;
            let _ = write!(expected_sub, "{b:02x}");
        }
        assert_eq!(claims.sub, expected_sub);
        assert_eq!(claims.call_id, "0102030405060708090a0b0c0d0e0f10");
        assert_eq!(claims.role, CallRole::Participant);
        assert_eq!(claims.exp, claims.iat + JWT_TTL_SECS);
        assert_eq!(parsed.expires_at_unix_secs, claims.exp);
    }

    /// Same shape as the happy path, but the `role` field
    /// is omitted from the request body — handler should
    /// default it to `Participant`.
    #[tokio::test]
    async fn issue_call_jwt_defaults_role_to_participant() {
        let (state, token_secret) = fresh_app_state();
        let signing = state.call_jwt_signing.clone();
        let app = test_call_jwt_router(state);

        let account_id = [0xAA; 16];
        let session_token = issue_test_session_token(&token_secret, &account_id);
        let body = serde_json::json!({
            "call_id": "11111111111111112222222222222222",
            // role omitted on purpose
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/call/jwt")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: IssueCallJwtRes = serde_json::from_slice(&bytes).unwrap();
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = verify_jwt(&signing.verifying, &signing.kid, &parsed.jwt, now_secs).unwrap();
        assert_eq!(claims.role, CallRole::Participant);
    }

    /// No `Authorization` header → middleware rejects with
    /// 401 BEFORE the handler runs. The body is never
    /// parsed.
    #[tokio::test]
    async fn issue_call_jwt_without_bearer_rejects_401() {
        let (state, _token_secret) = fresh_app_state();
        let app = test_call_jwt_router(state);
        let body = serde_json::json!({
            "call_id": "11111111111111112222222222222222",
            "role": "participant",
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/call/jwt")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// Forged bearer token (different HMAC secret) → 401.
    /// Defence against an attacker who got the wire shape
    /// right but doesn't have the secret.
    #[tokio::test]
    async fn issue_call_jwt_with_wrong_secret_rejects_401() {
        let (state, _real_secret) = fresh_app_state();
        let app = test_call_jwt_router(state);

        // Mint a token with a DIFFERENT secret — same shape,
        // wrong tag.
        let attacker_secret = TokenSecret::new(vec![0xFFu8; 64]).unwrap();
        let account_id = [0x11; 16];
        let forged = issue_test_session_token(&attacker_secret, &account_id);

        let body = serde_json::json!({
            "call_id": "11111111111111112222222222222222",
            "role": "participant",
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/call/jwt")
                    .header(header::AUTHORIZATION, format!("Bearer {forged}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// Malformed call_id (wrong length, not hex) → 400.
    /// Bearer-auth passes; the handler-side validation
    /// catches the shape. Each iteration builds a fresh
    /// router (because `oneshot` consumes) sharing the
    /// same `TokenSecret` so the bearer token stays valid.
    #[tokio::test]
    async fn issue_call_jwt_with_malformed_call_id_rejects_400() {
        let token_secret = TokenSecret::new(vec![0u8; 64]).expect("64-byte HMAC key");
        let account_id = [0x33; 16];
        let session_token = issue_test_session_token(&token_secret, &account_id);

        for bad_call_id in [
            "too-short",
            "wayyyytoolongtobesixteenbyteshexencodedstring",
            "nothexnothexnothexnothexnothexno", // 32 chars but not hex
            "0102030405060708090a0b0c0d0e0f1G", // 32 chars but trailing non-hex
        ] {
            let body = serde_json::json!({
                "call_id": bad_call_id,
                "role": "participant",
            });
            let app = test_call_jwt_router(test_app_state(&token_secret));
            let response = app
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/call/jwt")
                        .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "expected 400 for call_id={bad_call_id:?}"
            );
        }
    }

    /// `GET /.well-known/jwks.json` is anonymous — no bearer
    /// needed, returns the JWK Set holding the public key.
    /// The fetched JWKS doc verifies a JWT minted by the
    /// same service (full client-side cache-then-verify
    /// round-trip).
    #[tokio::test]
    async fn jwks_endpoint_full_chain() {
        let (state, token_secret) = fresh_app_state();
        let expected_kid = state.call_jwt_signing.kid.clone();
        let app = test_call_jwt_router(state);

        // Step 1: mint a JWT via /call/jwt.
        let account_id = [0x55; 16];
        let session_token = issue_test_session_token(&token_secret, &account_id);
        let body = serde_json::json!({
            "call_id": "abababababababababababababababab",
            "role": "publisher",
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/call/jwt")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let mint_res: IssueCallJwtRes = serde_json::from_slice(&bytes).unwrap();

        // Step 2: fetch the JWKS doc.
        let jwks_response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/.well-known/jwks.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(jwks_response.status(), StatusCode::OK);
        let jwks_bytes = jwks_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let jwks: Jwks = serde_json::from_slice(&jwks_bytes).expect("parse JWKS");
        assert_eq!(jwks.keys.len(), 1);
        assert_eq!(jwks.keys[0].kid, expected_kid);
        assert_eq!(jwks.keys[0].kty, "OKP");
        assert_eq!(jwks.keys[0].crv, "Ed25519");
        assert_eq!(jwks.keys[0].alg, "EdDSA");

        // Step 3: verify the minted JWT using the fetched
        // JWKS doc — the same path a real SFU process would
        // take. This is the production end-to-end smoke
        // test for invariant C-22.
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = verify_with_jwks(&jwks, &mint_res.jwt, now_secs).expect("verify via JWKS");
        assert_eq!(claims.role, CallRole::Publisher);
        assert_eq!(claims.call_id, "abababababababababababababababab");
    }
}
