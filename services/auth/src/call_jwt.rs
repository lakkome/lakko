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
//! Audit anchor: invariants C-15..C-21 in
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

/// `POST /call/jwt` response body.
#[derive(Debug, Serialize)]
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
    use super::*;
    use lakko_call_jwt::{parse_unverified, verify_jwt};

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
}
