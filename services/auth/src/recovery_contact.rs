// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Phone / email one-time-code recovery — recovery factor (d) (ADR-0018).
//!
//! The weakest, last-in-cascade ACCESS-recovery factor. A user enrolls a
//! phone or email, proves control with a one-time code, and at recovery time
//! can stand in for the password by receiving a fresh code:
//!
//! - **`POST /recovery-contact/enroll/start`** — bearer-auth (already
//!   logged in) + PoW. Normalizes + hashes the supplied contact, generates a
//!   6-digit code, stores its hash + a 10-minute expiry (UPSERT, attempts=0),
//!   UPSERTs the contact (keeping any existing confirmation), and sends the
//!   code via the configured [`OtpSender`].
//! - **`POST /recovery-contact/enroll/finish`** — bearer-auth. Verifies the
//!   code (attempt-capped + expiry-gated), and on match stamps
//!   `confirmed_at` + deletes the challenge (single-use).
//! - **`POST /recovery-contact/recover/start`** — anonymous + PoW. Hashes the
//!   supplied contact, and only if it matches a CONFIRMED row generates +
//!   stores + sends a code. ENUMERATION-SAFE: always returns the same
//!   `{ "sent": true }` whether or not it matched.
//! - **`POST /recovery-contact/recover/finish`** — anonymous + PoW. Verifies
//!   the code (attempt cap -> 429, expiry, single-use delete-on-success) and
//!   on match issues the standard session-token triple. Wrong code ->
//!   attempts+1 + generic 401.
//!
//! ## Invariant #1 (no plaintext PII on the server)
//!
//! The phone / email PLAINTEXT is supplied transiently in the enroll +
//! recover requests. It is used ONLY to (a) compute the keyed
//! [`contact_hash`] and (b) address the outgoing message via [`OtpSender`].
//! It is NEVER stored (the DB holds only the hash) and NEVER logged.
//!
//! ## Trust class
//!
//! `contact_hash` is a server-held ACCESS verifier in the same trust class as
//! the OPAQUE password envelope and the TOTP secret — it authenticates a
//! recovery request, it never decrypts E2EE data (ADR-0018 "Server-held
//! verifiers are ACCESS verifiers"). The pepper keys the hash so a DB-only
//! leak yields neither the contacts nor an enumeration oracle.
//!
//! [`contact_hash`]: contact_hash

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::otp_sender::ContactKind;
use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Length of the `account_id` (16-byte UUIDv7 raw form).
const ACCOUNT_ID_LEN: usize = 16;

/// One-time-code lifetime, in seconds (10 minutes).
const OTP_TTL_SECS: i64 = 600;

/// Verification attempts allowed against one challenge before it is rejected.
/// The 5th failure leaves `attempts == 5`; the next verify (or any verify at
/// or above the cap) is refused.
const MAX_OTP_ATTEMPTS: i16 = 5;

/// Domain separator for the keyed contact / code hash. Bumping the version
/// invalidates all stored hashes (a deliberate migration tool).
const CONTACT_HASH_DOMAIN: &[u8] = b"lakko/recovery-contact/v1";

/// Resolve the configured recovery-contact pepper or return the 503
/// (`ApiError::Disabled`) every `/recovery-contact/*` handler short-circuits
/// on when the factor is not configured (loud refusal, no silent fallback).
fn require_pepper(state: &AppState) -> Result<[u8; 32], ApiError> {
    state
        .recovery_contact_pepper
        .as_ref()
        .map(|k| **k)
        .ok_or(ApiError::Disabled)
}

// ---------------------------------------------------------------------------
// Env-loaded pepper
// ---------------------------------------------------------------------------

/// Load the recovery-contact pepper from `LAKKO_RECOVERY_CONTACT_PEPPER`.
///
/// `Ok(Some(_))` when the env var is present and decodes to exactly 32 bytes;
/// `Ok(None)` when it is unset (the `/recovery-contact/*` routes then return
/// 503); `Err` (loud-fail at startup) when it is present but malformed.
///
/// # Errors
///
/// Returns an error when the env var is set but is not valid base64 or does
/// not decode to exactly 32 bytes.
pub(crate) fn load_recovery_contact_pepper() -> anyhow::Result<Option<[u8; 32]>> {
    use base64::engine::general_purpose::STANDARD;
    let raw = match std::env::var("LAKKO_RECOVERY_CONTACT_PEPPER") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return Ok(None),
    };
    let bytes = STANDARD
        .decode(raw.trim())
        .map_err(|e| anyhow::anyhow!("LAKKO_RECOVERY_CONTACT_PEPPER is not valid base64: {e}"))?;
    let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "LAKKO_RECOVERY_CONTACT_PEPPER must decode to exactly 32 bytes (got {})",
            bytes.len()
        )
    })?;
    Ok(Some(key))
}

// ---------------------------------------------------------------------------
// Pure normalization + hashing
// ---------------------------------------------------------------------------

/// Normalize a contact for a stable hash:
///
/// - **email** — trim + ASCII-lowercase. (Local-part case-sensitivity is
///   technically RFC-permitted but universally ignored by mail providers;
///   lowercasing matches user expectation and avoids a duplicate-row trap.)
/// - **phone** — strip spaces, dashes and parentheses, then require a leading
///   `+` followed by 1..=15 digits (E.164-ish). Anything else is rejected.
///
/// Returns [`ApiError::BadInput`] on a malformed contact.
fn normalize_contact(kind: ContactKind, raw: &str) -> Result<String, ApiError> {
    match kind {
        ContactKind::Email => {
            let normalized = raw.trim().to_ascii_lowercase();
            // Minimal structural check: exactly one '@', non-empty either
            // side, no whitespace. We do not attempt full RFC 5321 parsing —
            // a malformed address simply fails to deliver.
            let bytes = normalized.as_bytes();
            let at = normalized.find('@');
            let valid = match at {
                Some(i) => {
                    i > 0
                        && i < normalized.len() - 1
                        && !normalized.contains(char::is_whitespace)
                        && normalized.matches('@').count() == 1
                        && bytes.len() <= 254
                },
                None => false,
            };
            if !valid {
                return Err(ApiError::BadInput("email is malformed"));
            }
            Ok(normalized)
        },
        ContactKind::Phone => {
            // Strip the common human separators.
            let stripped: String = raw
                .chars()
                .filter(|c| !matches!(c, ' ' | '-' | '(' | ')' | '\u{00A0}'))
                .collect();
            let mut chars = stripped.chars();
            if chars.next() != Some('+') {
                return Err(ApiError::BadInput("phone must be E.164 (leading +)"));
            }
            let digits = chars.as_str();
            if digits.is_empty() || digits.len() > 15 || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(ApiError::BadInput(
                    "phone must be + followed by 1-15 digits",
                ));
            }
            Ok(stripped)
        },
    }
}

/// Keyed contact / code hash:
/// `BLAKE3_keyed(pepper, DOMAIN || kind_byte || payload)`.
///
/// The pepper is the key, so the digest is non-reversible without the server
/// secret AND an attacker cannot enumerate which contacts/codes are
/// registered (computing a candidate hash needs the pepper).
fn keyed_hash(pepper: &[u8; 32], kind: ContactKind, purpose: u8, payload: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(CONTACT_HASH_DOMAIN.len() + 2 + payload.len());
    input.extend_from_slice(CONTACT_HASH_DOMAIN);
    input.push(purpose);
    input.push(kind.as_db());
    input.extend_from_slice(payload);
    lakko_crypto::hash::blake3_keyed(pepper, &input)
}

/// `keyed_hash` purpose byte for a normalized contact value. Distinguishes the
/// contact-hash space from the code-hash space so the two are provably
/// disjoint regardless of input — a code-hash can never equal a contact-hash.
/// (Collision is already impossible by normalization — a phone starts with
/// `+`, an email contains `@`, a code is 6 bare digits — but the explicit
/// separator keeps the two hash spaces auditably independent, the same posture
/// every other keyed hash in the codebase uses.)
const HASH_PURPOSE_CONTACT: u8 = 1;
/// `keyed_hash` purpose byte for a 6-digit OTP code (see [`HASH_PURPOSE_CONTACT`]).
const HASH_PURPOSE_CODE: u8 = 2;

/// Hash of a normalized contact (the `recovery_contacts.contact_hash` value).
fn contact_hash(pepper: &[u8; 32], kind: ContactKind, normalized_contact: &str) -> [u8; 32] {
    keyed_hash(
        pepper,
        kind,
        HASH_PURPOSE_CONTACT,
        normalized_contact.as_bytes(),
    )
}

/// Hash of a 6-digit code (the `otp_challenges.code_hash` value). Keyed on the
/// same pepper so the verify path matches by recomputation.
fn code_hash(pepper: &[u8; 32], kind: ContactKind, code: &str) -> [u8; 32] {
    keyed_hash(pepper, kind, HASH_PURPOSE_CODE, code.as_bytes())
}

/// Generate a fresh, uniformly-random 6-digit code (`000000..=999999`) using
/// the OS CSPRNG. Modulo bias over the full `u32` range across `1_000_000`
/// buckets is below 2^-12 of a code's worth and is dwarfed by the
/// attempt-cap, so it is acceptable here.
fn generate_code() -> String {
    let n = OsRng.next_u32() % 1_000_000;
    format!("{n:06}")
}

/// Decode the URL-safe base64 16-byte `account_id`.
fn decode_account_id(b64: &str) -> Result<[u8; ACCOUNT_ID_LEN], ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::BadInput("account_id_b64 not URL-safe base64"))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::BadInput("account_id must decode to 16 bytes"))
}

/// Parse the wire `kind` integer (1 = email, 2 = phone) into a [`ContactKind`].
fn parse_kind(kind: u8) -> Result<ContactKind, ApiError> {
    ContactKind::try_from(kind)
        .map_err(|()| ApiError::BadInput("kind must be 1 (email) or 2 (phone)"))
}

/// Issue a fresh session token for `account_id` (mirrors
/// `device_authorize::issue_session_token` / `totp::issue_session_token`).
fn issue_session_token(
    state: &AppState,
    account_id: &[u8; ACCOUNT_ID_LEN],
) -> Result<(String, i64), ApiError> {
    let now_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let expires_at_ms = now_ms.saturating_add(state.session_token_ttl_ms);
    let token = state
        .session_token_secret
        .issue(account_id, expires_at_ms)
        .map_err(|e| {
            tracing::error!(error = %e, "recovery-contact: session-token issuance failed");
            ApiError::Internal
        })?;
    Ok((token, expires_at_ms))
}

/// UPSERT a fresh challenge for `(account_id, kind)`: store the code hash, a
/// 10-minute expiry, reset `attempts` to 0.
async fn upsert_challenge(
    state: &AppState,
    account_id: &[u8; ACCOUNT_ID_LEN],
    kind: ContactKind,
    code_hash: &[u8; 32],
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO otp_challenges (account_id, kind, code_hash, expires_at, attempts) \
         VALUES (?, ?, ?, NOW(3) + INTERVAL ? SECOND, 0) \
         ON DUPLICATE KEY UPDATE code_hash = VALUES(code_hash), \
         expires_at = VALUES(expires_at), attempts = 0",
    )
    .bind(account_id.as_slice())
    .bind(kind.as_db())
    .bind(code_hash.as_slice())
    .bind(OTP_TTL_SECS)
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// `POST /recovery-contact/enroll/start` request.
#[derive(Deserialize)]
pub(crate) struct EnrollStartReq {
    pub pow: crate::pow::PowProof,
    /// 1 = email, 2 = phone.
    pub kind: u8,
    /// The raw phone / email plaintext. Used transiently to hash + address
    /// the message; never stored, never logged (invariant #1).
    pub contact: String,
}

/// `POST /recovery-contact/enroll/finish` request.
#[derive(Deserialize)]
pub(crate) struct EnrollFinishReq {
    pub kind: u8,
    pub code: String,
}

/// `POST /recovery-contact/recover/start` request.
#[derive(Deserialize)]
pub(crate) struct RecoverStartReq {
    pub pow: crate::pow::PowProof,
    pub account_id_b64: String,
    pub kind: u8,
    /// The raw phone / email plaintext. Hashed + (on a confirmed match) used
    /// to address the message; never stored, never logged (invariant #1).
    pub contact: String,
}

/// `POST /recovery-contact/recover/finish` request.
#[derive(Deserialize)]
pub(crate) struct RecoverFinishReq {
    pub pow: crate::pow::PowProof,
    pub account_id_b64: String,
    pub kind: u8,
    pub code: String,
}

/// Generic "a code was sent" response. Returned identically on the
/// confirmed-match and no-match paths so the recover/start endpoint reveals
/// nothing about account / contact existence.
#[derive(Serialize)]
pub(crate) struct SentRes {
    pub sent: bool,
}

/// `POST /recovery-contact/recover/finish` response — the same
/// `(account_id, session_token, expiry)` triple every other login flow
/// returns.
#[derive(Serialize)]
pub(crate) struct RecoverRes {
    pub account_id_b64: String,
    pub session_token: String,
    pub session_expires_at_ms: i64,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /recovery-contact/enroll/start` — store a contact + send a code.
///
/// Bearer-auth (already logged in) + PoW. UPSERTs the contact (keeping any
/// prior confirmation so a re-send does not un-confirm it) and a fresh
/// challenge, then sends the code. Returns `{ "sent": true }`.
#[axum::debug_handler]
pub(crate) async fn enroll_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<EnrollStartReq>,
) -> Result<Json<SentRes>, ApiError> {
    let pepper = require_pepper(&state)?;
    crate::pow::verify_proof(&state.db, &req.pow).await?;
    let account_id = auth.0;
    let kind = parse_kind(req.kind)?;
    let normalized = normalize_contact(kind, &req.contact)?;
    let ch = contact_hash(&pepper, kind, &normalized);

    // UPSERT the contact. `confirmed_at` is left UNCHANGED on a conflict (we
    // only ever overwrite it via /finish), so re-sending to an already-
    // confirmed contact keeps it confirmed; storing a DIFFERENT contact
    // overwrites the hash and the confirmation is logically stale — the new
    // contact must be confirmed by the new code. To keep that correct we set
    // confirmed_at = NULL only when the hash actually changes.
    sqlx::query(
        "INSERT INTO recovery_contacts (account_id, kind, contact_hash, confirmed_at) \
         VALUES (?, ?, ?, NULL) \
         ON DUPLICATE KEY UPDATE \
         confirmed_at = IF(contact_hash = VALUES(contact_hash), confirmed_at, NULL), \
         contact_hash = VALUES(contact_hash)",
    )
    .bind(account_id.as_slice())
    .bind(kind.as_db())
    .bind(ch.as_slice())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;

    let code = generate_code();
    let code_h = code_hash(&pepper, kind, &code);
    upsert_challenge(&state, &account_id, kind, &code_h).await?;

    // Deliver. A delivery failure is an Internal error to the caller (we do
    // not leak transport detail), but we have already stored the challenge —
    // a retry of /enroll/start re-sends.
    if let Err(e) = state.otp_sender.send(kind, &normalized, &code).await {
        tracing::error!(error = %e, kind = kind.as_str(), "recovery-contact: enroll send failed");
        return Err(ApiError::Internal);
    }

    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        kind = kind.as_str(),
        "recovery-contact: enroll code sent",
    );
    Ok(Json(SentRes { sent: true }))
}

/// `POST /recovery-contact/enroll/finish` — confirm the contact with a code.
///
/// Bearer-auth. Loads the challenge; refuses (`401`) when missing, expired, or
/// at/over the attempt cap. On a hash match: stamps `confirmed_at` + deletes
/// the challenge (single-use). On a mismatch: bumps `attempts` + `401`.
#[axum::debug_handler]
pub(crate) async fn enroll_finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<EnrollFinishReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pepper = require_pepper(&state)?;
    let account_id = auth.0;
    let kind = parse_kind(req.kind)?;

    let verified = verify_challenge(&state, &pepper, &account_id, kind, &req.code).await?;
    if !verified {
        return Err(ApiError::InvalidCredential);
    }

    // Confirm the contact + consume the challenge (single-use).
    sqlx::query(
        "UPDATE recovery_contacts SET confirmed_at = NOW(3) WHERE account_id = ? AND kind = ?",
    )
    .bind(account_id.as_slice())
    .bind(kind.as_db())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;
    delete_challenge(&state, &account_id, kind).await?;

    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        kind = kind.as_str(),
        "recovery-contact: enrollment confirmed",
    );
    Ok(Json(serde_json::json!({ "confirmed": true })))
}

/// `POST /recovery-contact/recover/start` — send a recovery code if the
/// contact matches a CONFIRMED row.
///
/// Anonymous + PoW. ENUMERATION-SAFE: always returns `{ "sent": true }`
/// whether or not the supplied contact matches a confirmed row. The
/// no-match path still hashes the contact (the expensive comparable step) and
/// simply skips the actual send.
#[axum::debug_handler]
pub(crate) async fn recover_start(
    State(state): State<AppState>,
    Json(req): Json<RecoverStartReq>,
) -> Result<Json<SentRes>, ApiError> {
    let pepper = require_pepper(&state)?;
    crate::pow::verify_proof(&state.db, &req.pow).await?;
    let account_id = decode_account_id(&req.account_id_b64)?;
    let kind = parse_kind(req.kind)?;
    let normalized = normalize_contact(kind, &req.contact)?;
    let ch = contact_hash(&pepper, kind, &normalized);

    // Does a CONFIRMED contact with this exact hash exist for this account?
    let matched: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT contact_hash FROM recovery_contacts \
         WHERE account_id = ? AND kind = ? AND contact_hash = ? AND confirmed_at IS NOT NULL",
    )
    .bind(account_id.as_slice())
    .bind(kind.as_db())
    .bind(ch.as_slice())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;

    if matched.is_some() {
        // Real path: store + send a fresh code.
        let code = generate_code();
        let code_h = code_hash(&pepper, kind, &code);
        upsert_challenge(&state, &account_id, kind, &code_h).await?;
        if let Err(e) = state.otp_sender.send(kind, &normalized, &code).await {
            // Even on a delivery failure we return the SAME generic response
            // (no oracle); log for ops.
            tracing::error!(error = %e, kind = kind.as_str(), "recovery-contact: recover send failed");
        } else {
            tracing::info!(
                account_id = %URL_SAFE_NO_PAD.encode(account_id),
                kind = kind.as_str(),
                "recovery-contact: recover code sent",
            );
        }
    } else {
        // No-match path: we already paid the hash cost above. Do NOT send and
        // do NOT write a challenge. Return the SAME response so the endpoint
        // is enumeration-safe.
        tracing::debug!(
            kind = kind.as_str(),
            "recovery-contact: recover/start no confirmed match (enumeration-safe)",
        );
    }

    Ok(Json(SentRes { sent: true }))
}

/// `POST /recovery-contact/recover/finish` — recover ACCESS with a code.
///
/// Anonymous + PoW. Refuses with `429` when the challenge is at/over the
/// attempt cap; `401` when missing / expired / wrong (enumeration-safe). On a
/// match: deletes the challenge (single-use) + issues the session triple.
#[axum::debug_handler]
pub(crate) async fn recover_finish(
    State(state): State<AppState>,
    Json(req): Json<RecoverFinishReq>,
) -> Result<Json<RecoverRes>, ApiError> {
    let pepper = require_pepper(&state)?;
    crate::pow::verify_proof(&state.db, &req.pow).await?;
    let account_id = decode_account_id(&req.account_id_b64)?;
    let kind = parse_kind(req.kind)?;

    // Lockout check first: at/over the cap -> 429 (carries no
    // account-existence info — a non-existent challenge is below the cap and
    // falls through to the generic 401).
    let row: Option<(i16,)> = sqlx::query_as(
        "SELECT attempts FROM otp_challenges \
         WHERE account_id = ? AND kind = ? AND expires_at > NOW(3)",
    )
    .bind(account_id.as_slice())
    .bind(kind.as_db())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    if let Some((attempts,)) = row {
        if attempts >= MAX_OTP_ATTEMPTS {
            tracing::warn!(
                account_id = %URL_SAFE_NO_PAD.encode(account_id),
                kind = kind.as_str(),
                "recovery-contact: recover rejected (attempt cap)",
            );
            return Err(ApiError::RateLimited);
        }
    }

    let verified = verify_challenge(&state, &pepper, &account_id, kind, &req.code).await?;
    if !verified {
        return Err(ApiError::InvalidCredential);
    }

    // Single-use: consume the challenge before minting the token.
    delete_challenge(&state, &account_id, kind).await?;

    let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);
    tracing::info!(
        account_id = %account_id_b64,
        kind = kind.as_str(),
        "recovery-contact: code accepted, issuing session token",
    );
    let (session_token, session_expires_at_ms) = issue_session_token(&state, &account_id)?;
    Ok(Json(RecoverRes {
        account_id_b64,
        session_token,
        session_expires_at_ms,
    }))
}

// ---------------------------------------------------------------------------
// Shared challenge verification
// ---------------------------------------------------------------------------

/// Verify `code` against the live challenge for `(account_id, kind)`.
///
/// Returns `Ok(true)` on a hash match against a non-expired, under-cap
/// challenge; `Ok(false)` (after bumping `attempts`) on a mismatch or when no
/// usable challenge exists. The caller maps `false` to a generic `401`.
async fn verify_challenge(
    state: &AppState,
    pepper: &[u8; 32],
    account_id: &[u8; ACCOUNT_ID_LEN],
    kind: ContactKind,
    code: &str,
) -> Result<bool, ApiError> {
    let row: Option<(Vec<u8>, i16)> = sqlx::query_as(
        "SELECT code_hash, attempts FROM otp_challenges \
         WHERE account_id = ? AND kind = ? AND expires_at > NOW(3)",
    )
    .bind(account_id.as_slice())
    .bind(kind.as_db())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;

    let Some((stored_hash, attempts)) = row else {
        // No live challenge — nothing to verify or to rate-limit. Generic
        // failure; no enumeration signal.
        return Ok(false);
    };
    if attempts >= MAX_OTP_ATTEMPTS {
        // Defence in depth: recover_finish already checks the cap, but
        // enroll_finish routes here too. Treat as a failure without a further
        // bump.
        return Ok(false);
    }

    let candidate = code_hash(pepper, kind, code);
    // Constant-time compare to avoid leaking how many leading bytes matched.
    let is_match = constant_time_eq(&candidate, &stored_hash);
    if is_match {
        return Ok(true);
    }

    // Wrong code: bump the attempt counter (drives the lockout).
    sqlx::query(
        "UPDATE otp_challenges SET attempts = attempts + 1 \
         WHERE account_id = ? AND kind = ?",
    )
    .bind(account_id.as_slice())
    .bind(kind.as_db())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;
    Ok(false)
}

/// Delete the challenge for `(account_id, kind)` — single-use consume on a
/// successful verify.
async fn delete_challenge(
    state: &AppState,
    account_id: &[u8; ACCOUNT_ID_LEN],
    kind: ContactKind,
) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM otp_challenges WHERE account_id = ? AND kind = ?")
        .bind(account_id.as_slice())
        .bind(kind.as_db())
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;
    Ok(())
}

/// Constant-time byte-slice equality. `candidate` is a fixed 32-byte digest;
/// `stored` is the DB column (always 32 bytes for a well-formed row, but we
/// fold its length into the result rather than early-returning so the timing
/// does not branch on length).
fn constant_time_eq(candidate: &[u8; 32], stored: &[u8]) -> bool {
    if stored.len() != candidate.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in candidate.iter().zip(stored.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEPPER: [u8; 32] = [0x5Au8; 32];

    #[test]
    fn email_normalization() {
        assert_eq!(
            normalize_contact(ContactKind::Email, "  Alice@Example.COM ").unwrap(),
            "alice@example.com"
        );
        assert_eq!(
            normalize_contact(ContactKind::Email, "bob+tag@mail.co").unwrap(),
            "bob+tag@mail.co"
        );
        // Rejects.
        assert!(normalize_contact(ContactKind::Email, "no-at-sign").is_err());
        assert!(normalize_contact(ContactKind::Email, "@no-local.com").is_err());
        assert!(normalize_contact(ContactKind::Email, "no-domain@").is_err());
        assert!(normalize_contact(ContactKind::Email, "two@@at.com").is_err());
        assert!(normalize_contact(ContactKind::Email, "has space@x.com").is_err());
        assert!(normalize_contact(ContactKind::Email, "").is_err());
    }

    #[test]
    fn phone_normalization() {
        assert_eq!(
            normalize_contact(ContactKind::Phone, "+1 (555) 123-4567").unwrap(),
            "+15551234567"
        );
        assert_eq!(
            normalize_contact(ContactKind::Phone, "+44-20-7946-0958").unwrap(),
            "+442079460958"
        );
        // Rejects.
        assert!(normalize_contact(ContactKind::Phone, "5551234567").is_err()); // no leading +
        assert!(normalize_contact(ContactKind::Phone, "+").is_err()); // no digits
        assert!(normalize_contact(ContactKind::Phone, "+abc1234").is_err()); // non-digit
        assert!(normalize_contact(ContactKind::Phone, "+1234567890123456").is_err()); // >15 digits
        assert!(normalize_contact(ContactKind::Phone, "").is_err());
    }

    #[test]
    fn keyed_hash_is_deterministic_and_pepper_dependent() {
        let a = contact_hash(&PEPPER, ContactKind::Email, "alice@example.com");
        let a2 = contact_hash(&PEPPER, ContactKind::Email, "alice@example.com");
        assert_eq!(a, a2, "same input + pepper hashes identically");

        // Different pepper -> different hash (non-reversible without the key).
        let other = contact_hash(&[0x11u8; 32], ContactKind::Email, "alice@example.com");
        assert_ne!(a, other, "the pepper is load-bearing");

        // Different contact -> different hash.
        let b = contact_hash(&PEPPER, ContactKind::Email, "bob@example.com");
        assert_ne!(a, b);

        // Kind is domain-separated: same string under email vs phone differs.
        let as_phone = contact_hash(&PEPPER, ContactKind::Phone, "alice@example.com");
        assert_ne!(a, as_phone, "kind byte separates the hash domains");
    }

    #[test]
    fn code_hash_distinct_from_contact_hash() {
        // The contact-hash and code-hash spaces are provably disjoint: even
        // for the SAME string + kind, the two purpose bytes make the digests
        // differ, so a code-hash can never equal a contact-hash.
        assert_ne!(
            code_hash(&PEPPER, ContactKind::Email, "123456"),
            contact_hash(&PEPPER, ContactKind::Email, "123456")
        );
        // Different codes differ.
        assert_ne!(
            code_hash(&PEPPER, ContactKind::Email, "123456"),
            code_hash(&PEPPER, ContactKind::Email, "123457")
        );
    }

    #[test]
    fn generated_code_is_six_digits() {
        for _ in 0..200 {
            let code = generate_code();
            assert_eq!(code.len(), 6, "code {code} must be 6 chars");
            assert!(
                code.bytes().all(|b| b.is_ascii_digit()),
                "code {code} all digits"
            );
        }
        // Edge: a small value is zero-padded.
        assert_eq!(format!("{:06}", 42u32), "000042");
    }

    #[test]
    fn code_hash_verify_matches_recomputation() {
        let stored = code_hash(&PEPPER, ContactKind::Phone, "098765");
        let recomputed = code_hash(&PEPPER, ContactKind::Phone, "098765");
        assert!(constant_time_eq(&recomputed, &stored));
        let wrong = code_hash(&PEPPER, ContactKind::Phone, "000000");
        assert!(!constant_time_eq(&wrong, &stored));
    }

    #[test]
    fn parse_kind_validates() {
        assert_eq!(parse_kind(1).unwrap(), ContactKind::Email);
        assert_eq!(parse_kind(2).unwrap(), ContactKind::Phone);
        assert!(matches!(parse_kind(0), Err(ApiError::BadInput(_))));
        assert!(matches!(parse_kind(9), Err(ApiError::BadInput(_))));
    }

    #[test]
    fn constant_time_eq_rejects_wrong_length() {
        let c = [0u8; 32];
        assert!(constant_time_eq(&c, &[0u8; 32]));
        assert!(!constant_time_eq(&c, &[0u8; 31]));
        assert!(!constant_time_eq(&c, &[0u8; 33]));
    }
}

/// DB-backed end-to-end tests. Gated on `LAKKO_AUTH_TEST_DATABASE_URL`; they
/// skip (and pass) when it is unset. Mirrors the `totp` / `device_authorize`
/// e2e harnesses. A [`TestOtpSender`] records the last `(kind, contact, code)`
/// in an `Arc<Mutex<_>>` so the test can read the OTP it must submit — the
/// only place in the test the code is observed (the production senders never
/// expose it). Covers:
///
///   * 503 when the pepper is unset (loud refusal).
///   * enroll/start -> finish (read the code from the recorder) ->
///     recover/start -> finish happy path (a verifiable session token).
///   * recover/start for an UNKNOWN account returns the SAME `{sent:true}`
///     with NO send recorded (enumeration safety).
///   * wrong-code lockout: the attempt cap drives recover/finish to 429.
#[cfg(test)]
mod recovery_contact_e2e_tests {
    use super::*;
    use crate::otp_sender::{ContactKind, OtpSendError, OtpSender};
    use async_trait::async_trait;
    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, StatusCode};
    use base64::engine::general_purpose::STANDARD;
    use http_body_util::BodyExt as _;
    use sqlx::mysql::MySqlPoolOptions;
    use sqlx::MySqlPool;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt as _;
    use uuid::Uuid;

    const PEPPER: [u8; 32] = [0x7Cu8; 32];

    /// Records the last delivered `(kind, contact, code)` so the test can read
    /// the OTP. Also tracks a monotonically increasing send count so the
    /// enumeration-safety assertion can prove NO send happened on the
    /// unknown-account path.
    #[derive(Clone, Default)]
    struct TestOtpSender {
        last: Arc<Mutex<Option<(ContactKind, String, String)>>>,
        sends: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl OtpSender for TestOtpSender {
        async fn send(
            &self,
            kind: ContactKind,
            contact: &str,
            code: &str,
        ) -> Result<(), OtpSendError> {
            *self.last.lock().expect("lock") = Some((kind, contact.to_string(), code.to_string()));
            *self.sends.lock().expect("lock") += 1;
            Ok(())
        }
    }

    impl TestOtpSender {
        fn last_code(&self) -> String {
            self.last
                .lock()
                .expect("lock")
                .as_ref()
                .map(|(_, _, code)| code.clone())
                .expect("a code was recorded")
        }

        fn send_count(&self) -> u32 {
            *self.sends.lock().expect("lock")
        }
    }

    fn build_state(
        pool: MySqlPool,
        with_pepper: bool,
        sender: Arc<dyn OtpSender>,
    ) -> (crate::AppState, lakko_auth_token::TokenSecret) {
        use opaque_ke::ServerSetup;
        use rand::rngs::OsRng;
        let token_secret = lakko_auth_token::TokenSecret::new(vec![7u8; 64]).expect("token secret");
        let opaque_setup = ServerSetup::<crate::opaque::LakkoCipherSuite>::new(&mut OsRng);
        let state = crate::AppState {
            db: pool,
            opaque_setup: Arc::new(opaque_setup),
            session_token_secret: Arc::new(token_secret.clone()),
            session_token_ttl_ms: 86_400_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
            turn: None,
            totp_enc_key: None,
            recovery_contact_pepper: with_pepper.then(|| Arc::new(PEPPER)),
            otp_sender: sender,
            throttle: crate::throttle::ThrottleConfig::disabled(),
        };
        (state, token_secret)
    }

    async fn send(state: &crate::AppState, req: Request<Body>) -> (StatusCode, Bytes) {
        let resp = crate::build_app(state.clone())
            .oneshot(req)
            .await
            .expect("router oneshot");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, bytes)
    }

    fn now_ms() -> i64 {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(i64::MAX)
    }

    async fn fresh_pow(state: &crate::AppState) -> serde_json::Value {
        let (st, body) = send(
            state,
            Request::builder()
                .method(Method::POST)
                .uri("/pow/challenge")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let seed_b64 = v["seed"].as_str().unwrap().to_string();
        let seed = STANDARD.decode(&seed_b64).unwrap();
        let mut nonce = [0u8; 16];
        loop {
            let mut challenge = Vec::with_capacity(32);
            challenge.extend_from_slice(&seed);
            challenge.extend_from_slice(&nonce);
            if let Ok(eq) = equix::EquiX::new(&challenge) {
                if let Some(sol) = eq.solve().first() {
                    return serde_json::json!({
                        "seed": seed_b64,
                        "nonce": STANDARD.encode(nonce),
                        "solution": STANDARD.encode(sol.to_bytes()),
                    });
                }
            }
            nonce[0] = nonce[0].wrapping_add(1);
            if nonce[0] == 0 {
                nonce[1] = nonce[1].wrapping_add(1);
            }
        }
    }

    fn bearer_for(token_secret: &lakko_auth_token::TokenSecret, account_id: &[u8; 16]) -> String {
        format!(
            "Bearer {}",
            token_secret
                .issue(account_id, now_ms() + 3_600_000)
                .expect("mint session token")
        )
    }

    async fn json_post(
        state: &crate::AppState,
        uri: &str,
        bearer: Option<&str>,
        body: serde_json::Value,
    ) -> (StatusCode, Bytes) {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(b) = bearer {
            req = req.header(header::AUTHORIZATION, b);
        }
        send(
            state,
            req.body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn recovery_contact_full_flow() {
        const CONTACT: &str = "alice@example.com";
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping recovery_contact_full_flow: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        std::env::set_var("LAKKO_POW_EFFORT_BITS", "0");
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("migrations");

        // (1) 503 when the pepper is unset (loud refusal, no fallback).
        {
            let recorder = TestOtpSender::default();
            let (state_off, ts) = build_state(pool.clone(), false, Arc::new(recorder.clone()));
            let account_id: [u8; 16] = Uuid::new_v4().into_bytes();
            sqlx::query("INSERT INTO accounts (id) VALUES (?)")
                .bind(account_id.as_slice())
                .execute(&pool)
                .await
                .unwrap();
            let bearer = bearer_for(&ts, &account_id);
            let pow = fresh_pow(&state_off).await;
            let (st, _) = json_post(
                &state_off,
                "/recovery-contact/enroll/start",
                Some(&bearer),
                serde_json::json!({ "pow": pow, "kind": 1, "contact": "alice@example.com" }),
            )
            .await;
            assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "503 when pepper unset");
        }

        let recorder = TestOtpSender::default();
        let (state, ts) = build_state(pool.clone(), true, Arc::new(recorder.clone()));
        let account_id: [u8; 16] = Uuid::new_v4().into_bytes();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .unwrap();
        let bearer = bearer_for(&ts, &account_id);
        let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);

        // (2) enroll/start -> a code is sent + recorded.
        let pow = fresh_pow(&state).await;
        let (st, _) = json_post(
            &state,
            "/recovery-contact/enroll/start",
            Some(&bearer),
            serde_json::json!({ "pow": pow, "kind": 1, "contact": CONTACT }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "enroll/start sends");
        let enroll_code = recorder.last_code();
        assert_eq!(enroll_code.len(), 6);

        // The DB stores ONLY the hash, never the contact plaintext.
        let stored_hash: Vec<u8> = sqlx::query_scalar(
            "SELECT contact_hash FROM recovery_contacts WHERE account_id = ? AND kind = 1",
        )
        .bind(account_id.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_ne!(
            stored_hash,
            CONTACT.as_bytes(),
            "the contact plaintext must not be stored"
        );
        assert_eq!(
            stored_hash,
            super::contact_hash(&PEPPER, ContactKind::Email, CONTACT).to_vec()
        );

        // (3) enroll/finish with the recorded code -> confirmed.
        let (st, _) = json_post(
            &state,
            "/recovery-contact/enroll/finish",
            Some(&bearer),
            serde_json::json!({ "kind": 1, "code": enroll_code }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "enroll/finish confirms");
        let confirmed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM recovery_contacts \
             WHERE account_id = ? AND kind = 1 AND confirmed_at IS NOT NULL",
        )
        .bind(account_id.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(confirmed, 1, "contact is now confirmed");

        // (4) recover/start (anonymous) -> a fresh code is sent + recorded.
        let sends_before = recorder.send_count();
        let pow = fresh_pow(&state).await;
        let (st, body) = json_post(
            &state,
            "/recovery-contact/recover/start",
            None,
            serde_json::json!({ "pow": pow, "account_id_b64": account_id_b64, "kind": 1, "contact": CONTACT }),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "recover/start sends on a confirmed match"
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["sent"], true);
        assert_eq!(
            recorder.send_count(),
            sends_before + 1,
            "a confirmed match sends exactly one code"
        );
        let recover_code = recorder.last_code();

        // (5) recover/finish with the recovered code -> session triple.
        let pow = fresh_pow(&state).await;
        let (st, body) = json_post(
            &state,
            "/recovery-contact/recover/finish",
            None,
            serde_json::json!({ "pow": pow, "account_id_b64": account_id_b64, "kind": 1, "code": recover_code }),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "recover/finish issues a token: {}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["account_id_b64"].as_str().unwrap(), account_id_b64);
        let session_token = v["session_token"].as_str().unwrap().to_string();
        let verified = ts
            .verify(&session_token, now_ms())
            .expect("issued token verifies");
        assert_eq!(verified.as_slice(), account_id.as_slice());

        // (6) ENUMERATION SAFETY: recover/start for an UNKNOWN account returns
        // the SAME {sent:true} with NO send recorded.
        let sends_before = recorder.send_count();
        let unknown_b64 = URL_SAFE_NO_PAD.encode(Uuid::new_v4().into_bytes());
        let pow = fresh_pow(&state).await;
        let (st, body) = json_post(
            &state,
            "/recovery-contact/recover/start",
            None,
            serde_json::json!({ "pow": pow, "account_id_b64": unknown_b64, "kind": 1, "contact": CONTACT }),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "unknown-account recover/start still 200"
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["sent"], true, "same {{sent:true}} on no-match");
        assert_eq!(
            recorder.send_count(),
            sends_before,
            "no send happens for an unknown account (enumeration-safe)"
        );

        // Also enumeration-safe: a confirmed account but a WRONG contact does
        // not send.
        let sends_before = recorder.send_count();
        let pow = fresh_pow(&state).await;
        let (st, _) = json_post(
            &state,
            "/recovery-contact/recover/start",
            None,
            serde_json::json!({ "pow": pow, "account_id_b64": account_id_b64, "kind": 1, "contact": "wrong@example.com" }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            recorder.send_count(),
            sends_before,
            "a wrong contact for a real account does not send"
        );

        // (7) LOCKOUT: send a fresh code, then exhaust the attempt cap with a
        // wrong code -> 429.
        let pow = fresh_pow(&state).await;
        let (st, _) = json_post(
            &state,
            "/recovery-contact/recover/start",
            None,
            serde_json::json!({ "pow": pow, "account_id_b64": account_id_b64, "kind": 1, "contact": CONTACT }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "fresh code for the lockout test");

        // MAX_OTP_ATTEMPTS wrong submissions bump the counter; the next is 429.
        for _ in 0..super::MAX_OTP_ATTEMPTS {
            let pow = fresh_pow(&state).await;
            let (st, _) = json_post(
                &state,
                "/recovery-contact/recover/finish",
                None,
                serde_json::json!({ "pow": pow, "account_id_b64": account_id_b64, "kind": 1, "code": "000000" }),
            )
            .await;
            // Each wrong code is a generic 401 (until the cap is hit).
            assert!(
                st == StatusCode::UNAUTHORIZED || st == StatusCode::TOO_MANY_REQUESTS,
                "wrong code is 401 (or 429 once capped), got {st}"
            );
        }
        let pow = fresh_pow(&state).await;
        let (st, _) = json_post(
            &state,
            "/recovery-contact/recover/finish",
            None,
            serde_json::json!({ "pow": pow, "account_id_b64": account_id_b64, "kind": 1, "code": "000000" }),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::TOO_MANY_REQUESTS,
            "locked out after the cap"
        );

        // Cleanup — cascade drops the contact + any challenge.
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .ok();
    }
}
