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
//! ## The recorded key ([ADR-0074] section 0.1)
//!
//! The first mint for an account records the attested key in
//! `accounts.identity_pubkey`, under the account's row lock, after the key's
//! own identity proof over the account id verified and its target was
//! claimed for the account (`crate::identity_target`, ADR-0098 F3: a key
//! belongs to one account). From then on the
//! route attests **only that key**: any other is answered `409
//! {"error":"identity-key-mismatch"}`. So a session can no longer obtain a
//! genesis attestation for an arbitrary key, and auth holds the public key a
//! later account statement (remove a device, and the recall and rotation of
//! the same ADR) is verified under. The mint is idempotent for the recorded
//! key; a client calls it on every session landing until it has succeeded
//! once. The one writer that moves the recorded key is the key reset
//! (`crate::key_reset`), which sets it to the key the reset installs.
//!
//! A key that does not decode as an Ed25519 point, or is of small order, is
//! refused (`400`): a signature "under" a small-order key can be forged for
//! any message, and this key will verify statements.
//!
//! A valid attestation for one's own account is self-limited by the
//! transparency gate's version monotonicity (a second "genesis" for a
//! `username_hash` that already has an entry is a rotation, refused as an
//! unauthorised genesis). The short [`GENESIS_ATTEST_TTL_SECS`] TTL bounds
//! the mint -> append replay window. The signing key + published JWKS are
//! REUSED from the device-attestation role ([ADR-0036] §3); the distinct
//! `lakko/genesis-attest/v1` domain separator makes the two attestation kinds
//! cryptographically non-interchangeable.
//!
//! [ADR-0074]: ../../../docs/adr/0074-account-takeover-resistance.md
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
    require_usable_identity_key(&out)?;
    Ok(out)
}

/// Refuse a key auth must not record as an account's identity key: one that
/// does not decode as an Ed25519 point, or one of small order (a signature
/// under it verifies for any message). Shared by the genesis mint and the key
/// reset, the two writers of `accounts.identity_pubkey`.
///
/// # Errors
///
/// [`ApiError::BadInput`].
pub(crate) fn require_usable_identity_key(key: &[u8; PUBKEY_LEN]) -> Result<(), ApiError> {
    let verifying = lakko_crypto::signing::VerifyingKey::from_bytes(key)
        .map_err(|_| ApiError::BadInput("pubkey is not an Ed25519 public key"))?;
    if verifying.is_weak() {
        return Err(ApiError::BadInput("pubkey is a small-order key"));
    }
    Ok(())
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
/// Bearer-authenticated. Records the key as the account's identity key if
/// none is recorded yet (one conditional `UPDATE`, so two racing first mints
/// record exactly one key), refuses a key other than the recorded one, then
/// derives `username_hash` from the authenticated `account_id` (NEVER from
/// the request body — the body carries only the client's genesis pubkey),
/// signs a [`GenesisAttestation`] over `(account_id, username_hash, pubkey,
/// expiry)` with the shared attestation key, and returns it base64-encoded.
///
/// # Errors
///
/// [`ApiError::BadInput`] when `pubkey_b64` is not 32-byte URL-safe base64 of
/// a usable Ed25519 key; `400 identity-proof-invalid` when the identity proof
/// is absent or does not verify; `409 identity-key-taken` when another
/// account holds the key; [`ApiError::IdentityKeyMismatch`] (`409`) when the
/// account's recorded key is another one; [`ApiError::InvalidCredential`]
/// when the account was deleted meanwhile; [`ApiError::Db`].
///
/// [ADR-0036]: ../../../docs/adr/0036-auth-attested-genesis.md
#[axum::debug_handler]
pub(crate) async fn genesis_attest(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<GenesisAttestReq>,
) -> Result<Json<GenesisAttestRes>, ApiError> {
    let pubkey = decode_pubkey(&req.pubkey_b64)?;
    // ADR-0098 F3: the caller holds the key it asks auth to record.
    crate::identity_target::require_identity_proof(
        req.identity_proof_b64.as_deref(),
        &auth.0,
        &pubkey,
    )?;

    // ADR-0074 section 0.1: record the key on the first mint, attest only the
    // recorded key afterwards. Under the account's row lock, so two racing
    // first mints record exactly one key; the history row (ADR-0098 F3) is
    // claimed in the same transaction, so a key another account holds is
    // refused and nothing is recorded.
    let mut tx = state.db.begin().await.map_err(ApiError::Db)?;
    let recorded: Option<Option<Vec<u8>>> =
        sqlx::query_scalar("SELECT identity_pubkey FROM accounts WHERE id = ? FOR UPDATE")
            .bind(&auth.0[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    match recorded {
        None => return Err(ApiError::InvalidCredential),
        Some(Some(key)) if key.as_slice() != pubkey.as_slice() => {
            tracing::info!("genesis attest: a key other than the recorded one refused");
            return Err(ApiError::IdentityKeyMismatch);
        },
        Some(Some(_)) => {
            // The recorded key: make sure its history row is there.
            crate::identity_target::claim(&mut tx, &auth.0, &pubkey, crate::session::now_ms())
                .await?;
        },
        Some(None) => {
            crate::identity_target::claim(&mut tx, &auth.0, &pubkey, crate::session::now_ms())
                .await?;
            sqlx::query(
                "UPDATE accounts SET identity_pubkey = ?, identity_target = ? \
                 WHERE id = ? AND identity_pubkey IS NULL",
            )
            .bind(&pubkey[..])
            .bind(&crate::identity_target::target_of(&pubkey)[..])
            .bind(&auth.0[..])
            .execute(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
        },
    }
    tx.commit().await.map_err(ApiError::Db)?;

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
    /// URL-safe base64 (any of the four spellings is read) of the 64-byte
    /// identity proof (`signIdentityProof`): the key's own signature over the
    /// account id and the key (ADR-0098 F3). Absent or invalid: `400
    /// identity-proof-invalid`.
    #[serde(default)]
    pub identity_proof_b64: Option<String>,
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

    /// A malformed, undecodable or small-order key is refused before the
    /// database is touched. (The mint itself is covered by the DB-gated
    /// route tests in `crate::devices_tests`.)
    #[test]
    fn unusable_keys_are_refused() {
        // 31 bytes: one short.
        assert!(matches!(
            decode_pubkey(&URL_SAFE_NO_PAD.encode([0x22; 31])),
            Err(ApiError::BadInput(_))
        ));
        assert!(matches!(
            decode_pubkey("not base64!"),
            Err(ApiError::BadInput(_))
        ));
        // The identity point: decodes, order 1.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(matches!(
            decode_pubkey(&URL_SAFE_NO_PAD.encode(identity)),
            Err(ApiError::BadInput(_))
        ));
        // A real key passes.
        let real = lakko_crypto::signing::SigningKey::from_seed(&[7; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(decode_pubkey(&URL_SAFE_NO_PAD.encode(real)).unwrap(), real);
    }
}
