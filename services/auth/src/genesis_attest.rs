// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Auth-attested KT-log genesis — the mint endpoint (G-30 finding 2,
//! [ADR-0036]).
//!
//! `POST /auth/genesis/attest` lets an OPAQUE-authenticated account obtain a
//! signed [`GenesisAttestation`] binding its `account_id` to the identity
//! pubkey it presents as its genesis (first-ever) KT-log key. The transparency
//! append gate later refuses a genesis leaf whose attestation is absent or does
//! not verify, so first-entry squatting is PREVENTED (not merely detected):
//! only the account holder — the sole party who can OPAQUE-authenticate as
//! `account_id`, hence hold a valid session token — can mint one.
//!
//! ## What the attestation proves (state it plainly)
//!
//! > the party that OPAQUE-authenticated as `account_id` presented THIS pubkey
//! > as its genesis identity key, at the time the attestation was minted.
//!
//! It is NOT an independently-verified claim that the pubkey is the
//! cryptographically "right" key for the account — auth has no such ground
//! truth under the zero-trust model (the identity key is client-generated,
//! invariant #2). It is a **binding of the OPAQUE-authenticated account to a
//! client-asserted key**, which is exactly sufficient to prevent squatting
//! ([ADR-0036] "the load-bearing design decision").
//!
//! ## Distinct from the passkey action flow
//!
//! This is a NEW endpoint, not an extension of `/auth/action/finish`: genesis
//! must work for OPAQUE-only accounts that have never registered a passkey. It
//! is authenticated by the session-token bearer middleware only
//! ([`AuthenticatedAccount`]) — holding a valid session token IS proof the
//! caller OPAQUE-authenticated as `account_id`, the entire security basis.
//!
//! ## `username_hash` is derived, not looked up
//!
//! Transparency never learns `account_id`, so the attestation carries the
//! bridging `username_hash` inside it. Auth derives it via the ONE shared
//! definition ([`lakko_transparency::account_username_hash`]) so it is
//! byte-identical to the discovery log-append side ([ADR-0036] drift hazard).
//!
//! Stateless: no DB row is written. A valid attestation for one's own account
//! is self-limited by the transparency gate's version monotonicity (a second
//! "genesis" for a `username_hash` that already has an entry is a rotation,
//! refused as an unauthorised genesis). The short [`GENESIS_ATTEST_TTL_SECS`]
//! TTL bounds the mint -> append replay window. The signing key + published
//! JWKS are REUSED from the device-attestation role ([ADR-0036] §3); the
//! distinct `lakko/genesis-attest/v1` domain separator makes the two
//! attestation kinds cryptographically non-interchangeable.
//!
//! [ADR-0036]: ../../../docs/adr/0036-auth-attested-genesis.md

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_attest::GenesisAttestation;
use lakko_transparency::account_username_hash;
use serde::{Deserialize, Serialize};

use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Length of the client-supplied genesis identity pubkey (Ed25519).
const PUBKEY_LEN: usize = 32;

/// Lifetime of a minted genesis attestation ([ADR-0036] §2). It bounds only the
/// mint -> transparency-append replay window (NOT the genesis leaf's lifetime —
/// the client verify path enforces no freshness), so it is deliberately short.
/// Mirrors the action-attestation TTL (`webauthn_action::ATTESTATION_TTL_SECS`,
/// one hour) — the same trust tier and comfortably long enough for the client
/// to publish its bundle + the reconciler to append.
///
/// [ADR-0036]: ../../../docs/adr/0036-auth-attested-genesis.md
pub(crate) const GENESIS_ATTEST_TTL_SECS: i64 = 3600;

/// Decode the URL-safe base64 32-byte genesis identity pubkey from the wire.
fn decode_pubkey(b64: &str) -> Result<[u8; PUBKEY_LEN], ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("pubkey_b64 not URL-safe base64"))?;
    if bytes.len() != PUBKEY_LEN {
        return Err(ApiError::BadInput("pubkey must decode to 32 bytes"));
    }
    let mut out = [0u8; PUBKEY_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Current time in unix seconds, saturating on the (impossible) overflow.
fn now_unix_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
    .unwrap_or(i64::MAX)
}

/// `POST /auth/genesis/attest` — mint a genesis attestation (G-30 finding 2,
/// [ADR-0036]).
///
/// Bearer-authenticated. Derives `username_hash` from the authenticated
/// `account_id` (NEVER from the request body — the body carries only the
/// client's genesis pubkey), signs a [`GenesisAttestation`] over
/// `(account_id, username_hash, pubkey, expiry)` with the shared attestation
/// key, and returns it base64-encoded. Stateless — no DB row.
///
/// # Errors
///
/// [`ApiError::BadInput`] when `pubkey_b64` is not 32-byte URL-safe base64.
///
/// [ADR-0036]: ../../../docs/adr/0036-auth-attested-genesis.md
#[axum::debug_handler]
pub(crate) async fn genesis_attest(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<GenesisAttestReq>,
) -> Result<Json<GenesisAttestRes>, ApiError> {
    let pubkey = decode_pubkey(&req.pubkey_b64)?;

    // Derived from the AUTHENTICATED account id — the bridge that lets the
    // transparency gate bind the attestation to the KT-log leaf without ever
    // learning `account_id`. The ONE shared definition (ADR-0036 drift hazard).
    let username_hash = account_username_hash(&auth.0);

    let expires_at = now_unix_secs().saturating_add(GENESIS_ATTEST_TTL_SECS);

    // Reuse the device-attestation signing key (ADR-0036 §3); the distinct
    // `lakko/genesis-attest/v1` domain keeps the two attestation kinds
    // cryptographically non-interchangeable under the shared key.
    let attestation = GenesisAttestation::new(auth.0, username_hash, pubkey, expires_at)
        .sign(&state.attest_key.signing);

    Ok(Json(GenesisAttestRes {
        genesis_attest_b64: URL_SAFE_NO_PAD.encode(attestation),
    }))
}

// ----------------------------------------------------------------
// Wire shapes — /auth/genesis/attest
// ----------------------------------------------------------------

/// Request body for `POST /auth/genesis/attest`.
#[derive(Deserialize)]
pub(crate) struct GenesisAttestReq {
    /// URL-safe base64 of the client's 32-byte genesis identity pubkey
    /// (Ed25519). The server binds it to the authenticated account; it does
    /// not independently verify the key is "correct" (it cannot, invariant #2).
    pub pubkey_b64: String,
}

/// Response body for `POST /auth/genesis/attest`.
#[derive(Serialize)]
pub(crate) struct GenesisAttestRes {
    /// URL-safe base64 of the signed [`GenesisAttestation`]. The client uploads
    /// it alongside its identity bundle; discovery forwards it opaquely to the
    /// transparency append gate.
    pub genesis_attest_b64: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opaque::LakkoCipherSuite;
    use lakko_attest::{verify_genesis_binding, GenesisAttestError};
    use opaque_ke::ServerSetup;
    use rand::rngs::OsRng;
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::Arc;

    /// Fixed seed for the test attestation key so the test reconstructs the
    /// verifying key that signed.
    const ATTEST_SEED: [u8; 32] = [0xCD; 32];

    /// Build an [`AppState`] WITHOUT a live database. The genesis handler is
    /// stateless (it never touches the pool), so a lazily-connected pool that
    /// is never awaited suffices — the whole test runs DB-free.
    fn build_state_no_db() -> AppState {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://lakko:devpass@127.0.0.1:3306/lakko_auth")
            .expect("lazy pool (never connects)");
        let opaque_setup = ServerSetup::<LakkoCipherSuite>::new(&mut OsRng);
        let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
        AppState {
            db: pool,
            opaque_setup: Arc::new(opaque_setup),
            session_token_secret: Arc::new(token_secret),
            session_token_ttl_ms: 86_400_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&ATTEST_SEED)),
            turn: None,
            totp_enc_key: None,
            recovery_contact_pepper: None,
            otp_sender: Arc::new(crate::otp_sender::LogOnlyOtpSender),
            throttle: crate::throttle::ThrottleConfig::disabled(),
        }
    }

    /// The handler mints an attestation that verifies under the published
    /// attestation key and is bound to the authenticated account, the derived
    /// `username_hash`, and the presented pubkey — and rejects a wrong pubkey.
    #[tokio::test]
    async fn mints_a_genesis_attestation_bound_to_account_and_pubkey() {
        let state = build_state_no_db();
        let account_id: [u8; 16] = [0x11; 16];
        let pubkey: [u8; 32] = [0x22; 32];

        let res = genesis_attest(
            State(state),
            Extension(AuthenticatedAccount(account_id)),
            Json(GenesisAttestReq {
                pubkey_b64: URL_SAFE_NO_PAD.encode(pubkey),
            }),
        )
        .await
        .expect("mint succeeds");

        let bytes = URL_SAFE_NO_PAD
            .decode(&res.0.genesis_attest_b64)
            .expect("attestation decodes");

        // The verifying key that signed (reconstructed from the fixed seed).
        let attest_vk = lakko_call_jwt::CallSigningKey::from_seed(&ATTEST_SEED).verifying;
        // The SAME derivation the handler used, from the shared crate.
        let username_hash = account_username_hash(&account_id);

        // Binds to the right account / username_hash / pubkey.
        let claims =
            verify_genesis_binding(&bytes, &attest_vk, &account_id, &username_hash, &pubkey)
                .expect("attestation verifies + binds to this account/username/pubkey");
        assert_eq!(claims.account_id, account_id);
        assert_eq!(claims.username_hash, username_hash);
        assert_eq!(claims.pubkey, pubkey);

        // A DIFFERENT pubkey must NOT verify against this attestation — the
        // core anti-squatting binding (auth attested exactly the presented key).
        assert_eq!(
            verify_genesis_binding(&bytes, &attest_vk, &account_id, &username_hash, &[0x33; 32],),
            Err(GenesisAttestError::PubkeyMismatch),
        );
    }

    #[tokio::test]
    async fn rejects_a_malformed_pubkey() {
        let state = build_state_no_db();
        let res = genesis_attest(
            State(state),
            Extension(AuthenticatedAccount([0x11; 16])),
            Json(GenesisAttestReq {
                // 31 bytes — one short.
                pubkey_b64: URL_SAFE_NO_PAD.encode([0x22; 31]),
            }),
        )
        .await;
        assert!(matches!(res, Err(ApiError::BadInput(_))));
    }
}
