// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! TOTP authenticator-code recovery — recovery factor (c) (ADR-0018).
//!
//! A standard RFC 6238 (TOTP) / RFC 4226 (HOTP) authenticator second factor
//! that, at recovery time, can stand in for the password to grant ACCESS:
//!
//! - **`POST /totp/enroll/start`** — bearer-authenticated (the caller is an
//!   already-logged-in account); no body. Generates a fresh 20-byte secret,
//!   stores it ENCRYPTED + UNCONFIRMED, and returns the `otpauth://` URI +
//!   base32 secret the authenticator app scans.
//! - **`POST /totp/enroll/finish`** — bearer-auth. Proves the authenticator
//!   actually holds the secret (a correct current code) before the factor
//!   becomes usable; sets `confirmed_at`.
//! - **`POST /totp/recover`** — anonymous (the caller lost their device).
//!   Names the account by login handle (ADR-0054 S5). Charged to the named
//!   handle's attempt counter (ADR-0054 S4, default 5
//!   per 15 minutes, `429` `too-many-attempts`) and the per-address bucket,
//!   then verifies a code against the confirmed secret with a ±1-step window
//!   and replay protection (the matched step must be strictly newer), and
//!   issues the session-token triple.
//!
//! **Trust class.** The TOTP secret is a server-held ACCESS verifier in the
//! same trust class as the OPAQUE password envelope — it authenticates the
//! user, it never decrypts E2EE data. Holding it does NOT violate invariant
//! #2 (no message / chain / root / media keys are involved); the
//! classification is recorded in ADR-0018 "Server-held verifiers are ACCESS
//! verifiers".
//!
//! **At-rest encryption.** Unlike the grant tables (`webauthn_action_grants`,
//! `device_grants`) we cannot store a hash — verification needs the raw
//! secret to recompute the HMAC. `secret_enc` is the `lakko-crypto`
//! `seal_with_key` envelope under the server-static `LAKKO_TOTP_ENC_KEY`. This
//! defends a DB-ONLY leak (dump / backup theft) — an attacker with the
//! database but not the service environment cannot mint codes. It does NOT
//! defend a full-host compromise (the key lives in the service env); that
//! residual is inherent to any server-verified OTP factor.
//!
//! **Enumeration safety.** `/totp/recover` returns the SAME generic `401`
//! whether the account is unknown, has no confirmed TOTP, or supplied a
//! wrong code — and performs a dummy HMAC verification on the
//! unknown-account path so the timing/shape does not distinguish the cases.
//! The `429` comes from a counter keyed on the login handle the request names
//! (ADR-0054 S5), charged before the lookup, so it trips for an unknown account exactly as
//! for a known one. (The per-row lockout it replaced answered `429` only for
//! an existing confirmed row, which told the caller the account existed.)

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Length of the raw TOTP shared secret (bytes). 20 bytes = 160 bits, the
/// RFC 4226 recommended minimum and the de-facto authenticator-app default.
const SECRET_LEN: usize = 20;

/// RFC 6238 time step, in seconds.
const TIME_STEP_SECS: u64 = 30;

/// Number of OTP digits (RFC 4226 truncation modulo `10^DIGITS`).
const DIGITS: u32 = 6;

/// Verification window, in steps either side of the server's current step.
/// ±1 tolerates a 30 s clock skew between the authenticator and the server.
const STEP_SKEW: i64 = 1;

type HmacSha1 = Hmac<Sha1>;

/// The `/totp/recover` row shape: `(secret_enc, confirmed_at,
/// last_accepted_step)`.
type RecoverRow = (
    Vec<u8>,
    Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
    i64,
);

// ---------------------------------------------------------------------------
// Env-loaded encryption key
// ---------------------------------------------------------------------------

/// Load the TOTP secret-encryption key from `LAKKO_TOTP_ENC_KEY`.
///
/// `Ok(Some(_))` when the env var is present and decodes to exactly 32 bytes;
/// `Ok(None)` when it is unset (the `/totp/*` routes then return 503);
/// `Err` (loud-fail at startup) when it is present but malformed.
///
/// # Errors
///
/// Returns an error when the env var is set but is not valid base64 or does
/// not decode to exactly 32 bytes.
pub(crate) fn load_totp_enc_key() -> anyhow::Result<Option<[u8; 32]>> {
    let raw = match std::env::var("LAKKO_TOTP_ENC_KEY") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return Ok(None),
    };
    let bytes = STANDARD
        .decode(raw.trim())
        .map_err(|e| anyhow::anyhow!("LAKKO_TOTP_ENC_KEY is not valid base64: {e}"))?;
    let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "LAKKO_TOTP_ENC_KEY must decode to exactly 32 bytes (got {})",
            bytes.len()
        )
    })?;
    Ok(Some(key))
}

/// Resolve the configured TOTP key or return the 503 (`ApiError::Disabled`)
/// every `/totp/*` handler short-circuits on when TOTP is not configured.
fn require_key(state: &AppState) -> Result<[u8; 32], ApiError> {
    state
        .totp_enc_key
        .as_ref()
        .map(|k| **k)
        .ok_or(ApiError::Disabled)
}

// ---------------------------------------------------------------------------
// Pure RFC 4226 / RFC 6238 / RFC 4648 functions
// ---------------------------------------------------------------------------

/// RFC 4226 §5.3 HOTP: HMAC-SHA1 of the 8-byte big-endian `counter` under
/// `secret`, dynamic-truncated to a [`DIGITS`]-digit code.
fn hotp(secret: &[u8], counter: u64) -> u32 {
    // HMAC-SHA1 with a fixed key length never fails to init; map the
    // theoretically-fallible constructor without unwrap.
    let Ok(mut mac) = HmacSha1::new_from_slice(secret) else {
        return 0;
    };
    mac.update(&counter.to_be_bytes());
    let hs = mac.finalize().into_bytes();
    // Dynamic truncation (RFC 4226 §5.3).
    let offset = (hs[hs.len() - 1] & 0x0f) as usize;
    let bin = (u32::from(hs[offset] & 0x7f) << 24)
        | (u32::from(hs[offset + 1]) << 16)
        | (u32::from(hs[offset + 2]) << 8)
        | u32::from(hs[offset + 3]);
    bin % 10u32.pow(DIGITS)
}

/// RFC 6238 TOTP at unix time `unix_secs`, offset by `step_skew` steps.
/// Used by the RFC-vector tests + as the building block of [`verify_code`]'s
/// window (which inlines the same `hotp(secret, step)` call directly).
#[cfg(test)]
fn totp_at(secret: &[u8], unix_secs: u64, step_skew: i64) -> u32 {
    let base = current_step(unix_secs);
    hotp(secret, step_to_counter(base.saturating_add(step_skew)))
}

/// The current RFC 6238 time step `floor(unix / 30)` as an `i64` (the DB
/// `last_accepted_step` column type). A real unix-step always fits; saturate
/// rather than wrap on an absurd clock.
fn current_step(unix_secs: u64) -> i64 {
    i64::try_from(unix_secs / TIME_STEP_SECS).unwrap_or(i64::MAX)
}

/// Convert an (always non-negative) step to the `u64` HOTP counter, clamping
/// a negative skew result to 0.
fn step_to_counter(step: i64) -> u64 {
    u64::try_from(step.max(0)).unwrap_or(0)
}

/// Verify `code` against `secret` within ±[`STEP_SKEW`] of the step for
/// `unix_secs`. Returns the matched absolute step on success.
///
/// Constant-ish: every candidate step in the window is always evaluated (no
/// early return on the first match) so the verification time does not leak
/// which offset matched.
fn verify_code(secret: &[u8], unix_secs: u64, code: u32) -> Option<i64> {
    let base = current_step(unix_secs);
    let mut matched: Option<i64> = None;
    let mut skew = -STEP_SKEW;
    while skew <= STEP_SKEW {
        let step = base.saturating_add(skew).max(0);
        let candidate = hotp(secret, step_to_counter(step));
        // `ct_eq`-style: fold into `matched` without breaking the loop.
        if candidate == code {
            matched = Some(step);
        }
        skew += 1;
    }
    matched
}

/// RFC 4648 §6 base32 encode (alphabet `A-Z2-7`), NO padding — the form
/// `otpauth://` URIs use.
fn base32_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buffer >> bits) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

/// Build the `otpauth://totp/...` provisioning URI for an authenticator app.
fn otpauth_uri(account_id_b64: &str, secret_b32: &str) -> String {
    // Label `Lakko:<account_id_b64>`; the account id is a UUIDv7, not PII.
    format!(
        "otpauth://totp/Lakko:{account_id_b64}?secret={secret_b32}\
         &issuer=Lakko&algorithm=SHA1&digits={DIGITS}&period={TIME_STEP_SECS}"
    )
}

/// Parse a [`DIGITS`]-digit numeric code into its `u32` value, refusing the
/// wrong length / non-digits.
fn parse_code(code: &str) -> Result<u32, ApiError> {
    let trimmed = code.trim();
    if trimmed.len() != DIGITS as usize || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::BadInput("code must be 6 digits"));
    }
    trimmed
        .parse::<u32>()
        .map_err(|_| ApiError::BadInput("code must be 6 digits"))
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// `POST /totp/enroll/start` response — the provisioning material the
/// authenticator app consumes. The raw secret leaves the server exactly once,
/// here, base32-encoded inside the URI (+ standalone for manual entry).
#[derive(Serialize)]
pub(crate) struct EnrollStartRes {
    pub otpauth_uri: String,
    pub secret_b32: String,
}

/// `POST /totp/enroll/finish` request — the first code the authenticator
/// produced, proving it holds the secret.
#[derive(Deserialize)]
pub(crate) struct EnrollFinishReq {
    pub code: String,
}

/// `POST /totp/recover` request.
#[derive(Deserialize)]
pub(crate) struct RecoverReq {
    /// The login handle of the account (ADR-0054 S5): 32 bytes, base64url
    /// without padding.
    pub handle: String,
    pub code: String,
    /// URL-safe base64 (no padding) of the 32-byte id of the device this
    /// session lands on: its conversation leaf signature key (ADR-0074
    /// section 0.4). Absent when the device has no keys yet; the session then
    /// names no device and the client calls `POST /auth/session/bind-device`
    /// once it has them.
    #[serde(default)]
    pub device_id_b64: Option<String>,
}

/// `POST /totp/recover` response — the account id plus the same session pair
/// every other login flow returns (ADR-0073).
#[derive(Serialize)]
pub(crate) struct RecoverRes {
    pub account_id_b64: String,
    #[serde(flatten)]
    pub session: crate::session::SessionPair,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /totp/enroll/start` — mint + store an unconfirmed TOTP secret.
///
/// Bearer-auth (the already-logged-in account); the body is ignored.
/// Re-enrolling BEFORE
/// confirmation replaces the pending secret; re-enrolling AFTER confirmation
/// is refused (`409`) so a stolen session cannot silently swap the
/// authenticator out from under the user.
#[axum::debug_handler]
pub(crate) async fn enroll_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<EnrollStartRes>, ApiError> {
    let key = require_key(&state)?;
    let account_id = auth.0;

    // Refuse a swap of an ALREADY-confirmed authenticator.
    let existing: Option<(Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,)> =
        sqlx::query_as("SELECT confirmed_at FROM totp_secrets WHERE account_id = ?")
            .bind(account_id.as_slice())
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::Db)?;
    if let Some((Some(_),)) = existing {
        return Err(ApiError::TotpAlreadyEnabled);
    }

    let mut secret = [0u8; SECRET_LEN];
    OsRng.fill_bytes(&mut secret);
    let secret_enc = lakko_crypto::aead::seal_with_key(&key, &secret).map_err(|e| {
        tracing::error!(error = %e, "totp: secret seal failed");
        ApiError::Internal
    })?;
    let secret_b32 = base32_encode(&secret);
    // Scrub the raw secret; only the sealed copy + the b32 (for the URI) live on.
    secret.fill(0);

    sqlx::query(
        "INSERT INTO totp_secrets (account_id, secret_enc, confirmed_at, last_accepted_step) \
         VALUES (?, ?, NULL, 0) \
         ON DUPLICATE KEY UPDATE secret_enc = VALUES(secret_enc), confirmed_at = NULL, \
         last_accepted_step = 0",
    )
    .bind(account_id.as_slice())
    .bind(secret_enc.as_slice())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;

    let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);
    tracing::info!("totp: enrollment started");
    Ok(Json(EnrollStartRes {
        otpauth_uri: otpauth_uri(&account_id_b64, &secret_b32),
        secret_b32,
    }))
}

/// `POST /totp/enroll/finish` — confirm enrollment with a current code.
///
/// Bearer-auth. Loads the UNCONFIRMED row, opens the secret, verifies the
/// code (±1 window), and on success stamps `confirmed_at` + records the
/// matched step (so an enroll-time code cannot be replayed at recovery).
#[axum::debug_handler]
pub(crate) async fn enroll_finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<EnrollFinishReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let key = require_key(&state)?;
    let account_id = auth.0;
    let code = parse_code(&req.code)?;

    let row: Option<(
        Vec<u8>,
        Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
    )> = sqlx::query_as("SELECT secret_enc, confirmed_at FROM totp_secrets WHERE account_id = ?")
        .bind(account_id.as_slice())
        .fetch_optional(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let Some((secret_enc, confirmed_at)) = row else {
        // No pending enrollment — caller must run /enroll/start first.
        return Err(ApiError::BadInput("no pending TOTP enrollment"));
    };
    if confirmed_at.is_some() {
        return Err(ApiError::TotpAlreadyEnabled);
    }

    let secret = lakko_crypto::aead::open_with_key(&key, &secret_enc).map_err(|e| {
        tracing::error!(error = %e, "totp: secret open failed (enroll/finish)");
        ApiError::Internal
    })?;
    let now = now_unix_secs();
    let Some(step) = verify_code(&secret, now, code) else {
        return Err(ApiError::InvalidCredential);
    };

    sqlx::query(
        "UPDATE totp_secrets SET confirmed_at = NOW(3), last_accepted_step = ? \
         WHERE account_id = ? AND confirmed_at IS NULL",
    )
    .bind(step)
    .bind(account_id.as_slice())
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;

    tracing::info!("totp: enrollment confirmed");
    Ok(Json(serde_json::json!({ "enabled": true })))
}

/// Whether `account_id` has a CONFIRMED authenticator. One primary-key read;
/// needs no key, so it answers even when `LAKKO_TOTP_ENC_KEY` is unset.
///
/// # Errors
///
/// Any `sqlx::Error` from the read.
pub(crate) async fn is_enabled(
    pool: &sqlx::MySqlPool,
    account_id: &[u8; 16],
) -> Result<bool, sqlx::Error> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM totp_secrets WHERE account_id = ? AND confirmed_at IS NOT NULL",
    )
    .bind(account_id.as_slice())
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Parse and check one authenticator code for `account_id`, with the rules
/// of [`recover`]: the +-1 step window, strictly newer than the last accepted
/// step, and an atomic compare-and-set so one step is accepted once. Shared
/// with the recovery-email route (ADR-0075 D8).
///
/// `Ok(false)` for a wrong, replayed or malformed code and for an account
/// without a confirmed authenticator.
///
/// # Errors
///
/// [`ApiError::Disabled`] when `LAKKO_TOTP_ENC_KEY` is unset;
/// [`ApiError::Db`]; [`ApiError::Internal`] when the stored secret does not
/// open.
pub(crate) async fn check_code(
    state: &AppState,
    account_id: &[u8; 16],
    code: &str,
) -> Result<bool, ApiError> {
    let key = require_key(state)?;
    let Ok(code) = parse_code(code) else {
        return Ok(false);
    };
    accept_code(&state.db, &key, account_id, code, now_unix_secs()).await
}

/// The verify-and-consume step behind [`recover`] and [`check_code`].
async fn accept_code(
    pool: &sqlx::MySqlPool,
    key: &[u8; 32],
    account_id: &[u8; 16],
    code: u32,
    now: u64,
) -> Result<bool, ApiError> {
    let row: Option<RecoverRow> = sqlx::query_as(
        "SELECT secret_enc, confirmed_at, last_accepted_step \
         FROM totp_secrets WHERE account_id = ?",
    )
    .bind(account_id.as_slice())
    .fetch_optional(pool)
    .await
    .map_err(ApiError::Db)?;

    // Unknown account OR unconfirmed enrollment: run a dummy verify against a
    // fixed secret so the timing/shape matches the wrong-code path.
    let Some((secret_enc, Some(_confirmed_at), last_step)) = row.filter(|r| r.1.is_some()) else {
        let _ = verify_code(&[0u8; SECRET_LEN], now, code);
        return Ok(false);
    };

    let secret = lakko_crypto::aead::open_with_key(key, &secret_enc).map_err(|e| {
        tracing::error!(error = %e, "totp: secret open failed");
        ApiError::Internal
    })?;

    let Some(step) = verify_code(&secret, now, code).filter(|&step| step > last_step) else {
        // Wrong code OR a replayed (not-strictly-newer) code.
        return Ok(false);
    };

    // Atomic single-use-per-step consume: only one concurrent request whose
    // matched step beats the stored `last_accepted_step` wins (`rows_affected
    // == 1`). A duplicate that read the same old value loses the CAS and is
    // treated as a replayed code.
    Ok(sqlx::query(
        "UPDATE totp_secrets SET last_accepted_step = ? \
         WHERE account_id = ? AND last_accepted_step < ?",
    )
    .bind(step)
    .bind(account_id.as_slice())
    .bind(step)
    .execute(pool)
    .await
    .map_err(ApiError::Db)?
    .rows_affected()
        == 1)
}

/// `POST /totp/recover`, counted as a TOTP login (ADR-0096); the flow is
/// [`recover_answer`].
pub(crate) async fn recover(
    state: State<AppState>,
    req: Json<RecoverReq>,
) -> axum::response::Response {
    crate::metrics::login(
        crate::metrics::LoginKind::Totp,
        recover_answer(state, req).await,
    )
}

/// `POST /totp/recover` — recover ACCESS with an authenticator code.
///
/// Anonymous; names the account by login handle (ADR-0054 S5). The named
/// handle's attempt counter and the per-address bucket
/// → `429` `too-many-attempts`; otherwise verify the code (±1 window) AND that the matched step is strictly newer than the
/// last accepted step (replay protection), then issue the session triple. The
/// unknown-account / unconfirmed paths run a dummy HMAC verify and return the
/// SAME generic shapes (no enumeration).
#[axum::debug_handler]
async fn recover_answer(
    State(state): State<AppState>,
    Json(req): Json<RecoverReq>,
) -> Result<Json<RecoverRes>, ApiError> {
    let key = require_key(&state)?;
    let pepper = crate::login_handle::pepper(&state)?;
    let handle = crate::login_handle::decode_handle(&req.handle)?;
    let code = parse_code(&req.code)?;
    let device_id = crate::session::decode_device_id(req.device_id_b64.as_deref())?;
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        crate::limits::Scope::TotpRecover,
        &handle,
    )
    .await?;
    // An unknown handle resolves to its stand-in id, whose lookup below
    // misses exactly like an account without an authenticator.
    let account_id = crate::login_handle::resolve(&state.db, &pepper, &handle)
        .await?
        .account_id;
    let now = now_unix_secs();
    // The generation the pair is issued at, read BEFORE the authenticator
    // row (ADR-0074 "T3 as built"): a recall deletes that row in the
    // transaction that bumps the generation, so a recovery that straddles
    // one either loses the compare-and-set below, or is issued at the
    // generation from before the change and refused on first use.
    let key_generation = crate::session::key_generation_of(&state.db, &account_id)
        .await
        .map_err(ApiError::Db)?;

    if !accept_code(&state.db, &key, &account_id, code, now).await? {
        // No authenticator, a wrong code, or a replayed one: the generic 401.
        return Err(ApiError::InvalidCredential);
    }

    let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);
    tracing::info!("totp: code accepted, issuing session token");
    let key_generation = key_generation.ok_or(ApiError::InvalidCredential)?;
    // ADR-0099 section 4: after the code was accepted (the replay counter is
    // part of authenticating), before anything is issued.
    crate::suspension::refuse_landing_if_suspended(
        &state,
        &account_id,
        key_generation,
        &device_id,
        crate::suspension::LandingGrant::None,
    )
    .await?;
    let session = crate::session::issue_pair_at(&state, &account_id, key_generation, &device_id)?;
    Ok(Json(RecoverRes {
        account_id_b64,
        session,
    }))
}

/// Give `account_id` a confirmed authenticator under `key` and return its
/// secret (tests of other modules).
#[cfg(test)]
pub(crate) async fn enroll_for_test(
    pool: &sqlx::MySqlPool,
    key: &[u8; 32],
    account_id: &[u8; 16],
) -> Vec<u8> {
    let mut secret = [0u8; SECRET_LEN];
    OsRng.fill_bytes(&mut secret);
    let secret_enc = lakko_crypto::aead::seal_with_key(key, &secret).expect("seal");
    sqlx::query(
        "INSERT INTO totp_secrets (account_id, secret_enc, confirmed_at, last_accepted_step) \
         VALUES (?, ?, NOW(3), 0)",
    )
    .bind(account_id.as_slice())
    .bind(secret_enc.as_slice())
    .execute(pool)
    .await
    .expect("insert totp secret");
    secret.to_vec()
}

/// The code an authenticator holding `secret` shows now (tests).
#[cfg(test)]
pub(crate) fn current_code_for_test(secret: &[u8]) -> String {
    format!(
        "{:06}",
        hotp(secret, step_to_counter(current_step(now_unix_secs())))
    )
}

/// RFC 4648 base32 DECODE (test-only) — round-trips [`base32_encode`] so the
/// e2e harness can recover the secret bytes from the enroll response and
/// compute a valid current code.
#[cfg(test)]
fn base32_decode(s: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    for c in s.bytes() {
        let Some(idx) = ALPHABET.iter().position(|&a| a == c) else {
            continue;
        };
        buffer = (buffer << 5) | u32::try_from(idx).unwrap_or(0);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 4226 Appendix D — secret "12345678901234567890", counts 0..9.
    const RFC4226_SECRET: &[u8] = b"12345678901234567890";
    const RFC4226_HOTP: [u32; 10] = [
        755_224, 287_082, 359_152, 969_429, 338_314, 254_676, 287_922, 162_583, 399_871, 520_489,
    ];

    #[test]
    fn hotp_rfc4226_appendix_d_vectors() {
        for (counter, &expected) in RFC4226_HOTP.iter().enumerate() {
            assert_eq!(
                hotp(RFC4226_SECRET, counter as u64),
                expected,
                "count {counter}"
            );
        }
    }

    #[test]
    fn totp_rfc6238_appendix_b_sha1_vectors() {
        // RFC 6238 Appendix B uses an 8-digit code + a 20-byte SHA-1 seed
        // "12345678901234567890". We run 6 digits (our DIGITS), so derive the
        // expected 6-digit codes by taking the low 6 of the published 8-digit
        // values: 94287082→287082, 07081804→081804, 14050471→050471,
        // 89005924→005924, 69279037→279037, 65353130→353130.
        let cases: [(u64, u32); 6] = [
            (59, 287_082),
            (1_111_111_109, 81_804),
            (1_111_111_111, 50_471),
            (1_234_567_890, 5_924),
            (2_000_000_000, 279_037),
            (20_000_000_000, 353_130),
        ];
        for (t, expected) in cases {
            assert_eq!(totp_at(RFC4226_SECRET, t, 0), expected, "t={t}");
        }
    }

    #[test]
    fn verify_window_accepts_neighbours_and_returns_step() {
        let t = 1_111_111_111u64; // step 37037037
        let base = current_step(t);
        let code_here = totp_at(RFC4226_SECRET, t, 0);
        let code_prev = totp_at(RFC4226_SECRET, t, -1);
        let code_next = totp_at(RFC4226_SECRET, t, 1);
        assert_eq!(verify_code(RFC4226_SECRET, t, code_here), Some(base));
        assert_eq!(verify_code(RFC4226_SECRET, t, code_prev), Some(base - 1));
        assert_eq!(verify_code(RFC4226_SECRET, t, code_next), Some(base + 1));
        // A code two steps away is outside the ±1 window.
        let code_far = totp_at(RFC4226_SECRET, t, 2);
        // (guard against a coincidental collision with an in-window code)
        if code_far != code_here && code_far != code_prev && code_far != code_next {
            assert_eq!(verify_code(RFC4226_SECRET, t, code_far), None);
        }
    }

    #[test]
    fn base32_encode_rfc4648_vectors() {
        // RFC 4648 §10 test vectors (no padding).
        assert_eq!(base32_encode(b""), "");
        assert_eq!(base32_encode(b"f"), "MY");
        assert_eq!(base32_encode(b"fo"), "MZXQ");
        assert_eq!(base32_encode(b"foo"), "MZXW6");
        assert_eq!(base32_encode(b"foob"), "MZXW6YQ");
        assert_eq!(base32_encode(b"fooba"), "MZXW6YTB");
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
    }

    #[test]
    fn seal_open_round_trips_a_secret() {
        let key = [0x11u8; 32];
        let mut secret = [0u8; SECRET_LEN];
        OsRng.fill_bytes(&mut secret);
        let enc = lakko_crypto::aead::seal_with_key(&key, &secret).unwrap();
        let dec = lakko_crypto::aead::open_with_key(&key, &enc).unwrap();
        assert_eq!(dec.as_slice(), &secret);
        // Wrong key fails.
        assert!(lakko_crypto::aead::open_with_key(&[0x22u8; 32], &enc).is_err());
    }

    #[test]
    fn parse_code_rejects_bad_shapes() {
        assert!(parse_code("123456").is_ok());
        assert_eq!(parse_code("000123").unwrap(), 123);
        assert!(parse_code("12345").is_err()); // too short
        assert!(parse_code("1234567").is_err()); // too long
        assert!(parse_code("12345a").is_err()); // non-digit
        assert!(parse_code(" 123456 ").is_ok()); // trimmed
    }

    #[test]
    fn otpauth_uri_shape() {
        let uri = otpauth_uri("AccountB64", "JBSWY3DP");
        assert!(uri.starts_with("otpauth://totp/Lakko:AccountB64?secret=JBSWY3DP"));
        assert!(uri.contains("issuer=Lakko"));
        assert!(uri.contains("algorithm=SHA1"));
        assert!(uri.contains("digits=6"));
        assert!(uri.contains("period=30"));
    }

    #[test]
    fn base32_decode_round_trips_encode() {
        for v in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(base32_decode(&base32_encode(v)), v);
        }
        let mut secret = [0u8; SECRET_LEN];
        OsRng.fill_bytes(&mut secret);
        assert_eq!(base32_decode(&base32_encode(&secret)), secret);
    }

    #[test]
    fn load_key_rejects_wrong_length() {
        // 31 bytes base64 → error; we can only test the decode+len logic
        // by calling the helper indirectly through env, so test the core
        // length check shape here on a known-bad input.
        let bad = STANDARD.encode([0u8; 31]);
        let decoded = STANDARD.decode(&bad).unwrap();
        let res: Result<[u8; 32], _> = decoded.as_slice().try_into();
        assert!(res.is_err());
        let good = STANDARD.encode([0u8; 32]);
        let decoded_ok = STANDARD.decode(&good).unwrap();
        let res_ok: Result<[u8; 32], _> = decoded_ok.as_slice().try_into();
        assert!(res_ok.is_ok());
    }
}

/// DB-backed end-to-end tests. Gated on `LAKKO_AUTH_TEST_DATABASE_URL`; they
/// skip (and pass) when it is unset. Mirrors the `device_authorize` e2e
/// harness. NOTE: a SUCCESSFUL `/totp/recover` cannot be exercised here —
/// `enroll/finish` consumes the current step, so a recover code for the same
/// step is (correctly) refused as a replay, and advancing to a newer step
/// needs a 30 s wall-clock wait or a time-injection seam neither of which is
/// suitable for a unit test. The acceptance logic is covered by the pure-fn
/// `verify_window_accepts_neighbours_and_returns_step` test; this module
/// covers the DB state machine: enrollment persistence, the confirmed gate,
/// the 409 swap-refusal, replay refusal, lockout, enumeration safety, and the
/// 503-when-unconfigured loud refusal.
#[cfg(test)]
mod totp_e2e_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, StatusCode};
    use http_body_util::BodyExt as _;
    use sqlx::mysql::MySqlPoolOptions;
    use sqlx::MySqlPool;
    use std::sync::Arc;
    use tower::ServiceExt as _;
    use uuid::Uuid;

    const TOTP_KEY: [u8; 32] = [0x5Au8; 32];

    fn build_state(
        pool: MySqlPool,
        with_totp_key: bool,
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
            refresh_token_ttl_ms: 30 * 86_400_000,
            fresh_grant_ttl_ms: 600_000,
            webauthn: None,
            webauthn_action_core: None,
            call_jwt_signing: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xAB; 32])),
            attest_key: Arc::new(lakko_call_jwt::CallSigningKey::from_seed(&[0xCD; 32])),
            turn: None,
            totp_enc_key: with_totp_key.then(|| Arc::new(TOTP_KEY)),
            vault_enc_key: None,
            room_cap: std::sync::Arc::new(crate::room_cap::RoomAdmissionCap::disabled()),
            backup_head_mint_rate: crate::backup_head::MintRateConfig::disabled(),
            anon_tokens: None,
            anon_cred: None,
            handle_pepper: Some(Arc::new(crate::login_handle::TEST_PEPPER)),
            recovery_email: None,
            channel_recovery: None,
            limits: Arc::new(crate::limits::Limits::defaults()),
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

    fn bearer_for(token_secret: &lakko_auth_token::TokenSecret, account_id: &[u8; 16]) -> String {
        format!(
            "Bearer {}",
            token_secret
                .issue(account_id, 1, &[0u8; 32], 0, now_ms() + 3_600_000)
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
    async fn totp_enroll_and_state_machine() {
        let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
            eprintln!("skipping totp_enroll_and_state_machine: LAKKO_AUTH_TEST_DATABASE_URL unset");
            return;
        };
        let _ = tracing_subscriber::fmt()
            .with_env_filter("lakko_svc_auth=debug")
            .with_test_writer()
            .try_init();
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("migrations");

        // (1) 503 when the server has no TOTP key configured.
        {
            let (state_off, ts) = build_state(pool.clone(), false);
            let account_id: [u8; 16] = Uuid::new_v4().into_bytes();
            sqlx::query("INSERT INTO accounts (id) VALUES (?)")
                .bind(account_id.as_slice())
                .execute(&pool)
                .await
                .unwrap();
            let bearer = bearer_for(&ts, &account_id);
            let (st, _) = json_post(
                &state_off,
                "/totp/enroll/start",
                Some(&bearer),
                serde_json::json!({}),
            )
            .await;
            assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "503 when key unset");
        }

        let (state, ts) = build_state(pool.clone(), true);
        let account_id: [u8; 16] = Uuid::new_v4().into_bytes();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(account_id.as_slice())
            .execute(&pool)
            .await
            .unwrap();
        let bearer = bearer_for(&ts, &account_id);
        // ADR-0054 S5: recovery names the account by its login handle.
        let (handle, known_b64) = crate::login_handle::random_handle();
        crate::login_handle::bind_for_test(&pool, &account_id, &handle).await;

        // (2) enroll/start -> secret; compute the current code; enroll/finish.
        let (st, body) = json_post(
            &state,
            "/totp/enroll/start",
            Some(&bearer),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let secret_b32 = v["secret_b32"].as_str().unwrap();
        let secret = base32_decode(secret_b32);
        let code = format!("{:06}", verify_code_now(&secret));

        let (st, _) = json_post(
            &state,
            "/totp/enroll/finish",
            Some(&bearer),
            serde_json::json!({ "code": code }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "enroll/finish confirms");

        // (3) re-enroll after confirmation -> 409.
        let (st, _) = json_post(
            &state,
            "/totp/enroll/start",
            Some(&bearer),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT, "swap of confirmed TOTP refused");

        // (4) recover with the SAME code -> 401 (replay: enroll consumed this step).
        let (st, _) = json_post(
            &state,
            "/totp/recover",
            None,
            serde_json::json!({ "handle": known_b64, "code": code }),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "replayed step refused");

        // (5) unknown account -> enumeration-safe 401 (same shape).
        let (_, unknown_b64) = crate::login_handle::random_handle();
        let (st, _) = json_post(
            &state,
            "/totp/recover",
            None,
            serde_json::json!({ "handle": unknown_b64, "code": "000000" }),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "unknown account enum-safe");

        // (6) the attempt limit (5 per 15 min by default) trips for the known
        // account and for the unknown one at the same count, with the same
        // 429 body. Each has one attempt spent above; "000000" is
        // overwhelmingly unlikely to be the live code (and would 401 as a
        // replay anyway).
        let limit = state
            .limits
            .name_window(crate::limits::Scope::TotpRecover)
            .limit;
        for id in [&known_b64, &unknown_b64] {
            for _ in 1..limit {
                let (st, _) = json_post(
                    &state,
                    "/totp/recover",
                    None,
                    serde_json::json!({ "handle": id, "code": "000000" }),
                )
                .await;
                assert_eq!(st, StatusCode::UNAUTHORIZED, "under the limit: plain 401");
            }
        }
        let mut refusals = Vec::new();
        for id in [&known_b64, &unknown_b64] {
            let (st, body) = json_post(
                &state,
                "/totp/recover",
                None,
                serde_json::json!({ "handle": id, "code": "000000" }),
            )
            .await;
            assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "limit reached");
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["error"], crate::limits::CODE_TOO_MANY_ATTEMPTS);
            assert!(v["retry_after_secs"].as_u64().unwrap() >= 1);
            refusals.push(v["error"].clone());
        }
        assert_eq!(refusals[0], refusals[1], "known and unknown refused alike");
    }

    /// The 6-digit code for `secret` at the server's current step.
    fn verify_code_now(secret: &[u8]) -> u32 {
        let now = now_unix_secs();
        hotp(secret, step_to_counter(current_step(now)))
    }
}
