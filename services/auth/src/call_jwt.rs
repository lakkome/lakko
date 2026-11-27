// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call-JWT HTTP handlers (Phase 3b4, anonymous since ADR-0078 section 2).
//!
//! The crypto + wire-format logic lives in [`lakko_call_jwt`]; this module is
//! the **server-side integration** — the keypair persistence + the two HTTP
//! handlers (`POST /call/jwt` + `GET /.well-known/jwks.json`).
//!
//! ## Anonymous admission (ADR-0078 section 2)
//!
//! `POST /call/jwt` takes no session. It spends one anonymous token of the
//! `call-join` context (`x-lakko-anon-token`, the ADR-0054 gate) and mints a
//! token whose claims are the requested `room`, a fresh random `peer_id`, the
//! role and the times: no `sub`, no `call_id`. The client computes the room
//! itself (`BLAKE3_derive_key("lakko/sfu/room/v1", call_id)`), so this
//! service never sees a call id and never learns which account joins which
//! room. Beside the token the response carries a TURN credential minted in
//! the same exchange, its username tag random (`turn.rs`).
//!
//! What this service still learns: that some holder of a `call-join` token
//! asked for some room, from some address, at some time (the class (c)
//! residual of ADR-0078 section 12). Nothing of it is stored: the spent-set
//! row holds a spend id, and the per-room cap (`room_cap.rs`) is in memory.
//!
//! Audit anchor: invariants C-15..C-23 in
//! [`docs/security/calls.md`](../../../docs/security/calls.md).

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use lakko_anon_token::TokenContext;
use lakko_anon_token_server::TokenVerifier;
use lakko_call_jwt::{
    is_lower_hex, mint_jwt, CallJwtClaims, CallRole, CallSigningKey, Jwk, Jwks, JWT_AUDIENCE,
    JWT_ISSUER, JWT_TTL_SECS, ROOM_HEX_LEN,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::MySqlPool;

use crate::room_cap::RoomAdmission;
use crate::turn::TurnCredentialResponse;
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
#[serde(deny_unknown_fields)]
pub(crate) struct IssueCallJwtReq {
    /// The room, 64 lower-case hex characters: the client's
    /// `BLAKE3_derive_key("lakko/sfu/room/v1", call_id)`.
    pub room: String,
    /// Requested role. Defaults to `participant` when omitted.
    #[serde(default = "default_role")]
    pub role: CallRole,
}

const fn default_role() -> CallRole {
    CallRole::Participant
}

/// `POST /call/jwt` response body. `Deserialize` is for the tests that parse
/// the handler's output.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct IssueCallJwtRes {
    /// JWS Compact representation of the token.
    pub jwt: String,
    /// `exp` of the token, for refresh scheduling. Duplicates the value
    /// inside the JWT; the JWT is the source of truth.
    pub expires_at_unix_secs: u64,
    /// A TURN credential minted in the same exchange, its username tag
    /// random; `null` when TURN is not configured (the client calls on STUN
    /// alone).
    pub turn: Option<TurnCredentialResponse>,
}

/// The 32 raw bytes of a validated room id.
fn decode_room(hex: &str) -> Option<[u8; 32]> {
    if !is_lower_hex(hex, ROOM_HEX_LEN) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// 16 random bytes as 32 lower-case hex characters: a fresh SFU peer id.
fn random_peer_id() -> String {
    use rand::RngCore as _;
    use std::fmt::Write as _;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(32);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn refusal(status: StatusCode, code: &'static str) -> Response {
    let mut response = (status, Json(json!({ "error": code }))).into_response();
    response
        .extensions_mut()
        .insert(lakko_metrics::ErrorCode(code));
    response
}

/// Issue a call admission token for the requested room, paid for by one
/// anonymous `call-join` token.
///
/// Order: validate the body (a malformed request spends nothing), spend the
/// token (the ADR-0054 gate answers `401` / `503` with its own codes), count
/// the admission against the room's cap (`429 room-admission-limit`), mint.
pub(crate) async fn issue_call_jwt(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Ok(req) = serde_json::from_slice::<IssueCallJwtReq>(&body) else {
        return refusal(StatusCode::BAD_REQUEST, "bad-request");
    };
    let Some(room) = decode_room(&req.room) else {
        return refusal(StatusCode::BAD_REQUEST, "bad-room");
    };

    let verifier = TokenVerifier::new(
        state.db.clone(),
        state.anon_tokens.as_ref().map(|i| i.server.clone()),
    );
    if let Err(refused) = verifier
        .spend_header(&headers, TokenContext::CallJoin)
        .await
    {
        return refused.into_response();
    }

    if state.room_cap.try_admit(&room, Instant::now()) == RoomAdmission::Full {
        return refusal(StatusCode::TOO_MANY_REQUESTS, "room-admission-limit");
    }

    let Ok(since_epoch) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal");
    };
    let now = since_epoch.as_secs();

    let claims = CallJwtClaims {
        iss: JWT_ISSUER.into(),
        aud: JWT_AUDIENCE.into(),
        room: req.room,
        peer_id: random_peer_id(),
        role: req.role,
        iat: now,
        exp: now + JWT_TTL_SECS,
    };
    let jwt = match mint_jwt(&state.call_jwt_signing, &claims) {
        Ok(jwt) => jwt,
        Err(e) => {
            tracing::error!(error = %e, "call jwt mint failed");
            return refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal");
        },
    };
    let turn = state.turn.as_ref().and_then(|t| t.mint_random(now));

    Json(IssueCallJwtRes {
        jwt,
        expires_at_unix_secs: claims.exp,
        turn,
    })
    .into_response()
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
    use lakko_anon_token::{MasterSecret, TokenContext};
    use lakko_anon_token_server::{current_epoch, AnonTokenServerConfig};
    use lakko_auth_token::TokenSecret;
    use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
    use tower::ServiceExt as _;

    use super::*;
    use lakko_call_jwt::{verify_jwt, verify_with_jwks, Jwks};

    const MASTER_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

    fn room_hex() -> String {
        lakko_sfu_proto::room_hex(&[0x01; 16])
    }

    fn token_header(context: TokenContext) -> String {
        let master = MasterSecret::from_hex(MASTER_HEX).expect("master");
        lakko_anon_token_server::testing::mint_header(
            &master,
            context,
            current_epoch(lakko_anon_token::DEFAULT_WINDOW_SECS),
        )
    }

    /// A lazy pool that never opens a connection, for the tests refused
    /// before the spent-set is touched.
    fn lazy_pool() -> MySqlPool {
        MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_millis(200))
            .connect_lazy_with(
                MySqlConnectOptions::new()
                    .host("127.0.0.1")
                    .port(1) // never actually dialed
                    .username("test")
                    .database("test"),
            )
    }

    /// The test database, migrated; `None` (with a note) when the env var
    /// is unset.
    async fn test_pool(name: &str) -> Option<MySqlPool> {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping {name}: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return None;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        Some(pool)
    }

    fn test_app_state(pool: MySqlPool, room_limit: u32, turn: bool) -> AppState {
        use opaque_ke::ServerSetup;
        use rand::rngs::OsRng;
        let signing = Arc::new(CallSigningKey::from_seed(&[0xAB; 32]));
        let opaque_setup = ServerSetup::<crate::opaque::LakkoCipherSuite>::new(&mut OsRng);
        let server = AnonTokenServerConfig::from_values(Some(MASTER_HEX), None)
            .expect("config")
            .expect("enabled");
        AppState {
            db: pool,
            opaque_setup: Arc::new(opaque_setup),
            session_token_secret: Arc::new(TokenSecret::new(vec![0u8; 64]).expect("secret")),
            session_token_ttl_ms: 86_400_000,
            refresh_token_ttl_ms: 30 * 86_400_000,
            fresh_grant_ttl_ms: 600_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: signing.clone(),
            attest_key: signing,
            turn: turn.then(|| Arc::new(crate::turn::TurnConfig::for_test())),
            totp_enc_key: None,
            vault_enc_key: None,
            room_cap: Arc::new(crate::room_cap::RoomAdmissionCap::new(
                room_limit,
                std::time::Duration::from_secs(60),
            )),
            backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
            anon_tokens: Some(Arc::new(crate::anon_tokens::AnonTokenIssuer::for_test(
                server,
            ))),
            anon_cred: None,
            handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
            recovery_email: None,
            owned_channels: Some(std::sync::Arc::new(
                crate::owned_channels::OwnedChannelsMaster::from_bytes([0x0C; 32]),
            )),
            reset_factor_grant_ttl_ms: 600_000,
            limits: Arc::new(crate::limits::Limits::defaults()),
        }
    }

    fn router(state: AppState) -> Router {
        Router::new()
            .route("/.well-known/jwks.json", get(jwks_handler))
            .route("/call/jwt", post(issue_call_jwt))
            .with_state(state)
    }

    fn mint_request(token: Option<&str>, body: &serde_json::Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/call/jwt")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            builder = builder.header(lakko_anon_token::TOKEN_HEADER, t);
        }
        builder
            .body(Body::from(serde_json::to_vec(body).expect("json")))
            .expect("request")
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs()
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        serde_json::from_slice(&bytes).expect("json")
    }

    /// ADR-0078 section 2: a `call-join` token buys a JWT naming the room
    /// and a random peer id, with no account (`sub`) and no call id, and a
    /// TURN credential whose tag is random. No bearer is read.
    #[tokio::test]
    async fn a_minted_jwt_names_no_account() {
        let Some(pool) = test_pool("a_minted_jwt_names_no_account").await else {
            return;
        };
        let state = test_app_state(pool, 0, true);
        let signing = state.call_jwt_signing.clone();
        let resp = router(state)
            .oneshot(mint_request(
                Some(&token_header(TokenContext::CallJoin)),
                &json!({ "room": room_hex(), "role": "participant" }),
            ))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let jwt = body["jwt"].as_str().expect("jwt").to_owned();
        let claims =
            verify_jwt(&signing.verifying, &signing.kid, &jwt, now_secs()).expect("verifies");
        assert_eq!(claims.room, room_hex());
        assert_eq!(claims.peer_id.len(), 32);
        assert_eq!(claims.role, CallRole::Participant);
        assert_eq!(body["expires_at_unix_secs"], claims.exp);

        let payload_b64 = jwt.split('.').nth(1).expect("payload");
        let payload = base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            payload_b64,
        )
        .expect("b64");
        let object: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&payload).expect("object");
        assert!(!object.contains_key("sub"), "the token names an account");
        assert!(!object.contains_key("call_id"), "the token names the call");

        let username = body["turn"]["username"].as_str().expect("turn username");
        let (_, tag) = username.split_once(':').expect("expiry:tag");
        assert_eq!(tag.len(), 32);
    }

    /// Two mints for the same room get two different peer ids.
    #[tokio::test]
    async fn every_mint_draws_a_fresh_peer_id() {
        let Some(pool) = test_pool("every_mint_draws_a_fresh_peer_id").await else {
            return;
        };
        let state = test_app_state(pool, 0, false);
        let signing = state.call_jwt_signing.clone();
        let mut peers = Vec::new();
        for _ in 0..2 {
            let resp = router(state.clone())
                .oneshot(mint_request(
                    Some(&token_header(TokenContext::CallJoin)),
                    &json!({ "room": room_hex() }),
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp).await;
            assert!(body["turn"].is_null());
            let claims = verify_jwt(
                &signing.verifying,
                &signing.kid,
                body["jwt"].as_str().expect("jwt"),
                now_secs(),
            )
            .expect("verifies");
            assert_eq!(claims.role, CallRole::Participant);
            peers.push(claims.peer_id);
        }
        assert_ne!(peers[0], peers[1]);
    }

    /// No token: `401 anon-token-required`, whatever bearer is offered.
    #[tokio::test]
    async fn a_mint_without_a_token_is_refused() {
        let state = test_app_state(lazy_pool(), 0, false);
        let mut req = mint_request(None, &json!({ "room": room_hex() }));
        req.headers_mut().insert(
            header::AUTHORIZATION,
            "Bearer v3.anything".parse().expect("header"),
        );
        let resp = router(state).oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(resp).await["error"], "anon-token-required");
    }

    /// A token of another context does not pay for a call.
    #[tokio::test]
    async fn a_token_of_another_context_is_refused() {
        let state = test_app_state(lazy_pool(), 0, false);
        let resp = router(state)
            .oneshot(mint_request(
                Some(&token_header(TokenContext::DsKnock)),
                &json!({ "room": room_hex() }),
            ))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(resp).await["error"], "anon-token-wrong-context");
    }

    /// A malformed body is refused before any token is read, so it spends
    /// nothing.
    #[tokio::test]
    async fn a_malformed_body_spends_nothing() {
        for body in [
            json!({ "room": "ab" }),
            json!({ "room": "AB".repeat(32) }),
            json!({ "room": room_hex(), "call_id": "00112233445566778899aabbccddeeff" }),
            json!({ "call_id": "00112233445566778899aabbccddeeff" }),
        ] {
            let state = test_app_state(lazy_pool(), 0, false);
            let resp = router(state)
                .oneshot(mint_request(None, &body))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
        }
    }

    /// The per-room cap refuses past its limit, room by room.
    #[tokio::test]
    async fn the_room_cap_refuses_past_its_limit() {
        let Some(pool) = test_pool("the_room_cap_refuses_past_its_limit").await else {
            return;
        };
        let state = test_app_state(pool, 2, false);
        for expected in [
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let resp = router(state.clone())
                .oneshot(mint_request(
                    Some(&token_header(TokenContext::CallJoin)),
                    &json!({ "room": room_hex() }),
                ))
                .await
                .expect("response");
            assert_eq!(resp.status(), expected);
        }
        let other = lakko_sfu_proto::room_hex(&[0x02; 16]);
        let resp = router(state)
            .oneshot(mint_request(
                Some(&token_header(TokenContext::CallJoin)),
                &json!({ "room": other }),
            ))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// A token is spent once: the same header a second time is refused.
    #[tokio::test]
    async fn a_call_join_token_is_spent_once() {
        let Some(pool) = test_pool("a_call_join_token_is_spent_once").await else {
            return;
        };
        let state = test_app_state(pool, 0, false);
        let token = token_header(TokenContext::CallJoin);
        let first = router(state.clone())
            .oneshot(mint_request(Some(&token), &json!({ "room": room_hex() })))
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::OK);
        let second = router(state)
            .oneshot(mint_request(Some(&token), &json!({ "room": room_hex() })))
            .await
            .expect("response");
        assert_eq!(second.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(second).await["error"], "anon-token-spent");
    }

    #[test]
    fn the_spent_set_migration_is_the_shared_crate_schema_byte_for_byte() {
        assert_eq!(
            include_str!("../migrations/0034_anon_token_spent.sql"),
            lakko_anon_token_server::SPENT_SET_MIGRATION_SQL
        );
    }

    /// ADR-0078 section 2: no table of this service pairs a call with an
    /// account any more - the ADR-0033 tables are dropped.
    #[tokio::test]
    async fn the_call_admission_tables_are_gone() {
        let Some(pool) = test_pool("the_call_admission_tables_are_gone").await else {
            return;
        };
        let left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_schema = DATABASE() \
             AND table_name IN ('call_admission_events', 'throttled_calls')",
        )
        .fetch_one(&pool)
        .await
        .expect("information_schema");
        assert_eq!(left, 0);
    }

    /// `GET /.well-known/jwks.json` is anonymous and verifies a JWT minted
    /// by the same service.
    #[tokio::test]
    async fn jwks_endpoint_full_chain() {
        let Some(pool) = test_pool("jwks_endpoint_full_chain").await else {
            return;
        };
        let state = test_app_state(pool, 0, false);
        let expected_kid = state.call_jwt_signing.kid.clone();
        let app = router(state);
        let resp = app
            .clone()
            .oneshot(mint_request(
                Some(&token_header(TokenContext::CallJoin)),
                &json!({ "room": room_hex(), "role": "publisher" }),
            ))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let mint: IssueCallJwtRes =
            serde_json::from_value(body_json(resp).await).expect("response shape");

        let jwks_response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/.well-known/jwks.json")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(jwks_response.status(), StatusCode::OK);
        let bytes = jwks_response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let jwks: Jwks = serde_json::from_slice(&bytes).expect("parse JWKS");
        assert_eq!(jwks.keys.len(), 1);
        assert_eq!(jwks.keys[0].kid, expected_kid);
        let claims = verify_with_jwks(&jwks, &mint.jwt, now_secs()).expect("verify via JWKS");
        assert_eq!(claims.role, CallRole::Publisher);
        assert_eq!(claims.room, room_hex());
    }
}
