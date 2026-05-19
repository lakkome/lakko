// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call-JWT issuance (Phase 3b4 forward — companion to 3b1a's
//! scaffolding).
//!
//! The SFU (services/sfu, scaffolded in 3b1a) gates admission
//! to a call on a JWT signed by **this** service. Shape per
//! [ADR-0008](../../../docs/adr/0008-livekit-fork-strategy.md)
//! §"Auth token format":
//!
//! ```text
//! {
//!   "iss": "lakko-auth",
//!   "aud": "lakko-sfu",
//!   "sub": "<account_id_hex>",        // 16-byte UUIDv7
//!   "call_id": "<call_id_hex>",       // 16-byte random
//!   "role": "participant" | "publisher" | "subscriber",
//!   "iat": <unix-seconds>,
//!   "exp": <iat + 3600>
//! }
//! ```
//!
//! Signed by an Ed25519 keypair persisted in
//! `call_jwt_signing_key`. The corresponding public key is
//! published at `GET /.well-known/jwks.json` — the SFU
//! process fetches it at startup + refreshes every 24 h.
//!
//! ## Why hand-rolled JWS (not the `jsonwebtoken` crate)
//!
//! - Smaller dep surface (per `CLAUDE.md` code-style rules).
//! - JWS Compact is three base64url segments — under 100
//!   lines of code, every byte is auditable.
//! - We already have Ed25519 in `lakko-crypto::signing`; no
//!   need for a second sign/verify implementation.
//!
//! ## What this module does NOT do
//!
//! - Validate the call_id against any state — the caller is
//!   responsible for choosing a fresh `call_id` per call. The
//!   SFU's JWT verifier (3b1c, Go-side) treats the JWT as the
//!   sole proof of admission; replaying a JWT is bounded by
//!   the 1-hour `exp` window.
//! - Track call membership — that's the SFU's job. The auth
//!   service is stateless w.r.t. calls (no call-table).
//! - Refresh expired JWTs — the client requests a new JWT
//!   when needed.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash;
use lakko_crypto::signing::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;

use crate::webauthn::AuthenticatedAccount;
use crate::AppState;

/// JWT validity window — 1 hour per ADR-0008. Bounds the
/// damage from a leaked JWT.
pub(crate) const JWT_TTL_SECS: u64 = 3600;

/// Issuer string baked into every JWT. The SFU verifier
/// (3b1c) checks this against an expected constant.
pub(crate) const JWT_ISSUER: &str = "lakko-auth";

/// Audience string baked into every JWT. The SFU verifier
/// checks this so a JWT minted for some other service can't
/// be replayed at the SFU.
pub(crate) const JWT_AUDIENCE: &str = "lakko-sfu";

/// JWS alg field per RFC 8037 §3 (EdDSA over Ed25519).
const JWS_ALG: &str = "EdDSA";

/// Call participation role. Mirrors the LiveKit shape but
/// owned by us so the JWT is a closed type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CallRole {
    /// Regular participant — can publish + subscribe to
    /// tracks. The default for 1×1 and small group calls.
    Participant,
    /// Publisher-only — can publish but not subscribe.
    /// Reserved for broadcast / audio-room hosts (Phase 4).
    Publisher,
    /// Subscriber-only — listen-only. Reserved for
    /// broadcast / audio-room audience (Phase 4).
    Subscriber,
}

/// The CallJwt claim set serialised as the JWT payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CallJwtClaims {
    /// `iss` — issuer; always `JWT_ISSUER`.
    pub iss: String,
    /// `aud` — audience; always `JWT_AUDIENCE`.
    pub aud: String,
    /// `sub` — account_id, 16-byte UUIDv7 as lower-case hex
    /// (no separators). The SFU pseudonymises this further
    /// for log lines; the auth service has no obligation to
    /// hide it from itself.
    pub sub: String,
    /// 16-byte random call_id as lower-case hex.
    pub call_id: String,
    /// Participation role.
    pub role: CallRole,
    /// `iat` — issued-at, unix seconds.
    pub iat: u64,
    /// `exp` — expires-at, unix seconds. Always `iat + JWT_TTL_SECS`.
    pub exp: u64,
}

/// JWS Compact header — serialised to JSON, base64url-encoded
/// as the JWT's first segment.
#[derive(Debug, Serialize, Deserialize)]
struct JwsHeader {
    alg: String,
    typ: String,
    kid: String,
}

/// Issuer-side signing key, paired with its key id. The
/// public half (`verifying`) is what we publish via
/// `/.well-known/jwks.json`; the private half (`signing`)
/// stays in the DB + process memory.
pub(crate) struct CallSigningKey {
    pub(crate) signing: SigningKey,
    pub(crate) verifying: VerifyingKey,
    /// Stable identifier for this keypair. Derived as the
    /// first 8 bytes of BLAKE3(public_key) rendered as hex
    /// (so 16 hex chars). Stable across restarts because
    /// the keypair is persisted; bumped only when the key
    /// is rotated.
    pub(crate) kid: String,
}

impl CallSigningKey {
    /// Build from a 32-byte Ed25519 seed. Used by
    /// `load_or_init_call_signing_key` and tests.
    pub(crate) fn from_seed(seed: &[u8; 32]) -> Self {
        let signing = SigningKey::from_seed(seed);
        let verifying = signing.verifying_key();
        let kid = kid_for(&verifying);
        Self {
            signing,
            verifying,
            kid,
        }
    }

    /// Sign the prepared JWS signing input `header_b64.payload_b64`
    /// and return the base64url-encoded signature segment.
    fn sign_segment(&self, signing_input: &[u8]) -> String {
        let sig = self.signing.sign(signing_input);
        URL_SAFE_NO_PAD.encode(sig.to_bytes())
    }
}

/// Derive the stable key id for a verifying key — first 8
/// bytes of BLAKE3, hex-encoded (16 chars). Auditable +
/// fits in a JWT header without bloating the byte count.
fn kid_for(vk: &VerifyingKey) -> String {
    let digest = hash::blake3(&vk.to_bytes());
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        let _ = write!(out, "{b:02x}");
    }
    out
}

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

/// Assemble + sign a JWT.
///
/// Returns the full JWS Compact representation
/// `header.payload.signature`. Per RFC 7515 §3.1, base64url
/// without padding.
///
/// # Errors
///
/// `Error::Internal` only — JSON encoding of fully-typed
/// inputs is practically infallible; we surface as
/// `axum::Error` for the handler to map to a 500.
pub(crate) fn mint_jwt(key: &CallSigningKey, claims: &CallJwtClaims) -> Result<String, JwtError> {
    let header = JwsHeader {
        alg: JWS_ALG.into(),
        typ: "JWT".into(),
        kid: key.kid.clone(),
    };
    let header_json = serde_json::to_vec(&header).map_err(|e| JwtError::Encode(e.to_string()))?;
    let payload_json = serde_json::to_vec(claims).map_err(|e| JwtError::Encode(e.to_string()))?;
    let header_b64 = URL_SAFE_NO_PAD.encode(&header_json);
    let payload_b64 = URL_SAFE_NO_PAD.encode(&payload_json);

    // Signing input is `header_b64 || "." || payload_b64`
    // per RFC 7515 §5.1.
    let mut signing_input = Vec::with_capacity(header_b64.len() + 1 + payload_b64.len());
    signing_input.extend_from_slice(header_b64.as_bytes());
    signing_input.push(b'.');
    signing_input.extend_from_slice(payload_b64.as_bytes());

    let signature_b64 = key.sign_segment(&signing_input);
    let mut out = String::with_capacity(signing_input.len() + 1 + signature_b64.len());
    out.push_str(&header_b64);
    out.push('.');
    out.push_str(&payload_b64);
    out.push('.');
    out.push_str(&signature_b64);
    Ok(out)
}

/// Verify a JWT against an expected signing key. Used by
/// the JWT-roundtrip tests + by `services/sfu/lakko-additions/auth`
/// equivalent (Go-side reimplementation lands in 3b1c — but
/// keeping a Rust verifier here means a Rust client can
/// also self-verify, e.g. for an SDK that doesn't trust the
/// SFU side).
///
/// `now_secs` is injected so tests can drive the clock.
///
/// # Errors
///
/// - [`JwtError::Format`] — JWT shape is malformed (wrong
///   number of segments, wrong algorithm, wrong issuer /
///   audience, base64 decode failure).
/// - [`JwtError::Signature`] — Ed25519 verification failed.
/// - [`JwtError::Expired`] — `now_secs >= claims.exp`.
/// - [`JwtError::Encode`] — JSON decode of header / payload
///   failed (malformed UTF-8 / not an object / etc.).
#[allow(dead_code)] // tests + future Rust-side SFU verifier consumers
pub(crate) fn verify_jwt(
    expected_key: &VerifyingKey,
    expected_kid: &str,
    jwt: &str,
    now_secs: u64,
) -> Result<CallJwtClaims, JwtError> {
    let mut parts = jwt.split('.');
    let header_b64 = parts.next().ok_or(JwtError::Format("missing header"))?;
    let payload_b64 = parts.next().ok_or(JwtError::Format("missing payload"))?;
    let signature_b64 = parts.next().ok_or(JwtError::Format("missing signature"))?;
    if parts.next().is_some() {
        return Err(JwtError::Format("extra segments"));
    }

    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|_| JwtError::Format("header base64"))?;
    let header: JwsHeader =
        serde_json::from_slice(&header_bytes).map_err(|e| JwtError::Encode(e.to_string()))?;
    if header.alg != JWS_ALG {
        return Err(JwtError::Format("alg != EdDSA"));
    }
    if header.kid != expected_kid {
        return Err(JwtError::Format("kid mismatch"));
    }

    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|_| JwtError::Format("signature base64"))?;
    let sig = lakko_crypto::signing::Signature::from_bytes(&signature_bytes)
        .map_err(|_| JwtError::Format("signature length"))?;

    let mut signing_input = Vec::with_capacity(header_b64.len() + 1 + payload_b64.len());
    signing_input.extend_from_slice(header_b64.as_bytes());
    signing_input.push(b'.');
    signing_input.extend_from_slice(payload_b64.as_bytes());

    expected_key
        .verify(&signing_input, &sig)
        .map_err(|_| JwtError::Signature)?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| JwtError::Format("payload base64"))?;
    let claims: CallJwtClaims =
        serde_json::from_slice(&payload_bytes).map_err(|e| JwtError::Encode(e.to_string()))?;

    if claims.iss != JWT_ISSUER {
        return Err(JwtError::Format("iss != lakko-auth"));
    }
    if claims.aud != JWT_AUDIENCE {
        return Err(JwtError::Format("aud != lakko-sfu"));
    }
    if now_secs >= claims.exp {
        return Err(JwtError::Expired);
    }
    Ok(claims)
}

/// Errors surfaced by `mint_jwt` / `verify_jwt`. Distinct
/// from `ApiError` so the unit tests can pattern-match
/// without going through axum.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // Format / Signature / Expired only constructed by verify_jwt (test path + future SFU verifier)
pub(crate) enum JwtError {
    /// JWT shape is wrong (segments, base64, headers).
    #[error("malformed JWT: {0}")]
    Format(&'static str),
    /// Ed25519 verify rejected the signature.
    #[error("signature verification failed")]
    Signature,
    /// Claims are past their expiry.
    #[error("token expired")]
    Expired,
    /// JSON serialisation surfaced an error.
    #[error("JWT encoding: {0}")]
    Encode(String),
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

/// `GET /.well-known/jwks.json` response body. The SFU
/// process fetches this at startup + refreshes every 24 h
/// per ADR-0008. Anonymous — the public key is public by
/// definition.
#[derive(Debug, Serialize)]
pub(crate) struct Jwks {
    pub keys: Vec<Jwk>,
}

/// A single JWK entry — Ed25519 in OKP form per RFC 8037.
#[derive(Debug, Serialize)]
pub(crate) struct Jwk {
    pub kty: &'static str,
    pub crv: &'static str,
    /// 32-byte public key, base64url-encoded.
    pub x: String,
    pub kid: String,
    #[serde(rename = "use")]
    pub use_: &'static str,
    pub alg: &'static str,
}

/// Anonymous handler — publishes the current public key.
#[axum::debug_handler]
pub(crate) async fn jwks_handler(State(state): State<AppState>) -> Json<Jwks> {
    let key = &state.call_jwt_signing;
    let x = URL_SAFE_NO_PAD.encode(key.verifying.to_bytes());
    Json(Jwks {
        keys: vec![Jwk {
            kty: "OKP",
            crv: "Ed25519",
            x,
            kid: key.kid.clone(),
            use_: "sig",
            alg: JWS_ALG,
        }],
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_key() -> CallSigningKey {
        CallSigningKey::from_seed(&[0xAB; 32])
    }

    fn fixture_claims(now: u64) -> CallJwtClaims {
        CallJwtClaims {
            iss: JWT_ISSUER.into(),
            aud: JWT_AUDIENCE.into(),
            sub: "00112233445566778899aabbccddeeff".into(),
            call_id: "0102030405060708090a0b0c0d0e0f10".into(),
            role: CallRole::Participant,
            iat: now,
            exp: now + JWT_TTL_SECS,
        }
    }

    #[test]
    fn mint_then_verify_round_trips() {
        let key = fixture_key();
        let now = 1_700_000_000;
        let claims = fixture_claims(now);
        let jwt = mint_jwt(&key, &claims).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have 3 segments");

        let parsed = verify_jwt(&key.verifying, &key.kid, &jwt, now + 60).unwrap();
        assert_eq!(parsed.sub, claims.sub);
        assert_eq!(parsed.call_id, claims.call_id);
        assert_eq!(parsed.role, CallRole::Participant);
        assert_eq!(parsed.iat, claims.iat);
        assert_eq!(parsed.exp, claims.exp);
    }

    #[test]
    fn expired_jwt_rejects() {
        let key = fixture_key();
        let claims = fixture_claims(1_000);
        let jwt = mint_jwt(&key, &claims).unwrap();
        // `now` past the exp.
        match verify_jwt(&key.verifying, &key.kid, &jwt, 1_000 + JWT_TTL_SECS) {
            Err(JwtError::Expired) => {},
            other => panic!("expected Expired, got {other:?}"),
        }
        match verify_jwt(&key.verifying, &key.kid, &jwt, 1_000 + JWT_TTL_SECS + 1) {
            Err(JwtError::Expired) => {},
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    #[test]
    fn wrong_signing_key_rejects() {
        let key_a = CallSigningKey::from_seed(&[0xAA; 32]);
        let key_b = CallSigningKey::from_seed(&[0xBB; 32]);
        let claims = fixture_claims(1_700_000_000);
        let jwt = mint_jwt(&key_a, &claims).unwrap();
        // Verify with the WRONG public key — but a matching
        // kid would let it through the kid check; here we
        // pass key_a's kid alongside key_b's vk, which is the
        // realistic "attacker swapped the JWKS doc" shape.
        // The Ed25519 verify must reject regardless.
        match verify_jwt(&key_b.verifying, &key_a.kid, &jwt, 1_700_000_001) {
            Err(JwtError::Signature) => {},
            other => panic!("expected Signature failure, got {other:?}"),
        }
    }

    #[test]
    fn wrong_kid_rejects() {
        let key = fixture_key();
        let claims = fixture_claims(1_700_000_000);
        let jwt = mint_jwt(&key, &claims).unwrap();
        match verify_jwt(&key.verifying, "deadbeefdeadbeef", &jwt, 1_700_000_001) {
            Err(JwtError::Format(msg)) => {
                assert!(msg.contains("kid"), "got {msg}");
            },
            other => panic!("expected Format(kid mismatch), got {other:?}"),
        }
    }

    #[test]
    fn tampered_payload_rejects() {
        let key = fixture_key();
        let claims = fixture_claims(1_700_000_000);
        let jwt = mint_jwt(&key, &claims).unwrap();

        // Mutate one character in the payload segment. The
        // signature was computed over the original; the
        // mutated input will not verify.
        let mut parts: Vec<&str> = jwt.split('.').collect();
        let mut payload = parts[1].to_string();
        let last = payload.pop().unwrap();
        // Flip the last char to a different base64url one.
        // (URL_SAFE_NO_PAD alphabet contains both 'A' and
        // 'B'; pick whichever differs from `last`.)
        let new_last = if last == 'A' { 'B' } else { 'A' };
        payload.push(new_last);
        parts[1] = &payload;
        let tampered = parts.join(".");
        match verify_jwt(&key.verifying, &key.kid, &tampered, 1_700_000_001) {
            Err(JwtError::Signature | JwtError::Format(_) | JwtError::Encode(_)) => {},
            other => panic!("expected verify failure, got {other:?}"),
        }
    }

    #[test]
    fn wrong_issuer_rejects() {
        let key = fixture_key();
        let now = 1_700_000_000;
        let mut claims = fixture_claims(now);
        claims.iss = "evil".into();
        let jwt = mint_jwt(&key, &claims).unwrap();
        match verify_jwt(&key.verifying, &key.kid, &jwt, now + 1) {
            Err(JwtError::Format(msg)) => {
                assert!(msg.contains("iss"), "got {msg}");
            },
            other => panic!("expected Format(iss), got {other:?}"),
        }
    }

    #[test]
    fn wrong_audience_rejects() {
        let key = fixture_key();
        let now = 1_700_000_000;
        let mut claims = fixture_claims(now);
        claims.aud = "evil".into();
        let jwt = mint_jwt(&key, &claims).unwrap();
        match verify_jwt(&key.verifying, &key.kid, &jwt, now + 1) {
            Err(JwtError::Format(msg)) => {
                assert!(msg.contains("aud"), "got {msg}");
            },
            other => panic!("expected Format(aud), got {other:?}"),
        }
    }

    #[test]
    fn role_round_trips_each_variant() {
        let key = fixture_key();
        let now = 1_700_000_000;
        for role in [
            CallRole::Participant,
            CallRole::Publisher,
            CallRole::Subscriber,
        ] {
            let mut claims = fixture_claims(now);
            claims.role = role;
            let jwt = mint_jwt(&key, &claims).unwrap();
            let parsed = verify_jwt(&key.verifying, &key.kid, &jwt, now + 1).unwrap();
            assert_eq!(parsed.role, role);
        }
    }

    #[test]
    fn kid_is_stable_per_keypair() {
        let key_a = CallSigningKey::from_seed(&[0x11; 32]);
        let key_a_again = CallSigningKey::from_seed(&[0x11; 32]);
        let key_b = CallSigningKey::from_seed(&[0x22; 32]);
        assert_eq!(key_a.kid, key_a_again.kid);
        assert_ne!(key_a.kid, key_b.kid);
        assert_eq!(key_a.kid.len(), 16);
    }
}
