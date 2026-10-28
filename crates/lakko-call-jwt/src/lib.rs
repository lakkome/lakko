// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call-admission JWT (Phase 3b4) — JWS Compact + EdDSA over
//! Ed25519, with JWKS publication per RFC 8037.
//!
//! Shared library between the **issuance side** (in
//! `services/auth`, which mints JWTs at `POST /call/jwt`)
//! and the **client / verifier side** (in
//! `crates/lakko-ffi-uniffi` + `crates/lakko-ffi-wasm`, which
//! lets a client parse / self-verify a JWT it received and
//! fetch the JWKS doc itself for the SFU verification path).
//!
//! The same module also serves as a reference Rust
//! implementation that the SFU-side Go verifier (3b1c)
//! interops against; that's how we keep the wire shape
//! immune to drift.
//!
//! Shape per [ADR-0008](../../docs/adr/0008-livekit-fork-strategy.md)
//! §"Auth token format":
//!
//! ```text
//! {
//!   "iss": "lakko-auth",
//!   "aud": "lakko-sfu",
//!   "room": "<room_hex>",             // 32 bytes, BLAKE3 over the call id
//!   "peer_id": "<peer_id_hex>",       // 16 random bytes, fresh per token
//!   "role": "participant" | "publisher" | "subscriber",
//!   "iat": <unix-seconds>,
//!   "exp": <iat + 3600>
//! }
//! ```
//!
//! ## No account in the token (ADR-0078 section 2)
//!
//! The token names no account and no call: `room` is a one-way function of
//! the 16-byte call id that only the call's members know
//! (`lakko_sfu_proto::room_id`), and `peer_id` is drawn at random by the
//! issuer for each token. Auth mints it against an anonymous `call-join`
//! token, not a session, so neither the issuer nor the SFU can tie a join
//! to an account. The claim set is closed (`deny_unknown_fields`): a token
//! carrying a `sub` or a `call_id` does not parse.
//!
//! Audit-side invariants live in
//! [`docs/security/calls.md`](../../docs/security/calls.md)
//! §"Call-admission JWT invariants" — C-15..C-21, each
//! cites a test in this crate's `tests` module.
//!
//! ## Why hand-rolled JWS (not the `jsonwebtoken` crate)
//!
//! - Smaller dep surface (per `CLAUDE.md` code-style rules).
//! - JWS Compact is three base64url segments — under 200
//!   lines of code, every byte is auditable.
//! - Ed25519 is already in [`lakko_crypto::signing`]; no
//!   need for a second sign/verify implementation.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt::Write as _;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// JWT validity window — 1 hour per ADR-0008. Bounds the
/// damage from a leaked JWT.
pub const JWT_TTL_SECS: u64 = 3600;

/// Issuer string baked into every JWT. The SFU verifier
/// (3b1c) checks this against an expected constant.
pub const JWT_ISSUER: &str = "lakko-auth";

/// Audience string baked into every JWT. The SFU verifier
/// checks this so a JWT minted for some other service can't
/// be replayed at the SFU.
pub const JWT_AUDIENCE: &str = "lakko-sfu";

/// JWS alg field per RFC 8037 §3 (EdDSA over Ed25519).
pub const JWS_ALG: &str = "EdDSA";

/// JWK `kty` value for the OKP Ed25519 key form (RFC 8037 §2).
pub const JWK_KTY: &str = "OKP";

/// JWK `crv` value for Ed25519 keys (RFC 8037 §2).
pub const JWK_CRV: &str = "Ed25519";

/// Call participation role. Mirrors the LiveKit shape but
/// owned by us so the JWT is a closed type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallRole {
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

/// Length of the `room` claim: 32 bytes as lower-case hex.
pub const ROOM_HEX_LEN: usize = 64;

/// Length of the `peer_id` claim: 16 bytes as lower-case hex.
pub const PEER_ID_HEX_LEN: usize = 32;

/// Whether `s` is exactly `len` lower-case hex characters.
#[must_use]
pub fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The CallJwt claim set serialised as the JWT payload.
///
/// Closed: an unknown claim (a `sub`, a `call_id`) fails the parse, so no
/// token naming an account can be accepted by a verifier built on this
/// crate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallJwtClaims {
    /// `iss` — issuer; always [`JWT_ISSUER`].
    pub iss: String,
    /// `aud` — audience; always [`JWT_AUDIENCE`].
    pub aud: String,
    /// The room the token admits to: 32 bytes as lower-case hex,
    /// `lakko_sfu_proto::room_id(call_id)`. Knowing it is the capability to
    /// probe the room; only members of the call can compute it.
    pub room: String,
    /// The SFU peer id this token admits as: 16 random bytes as lower-case
    /// hex, drawn by the issuer per token. The SFU keys the peer on it.
    pub peer_id: String,
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
pub struct JwsHeader {
    /// Algorithm — always `"EdDSA"` for our JWTs.
    pub alg: String,
    /// Token type — always `"JWT"`.
    pub typ: String,
    /// Key id — first 8 bytes of BLAKE3(public_key), hex.
    /// The SFU verifier picks the matching JWK from the
    /// JWKS doc by this field.
    pub kid: String,
}

/// Issuer-side signing key, paired with its key id. The
/// public half (`verifying`) is what we publish via
/// `/.well-known/jwks.json`; the private half (`signing`)
/// stays in the DB + process memory.
pub struct CallSigningKey {
    /// Ed25519 signing key.
    pub signing: SigningKey,
    /// Ed25519 verifying key.
    pub verifying: VerifyingKey,
    /// Stable identifier for this keypair. Derived as the
    /// first 8 bytes of BLAKE3(public_key) rendered as hex
    /// (so 16 hex chars). Stable across restarts because
    /// the keypair is persisted; bumped only when the key
    /// is rotated.
    pub kid: String,
}

impl CallSigningKey {
    /// Build from a 32-byte Ed25519 seed.
    #[must_use]
    pub fn from_seed(seed: &[u8; 32]) -> Self {
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
#[must_use]
pub fn kid_for(vk: &VerifyingKey) -> String {
    let digest = hash::blake3(&vk.to_bytes());
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Assemble + sign a JWT.
///
/// Returns the full JWS Compact representation
/// `header.payload.signature`. Per RFC 7515 §3.1, base64url
/// without padding.
///
/// # Errors
///
/// [`JwtError::Encode`] only — JSON encoding of fully-typed
/// inputs is practically infallible; surfaced for completeness.
pub fn mint_jwt(key: &CallSigningKey, claims: &CallJwtClaims) -> Result<String, JwtError> {
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

/// Decode the JWT into its three parts without verification.
/// Returns (header, payload-claims) on a well-formed token.
///
/// Useful client-side for inspecting `exp` / `role` before
/// presenting the JWT to the SFU. **Do NOT** trust the
/// returned claims without a subsequent verify step.
///
/// # Errors
///
/// - [`JwtError::Format`] when the JWS shape is wrong
///   (wrong segment count, base64 decode failure).
/// - [`JwtError::Encode`] when JSON decode of the
///   header / payload fails (malformed UTF-8 / not an
///   object).
pub fn parse_unverified(jwt: &str) -> Result<(JwsHeader, CallJwtClaims), JwtError> {
    let mut parts = jwt.split('.');
    let header_b64 = parts.next().ok_or(JwtError::Format("missing header"))?;
    let payload_b64 = parts.next().ok_or(JwtError::Format("missing payload"))?;
    let _signature_b64 = parts.next().ok_or(JwtError::Format("missing signature"))?;
    if parts.next().is_some() {
        return Err(JwtError::Format("extra segments"));
    }
    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|_| JwtError::Format("header base64"))?;
    let header: JwsHeader =
        serde_json::from_slice(&header_bytes).map_err(|e| JwtError::Encode(e.to_string()))?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| JwtError::Format("payload base64"))?;
    let claims: CallJwtClaims =
        serde_json::from_slice(&payload_bytes).map_err(|e| JwtError::Encode(e.to_string()))?;
    Ok((header, claims))
}

/// Verify a JWT against an expected signing key.
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
pub fn verify_jwt(
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
    let sig = Signature::from_bytes(&signature_bytes)
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
    if !is_lower_hex(&claims.room, ROOM_HEX_LEN) {
        return Err(JwtError::Format("room must be 64 lower-case hex"));
    }
    if !is_lower_hex(&claims.peer_id, PEER_ID_HEX_LEN) {
        return Err(JwtError::Format("peer_id must be 32 lower-case hex"));
    }
    if now_secs >= claims.exp {
        return Err(JwtError::Expired);
    }
    Ok(claims)
}

// ----------------------------------------------------------------
// JWKS (RFC 8037) — JWK Set parsing + key lookup
// ----------------------------------------------------------------

/// A single JWK entry — Ed25519 in OKP form per RFC 8037.
/// Serialisable both ways (issuer publishes; verifier
/// consumes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Jwk {
    /// Key type — always `"OKP"` for our keys.
    pub kty: String,
    /// Curve — always `"Ed25519"` for our keys.
    pub crv: String,
    /// 32-byte public key, base64url-encoded (no padding).
    pub x: String,
    /// Key id — matches the `kid` field in the JWT header.
    pub kid: String,
    /// Intended use — always `"sig"`.
    #[serde(rename = "use")]
    pub use_: String,
    /// Algorithm — always `"EdDSA"`.
    pub alg: String,
}

/// JWKS document — array of [`Jwk`] entries. The auth
/// service publishes one at `/.well-known/jwks.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Jwks {
    /// Set of public keys.
    pub keys: Vec<Jwk>,
}

impl Jwk {
    /// Build a JWK from a Lakko signing key. The `x` field
    /// is the base64url of the 32-byte public key.
    #[must_use]
    pub fn from_signing_key(key: &CallSigningKey) -> Self {
        Self {
            kty: JWK_KTY.into(),
            crv: JWK_CRV.into(),
            x: URL_SAFE_NO_PAD.encode(key.verifying.to_bytes()),
            kid: key.kid.clone(),
            use_: "sig".into(),
            alg: JWS_ALG.into(),
        }
    }

    /// Decode the JWK back to a [`VerifyingKey`]. Validates
    /// `kty` / `crv` / `alg` shape.
    ///
    /// # Errors
    ///
    /// [`JwtError::Format`] for any shape or decode failure.
    pub fn to_verifying_key(&self) -> Result<VerifyingKey, JwtError> {
        if self.kty != JWK_KTY {
            return Err(JwtError::Format("jwk.kty != OKP"));
        }
        if self.crv != JWK_CRV {
            return Err(JwtError::Format("jwk.crv != Ed25519"));
        }
        if self.alg != JWS_ALG {
            return Err(JwtError::Format("jwk.alg != EdDSA"));
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.x)
            .map_err(|_| JwtError::Format("jwk.x base64"))?;
        VerifyingKey::from_bytes(&bytes).map_err(|_| JwtError::Format("jwk.x length"))
    }
}

/// Parse a JWKS doc (the bytes from `/.well-known/jwks.json`).
///
/// # Errors
///
/// [`JwtError::Encode`] when the JSON fails to decode.
pub fn parse_jwks(doc_bytes: &[u8]) -> Result<Jwks, JwtError> {
    serde_json::from_slice(doc_bytes).map_err(|e| JwtError::Encode(e.to_string()))
}

/// Verify a JWT against a JWKS document: parse the JWT
/// header, find the matching `kid` in the JWKS, build a
/// [`VerifyingKey`] from that JWK, run [`verify_jwt`].
///
/// `now_secs` is injected so the caller (SFU verifier,
/// or a client self-verifying) controls the clock source.
///
/// # Errors
///
/// - [`JwtError::Format`] when the JWT is malformed or no
///   JWK matches the `kid` in the JWT header.
/// - [`JwtError::Signature`] / [`JwtError::Expired`] /
///   [`JwtError::Encode`] surfaced from [`verify_jwt`].
pub fn verify_with_jwks(jwks: &Jwks, jwt: &str, now_secs: u64) -> Result<CallJwtClaims, JwtError> {
    let (header, _) = parse_unverified(jwt)?;
    let jwk = jwks
        .keys
        .iter()
        .find(|k| k.kid == header.kid)
        .ok_or(JwtError::Format("no JWK matches header.kid"))?;
    let vk = jwk.to_verifying_key()?;
    verify_jwt(&vk, &jwk.kid, jwt, now_secs)
}

/// Errors surfaced by [`mint_jwt`] / [`verify_jwt`] /
/// [`verify_with_jwks`] / [`parse_unverified`] /
/// [`parse_jwks`] / [`Jwk::to_verifying_key`]. Pattern-match
/// on this in tests + in FFI translation layers.
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
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
            room: "0102030405060708090a0b0c0d0e0f100102030405060708090a0b0c0d0e0f10".into(),
            peer_id: "00112233445566778899aabbccddeeff".into(),
            role: CallRole::Participant,
            iat: now,
            exp: now + JWT_TTL_SECS,
        }
    }

    /// ADR-0078 section 2: a minted token's payload names no account and no
    /// call id - its keys are exactly the closed claim set.
    #[test]
    fn a_minted_jwt_has_no_sub_and_no_call_id() {
        let key = fixture_key();
        let jwt = mint_jwt(&key, &fixture_claims(1_700_000_000)).unwrap();
        let payload_b64 = jwt.split('.').nth(1).unwrap();
        let payload = URL_SAFE_NO_PAD.decode(payload_b64).unwrap();
        let object: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&payload).unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["aud", "exp", "iat", "iss", "peer_id", "role", "room"]
        );
        assert!(!object.contains_key("sub"));
        assert!(!object.contains_key("call_id"));
    }

    /// A token carrying an account (`sub`) does not parse at all.
    #[test]
    fn a_token_with_a_sub_claim_is_refused() {
        let key = fixture_key();
        let now = 1_700_000_000;
        let mut value = serde_json::to_value(fixture_claims(now)).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("sub".into(), "00112233445566778899aabbccddeeff".into());
        let header = JwsHeader {
            alg: JWS_ALG.into(),
            typ: "JWT".into(),
            kid: key.kid.clone(),
        };
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
        let input = format!("{header_b64}.{payload_b64}");
        let sig = URL_SAFE_NO_PAD.encode(key.signing.sign(input.as_bytes()).to_bytes());
        let jwt = format!("{input}.{sig}");
        assert!(matches!(
            verify_jwt(&key.verifying, &key.kid, &jwt, now + 1),
            Err(JwtError::Encode(_))
        ));
    }

    #[test]
    fn malformed_room_or_peer_id_is_refused() {
        let key = fixture_key();
        let now = 1_700_000_000;
        for (room, peer) in [
            (
                "ab".to_owned(),
                "00112233445566778899aabbccddeeff".to_owned(),
            ),
            (
                "AB".repeat(32),
                "00112233445566778899aabbccddeeff".to_owned(),
            ),
            ("ab".repeat(32), "0011".to_owned()),
            (
                "ab".repeat(32),
                "00112233445566778899AABBCCDDEEFF".to_owned(),
            ),
        ] {
            let mut claims = fixture_claims(now);
            claims.room = room;
            claims.peer_id = peer;
            let jwt = mint_jwt(&key, &claims).unwrap();
            assert!(matches!(
                verify_jwt(&key.verifying, &key.kid, &jwt, now + 1),
                Err(JwtError::Format(_))
            ));
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
        assert_eq!(parsed, claims);
    }

    #[test]
    fn parse_unverified_extracts_claims() {
        let key = fixture_key();
        let now = 1_700_000_000;
        let claims = fixture_claims(now);
        let jwt = mint_jwt(&key, &claims).unwrap();
        let (header, parsed) = parse_unverified(&jwt).unwrap();
        assert_eq!(header.alg, JWS_ALG);
        assert_eq!(header.kid, key.kid);
        assert_eq!(parsed, claims);
    }

    #[test]
    fn expired_jwt_rejects() {
        let key = fixture_key();
        let claims = fixture_claims(1_000);
        let jwt = mint_jwt(&key, &claims).unwrap();
        match verify_jwt(&key.verifying, &key.kid, &jwt, 1_000 + JWT_TTL_SECS) {
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
            Err(JwtError::Format(msg)) => assert!(msg.contains("kid")),
            other => panic!("expected Format(kid mismatch), got {other:?}"),
        }
    }

    #[test]
    fn tampered_payload_rejects() {
        let key = fixture_key();
        let claims = fixture_claims(1_700_000_000);
        let jwt = mint_jwt(&key, &claims).unwrap();

        let mut parts: Vec<&str> = jwt.split('.').collect();
        let mut payload = parts[1].to_string();
        let last = payload.pop().unwrap();
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
            Err(JwtError::Format(msg)) => assert!(msg.contains("iss")),
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
            Err(JwtError::Format(msg)) => assert!(msg.contains("aud")),
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

    #[test]
    fn jwks_round_trips_through_json() {
        let key = fixture_key();
        let jwks = Jwks {
            keys: vec![Jwk::from_signing_key(&key)],
        };
        let doc = serde_json::to_vec(&jwks).unwrap();
        let parsed = parse_jwks(&doc).unwrap();
        assert_eq!(parsed.keys.len(), 1);
        let vk = parsed.keys[0].to_verifying_key().unwrap();
        assert_eq!(vk.to_bytes(), key.verifying.to_bytes());
    }

    #[test]
    fn verify_with_jwks_resolves_by_kid() {
        let key_a = CallSigningKey::from_seed(&[0xAA; 32]);
        let key_b = CallSigningKey::from_seed(&[0xBB; 32]);
        let now = 1_700_000_000;
        let claims_a = fixture_claims(now);
        let jwt_a = mint_jwt(&key_a, &claims_a).unwrap();

        // JWKS doc carries BOTH keys (during rotation). The
        // verifier must pick the right one by kid.
        let jwks = Jwks {
            keys: vec![Jwk::from_signing_key(&key_a), Jwk::from_signing_key(&key_b)],
        };
        let parsed = verify_with_jwks(&jwks, &jwt_a, now + 60).unwrap();
        assert_eq!(parsed, claims_a);
    }

    #[test]
    fn verify_with_jwks_rejects_unknown_kid() {
        let key = fixture_key();
        let now = 1_700_000_000;
        let claims = fixture_claims(now);
        let jwt = mint_jwt(&key, &claims).unwrap();
        // JWKS contains a DIFFERENT key — no matching kid.
        let other = CallSigningKey::from_seed(&[0xCC; 32]);
        let jwks = Jwks {
            keys: vec![Jwk::from_signing_key(&other)],
        };
        match verify_with_jwks(&jwks, &jwt, now + 60) {
            Err(JwtError::Format(msg)) => assert!(msg.contains("kid")),
            other => panic!("expected no-kid-match Format error, got {other:?}"),
        }
    }

    #[test]
    fn jwk_to_verifying_key_rejects_wrong_kty() {
        let key = fixture_key();
        let mut jwk = Jwk::from_signing_key(&key);
        jwk.kty = "RSA".into();
        match jwk.to_verifying_key() {
            Err(JwtError::Format(msg)) => assert!(msg.contains("kty")),
            other => panic!("expected Format(kty), got {other:?}"),
        }
    }
}
